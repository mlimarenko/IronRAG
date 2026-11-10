use argon2::{
    Argon2,
    password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash},
};
use chrono::Utc;

use super::*;
use crate::shared::auth_tokens::{hash_session_secret, mint_plaintext_session_secret};

pub(crate) fn hash_password(password: &str) -> Result<String, ApiError> {
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|hash| hash.to_string())
        .map_err(|e| ApiError::internal_with_log(e, "internal"))
}

/// Valid Argon2 material reserved for identities that have no password login.
///
/// It is deliberately not derived from any operator-provided password. OIDC
/// users retain it only because the deployed baseline makes `password_hash`
/// non-null; authentication explicitly rejects their provider kind before it
/// can be used as a login credential.
pub(crate) const OIDC_ONLY_PASSWORD_SENTINEL_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$dkeGvbrthjpCcSNa13D2gw$MvKd4kJzfARfCtqYDp3QVe7GgjHv8k9PevbjtUYAybs";

pub(super) const DUMMY_PASSWORD_HASH: &str = OIDC_ONLY_PASSWORD_SENTINEL_HASH;

pub(super) fn verify_password(password: &str, password_hash: &str) -> Result<(), ApiError> {
    let parsed_hash =
        PasswordHash::new(password_hash).map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed_hash)
        .map_err(|_| ApiError::Unauthorized)
}

/// Issue a browser session under the deployment's session lifetime policy.
///
/// The absolute cap is stamped here, once, from the configured policy; the
/// sliding idle window is enforced on every request against `last_seen_at`.
pub(super) async fn issue_session_for_user(
    state: &AppState,
    principal_id: Uuid,
    login: &str,
    email: &str,
    display_name: &str,
) -> Result<AuthenticateSessionOutcome, ApiError> {
    let expires_at = state.ui_session_cookie.lifetime.absolute_expiry_at(Utc::now());
    let session_secret = Zeroizing::new(mint_plaintext_session_secret());
    let session = iam_repository::create_session(
        &state.persistence.postgres,
        principal_id,
        &hash_session_secret(&session_secret),
        expires_at,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

    Ok(AuthenticateSessionOutcome {
        session_id: session.id,
        session_secret: session_secret.to_string(),
        principal_id,
        login: login.to_string(),
        email: email.to_string(),
        display_name: display_name.to_string(),
        expires_at: session.expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::{hash_password, verify_password};
    use crate::interfaces::http::router_support::ApiError;

    #[test]
    fn persisted_compatibility_reads_accepted_password_hashes() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/credentials/accepted_writer.json"
        )))
        .expect("synthetic accepted-writer fixture");
        let password = fixture["password"].as_str().expect("fixture password");
        for field in ["bootstrap_hash", "iam_hash"] {
            let hash = fixture[field].as_str().expect("accepted password hash");
            assert!(verify_password(password, hash).is_ok());
            assert!(matches!(verify_password("wrong-password", hash), Err(ApiError::Unauthorized)));
        }
        assert!(matches!(verify_password(password, "malformed"), Err(ApiError::Internal)));

        let hash = hash_password(password).expect("new IAM writer");
        assert!(hash.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
        assert!(verify_password(password, &hash).is_ok());
        let fields: Vec<_> = hash.split('$').collect();
        assert_eq!(fields.len(), 6);
        assert_eq!(fields[4].len(), 22, "16-byte unpadded PHC salt");
        assert_eq!(fields[5].len(), 43, "32-byte unpadded PHC output");
    }
}
