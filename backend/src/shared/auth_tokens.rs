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
    format!("rtrg_{}", Uuid::now_v7().simple())
}

#[must_use]
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

pub fn parse_session_cookie_value(raw: &str) -> Option<(Uuid, String)> {
    let (session_id, secret) = raw.trim().split_once('.')?;
    Some((Uuid::parse_str(session_id).ok()?, secret.to_string()))
}
