use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use serde::Deserialize;
use uuid::Uuid;

use super::{CredentialCipher, SecretEncryptionError, SecretPurpose};

#[derive(Deserialize)]
struct Fixture {
    key_byte: u8,
    record_id: Uuid,
    envelopes: Vec<Envelope>,
}

#[derive(Deserialize)]
struct Envelope {
    purpose: Purpose,
    key_id: String,
    plaintext: String,
    ciphertext: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Purpose {
    AiAccount,
    ServiceIdentity,
    WebhookSecret,
    WebhookHeaders,
}

impl Purpose {
    const fn secret_purpose(&self) -> SecretPurpose {
        match self {
            Self::AiAccount => SecretPurpose::AiAccountApiKey,
            Self::ServiceIdentity => SecretPurpose::ServiceIdentityCredential,
            Self::WebhookSecret => SecretPurpose::WebhookSigningSecret,
            Self::WebhookHeaders => SecretPurpose::WebhookCustomHeaders,
        }
    }
}

#[test]
fn persisted_compatibility_reads_accepted_v3_envelopes() {
    let fixture: Fixture = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/credentials/accepted_writer.json"
    )))
    .expect("synthetic accepted-writer fixture");
    assert_eq!(fixture.envelopes.len(), 8, "four purposes under default and rotated key IDs");
    for envelope in fixture.envelopes {
        let purpose = envelope.purpose.secret_purpose();
        let key = STANDARD.encode([fixture.key_byte; 32]);
        let previous = format!("{}={key}", envelope.key_id);
        let rotated = CredentialCipher::from_keyring_base64(
            Some("current"),
            Some(&STANDARD.encode([72_u8; 32])),
            Some(&previous),
        )
        .expect("rotated synthetic keyring");
        let same_key =
            CredentialCipher::from_keyring_base64(Some(&envelope.key_id), Some(&key), None)
                .expect("original synthetic keyring");
        for cipher in [&same_key, &rotated] {
            let value = cipher
                .decrypt(purpose, fixture.record_id, &envelope.ciphertext)
                .expect("accepted writer envelope must remain readable");
            assert!(value.expose_secret() == envelope.plaintext);
            assert!(!format!("{value:?}").contains(&envelope.plaintext));
        }
        assert!(!same_key.needs_rewrap(&envelope.ciphertext).expect("original key"));
        assert!(rotated.needs_rewrap(&envelope.ciphertext).expect("previous key"));
        assert_rejections(&rotated, &envelope, fixture.record_id);
    }
}

fn assert_rejections(cipher: &CredentialCipher, envelope: &Envelope, record: Uuid) {
    let purpose = envelope.purpose.secret_purpose();
    let other_purpose = if purpose == SecretPurpose::AiAccountApiKey {
        SecretPurpose::WebhookSigningSecret
    } else {
        SecretPurpose::AiAccountApiKey
    };
    assert!(matches!(
        cipher.decrypt(other_purpose, record, &envelope.ciphertext),
        Err(SecretEncryptionError::DecryptionFailed)
    ));
    assert!(matches!(
        cipher.decrypt(purpose, Uuid::nil(), &envelope.ciphertext),
        Err(SecretEncryptionError::DecryptionFailed)
    ));
    let wrong_key = CredentialCipher::from_keyring_base64(
        Some(&envelope.key_id),
        Some(&STANDARD.encode([73_u8; 32])),
        None,
    )
    .expect("wrong synthetic key");
    assert!(matches!(
        wrong_key.decrypt(purpose, record, &envelope.ciphertext),
        Err(SecretEncryptionError::DecryptionFailed)
    ));
    let (prefix, encoded) = envelope.ciphertext.rsplit_once(':').expect("v3 envelope");
    let mut bytes = URL_SAFE_NO_PAD.decode(encoded).expect("normalized ciphertext");
    bytes[0] ^= 1;
    let tampered = format!("{prefix}:{}", URL_SAFE_NO_PAD.encode(bytes));
    let error = cipher.decrypt(purpose, record, &tampered).expect_err("tamper must fail closed");
    assert!(matches!(error, SecretEncryptionError::DecryptionFailed));
    assert!(!format!("{error:?}").contains(&envelope.plaintext));
    assert!(!error.to_string().contains(&envelope.plaintext));
}
