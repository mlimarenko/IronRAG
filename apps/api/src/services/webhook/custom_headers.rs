//! Validation and at-rest protection for outbound webhook custom headers.

use std::collections::{BTreeMap, HashSet};

use http::{HeaderName, HeaderValue};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;
use zeroize::{Zeroize as _, Zeroizing};

use crate::shared::secret_encryption::{CredentialCipher, SecretEncryptionError, SecretPurpose};

pub const MAX_CUSTOM_HEADER_COUNT: usize = 32;
pub const MAX_CUSTOM_HEADER_NAME_BYTES: usize = 128;
pub const MAX_CUSTOM_HEADER_VALUE_BYTES: usize = 1_024;
pub const MAX_CUSTOM_HEADERS_JSON_BYTES: usize = 4_096;

const RESERVED_HEADERS: &[&str] = &[
    "connection",
    "content-encoding",
    "content-length",
    "content-type",
    "expect",
    "host",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "x-ironrag-event-id",
    "x-ironrag-event-type",
    "x-ironrag-signature",
    // Trace context belongs to the runtime instrumentation boundary.
    "baggage",
    "traceparent",
    "tracestate",
];

#[derive(Debug, Error)]
pub enum CustomHeadersError {
    #[error("custom_headers must be a JSON object with string values")]
    InvalidShape,
    #[error("custom_headers exceeds the supported header count")]
    TooManyHeaders,
    #[error("custom_headers contains an invalid or oversized header name")]
    InvalidHeaderName,
    #[error("custom_headers contains the same header name more than once")]
    DuplicateHeaderName,
    #[error("custom_headers contains a runtime-reserved header name")]
    ReservedHeaderName,
    #[error("custom_headers contains an invalid or oversized header value")]
    InvalidHeaderValue,
    #[error("custom_headers exceeds the supported serialized size")]
    SerializedSizeExceeded,
    #[error("stored custom_headers cannot be decoded")]
    InvalidStoredValue,
    #[error("stored custom_headers protection is unavailable: {0}")]
    CredentialProtection(#[from] SecretEncryptionError),
}

/// Validated plaintext header material with redacted debug output and
/// best-effort zeroization of values when it leaves the delivery scope.
pub struct SensitiveWebhookHeaders {
    headers: Vec<(String, String)>,
}

impl SensitiveWebhookHeaders {
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.headers.iter().map(|(name, value)| (name.as_str(), value.as_str()))
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.headers.is_empty()
    }

    /// Serializes validated headers without constructing another plaintext
    /// value map. The returned JSON buffer is zeroized after re-encryption.
    pub fn serialized(&self) -> Result<Zeroizing<String>, CustomHeadersError> {
        let values: BTreeMap<&str, &str> =
            self.headers.iter().map(|(name, value)| (name.as_str(), value.as_str())).collect();
        let serialized =
            serde_json::to_string(&values).map_err(|_| CustomHeadersError::InvalidStoredValue)?;
        if serialized.len() > MAX_CUSTOM_HEADERS_JSON_BYTES {
            return Err(CustomHeadersError::SerializedSizeExceeded);
        }
        Ok(Zeroizing::new(serialized))
    }
}

impl std::fmt::Debug for SensitiveWebhookHeaders {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SensitiveWebhookHeaders")
            .field("header_count", &self.headers.len())
            .field("values", &"<redacted>")
            .finish()
    }
}

impl Drop for SensitiveWebhookHeaders {
    fn drop(&mut self) {
        for (_, value) in &mut self.headers {
            value.zeroize();
        }
    }
}

/// Validates an API value and returns its bounded canonical JSON plaintext.
/// The returned buffer is zeroized after the caller encrypts it.
pub fn validate_and_serialize(value: &Value) -> Result<Zeroizing<String>, CustomHeadersError> {
    let validated = validate_value(value)?;
    drop(validated);
    let serialized = serde_json::to_string(value).map_err(|_| CustomHeadersError::InvalidShape)?;
    if serialized.len() > MAX_CUSTOM_HEADERS_JSON_BYTES {
        return Err(CustomHeadersError::SerializedSizeExceeded);
    }
    Ok(Zeroizing::new(serialized))
}

/// Reads a current encrypted JSON string or a legacy object/null row, then
/// applies exactly the same validation used at the HTTP boundary.
pub fn decrypt_and_validate_stored(
    cipher: &CredentialCipher,
    subscription_id: Uuid,
    stored: &Value,
) -> Result<SensitiveWebhookHeaders, CustomHeadersError> {
    match stored {
        Value::String(stored_value) => {
            let plaintext = cipher.decrypt(
                SecretPurpose::WebhookCustomHeaders,
                subscription_id,
                stored_value,
            )?;
            if plaintext.expose_secret().len() > MAX_CUSTOM_HEADERS_JSON_BYTES {
                return Err(CustomHeadersError::SerializedSizeExceeded);
            }
            let value: Value = serde_json::from_str(plaintext.expose_secret())
                .map_err(|_| CustomHeadersError::InvalidStoredValue)?;
            validate_value(&value)
        }
        Value::Object(_) => validate_value(stored),
        // Older API requests that omitted the field were deserialized as JSON
        // null. Treat that historical representation as an empty object.
        Value::Null => validate_value(&serde_json::json!({})),
        _ => Err(CustomHeadersError::InvalidStoredValue),
    }
}

/// Best-effort scrubbing for request/database JSON buffers that may contain
/// plaintext header values or a legacy plaintext object.
pub(crate) fn scrub_json_strings(value: &mut Value) {
    match value {
        Value::String(value) => value.zeroize(),
        Value::Array(values) => {
            for value in values {
                scrub_json_strings(value);
            }
        }
        Value::Object(values) => {
            let values = std::mem::take(values);
            for (mut key, mut value) in values {
                key.zeroize();
                scrub_json_strings(&mut value);
            }
        }
        _ => {}
    }
}

fn validate_value(value: &Value) -> Result<SensitiveWebhookHeaders, CustomHeadersError> {
    let object = value.as_object().ok_or(CustomHeadersError::InvalidShape)?;
    if object.len() > MAX_CUSTOM_HEADER_COUNT {
        return Err(CustomHeadersError::TooManyHeaders);
    }

    let mut names = HashSet::with_capacity(object.len());
    let mut headers = Vec::with_capacity(object.len());
    for (name, value) in object {
        if name.is_empty()
            || name.len() > MAX_CUSTOM_HEADER_NAME_BYTES
            || HeaderName::from_bytes(name.as_bytes()).is_err()
        {
            return Err(CustomHeadersError::InvalidHeaderName);
        }
        let normalized_name = name.to_ascii_lowercase();
        if !names.insert(normalized_name.clone()) {
            return Err(CustomHeadersError::DuplicateHeaderName);
        }
        if RESERVED_HEADERS.contains(&normalized_name.as_str()) {
            return Err(CustomHeadersError::ReservedHeaderName);
        }
        let value = value.as_str().ok_or(CustomHeadersError::InvalidShape)?;
        if value.len() > MAX_CUSTOM_HEADER_VALUE_BYTES
            || HeaderValue::from_bytes(value.as_bytes()).is_err()
        {
            return Err(CustomHeadersError::InvalidHeaderValue);
        }
        headers.push((name.clone(), value.to_owned()));
    }
    Ok(SensitiveWebhookHeaders { headers })
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use serde_json::json;

    use super::*;

    fn cipher() -> CredentialCipher {
        CredentialCipher::from_optional_base64(Some(&STANDARD.encode([73_u8; 32])))
            .expect("test key must be valid")
    }

    #[test]
    fn encrypted_headers_round_trip_without_exposing_values_in_debug() {
        let subscription_id = Uuid::now_v7();
        let value = json!({
            "Authorization": "Bearer private-value",
            "X-Tenant": "neutral-fixture"
        });
        let serialized = validate_and_serialize(&value).expect("valid headers");
        let envelope = cipher()
            .encrypt(SecretPurpose::WebhookCustomHeaders, subscription_id, serialized.as_str())
            .expect("encrypt headers");

        let decoded = decrypt_and_validate_stored(
            &cipher(),
            subscription_id,
            &Value::String(envelope.as_str().to_owned()),
        )
        .expect("decrypt headers");

        assert_eq!(decoded.iter().count(), 2);
        assert!(decoded.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("authorization") && value == "Bearer private-value"
        }));
        assert!(!format!("{decoded:?}").contains("private-value"));
    }

    #[test]
    fn legacy_object_and_null_are_read_compatibly() {
        let legacy =
            decrypt_and_validate_stored(&cipher(), Uuid::now_v7(), &json!({"X-Legacy": "value"}))
                .expect("legacy object should remain readable");
        let omitted = decrypt_and_validate_stored(&cipher(), Uuid::now_v7(), &Value::Null)
            .expect("legacy null should mean empty headers");

        assert_eq!(legacy.iter().count(), 1);
        assert!(omitted.is_empty());
    }

    #[test]
    fn invalid_shapes_and_case_insensitive_duplicates_are_rejected() {
        assert!(matches!(
            validate_and_serialize(&json!(["X-Test", "value"])),
            Err(CustomHeadersError::InvalidShape)
        ));
        assert!(matches!(
            validate_and_serialize(&json!({"X-Test": 7})),
            Err(CustomHeadersError::InvalidShape)
        ));
        assert!(matches!(
            validate_and_serialize(&json!({"X-Test": "one", "x-test": "two"})),
            Err(CustomHeadersError::DuplicateHeaderName)
        ));
    }

    #[test]
    fn reserved_and_crlf_headers_are_rejected() {
        assert!(matches!(
            validate_and_serialize(&json!({"Content-Length": "99"})),
            Err(CustomHeadersError::ReservedHeaderName)
        ));
        assert!(matches!(
            validate_and_serialize(&json!({"X-Ironrag-Signature": "replacement"})),
            Err(CustomHeadersError::ReservedHeaderName)
        ));
        assert!(matches!(
            validate_and_serialize(&json!({"X-Ironrag-Event-Type": "replacement"})),
            Err(CustomHeadersError::ReservedHeaderName)
        ));
        assert!(matches!(
            validate_and_serialize(&json!({"X-Ironrag-Event-Id": "replacement"})),
            Err(CustomHeadersError::ReservedHeaderName)
        ));
        assert!(matches!(
            validate_and_serialize(&json!({"X-Test": "ok\r\nX-Injected: yes"})),
            Err(CustomHeadersError::InvalidHeaderValue)
        ));
    }

    #[test]
    fn count_and_size_limits_are_fail_closed() {
        let too_many = Value::Object(
            (0..=MAX_CUSTOM_HEADER_COUNT)
                .map(|index| (format!("X-Field-{index}"), Value::String("v".into())))
                .collect(),
        );
        assert!(matches!(
            validate_and_serialize(&too_many),
            Err(CustomHeadersError::TooManyHeaders)
        ));
        assert!(matches!(
            validate_and_serialize(&json!({
                "X-Large": "x".repeat(MAX_CUSTOM_HEADER_VALUE_BYTES + 1)
            })),
            Err(CustomHeadersError::InvalidHeaderValue)
        ));
    }

    #[test]
    fn ciphertext_is_bound_to_subscription_and_purpose() {
        let subscription_id = Uuid::now_v7();
        let serialized =
            validate_and_serialize(&json!({"X-Test": "value"})).expect("valid headers");
        let envelope = cipher()
            .encrypt(SecretPurpose::WebhookCustomHeaders, subscription_id, serialized.as_str())
            .expect("encrypt headers");
        let stored = Value::String(envelope.as_str().to_owned());

        assert!(
            decrypt_and_validate_stored(&cipher(), Uuid::now_v7(), &stored).is_err(),
            "copying an envelope to another subscription must fail"
        );
        assert!(
            cipher()
                .decrypt(SecretPurpose::WebhookSigningSecret, subscription_id, envelope.as_str())
                .is_err(),
            "copying an envelope to another column purpose must fail"
        );
    }
}
