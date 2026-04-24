use anyhow::{Context, Result};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const GROUNDED_ANSWER_CACHE_TTL_SECONDS: u64 = 300;
const GROUNDED_ANSWER_CACHE_VERSION: &str = "v12";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CachedGroundedAnswer {
    pub human_text: String,
    pub structured_json: serde_json::Value,
}

pub(crate) fn cache_key(
    library_id: Uuid,
    projection_version: i64,
    binding_id: Option<Uuid>,
    question: &str,
    conversation_context: Option<&str>,
) -> String {
    let normalized = question.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    let mut hasher = Sha256::new();
    hasher.update(library_id.as_bytes());
    hasher.update(projection_version.to_le_bytes());
    if let Some(id) = binding_id {
        hasher.update(id.as_bytes());
    } else {
        hasher.update([0u8; 16]);
    }
    if let Some(context) = conversation_context {
        hasher.update(context.as_bytes());
    }
    hasher.update(normalized.as_bytes());
    let digest = hasher.finalize();
    let mut hash = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hash.push_str(&format!("{byte:02x}"));
    }
    format!("grounded_answer:{GROUNDED_ANSWER_CACHE_VERSION}:{hash}")
}

pub(crate) async fn get_cached(
    client: &redis::Client,
    key: &str,
) -> Result<Option<CachedGroundedAnswer>> {
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .context("connect to redis for grounded_answer cache read")?;
    let raw: Option<Vec<u8>> = conn.get(key).await.context("redis GET grounded_answer cache")?;
    match raw {
        Some(bytes) => {
            let entry: CachedGroundedAnswer =
                serde_json::from_slice(&bytes).context("decode grounded_answer cache payload")?;
            Ok(Some(entry))
        }
        None => Ok(None),
    }
}

pub(crate) async fn put_cached(
    client: &redis::Client,
    key: &str,
    entry: &CachedGroundedAnswer,
) -> Result<()> {
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .context("connect to redis for grounded_answer cache write")?;
    let bytes = serde_json::to_vec(entry).context("encode grounded_answer cache payload")?;
    let _: () = conn
        .set_ex(key, bytes, GROUNDED_ANSWER_CACHE_TTL_SECONDS)
        .await
        .context("redis SET EX grounded_answer cache")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_carries_pipeline_version() {
        let key = cache_key(Uuid::nil(), 1, None, "TargetName how", None);
        assert!(key.starts_with("grounded_answer:v12:"));
    }
}
