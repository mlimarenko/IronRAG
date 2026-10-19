use uuid::Uuid;

use crate::shared::auth_tokens;

#[must_use]
pub fn hash_token(raw: &str) -> String {
    auth_tokens::hash_api_token(raw)
}

#[must_use]
pub fn hash_session_secret(raw: &str) -> String {
    auth_tokens::hash_session_secret(raw)
}

#[must_use]
pub fn mint_plaintext_session_secret() -> String {
    auth_tokens::mint_plaintext_session_secret()
}

#[must_use]
pub fn build_session_cookie_value(session_id: Uuid, secret: &str) -> String {
    auth_tokens::build_session_cookie_value(session_id, secret)
}

#[must_use]
pub fn parse_session_cookie_value(raw: &str) -> Option<(Uuid, String)> {
    auth_tokens::parse_session_cookie_value(raw)
}

#[cfg(test)]
mod tests {
    use super::hash_token;

    #[test]
    fn hash_token_is_deterministic_and_sensitive_to_input() {
        let first = hash_token("secret-token");
        let second = hash_token("secret-token");
        let different = hash_token("secret-token-2");

        assert_eq!(first, second);
        assert_ne!(first, different);
        assert_eq!(first.len(), 64);
    }
}
