use sha2::{Digest, Sha256};
use uuid::Uuid;

#[must_use]
pub fn hash_api_token(raw: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    hex::encode(hasher.finalize())
}

#[must_use]
pub fn mint_plaintext_api_token() -> String {
    format!("inrg_{}", Uuid::now_v7().simple())
}

#[must_use]
pub fn is_well_formed_api_token(raw: &str) -> bool {
    raw.len() == 37
        && raw.starts_with("inrg_")
        && raw.as_bytes()[5..].iter().all(u8::is_ascii_hexdigit)
}

pub fn preview_api_token(raw: &str) -> String {
    if raw.len() <= 10 {
        return raw.to_string();
    }

    let prefix = &raw[..5];
    let suffix = &raw[raw.len().saturating_sub(4)..];
    format!("{prefix}***{suffix}")
}

#[must_use]
pub fn hash_session_secret(raw: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    hex::encode(hasher.finalize())
}

#[must_use]
pub fn mint_plaintext_session_secret() -> String {
    format!("rtrs_{}", Uuid::now_v7().simple())
}

#[must_use]
pub fn build_session_cookie_value(session_id: Uuid, secret: &str) -> String {
    format!("{session_id}.{secret}")
}

#[must_use]
pub fn parse_session_cookie_value(raw: &str) -> Option<(Uuid, String)> {
    let (session_id, secret) = raw.trim().split_once('.')?;
    Some((Uuid::parse_str(session_id).ok()?, secret.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_well_formed_inrg_tokens() {
        assert!(is_well_formed_api_token(&mint_plaintext_api_token()));
        assert!(!is_well_formed_api_token("legacy_0123456789abcdef0123456789abcdef"));
        assert!(!is_well_formed_api_token("inrg_not-a-token"));
    }
}
