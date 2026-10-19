use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng},
};
use chrono::Utc;

use super::*;
use crate::shared::auth_tokens::{hash_session_secret, mint_plaintext_session_secret};

pub(crate) fn hash_password(password: &str) -> Result<String, ApiError> {
    Argon2::default()
        .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
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
