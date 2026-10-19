//! Authenticated encryption boundary for credentials persisted in `PostgreSQL`.
//!
//! Every credential is stored as a row-bound v3 XChaCha20-Poly1305 envelope
//! carrying an authenticated key identifier. A bounded keyring keeps older v3
//! envelopes readable inside an explicit key-rotation window. The master key is
//! mandatory: there is no plaintext path and no pre-v3 read path. Values are
//! parsed strictly and fail closed rather than ever being treated as plaintext.

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
    /// Replayable receipt for the currently active owner-scoped service
    /// identity credential. The plaintext is never stored.
    ServiceIdentityCredential,
    WebhookSigningSecret,
    /// Serialized outbound webhook custom headers stored on a subscription.
    WebhookCustomHeaders,
}

impl SecretPurpose {
    const fn storage_label(self) -> &'static [u8] {
        match self {
            Self::AiAccountApiKey => b"ai_account.api_key",
            Self::ServiceIdentityCredential => {
                b"iam_service_identity_operation.encrypted_credential"
            }
            Self::WebhookSigningSecret => b"webhook_subscription.secret",
            Self::WebhookCustomHeaders => b"webhook_subscription.custom_headers_json",
        }
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
}

impl DecryptedSecret {
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        self.value.as_str()
    }
}

impl fmt::Debug for DecryptedSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("DecryptedSecret").field("value", &"<redacted>").finish()
    }
}

#[derive(Error, Debug)]
pub enum SecretEncryptionError {
    #[error("credential master key is not configured")]
    MasterKeyNotConfigured,
    #[error("credential master key must be strict base64 encoding of exactly 32 bytes")]
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
}

/// Process-wide credential cipher backed by a bounded keyring.
///
/// Clones share one zeroizing keyring allocation; key bytes are never cloned or
/// included in `Debug`. The keyring is mandatory, so every instance can both
/// decrypt current v3 envelopes and encrypt new ones.
#[derive(Clone)]
pub struct CredentialCipher {
    keyring: Arc<CredentialKeyring>,
}

impl fmt::Debug for CredentialCipher {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialCipher")
            .field("key_count", &(1 + self.keyring.previous.len()))
            .finish()
    }
}

impl CredentialCipher {
    /// Parses the dedicated credential master key.
    ///
    /// The value must be the strict padded standard-base64 encoding of
    /// exactly 32 random bytes.
    ///
    /// # Errors
    /// Returns [`SecretEncryptionError::InvalidMasterKey`] for an empty,
    /// malformed, non-normalized, or incorrectly sized value.
    pub fn from_base64(encoded_master_key: &str) -> Result<Self, SecretEncryptionError> {
        Self::from_keyring_base64(None, Some(encoded_master_key), None)
    }

    /// Parses an active credential key and an optional bounded previous-key map.
    ///
    /// `active_key_id` defaults to `default` when the existing
    /// `IRONRAG_CREDENTIAL_MASTER_KEY` configuration is used by itself. Explicit
    /// identifiers contain 1-32 lowercase ASCII letters, digits, `.`, `_` or
    /// `-`, begin with a letter or digit, and are embedded in new v3 envelopes.
    /// The previous-key map is a comma-separated, strictly key-id-sorted list of
    /// `key-id=strict-base64-key` entries with no whitespace and at most
    /// [`MAX_PREVIOUS_CREDENTIAL_KEYS`] entries.
    ///
    /// An absent active key is rejected: the cipher is mandatory and cannot be
    /// constructed without key material. Encoded inputs are borrowed; their
    /// owner must zeroize those source buffers after construction. Application
    /// state does so before retaining or cloning settings.
    ///
    /// # Errors
    /// Returns [`SecretEncryptionError::MasterKeyNotConfigured`] when no active
    /// key is supplied, or a typed error when identifiers or encoded inputs are
    /// invalid. Duplicate or unsorted entries, active-ID reuse, and oversized
    /// rotation sets are rejected as well.
    pub fn from_keyring_base64(
        active_key_id: Option<&str>,
        encoded_active_key: Option<&str>,
        encoded_previous_key_map: Option<&str>,
    ) -> Result<Self, SecretEncryptionError> {
        let Some(encoded_active_key) = encoded_active_key else {
            return Err(SecretEncryptionError::MasterKeyNotConfigured);
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
        Ok(Self { keyring: Arc::new(CredentialKeyring { active, previous }) })
    }

    /// Reports whether a stored secret must be rewrapped by the active key.
    ///
    /// A v3 envelope is current only when its validated key ID matches the
    /// configured active key ID. Any non-v3 envelope is rejected as
    /// unsupported. An unknown v3 key ID fails closed rather than being treated
    /// as an already-current value.
    ///
    /// # Errors
    /// Returns a typed error for malformed or non-v3 values, or a v3 envelope
    /// whose key ID is absent from the configured keyring.
    pub fn needs_rewrap(&self, stored_value: &str) -> Result<bool, SecretEncryptionError> {
        let encoded_fields = parse_envelope(stored_value)?;
        let key_id = encoded_fields.key_id;
        if self.keyring.find(key_id).is_none() {
            return Err(SecretEncryptionError::UnknownKeyId);
        }
        Ok(key_id != self.keyring.active.id)
    }

    /// Encrypts one non-empty secret into the normalized row-bound v3 envelope.
    ///
    /// # Errors
    /// Fails closed when the input is out of bounds or the AEAD implementation
    /// rejects the operation.
    pub fn encrypt(
        &self,
        purpose: SecretPurpose,
        record_id: Uuid,
        plaintext: &str,
    ) -> Result<EncryptedSecret, SecretEncryptionError> {
        validate_plaintext(plaintext)?;
        let keyring = &self.keyring;
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

    /// Decrypts a keyed row-bound v3 ciphertext.
    ///
    /// Values are parsed strictly against the reserved `ironrag:enc:` namespace.
    /// Unknown versions, malformed fields and authentication failures never fall
    /// back to plaintext.
    ///
    /// # Errors
    /// Returns a typed, redacted failure for malformed envelopes, missing keys,
    /// authentication failures, invalid UTF-8, or out-of-bounds values.
    pub fn decrypt(
        &self,
        purpose: SecretPurpose,
        record_id: Uuid,
        stored_value: &str,
    ) -> Result<DecryptedSecret, SecretEncryptionError> {
        let encoded_fields = parse_envelope(stored_value)?;
        let nonce =
            decode_normalized_urlsafe_field(encoded_fields.nonce, NONCE_BYTES, NONCE_BYTES)?;
        let ciphertext = decode_normalized_urlsafe_field(
            encoded_fields.ciphertext,
            AUTH_TAG_BYTES + 1,
            MAX_PLAINTEXT_SECRET_BYTES + AUTH_TAG_BYTES,
        )?;
        let key_id = encoded_fields.key_id;
        let aad = purpose.row_bound_v3_aad(record_id, key_id);
        let material = self.keyring.find(key_id).ok_or(SecretEncryptionError::UnknownKeyId)?;
        let plaintext = decrypt_with_key(material, &nonce, &ciphertext, &aad)?;
        decrypted_secret(plaintext)
    }
}

struct EncodedEnvelopeFields<'value> {
    key_id: &'value str,
    nonce: &'value str,
    ciphertext: &'value str,
}

fn parse_envelope(stored_value: &str) -> Result<EncodedEnvelopeFields<'_>, SecretEncryptionError> {
    if stored_value.len() > MAX_ENVELOPE_BYTES {
        return Err(SecretEncryptionError::InvalidEnvelope);
    }
    let remainder = stored_value
        .strip_prefix(ENVELOPE_NAMESPACE)
        .ok_or(SecretEncryptionError::InvalidEnvelope)?;
    let mut fields = remainder.split(':');
    let version = fields.next().ok_or(SecretEncryptionError::InvalidEnvelope)?;
    if version != "v3" {
        return Err(SecretEncryptionError::UnsupportedEnvelope);
    }
    let algorithm = fields.next().ok_or(SecretEncryptionError::InvalidEnvelope)?;
    if algorithm != "xchacha20poly1305" {
        return Err(SecretEncryptionError::UnsupportedEnvelope);
    }
    let key_id = fields.next().ok_or(SecretEncryptionError::InvalidEnvelope)?;
    if !is_valid_key_id(key_id) {
        return Err(SecretEncryptionError::InvalidEnvelope);
    }
    let nonce = fields.next().ok_or(SecretEncryptionError::InvalidEnvelope)?;
    let ciphertext = fields.next().ok_or(SecretEncryptionError::InvalidEnvelope)?;
    if nonce.is_empty() || ciphertext.is_empty() || fields.next().is_some() {
        return Err(SecretEncryptionError::InvalidEnvelope);
    }
    Ok(EncodedEnvelopeFields { key_id, nonce, ciphertext })
}

fn decode_master_key(encoded_master_key: &str) -> Result<MasterKey, SecretEncryptionError> {
    if encoded_master_key.len() != 44 {
        return Err(SecretEncryptionError::InvalidMasterKey);
    }
    let decoded = Zeroizing::new(
        STANDARD.decode(encoded_master_key).map_err(|_| SecretEncryptionError::InvalidMasterKey)?,
    );
    let normalized_encoding = Zeroizing::new(STANDARD.encode(decoded.as_slice()));
    if decoded.len() != MASTER_KEY_BYTES || normalized_encoding.as_str() != encoded_master_key {
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
) -> Result<DecryptedSecret, SecretEncryptionError> {
    if plaintext.is_empty() || plaintext.len() > MAX_PLAINTEXT_SECRET_BYTES {
        return Err(SecretEncryptionError::DecryptionFailed);
    }
    let plaintext = std::str::from_utf8(plaintext.as_slice())
        .map_err(|_| SecretEncryptionError::DecryptionFailed)?;
    Ok(DecryptedSecret { value: Zeroizing::new(plaintext.to_owned()) })
}

fn decode_normalized_urlsafe_field(
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
    use uuid::Uuid;

    use super::{
        CredentialCipher, MAX_PLAINTEXT_SECRET_BYTES, SecretEncryptionError, SecretPurpose,
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

    #[test]
    fn configured_master_key_is_strict_base64_of_exactly_32_bytes() {
        assert!(matches!(
            CredentialCipher::from_base64("not-base64"),
            Err(SecretEncryptionError::InvalidMasterKey)
        ));
        assert!(matches!(
            CredentialCipher::from_base64(&STANDARD.encode([7_u8; 31])),
            Err(SecretEncryptionError::InvalidMasterKey)
        ));

        let unpadded = encoded_key(9).trim_end_matches('=').to_string();
        assert!(matches!(
            CredentialCipher::from_base64(&unpadded),
            Err(SecretEncryptionError::InvalidMasterKey)
        ));

        let encoded = encoded_key(9);
        let invalid_trailing_bits = format!("{}l=", &encoded[..42]);
        assert!(matches!(
            CredentialCipher::from_base64(&invalid_trailing_bits),
            Err(SecretEncryptionError::InvalidMasterKey)
        ));
        assert!(CredentialCipher::from_base64(&encoded).is_ok());
    }

    #[test]
    fn keyring_configuration_is_bounded_normalized_and_unambiguous() {
        assert_invalid_key_ids_rejected();
        assert_invalid_previous_key_maps_rejected();
        assert_keyring_bounds_and_master_key_requirement();
    }

    fn assert_invalid_key_ids_rejected() {
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
    }

    fn assert_invalid_previous_key_maps_rejected() {
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
    }

    fn assert_keyring_bounds_and_master_key_requirement() {
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
        assert!(matches!(
            CredentialCipher::from_keyring_base64(None, None, None),
            Err(SecretEncryptionError::MasterKeyNotConfigured)
        ));
    }

    #[test]
    fn active_key_encrypts_and_previous_key_decrypts_older_v3() {
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
        let cipher = CredentialCipher::from_base64(&encoded_key(26)).expect("valid master key");
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
        let cipher = CredentialCipher::from_base64(&encoded_key(11)).expect("valid master key");

        let first = cipher
            .encrypt(SecretPurpose::AiAccountApiKey, record_id(), "provider-value")
            .expect("encrypt first value");
        let second = cipher
            .encrypt(SecretPurpose::AiAccountApiKey, record_id(), "provider-value")
            .expect("encrypt second value");

        assert_ne!(first.as_str(), second.as_str());
        assert!(first.as_str().starts_with("ironrag:enc:v3:xchacha20poly1305:default:"));
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
        let cipher = CredentialCipher::from_base64(&encoded_key(12)).expect("valid master key");
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
    fn reserved_envelope_namespace_rejects_pre_v3_and_malformed_values() {
        let cipher = CredentialCipher::from_base64(&encoded_key(1)).expect("valid master key");

        for value in [
            "ironrag:enc:v2:xchacha20poly1305:nonce:ciphertext",
            "ironrag:enc:v1:unknown:nonce:ciphertext",
            "ironrag:enc:v1:xchacha20poly1305:missing-ciphertext",
            "legacy-plaintext-value",
        ] {
            assert!(matches!(
                cipher.decrypt(SecretPurpose::AiAccountApiKey, record_id(), value),
                Err(SecretEncryptionError::InvalidEnvelope)
                    | Err(SecretEncryptionError::UnsupportedEnvelope)
            ));
        }
    }

    #[test]
    fn tampered_or_malformed_envelope_is_rejected_without_plaintext_in_error() {
        let cipher = CredentialCipher::from_base64(&encoded_key(13)).expect("valid master key");
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
        use chacha20poly1305::{
            Key, KeyInit, XChaCha20Poly1305, XNonce,
            aead::{Aead, Payload},
        };

        let key_byte = 29_u8;
        let cipher =
            CredentialCipher::from_base64(&encoded_key(key_byte)).expect("valid master key");
        // Forge a structurally valid v3 envelope whose authenticated plaintext
        // is not UTF-8. It authenticates under the active key and AAD, so the
        // failure must come from the UTF-8 boundary rather than authentication.
        let record = record_id();
        let aad =
            SecretPurpose::AiAccountApiKey.row_bound_v3_aad(record, super::DEFAULT_ACTIVE_KEY_ID);
        let aead = XChaCha20Poly1305::new(Key::from_slice(&[key_byte; 32]));
        let nonce = [31_u8; super::NONCE_BYTES];
        let ciphertext = aead
            .encrypt(XNonce::from_slice(&nonce), Payload { msg: &[0xff, 0xfe], aad: &aad })
            .expect("synthetic invalid UTF-8 should encrypt");
        let envelope = format!(
            "{}{}:{}:{}",
            super::ENVELOPE_PREFIX_V3,
            super::DEFAULT_ACTIVE_KEY_ID,
            super::URL_SAFE_NO_PAD.encode(nonce),
            super::URL_SAFE_NO_PAD.encode(ciphertext)
        );

        assert!(matches!(
            cipher.decrypt(SecretPurpose::AiAccountApiKey, record, &envelope),
            Err(SecretEncryptionError::DecryptionFailed)
        ));
    }

    #[test]
    fn empty_and_oversized_plaintext_are_rejected_before_encryption() {
        let cipher = CredentialCipher::from_base64(&encoded_key(17)).expect("valid master key");

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
