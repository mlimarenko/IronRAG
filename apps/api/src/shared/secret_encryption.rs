//! Authenticated encryption boundary for credentials persisted in `PostgreSQL`.
//!
//! New values use a row-bound v3 XChaCha20-Poly1305 envelope carrying an
//! authenticated key identifier. A bounded keyring keeps older v3 envelopes
//! and legacy v1/v2 envelopes readable during an explicit rotation window.
//! Existing rows outside the reserved `ironrag:enc:` namespace are deliberately
//! treated as legacy plaintext so operators can rewrap them with the explicit
//! maintenance command. Anything inside the reserved namespace is parsed
//! strictly and never falls back to plaintext.

use std::{fmt, sync::Arc};

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use chacha20poly1305::{
    Key, KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use rand::RngExt as _;
use thiserror::Error;
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

const MASTER_KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 24;
const AUTH_TAG_BYTES: usize = 16;
const ENVELOPE_NAMESPACE: &str = "ironrag:enc:";
#[cfg(test)]
const ENVELOPE_PREFIX_V1: &str = "ironrag:enc:v1:xchacha20poly1305:";
#[cfg(test)]
const ENVELOPE_PREFIX_V2: &str = "ironrag:enc:v2:xchacha20poly1305:";
const ENVELOPE_PREFIX_V3: &str = "ironrag:enc:v3:xchacha20poly1305:";
const MAX_ENVELOPE_BYTES: usize = 8_192;
const DEFAULT_ACTIVE_KEY_ID: &str = "default";
const MAX_KEY_ID_BYTES: usize = 32;
const MAX_PREVIOUS_KEY_MAP_BYTES: usize = 1_024;

/// Maximum number of previous keys accepted during one rotation window.
pub const MAX_PREVIOUS_CREDENTIAL_KEYS: usize = 8;

/// Upper bound for any persisted API key or webhook signing secret.
pub const MAX_PLAINTEXT_SECRET_BYTES: usize = 4_096;

/// The database field protected by an envelope.
///
/// A purpose-specific AAD value prevents a valid ciphertext copied between
/// credential-bearing columns from decrypting in a different column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretPurpose {
    AiAccountApiKey,
    WebhookSigningSecret,
    /// Serialized outbound webhook custom headers stored on a subscription.
    WebhookCustomHeaders,
}

impl SecretPurpose {
    const fn legacy_v1_aad(self) -> &'static [u8] {
        match self {
            Self::AiAccountApiKey => b"ironrag:secret:v1:ai_account.api_key",
            Self::WebhookSigningSecret => b"ironrag:secret:v1:webhook_subscription.secret",
            Self::WebhookCustomHeaders => {
                b"ironrag:secret:v1:webhook_subscription.custom_headers_json"
            }
        }
    }

    const fn storage_label(self) -> &'static [u8] {
        match self {
            Self::AiAccountApiKey => b"ai_account.api_key",
            Self::WebhookSigningSecret => b"webhook_subscription.secret",
            Self::WebhookCustomHeaders => b"webhook_subscription.custom_headers_json",
        }
    }

    fn row_bound_v2_aad(self, record_id: Uuid) -> Vec<u8> {
        const PREFIX: &[u8] = b"ironrag:secret:v2\0";
        let label = self.storage_label();
        let mut aad = Vec::with_capacity(PREFIX.len() + label.len() + 1 + 16);
        aad.extend_from_slice(PREFIX);
        aad.extend_from_slice(label);
        aad.push(0);
        aad.extend_from_slice(record_id.as_bytes());
        aad
    }

    fn row_bound_v3_aad(self, record_id: Uuid, key_id: &str) -> Vec<u8> {
        const PREFIX: &[u8] = b"ironrag:secret:v3\0";
        let label = self.storage_label();
        let mut aad = Vec::with_capacity(PREFIX.len() + label.len() + 1 + 16 + 1 + key_id.len());
        aad.extend_from_slice(PREFIX);
        aad.extend_from_slice(label);
        aad.push(0);
        aad.extend_from_slice(record_id.as_bytes());
        aad.push(0);
        aad.extend_from_slice(key_id.as_bytes());
        aad
    }
}

/// Storage representation observed while decrypting a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretStorageFormat {
    EncryptedV1,
    EncryptedV2,
    EncryptedV3,
    LegacyPlaintext,
}

/// A ciphertext that is safe to pass into repository write functions.
///
/// Its constructor is private: callers can only obtain one through
/// [`CredentialCipher::encrypt`].
#[derive(Clone)]
pub struct EncryptedSecret {
    envelope: String,
    purpose: SecretPurpose,
    record_id: Uuid,
}

impl EncryptedSecret {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.envelope
    }

    #[must_use]
    pub fn is_bound_to(&self, purpose: SecretPurpose, record_id: Uuid) -> bool {
        self.purpose == purpose && self.record_id == record_id
    }
}

impl fmt::Debug for EncryptedSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EncryptedSecret(<redacted>)")
    }
}

/// Decrypted material with redacted `Debug` and best-effort zeroization.
pub struct DecryptedSecret {
    value: Zeroizing<String>,
    storage_format: SecretStorageFormat,
}

impl DecryptedSecret {
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        self.value.as_str()
    }

    #[must_use]
    pub const fn storage_format(&self) -> SecretStorageFormat {
        self.storage_format
    }
}

impl fmt::Debug for DecryptedSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecryptedSecret")
            .field("value", &"<redacted>")
            .field("storage_format", &self.storage_format)
            .finish()
    }
}

#[derive(Error, Debug)]
pub enum SecretEncryptionError {
    #[error("credential master key is not configured")]
    MasterKeyNotConfigured,
    #[error("credential master key must be canonical base64 encoding of exactly 32 bytes")]
    InvalidMasterKey,
    #[error("credential master key id is invalid")]
    InvalidKeyId,
    #[error("credential previous-key map is invalid")]
    InvalidPreviousKeyMap,
    #[error("stored credential envelope references an unavailable key id")]
    UnknownKeyId,
    #[error("secret plaintext is empty or exceeds the configured size limit")]
    InvalidPlaintext,
    #[error("stored credential envelope is malformed")]
    InvalidEnvelope,
    #[error("stored credential envelope version or algorithm is unsupported")]
    UnsupportedEnvelope,
    #[error("credential encryption failed")]
    EncryptionFailed,
    #[error("credential decryption or authentication failed")]
    DecryptionFailed,
}

struct MasterKey([u8; MASTER_KEY_BYTES]);

impl Drop for MasterKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

struct CredentialKey {
    id: String,
    material: MasterKey,
}

struct CredentialKeyring {
    active: CredentialKey,
    previous: Vec<CredentialKey>,
}

impl CredentialKeyring {
    fn find(&self, key_id: &str) -> Option<&MasterKey> {
        if self.active.id == key_id {
            return Some(&self.active.material);
        }
        self.previous.iter().find(|key| key.id == key_id).map(|key| &key.material)
    }

    fn all_material(&self) -> impl Iterator<Item = &MasterKey> {
        std::iter::once(&self.active.material).chain(self.previous.iter().map(|key| &key.material))
    }
}

/// Process-wide credential cipher backed by an optional bounded keyring.
///
/// Clones share one zeroizing keyring allocation; key bytes are never cloned or
/// included in `Debug`. A disabled instance supports legacy reads only.
#[derive(Clone, Default)]
pub struct CredentialCipher {
    keyring: Option<Arc<CredentialKeyring>>,
}

impl fmt::Debug for CredentialCipher {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialCipher")
            .field("configured", &self.keyring.is_some())
            .field(
                "key_count",
                &self.keyring.as_ref().map_or(0, |keyring| 1 + keyring.previous.len()),
            )
            .finish()
    }
}

impl CredentialCipher {
    /// Parses the optional dedicated credential master key.
    ///
    /// `None` is a supported compatibility state. A present value must be the
    /// canonical padded standard-base64 encoding of exactly 32 random bytes.
    ///
    /// # Errors
    /// Returns [`SecretEncryptionError::InvalidMasterKey`] for an empty,
    /// non-canonical, malformed or incorrectly sized configured value.
    pub fn from_optional_base64(
        encoded_master_key: Option<&str>,
    ) -> Result<Self, SecretEncryptionError> {
        Self::from_keyring_base64(None, encoded_master_key, None)
    }

    /// Parses an active credential key and an optional bounded previous-key map.
    ///
    /// `active_key_id` defaults to `default` when the existing
    /// `IRONRAG_CREDENTIAL_MASTER_KEY` configuration is used by itself. Explicit
    /// identifiers contain 1-32 lowercase ASCII letters, digits, `.`, `_` or
    /// `-`, begin with a letter or digit, and are embedded in new v3 envelopes.
    /// The previous-key map is a comma-separated, strictly key-id-sorted list of
    /// `key-id=canonical-base64-key` entries with no whitespace and at most
    /// [`MAX_PREVIOUS_CREDENTIAL_KEYS`] entries.
    ///
    /// `None` for all three inputs is a supported compatibility state. A key ID
    /// or previous-key map without an active key is rejected.
    /// Encoded inputs are borrowed; their owner must zeroize those source
    /// buffers after construction. Application state does so before retaining
    /// or cloning settings.
    ///
    /// # Errors
    /// Returns a typed error when identifiers or encoded inputs are invalid.
    /// Duplicate or unsorted entries, active-ID reuse, and oversized rotation
    /// sets are rejected as well.
    pub fn from_keyring_base64(
        active_key_id: Option<&str>,
        encoded_active_key: Option<&str>,
        encoded_previous_key_map: Option<&str>,
    ) -> Result<Self, SecretEncryptionError> {
        let Some(encoded_active_key) = encoded_active_key else {
            if active_key_id.is_some() || encoded_previous_key_map.is_some() {
                return Err(SecretEncryptionError::MasterKeyNotConfigured);
            }
            return Ok(Self::default());
        };

        let active_id = active_key_id.unwrap_or(DEFAULT_ACTIVE_KEY_ID);
        if !is_valid_key_id(active_id) {
            return Err(SecretEncryptionError::InvalidKeyId);
        }
        let active = CredentialKey {
            id: active_id.to_owned(),
            material: decode_master_key(encoded_active_key)?,
        };
        let previous = parse_previous_key_map(encoded_previous_key_map, active_id)?;
        Ok(Self { keyring: Some(Arc::new(CredentialKeyring { active, previous })) })
    }

    /// Fails unless an encryption key is configured.
    ///
    /// The explicit maintenance migration calls this before selecting rows so
    /// it cannot begin a partial migration in a misconfigured process.
    ///
    /// # Errors
    /// Returns [`SecretEncryptionError::MasterKeyNotConfigured`] when disabled.
    pub fn require_configured(&self) -> Result<(), SecretEncryptionError> {
        self.keyring.as_ref().map(|_| ()).ok_or(SecretEncryptionError::MasterKeyNotConfigured)
    }

    /// Classifies a stored value without exposing or copying its plaintext.
    ///
    /// Reserved-namespace envelopes are structurally and canonically parsed;
    /// unknown versions or malformed payloads fail closed.
    ///
    /// # Errors
    /// Returns a typed error for empty/oversized legacy values or invalid
    /// envelopes.
    pub fn storage_format(
        &self,
        stored_value: &str,
    ) -> Result<SecretStorageFormat, SecretEncryptionError> {
        if !stored_value.starts_with(ENVELOPE_NAMESPACE) {
            validate_plaintext(stored_value)?;
            return Ok(SecretStorageFormat::LegacyPlaintext);
        }
        if stored_value.len() > MAX_ENVELOPE_BYTES {
            return Err(SecretEncryptionError::InvalidEnvelope);
        }
        let encoded_fields = parse_envelope(stored_value)?;
        decode_canonical_url_field(encoded_fields.nonce, NONCE_BYTES, NONCE_BYTES)?;
        decode_canonical_url_field(
            encoded_fields.ciphertext,
            AUTH_TAG_BYTES + 1,
            MAX_PLAINTEXT_SECRET_BYTES + AUTH_TAG_BYTES,
        )?;
        Ok(match encoded_fields.version {
            EnvelopeVersion::V1 => SecretStorageFormat::EncryptedV1,
            EnvelopeVersion::V2 => SecretStorageFormat::EncryptedV2,
            EnvelopeVersion::V3 => SecretStorageFormat::EncryptedV3,
        })
    }

    /// Reports whether a stored secret must be rewrapped by the active key.
    ///
    /// Plaintext and v1/v2 values always require rewrapping. A v3 envelope is
    /// current only when its validated key ID matches the configured active key
    /// ID. An unknown v3 key ID fails closed rather than being treated as
    /// plaintext or as an already-current value.
    ///
    /// # Errors
    /// Returns a typed error for missing key configuration, malformed values,
    /// or a v3 envelope whose key ID is absent from the configured keyring.
    pub fn needs_rewrap(&self, stored_value: &str) -> Result<bool, SecretEncryptionError> {
        let storage_format = self.storage_format(stored_value)?;
        if storage_format != SecretStorageFormat::EncryptedV3 {
            return Ok(true);
        }
        let encoded_fields = parse_envelope(stored_value)?;
        let key_id = encoded_fields.key_id.ok_or(SecretEncryptionError::InvalidEnvelope)?;
        let keyring = self.keyring.as_ref().ok_or(SecretEncryptionError::MasterKeyNotConfigured)?;
        if keyring.find(key_id).is_none() {
            return Err(SecretEncryptionError::UnknownKeyId);
        }
        Ok(key_id != keyring.active.id)
    }

    /// Encrypts one non-empty secret into the canonical row-bound v3 envelope.
    ///
    /// # Errors
    /// Fails closed when no key is configured, the input is out of bounds, or
    /// the AEAD implementation rejects the operation.
    pub fn encrypt(
        &self,
        purpose: SecretPurpose,
        record_id: Uuid,
        plaintext: &str,
    ) -> Result<EncryptedSecret, SecretEncryptionError> {
        validate_plaintext(plaintext)?;
        let keyring = self.keyring.as_ref().ok_or(SecretEncryptionError::MasterKeyNotConfigured)?;
        let cipher = XChaCha20Poly1305::new(Key::from_slice(&keyring.active.material.0));
        let aad = purpose.row_bound_v3_aad(record_id, &keyring.active.id);
        let mut nonce = [0_u8; NONCE_BYTES];
        rand::rng().fill(&mut nonce);
        let ciphertext = cipher
            .encrypt(XNonce::from_slice(&nonce), Payload { msg: plaintext.as_bytes(), aad: &aad })
            .map_err(|_| SecretEncryptionError::EncryptionFailed)?;
        let envelope = format!(
            "{ENVELOPE_PREFIX_V3}{}:{}:{}",
            keyring.active.id,
            URL_SAFE_NO_PAD.encode(nonce),
            URL_SAFE_NO_PAD.encode(ciphertext)
        );
        if envelope.len() > MAX_ENVELOPE_BYTES {
            return Err(SecretEncryptionError::EncryptionFailed);
        }
        Ok(EncryptedSecret { envelope, purpose, record_id })
    }

    /// Decrypts keyed v3, row-bound v2 or purpose-bound v1 ciphertext, or reads
    /// a narrowly scoped legacy plaintext row.
    ///
    /// Any value beginning with the reserved `ironrag:enc:` namespace is
    /// parsed strictly. Unknown versions, malformed fields and authentication
    /// failures never fall back to plaintext.
    ///
    /// # Errors
    /// Returns a typed, redacted failure for malformed envelopes, missing
    /// keys, authentication failures, invalid UTF-8, or out-of-bounds values.
    pub fn decrypt(
        &self,
        purpose: SecretPurpose,
        record_id: Uuid,
        stored_value: &str,
    ) -> Result<DecryptedSecret, SecretEncryptionError> {
        if self.storage_format(stored_value)? == SecretStorageFormat::LegacyPlaintext {
            return Ok(DecryptedSecret {
                value: Zeroizing::new(stored_value.to_owned()),
                storage_format: SecretStorageFormat::LegacyPlaintext,
            });
        }
        let encoded_fields = parse_envelope(stored_value)?;
        let nonce = decode_canonical_url_field(encoded_fields.nonce, NONCE_BYTES, NONCE_BYTES)?;
        let ciphertext = decode_canonical_url_field(
            encoded_fields.ciphertext,
            AUTH_TAG_BYTES + 1,
            MAX_PLAINTEXT_SECRET_BYTES + AUTH_TAG_BYTES,
        )?;
        let storage_format = match encoded_fields.version {
            EnvelopeVersion::V1 => SecretStorageFormat::EncryptedV1,
            EnvelopeVersion::V2 => SecretStorageFormat::EncryptedV2,
            EnvelopeVersion::V3 => SecretStorageFormat::EncryptedV3,
        };
        let row_bound_aad;
        let keyed_aad;
        let aad = match encoded_fields.version {
            EnvelopeVersion::V1 => purpose.legacy_v1_aad(),
            EnvelopeVersion::V2 => {
                row_bound_aad = purpose.row_bound_v2_aad(record_id);
                row_bound_aad.as_slice()
            }
            EnvelopeVersion::V3 => {
                let key_id = encoded_fields.key_id.ok_or(SecretEncryptionError::InvalidEnvelope)?;
                keyed_aad = purpose.row_bound_v3_aad(record_id, key_id);
                keyed_aad.as_slice()
            }
        };
        let keyring = self.keyring.as_ref().ok_or(SecretEncryptionError::MasterKeyNotConfigured)?;
        let plaintext = match encoded_fields.version {
            EnvelopeVersion::V1 | EnvelopeVersion::V2 => keyring
                .all_material()
                .find_map(|material| decrypt_with_key(material, &nonce, &ciphertext, aad).ok())
                .ok_or(SecretEncryptionError::DecryptionFailed)?,
            EnvelopeVersion::V3 => {
                let key_id = encoded_fields.key_id.ok_or(SecretEncryptionError::InvalidEnvelope)?;
                let material = keyring.find(key_id).ok_or(SecretEncryptionError::UnknownKeyId)?;
                decrypt_with_key(material, &nonce, &ciphertext, aad)?
            }
        };
        decrypted_secret(plaintext, storage_format)
    }
}

struct EncodedEnvelopeFields<'value> {
    version: EnvelopeVersion,
    key_id: Option<&'value str>,
    nonce: &'value str,
    ciphertext: &'value str,
}

#[derive(Clone, Copy)]
enum EnvelopeVersion {
    V1,
    V2,
    V3,
}

fn parse_envelope(stored_value: &str) -> Result<EncodedEnvelopeFields<'_>, SecretEncryptionError> {
    let remainder = stored_value
        .strip_prefix(ENVELOPE_NAMESPACE)
        .ok_or(SecretEncryptionError::InvalidEnvelope)?;
    let mut fields = remainder.split(':');
    let version = fields.next().ok_or(SecretEncryptionError::InvalidEnvelope)?;
    let version = match version {
        "v1" => EnvelopeVersion::V1,
        "v2" => EnvelopeVersion::V2,
        "v3" => EnvelopeVersion::V3,
        _ => return Err(SecretEncryptionError::UnsupportedEnvelope),
    };
    let algorithm = fields.next().ok_or(SecretEncryptionError::InvalidEnvelope)?;
    if algorithm != "xchacha20poly1305" {
        return Err(SecretEncryptionError::UnsupportedEnvelope);
    }
    let key_id = if matches!(version, EnvelopeVersion::V3) {
        let key_id = fields.next().ok_or(SecretEncryptionError::InvalidEnvelope)?;
        if !is_valid_key_id(key_id) {
            return Err(SecretEncryptionError::InvalidEnvelope);
        }
        Some(key_id)
    } else {
        None
    };
    let nonce = fields.next().ok_or(SecretEncryptionError::InvalidEnvelope)?;
    let ciphertext = fields.next().ok_or(SecretEncryptionError::InvalidEnvelope)?;
    if nonce.is_empty() || ciphertext.is_empty() || fields.next().is_some() {
        return Err(SecretEncryptionError::InvalidEnvelope);
    }
    Ok(EncodedEnvelopeFields { version, key_id, nonce, ciphertext })
}

fn decode_master_key(encoded_master_key: &str) -> Result<MasterKey, SecretEncryptionError> {
    if encoded_master_key.len() != 44 {
        return Err(SecretEncryptionError::InvalidMasterKey);
    }
    let decoded = Zeroizing::new(
        STANDARD.decode(encoded_master_key).map_err(|_| SecretEncryptionError::InvalidMasterKey)?,
    );
    let canonical_encoding = Zeroizing::new(STANDARD.encode(decoded.as_slice()));
    if decoded.len() != MASTER_KEY_BYTES || canonical_encoding.as_str() != encoded_master_key {
        return Err(SecretEncryptionError::InvalidMasterKey);
    }
    let mut key = [0_u8; MASTER_KEY_BYTES];
    key.copy_from_slice(decoded.as_slice());
    Ok(MasterKey(key))
}

fn parse_previous_key_map(
    encoded_previous_key_map: Option<&str>,
    active_key_id: &str,
) -> Result<Vec<CredentialKey>, SecretEncryptionError> {
    let Some(encoded_previous_key_map) = encoded_previous_key_map else {
        return Ok(Vec::new());
    };
    if encoded_previous_key_map.is_empty()
        || encoded_previous_key_map.len() > MAX_PREVIOUS_KEY_MAP_BYTES
        || encoded_previous_key_map.bytes().any(|byte| byte.is_ascii_whitespace())
    {
        return Err(SecretEncryptionError::InvalidPreviousKeyMap);
    }

    let entries = encoded_previous_key_map.split(',');
    let mut previous = Vec::new();
    let mut last_key_id: Option<&str> = None;
    for entry in entries {
        if previous.len() == MAX_PREVIOUS_CREDENTIAL_KEYS {
            return Err(SecretEncryptionError::InvalidPreviousKeyMap);
        }
        let (key_id, encoded_key) =
            entry.split_once('=').ok_or(SecretEncryptionError::InvalidPreviousKeyMap)?;
        if !is_valid_key_id(key_id) {
            return Err(SecretEncryptionError::InvalidPreviousKeyMap);
        }
        if key_id == active_key_id || last_key_id.is_some_and(|last| last >= key_id) {
            return Err(SecretEncryptionError::InvalidPreviousKeyMap);
        }
        let material = decode_master_key(encoded_key)
            .map_err(|_| SecretEncryptionError::InvalidPreviousKeyMap)?;
        previous.push(CredentialKey { id: key_id.to_owned(), material });
        last_key_id = Some(key_id);
    }
    Ok(previous)
}

fn is_valid_key_id(key_id: &str) -> bool {
    let bytes = key_id.as_bytes();
    let Some(first) = bytes.first() else {
        return false;
    };
    if bytes.len() > MAX_KEY_ID_BYTES || !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    !bytes.iter().any(|byte| {
        !byte.is_ascii_lowercase() && !byte.is_ascii_digit() && !matches!(byte, b'.' | b'_' | b'-')
    })
}

fn decrypt_with_key(
    master_key: &MasterKey,
    nonce: &[u8],
    ciphertext: &[u8],
    aad: &[u8],
) -> Result<Zeroizing<Vec<u8>>, SecretEncryptionError> {
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&master_key.0));
    cipher
        .decrypt(XNonce::from_slice(nonce), Payload { msg: ciphertext, aad })
        .map(Zeroizing::new)
        .map_err(|_| SecretEncryptionError::DecryptionFailed)
}

fn decrypted_secret(
    plaintext: Zeroizing<Vec<u8>>,
    storage_format: SecretStorageFormat,
) -> Result<DecryptedSecret, SecretEncryptionError> {
    if plaintext.is_empty() || plaintext.len() > MAX_PLAINTEXT_SECRET_BYTES {
        return Err(SecretEncryptionError::DecryptionFailed);
    }
    let plaintext = std::str::from_utf8(plaintext.as_slice())
        .map_err(|_| SecretEncryptionError::DecryptionFailed)?;
    Ok(DecryptedSecret { value: Zeroizing::new(plaintext.to_owned()), storage_format })
}

fn decode_canonical_url_field(
    encoded: &str,
    min_decoded_bytes: usize,
    max_decoded_bytes: usize,
) -> Result<Vec<u8>, SecretEncryptionError> {
    if encoded.len() > MAX_ENVELOPE_BYTES || encoded.contains('=') {
        return Err(SecretEncryptionError::InvalidEnvelope);
    }
    let decoded =
        URL_SAFE_NO_PAD.decode(encoded).map_err(|_| SecretEncryptionError::InvalidEnvelope)?;
    if decoded.len() < min_decoded_bytes
        || decoded.len() > max_decoded_bytes
        || URL_SAFE_NO_PAD.encode(&decoded) != encoded
    {
        return Err(SecretEncryptionError::InvalidEnvelope);
    }
    Ok(decoded)
}

const fn validate_plaintext(plaintext: &str) -> Result<(), SecretEncryptionError> {
    if plaintext.is_empty() || plaintext.len() > MAX_PLAINTEXT_SECRET_BYTES {
        return Err(SecretEncryptionError::InvalidPlaintext);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use chacha20poly1305::{
        Key, KeyInit, XChaCha20Poly1305, XNonce,
        aead::{Aead, Payload},
    };
    use uuid::Uuid;

    use super::{
        CredentialCipher, MAX_PLAINTEXT_SECRET_BYTES, SecretEncryptionError, SecretPurpose,
        SecretStorageFormat,
    };

    fn encoded_key(byte: u8) -> String {
        STANDARD.encode([byte; 32])
    }

    fn record_id() -> Uuid {
        Uuid::from_u128(0x1020_3040_5060_7080_90a0_b0c0_d0e0_f001)
    }

    fn previous_key_map(entries: &[(&str, u8)]) -> String {
        entries
            .iter()
            .map(|(key_id, byte)| format!("{key_id}={}", encoded_key(*byte)))
            .collect::<Vec<_>>()
            .join(",")
    }

    fn legacy_envelope(
        version: super::EnvelopeVersion,
        key_byte: u8,
        purpose: SecretPurpose,
        owner_id: Uuid,
        plaintext: &[u8],
    ) -> String {
        let key = [key_byte; 32];
        let aead = XChaCha20Poly1305::new(Key::from_slice(&key));
        let nonce = [key_byte.wrapping_add(1); super::NONCE_BYTES];
        let row_bound_aad;
        let aad = match version {
            super::EnvelopeVersion::V1 => purpose.legacy_v1_aad(),
            super::EnvelopeVersion::V2 => {
                row_bound_aad = purpose.row_bound_v2_aad(owner_id);
                row_bound_aad.as_slice()
            }
            super::EnvelopeVersion::V3 => unreachable!("v3 uses a keyed envelope helper"),
        };
        let ciphertext = aead
            .encrypt(XNonce::from_slice(&nonce), Payload { msg: plaintext, aad })
            .expect("synthetic legacy value should encrypt");
        let prefix = match version {
            super::EnvelopeVersion::V1 => super::ENVELOPE_PREFIX_V1,
            super::EnvelopeVersion::V2 => super::ENVELOPE_PREFIX_V2,
            super::EnvelopeVersion::V3 => unreachable!("v3 uses a keyed envelope helper"),
        };
        format!(
            "{prefix}{}:{}",
            super::URL_SAFE_NO_PAD.encode(nonce),
            super::URL_SAFE_NO_PAD.encode(ciphertext)
        )
    }

    #[test]
    fn missing_master_key_allows_legacy_reads_but_rejects_new_encryption() {
        let cipher = CredentialCipher::from_optional_base64(None)
            .expect("missing key is a supported disabled state");

        let legacy = cipher
            .decrypt(SecretPurpose::AiAccountApiKey, record_id(), "legacy-value")
            .expect("legacy plaintext remains readable during migration");

        assert_eq!(legacy.expose_secret(), "legacy-value");
        assert_eq!(legacy.storage_format(), SecretStorageFormat::LegacyPlaintext);
        assert!(matches!(
            cipher.encrypt(SecretPurpose::AiAccountApiKey, record_id(), "new-value"),
            Err(SecretEncryptionError::MasterKeyNotConfigured)
        ));
    }

    #[test]
    fn configured_master_key_is_strict_canonical_base64_of_exactly_32_bytes() {
        assert!(matches!(
            CredentialCipher::from_optional_base64(Some("not-base64")),
            Err(SecretEncryptionError::InvalidMasterKey)
        ));
        assert!(matches!(
            CredentialCipher::from_optional_base64(Some(&STANDARD.encode([7_u8; 31]))),
            Err(SecretEncryptionError::InvalidMasterKey)
        ));

        let non_canonical = encoded_key(9).trim_end_matches('=').to_string();
        assert!(matches!(
            CredentialCipher::from_optional_base64(Some(&non_canonical)),
            Err(SecretEncryptionError::InvalidMasterKey)
        ));

        let canonical = encoded_key(9);
        let non_canonical_trailing_bits = format!("{}l=", &canonical[..42]);
        assert!(matches!(
            CredentialCipher::from_optional_base64(Some(&non_canonical_trailing_bits)),
            Err(SecretEncryptionError::InvalidMasterKey)
        ));
        assert!(CredentialCipher::from_optional_base64(Some(&canonical)).is_ok());
    }

    #[test]
    fn keyring_configuration_is_bounded_canonical_and_unambiguous() {
        for invalid_key_id in ["", "Uppercase", "-leading", "has space", "x/y"] {
            assert!(matches!(
                CredentialCipher::from_keyring_base64(
                    Some(invalid_key_id),
                    Some(&encoded_key(1)),
                    None,
                ),
                Err(SecretEncryptionError::InvalidKeyId)
            ));
        }
        assert!(matches!(
            CredentialCipher::from_keyring_base64(
                Some(&"x".repeat(super::MAX_KEY_ID_BYTES + 1)),
                Some(&encoded_key(1)),
                None,
            ),
            Err(SecretEncryptionError::InvalidKeyId)
        ));

        for invalid_map in [
            String::new(),
            format!("old-b={},old-a={}", encoded_key(2), encoded_key(3)),
            format!("old-a={},old-a={}", encoded_key(2), encoded_key(3)),
            format!("current={}", encoded_key(2)),
            format!("old-a ={}", encoded_key(2)),
            format!("old-a={},", encoded_key(2)),
            "old-a=not-base64".to_string(),
            "old-a".to_string(),
        ] {
            assert!(matches!(
                CredentialCipher::from_keyring_base64(
                    Some("current"),
                    Some(&encoded_key(1)),
                    Some(&invalid_map),
                ),
                Err(SecretEncryptionError::InvalidPreviousKeyMap)
            ));
        }

        let maximum_map = previous_key_map(&[
            ("old-0", 10),
            ("old-1", 11),
            ("old-2", 12),
            ("old-3", 13),
            ("old-4", 14),
            ("old-5", 15),
            ("old-6", 16),
            ("old-7", 17),
        ]);
        assert_eq!(super::MAX_PREVIOUS_CREDENTIAL_KEYS, 8);
        assert!(
            CredentialCipher::from_keyring_base64(
                Some("current"),
                Some(&encoded_key(1)),
                Some(&maximum_map),
            )
            .is_ok()
        );

        let over_limit = format!("{maximum_map},old-z={}", encoded_key(99));
        assert!(matches!(
            CredentialCipher::from_keyring_base64(
                Some("current"),
                Some(&encoded_key(1)),
                Some(&over_limit),
            ),
            Err(SecretEncryptionError::InvalidPreviousKeyMap)
        ));
        assert!(matches!(
            CredentialCipher::from_keyring_base64(Some("current"), None, None),
            Err(SecretEncryptionError::MasterKeyNotConfigured)
        ));
    }

    #[test]
    fn active_key_encrypts_and_previous_key_decrypts_older_v3_and_v1_v2() {
        let old_cipher =
            CredentialCipher::from_keyring_base64(Some("old-2026"), Some(&encoded_key(21)), None)
                .expect("valid old key");
        let old_v3 = old_cipher
            .encrypt(SecretPurpose::AiAccountApiKey, record_id(), "old-v3-value")
            .expect("encrypt old v3 value");
        let previous = previous_key_map(&[("old-2026", 21)]);
        let rotated = CredentialCipher::from_keyring_base64(
            Some("new-2026"),
            Some(&encoded_key(22)),
            Some(&previous),
        )
        .expect("valid rotated keyring");

        assert!(old_v3.as_str().starts_with("ironrag:enc:v3:xchacha20poly1305:old-2026:"));
        assert_eq!(
            rotated
                .decrypt(SecretPurpose::AiAccountApiKey, record_id(), old_v3.as_str())
                .expect("previous key should decrypt old v3")
                .expose_secret(),
            "old-v3-value"
        );
        assert!(rotated.needs_rewrap(old_v3.as_str()).expect("old v3 must be rewrapped"));

        for version in [super::EnvelopeVersion::V1, super::EnvelopeVersion::V2] {
            let envelope = legacy_envelope(
                version,
                21,
                SecretPurpose::AiAccountApiKey,
                record_id(),
                b"legacy-value",
            );
            assert_eq!(
                rotated
                    .decrypt(SecretPurpose::AiAccountApiKey, record_id(), &envelope)
                    .expect("previous key should decrypt legacy envelope")
                    .expose_secret(),
                "legacy-value"
            );
            assert!(rotated.needs_rewrap(&envelope).expect("legacy envelope needs rewrap"));
        }

        let current = rotated
            .encrypt(SecretPurpose::AiAccountApiKey, record_id(), "current-value")
            .expect("active key should encrypt");
        assert!(current.as_str().starts_with("ironrag:enc:v3:xchacha20poly1305:new-2026:"));
        assert!(!rotated.needs_rewrap(current.as_str()).expect("new v3 is current"));
    }

    #[test]
    fn unknown_or_tampered_v3_key_id_fails_closed() {
        let duplicate_material_map = previous_key_map(&[("old-key", 25)]);
        let cipher = CredentialCipher::from_keyring_base64(
            Some("new-key"),
            Some(&encoded_key(25)),
            Some(&duplicate_material_map),
        )
        .expect("valid keyring");
        let encrypted = cipher
            .encrypt(SecretPurpose::AiAccountApiKey, record_id(), "key-id-bound")
            .expect("encrypt v3");

        let unknown = encrypted.as_str().replacen(":new-key:", ":missing-key:", 1);
        assert!(matches!(
            cipher.decrypt(SecretPurpose::AiAccountApiKey, record_id(), &unknown),
            Err(SecretEncryptionError::UnknownKeyId)
        ));
        assert!(matches!(cipher.needs_rewrap(&unknown), Err(SecretEncryptionError::UnknownKeyId)));

        let authenticated_id = encrypted.as_str().replacen(":new-key:", ":old-key:", 1);
        assert!(matches!(
            cipher.decrypt(SecretPurpose::AiAccountApiKey, record_id(), &authenticated_id),
            Err(SecretEncryptionError::DecryptionFailed)
        ));
    }

    #[test]
    fn custom_webhook_headers_are_row_and_purpose_bound() {
        let cipher = CredentialCipher::from_optional_base64(Some(&encoded_key(26)))
            .expect("valid master key");
        let encrypted = cipher
            .encrypt(SecretPurpose::WebhookCustomHeaders, record_id(), r#"{"X-Token":"value"}"#)
            .expect("encrypt header JSON");

        assert_eq!(
            cipher
                .decrypt(SecretPurpose::WebhookCustomHeaders, record_id(), encrypted.as_str())
                .expect("matching purpose and row should decrypt")
                .expose_secret(),
            r#"{"X-Token":"value"}"#
        );
        assert!(matches!(
            cipher.decrypt(SecretPurpose::WebhookSigningSecret, record_id(), encrypted.as_str()),
            Err(SecretEncryptionError::DecryptionFailed)
        ));
        assert!(matches!(
            cipher
                .decrypt(SecretPurpose::WebhookCustomHeaders, Uuid::now_v7(), encrypted.as_str(),),
            Err(SecretEncryptionError::DecryptionFailed)
        ));
    }

    #[test]
    fn encryption_is_randomized_authenticated_and_purpose_bound() {
        let cipher = CredentialCipher::from_optional_base64(Some(&encoded_key(11)))
            .expect("valid master key");

        let first = cipher
            .encrypt(SecretPurpose::AiAccountApiKey, record_id(), "provider-value")
            .expect("encrypt first value");
        let second = cipher
            .encrypt(SecretPurpose::AiAccountApiKey, record_id(), "provider-value")
            .expect("encrypt second value");

        assert_ne!(first.as_str(), second.as_str());
        assert!(first.as_str().starts_with("ironrag:enc:v3:xchacha20poly1305:default:"));
        assert_eq!(
            cipher.storage_format(first.as_str()).expect("classify v3 envelope"),
            SecretStorageFormat::EncryptedV3
        );
        assert!(!cipher.needs_rewrap(first.as_str()).expect("default active key is current"));
        assert_eq!(
            cipher
                .decrypt(SecretPurpose::AiAccountApiKey, record_id(), first.as_str())
                .expect("decrypt matching purpose")
                .expose_secret(),
            "provider-value"
        );
        assert!(matches!(
            cipher.decrypt(SecretPurpose::WebhookSigningSecret, record_id(), first.as_str()),
            Err(SecretEncryptionError::DecryptionFailed)
        ));
    }

    #[test]
    fn new_encryption_is_bound_to_the_stable_record_id() {
        let cipher = CredentialCipher::from_optional_base64(Some(&encoded_key(12)))
            .expect("valid master key");
        let owner_id = Uuid::now_v7();
        let different_owner_id = Uuid::now_v7();

        let encrypted = cipher
            .encrypt(SecretPurpose::AiAccountApiKey, owner_id, "provider-value")
            .expect("encrypt row-bound value");

        assert!(encrypted.is_bound_to(SecretPurpose::AiAccountApiKey, owner_id));
        assert!(!encrypted.is_bound_to(SecretPurpose::AiAccountApiKey, different_owner_id));
        assert!(encrypted.as_str().starts_with("ironrag:enc:v3:xchacha20poly1305:default:"));
        assert_eq!(
            cipher
                .decrypt(SecretPurpose::AiAccountApiKey, owner_id, encrypted.as_str())
                .expect("matching owner must decrypt")
                .expose_secret(),
            "provider-value"
        );
        assert!(matches!(
            cipher.decrypt(SecretPurpose::AiAccountApiKey, different_owner_id, encrypted.as_str(),),
            Err(SecretEncryptionError::DecryptionFailed)
        ));
    }

    #[test]
    fn legacy_v1_ciphertext_remains_readable_and_is_classified_for_migration() {
        let cipher = CredentialCipher::from_optional_base64(Some(&encoded_key(14)))
            .expect("valid master key");
        let key = [14_u8; 32];
        let aead = XChaCha20Poly1305::new(Key::from_slice(&key));
        let nonce = [15_u8; super::NONCE_BYTES];
        let ciphertext = aead
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: b"legacy-encrypted-value",
                    aad: SecretPurpose::AiAccountApiKey.legacy_v1_aad(),
                },
            )
            .expect("synthetic legacy value should encrypt");
        let envelope = format!(
            "{}{}:{}",
            super::ENVELOPE_PREFIX_V1,
            super::URL_SAFE_NO_PAD.encode(nonce),
            super::URL_SAFE_NO_PAD.encode(ciphertext)
        );

        let decrypted = cipher
            .decrypt(SecretPurpose::AiAccountApiKey, Uuid::now_v7(), &envelope)
            .expect("legacy v1 AAD must remain compatible");

        assert_eq!(decrypted.expose_secret(), "legacy-encrypted-value");
        assert_eq!(decrypted.storage_format(), SecretStorageFormat::EncryptedV1);
    }

    #[test]
    fn reserved_envelope_namespace_never_falls_back_to_legacy_plaintext() {
        let cipher = CredentialCipher::from_optional_base64(None)
            .expect("missing key is a supported disabled state");

        for value in [
            "ironrag:enc:v2:xchacha20poly1305:nonce:ciphertext",
            "ironrag:enc:v1:unknown:nonce:ciphertext",
            "ironrag:enc:v1:xchacha20poly1305:missing-ciphertext",
        ] {
            assert!(matches!(
                cipher.decrypt(SecretPurpose::AiAccountApiKey, record_id(), value),
                Err(SecretEncryptionError::InvalidEnvelope)
                    | Err(SecretEncryptionError::UnsupportedEnvelope)
            ));
        }
    }

    #[test]
    fn tampered_or_noncanonical_envelope_is_rejected_without_plaintext_in_error() {
        let cipher = CredentialCipher::from_optional_base64(Some(&encoded_key(13)))
            .expect("valid master key");
        let encrypted = cipher
            .encrypt(SecretPurpose::WebhookSigningSecret, record_id(), "webhook-value")
            .expect("encrypt value");
        let mut tampered = encrypted.as_str().to_string();
        let ciphertext_start = tampered.rfind(':').expect("ciphertext separator") + 1;
        let original = tampered.as_bytes()[ciphertext_start];
        let replacement = if original == b'A' { "B" } else { "A" };
        tampered.replace_range(ciphertext_start..ciphertext_start + 1, replacement);

        let error = cipher
            .decrypt(SecretPurpose::WebhookSigningSecret, record_id(), &tampered)
            .expect_err("tampering must fail authentication");
        assert!(matches!(error, SecretEncryptionError::DecryptionFailed));
        assert!(!format!("{error:?}").contains("webhook-value"));
        assert!(!error.to_string().contains("webhook-value"));

        let padded = format!("{}=", encrypted.as_str());
        assert!(matches!(
            cipher.decrypt(SecretPurpose::WebhookSigningSecret, record_id(), &padded),
            Err(SecretEncryptionError::InvalidEnvelope)
        ));
    }

    #[test]
    fn authenticated_non_utf8_plaintext_is_rejected() {
        let cipher = CredentialCipher::from_optional_base64(Some(&encoded_key(29)))
            .expect("valid master key");
        let key = [29_u8; 32];
        let aead = XChaCha20Poly1305::new(Key::from_slice(&key));
        let nonce = [31_u8; super::NONCE_BYTES];
        let ciphertext = aead
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload { msg: &[0xff, 0xfe], aad: SecretPurpose::AiAccountApiKey.legacy_v1_aad() },
            )
            .expect("synthetic invalid UTF-8 should encrypt");
        let envelope = format!(
            "{}{}:{}",
            super::ENVELOPE_PREFIX_V1,
            super::URL_SAFE_NO_PAD.encode(nonce),
            super::URL_SAFE_NO_PAD.encode(ciphertext)
        );

        assert!(matches!(
            cipher.decrypt(SecretPurpose::AiAccountApiKey, record_id(), &envelope),
            Err(SecretEncryptionError::DecryptionFailed)
        ));
    }

    #[test]
    fn empty_and_oversized_plaintext_are_rejected_before_encryption() {
        let cipher = CredentialCipher::from_optional_base64(Some(&encoded_key(17)))
            .expect("valid master key");

        assert!(matches!(
            cipher.encrypt(SecretPurpose::WebhookSigningSecret, record_id(), ""),
            Err(SecretEncryptionError::InvalidPlaintext)
        ));
        let oversized = "x".repeat(MAX_PLAINTEXT_SECRET_BYTES + 1);
        assert!(matches!(
            cipher.encrypt(SecretPurpose::WebhookSigningSecret, record_id(), &oversized),
            Err(SecretEncryptionError::InvalidPlaintext)
        ));
    }

    #[test]
    fn secret_holding_types_have_redacted_debug_output() {
        let previous = previous_key_map(&[("old-key", 18)]);
        let cipher = CredentialCipher::from_keyring_base64(
            Some("current-key"),
            Some(&encoded_key(19)),
            Some(&previous),
        )
        .expect("valid master keyring");
        let encrypted = cipher
            .encrypt(SecretPurpose::AiAccountApiKey, record_id(), "sensitive-value")
            .expect("encrypt value");
        let decrypted = cipher
            .decrypt(SecretPurpose::AiAccountApiKey, record_id(), encrypted.as_str())
            .expect("decrypt value");

        for debug in [format!("{cipher:?}"), format!("{encrypted:?}"), format!("{decrypted:?}")] {
            assert!(!debug.contains("sensitive-value"));
            assert!(!debug.contains(&encoded_key(19)));
            assert!(!debug.contains(&encoded_key(18)));
        }
    }
}
