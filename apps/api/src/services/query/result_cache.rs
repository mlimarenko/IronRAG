use std::{sync::LazyLock, time::Duration};

use anyhow::{Context, Result};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;
use tracing::warn;
use uuid::Uuid;

pub(crate) const QUERY_RESULT_CACHE_TTL_SECONDS: u64 = 300;
pub(crate) const QUERY_RESULT_CACHE_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const QUERY_RESULT_CACHE_WAIT_INTERVAL: Duration = Duration::from_millis(250);
const QUERY_RESULT_CACHE_LOCK_TTL_SECONDS: u64 = 180;
const QUERY_RESULT_CACHE_LOCK_RENEW_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub(crate) struct QueryResultCacheKeyInput<'a> {
    pub(crate) workspace_id: Uuid,
    pub(crate) library_id: Uuid,
    pub(crate) readable_content_fingerprint: &'a str,
    pub(crate) graph_projection_version: i64,
    pub(crate) binding_fingerprint: &'a str,
    pub(crate) answer_system_prompt: &'a str,
    pub(crate) answer_runtime_fingerprint: &'a str,
    pub(crate) mode_label: &'static str,
    pub(crate) top_k: usize,
    pub(crate) source_links_enabled: bool,
    pub(crate) user_question: &'a str,
    pub(crate) prompt_history_text: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
struct CachedQueryResult {
    source_execution_id: Uuid,
}

const ANSWER_RUNTIME_FINGERPRINT_SOURCES: &[(&str, &str)] = &[
    ("assistant_prompt.rs", include_str!("assistant_prompt.rs")),
    ("compiler.rs", include_str!("compiler.rs")),
    ("latest_versions.rs", include_str!("latest_versions.rs")),
    ("execution/answer.rs", include_str!("execution/answer.rs")),
    ("execution/answer_pipeline.rs", include_str!("execution/answer_pipeline.rs")),
    (
        "execution/canonical_answer_context.rs",
        include_str!("execution/canonical_answer_context.rs"),
    ),
    ("execution/consolidation.rs", include_str!("execution/consolidation.rs")),
    ("execution/context.rs", include_str!("execution/context.rs")),
    ("execution/document_target.rs", include_str!("execution/document_target.rs")),
    ("execution/embed.rs", include_str!("execution/embed.rs")),
    ("execution/endpoint_answer.rs", include_str!("execution/endpoint_answer.rs")),
    ("execution/endpoint_chunk_answer.rs", include_str!("execution/endpoint_chunk_answer.rs")),
    ("execution/fact_lookup.rs", include_str!("execution/fact_lookup.rs")),
    ("execution/focused_document_answer.rs", include_str!("execution/focused_document_answer.rs")),
    ("execution/graph_retrieval.rs", include_str!("execution/graph_retrieval.rs")),
    ("execution/hyde_crag.rs", include_str!("execution/hyde_crag.rs")),
    ("execution/port_answer.rs", include_str!("execution/port_answer.rs")),
    ("execution/preflight.rs", include_str!("execution/preflight.rs")),
    ("execution/question_intent.rs", include_str!("execution/question_intent.rs")),
    ("execution/rerank.rs", include_str!("execution/rerank.rs")),
    ("execution/retrieve.rs", include_str!("execution/retrieve.rs")),
    ("execution/source_context.rs", include_str!("execution/source_context.rs")),
    ("execution/source_profile.rs", include_str!("execution/source_profile.rs")),
    (
        "execution/structured_query_pipeline.rs",
        include_str!("execution/structured_query_pipeline.rs"),
    ),
    ("execution/table_retrieval.rs", include_str!("execution/table_retrieval.rs")),
    ("execution/table_row_answer.rs", include_str!("execution/table_row_answer.rs")),
    ("execution/table_summary_answer.rs", include_str!("execution/table_summary_answer.rs")),
    ("execution/technical_answer.rs", include_str!("execution/technical_answer.rs")),
    (
        "execution/technical_literal_context.rs",
        include_str!("execution/technical_literal_context.rs"),
    ),
    (
        "execution/technical_literal_extractors.rs",
        include_str!("execution/technical_literal_extractors.rs"),
    ),
    ("execution/technical_literal_focus.rs", include_str!("execution/technical_literal_focus.rs")),
    ("execution/technical_literals.rs", include_str!("execution/technical_literals.rs")),
    (
        "execution/technical_parameter_answer.rs",
        include_str!("execution/technical_parameter_answer.rs"),
    ),
    ("execution/technical_url_answer.rs", include_str!("execution/technical_url_answer.rs")),
    ("execution/transport_answer.rs", include_str!("execution/transport_answer.rs")),
    ("execution/tuning.rs", include_str!("execution/tuning.rs")),
    ("execution/types.rs", include_str!("execution/types.rs")),
    ("execution/verification.rs", include_str!("execution/verification.rs")),
    ("text_match.rs", include_str!("text_match.rs")),
    ("result_cache.rs", include_str!("result_cache.rs")),
];

#[must_use]
pub(crate) fn cache_key(input: &QueryResultCacheKeyInput<'_>) -> String {
    let mut hasher = Sha256::new();
    update_uuid(&mut hasher, input.workspace_id);
    update_uuid(&mut hasher, input.library_id);
    update_str(&mut hasher, input.readable_content_fingerprint);
    update_i64(&mut hasher, input.graph_projection_version);
    update_str(&mut hasher, input.binding_fingerprint);
    update_str(&mut hasher, input.answer_system_prompt);
    update_str(&mut hasher, input.answer_runtime_fingerprint);
    update_str(&mut hasher, input.mode_label);
    update_usize(&mut hasher, input.top_k);
    hasher.update([u8::from(input.source_links_enabled)]);
    update_normalized_text(&mut hasher, input.user_question);
    match input.prompt_history_text {
        Some(history) => {
            hasher.update([1]);
            update_normalized_text(&mut hasher, history);
        }
        None => hasher.update([0]),
    }
    let hash = hex_digest(hasher.finalize());
    format!("query_result:{hash}")
}

pub(crate) fn answer_runtime_fingerprint() -> &'static str {
    static FINGERPRINT: LazyLock<String> = LazyLock::new(|| {
        let mut hasher = Sha256::new();
        for (path, source) in ANSWER_RUNTIME_FINGERPRINT_SOURCES {
            update_str(&mut hasher, path);
            update_str(&mut hasher, source);
        }
        hex_digest(hasher.finalize())
    });
    FINGERPRINT.as_str()
}

#[must_use]
pub(crate) fn lock_key(cache_key: &str) -> String {
    format!("query_result_lock:{cache_key}")
}

#[derive(Debug)]
pub(crate) struct QueryResultCacheFillGuard {
    client: redis::Client,
    cache_key: String,
    owner: Uuid,
    stop_renewal: Option<oneshot::Sender<()>>,
}

impl QueryResultCacheFillGuard {
    #[must_use]
    fn new(client: redis::Client, cache_key: &str, owner: Uuid) -> Self {
        let cache_key = cache_key.to_string();
        let stop_renewal = spawn_fill_lock_renewal(client.clone(), cache_key.clone(), owner);
        Self { client, cache_key, owner, stop_renewal }
    }
}

impl Drop for QueryResultCacheFillGuard {
    fn drop(&mut self) {
        if let Some(stop_renewal) = self.stop_renewal.take() {
            let _ = stop_renewal.send(());
        }
        let client = self.client.clone();
        let cache_key = self.cache_key.clone();
        let owner = self.owner;
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    if let Err(error) = release_fill_lock(&client, &cache_key, owner).await {
                        warn!(
                            error = %error,
                            cache_key = %cache_key,
                            owner = %owner,
                            "query result cache fill lock release failed"
                        );
                    }
                });
            }
            Err(error) => {
                warn!(
                    error = %error,
                    cache_key = %cache_key,
                    owner = %owner,
                    "query result cache fill lock dropped outside tokio runtime"
                );
            }
        }
    }
}

fn spawn_fill_lock_renewal(
    client: redis::Client,
    cache_key: String,
    owner: Uuid,
) -> Option<oneshot::Sender<()>> {
    let (stop_tx, mut stop_rx) = oneshot::channel();
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move {
                loop {
                    tokio::select! {
                        () = tokio::time::sleep(QUERY_RESULT_CACHE_LOCK_RENEW_INTERVAL) => {
                            match renew_fill_lock(&client, &cache_key, owner).await {
                                Ok(true) => {}
                                Ok(false) => {
                                    warn!(
                                        cache_key = %cache_key,
                                        owner = %owner,
                                        "query result cache fill lock renewal lost ownership"
                                    );
                                    break;
                                }
                                Err(error) => {
                                    warn!(
                                        error = %error,
                                        cache_key = %cache_key,
                                        owner = %owner,
                                        "query result cache fill lock renewal failed"
                                    );
                                    break;
                                }
                            }
                        }
                        _ = &mut stop_rx => break,
                    }
                }
            });
            Some(stop_tx)
        }
        Err(error) => {
            warn!(
                error = %error,
                cache_key = %cache_key,
                owner = %owner,
                "query result cache fill lock renewal unavailable outside tokio runtime"
            );
            None
        }
    }
}

pub(crate) async fn get_cached_execution_id(
    client: &redis::Client,
    key: &str,
) -> Result<Option<Uuid>> {
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .context("connect to redis for query result cache read")?;
    let raw: Option<Vec<u8>> = conn.get(key).await.context("redis GET query result cache")?;
    match raw {
        Some(bytes) => {
            let entry: CachedQueryResult =
                serde_json::from_slice(&bytes).context("decode query result cache payload")?;
            Ok(Some(entry.source_execution_id))
        }
        None => Ok(None),
    }
}

pub(crate) async fn put_cached_execution_id(
    client: &redis::Client,
    key: &str,
    source_execution_id: Uuid,
) -> Result<()> {
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .context("connect to redis for query result cache write")?;
    let bytes = serde_json::to_vec(&CachedQueryResult { source_execution_id })
        .context("encode query result cache payload")?;
    let _: () = conn
        .set_ex(key, bytes, QUERY_RESULT_CACHE_TTL_SECONDS)
        .await
        .context("redis SET EX query result cache")?;
    Ok(())
}

pub(crate) async fn try_acquire_fill_guard(
    client: &redis::Client,
    key: &str,
    owner: Uuid,
) -> Result<Option<QueryResultCacheFillGuard>> {
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .context("connect to redis for query result cache lock")?;
    let response: Option<String> = redis::cmd("SET")
        .arg(lock_key(key))
        .arg(owner.to_string())
        .arg("NX")
        .arg("EX")
        .arg(QUERY_RESULT_CACHE_LOCK_TTL_SECONDS)
        .query_async(&mut conn)
        .await
        .context("redis SET NX query result cache lock")?;
    Ok(response.is_some().then(|| QueryResultCacheFillGuard::new(client.clone(), key, owner)))
}

async fn release_fill_lock(client: &redis::Client, key: &str, owner: Uuid) -> Result<bool> {
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .context("connect to redis for query result cache lock release")?;
    let released: i64 = redis::cmd("EVAL")
        .arg(
            "if redis.call('GET', KEYS[1]) == ARGV[1] \
             then return redis.call('DEL', KEYS[1]) \
             else return 0 end",
        )
        .arg(1)
        .arg(lock_key(key))
        .arg(owner.to_string())
        .query_async(&mut conn)
        .await
        .context("redis EVAL query result cache lock release")?;
    Ok(released > 0)
}

async fn renew_fill_lock(client: &redis::Client, key: &str, owner: Uuid) -> Result<bool> {
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .context("connect to redis for query result cache lock renewal")?;
    let renewed: i64 = redis::cmd("EVAL")
        .arg(
            "if redis.call('GET', KEYS[1]) == ARGV[1] \
             then return redis.call('EXPIRE', KEYS[1], ARGV[2]) \
             else return 0 end",
        )
        .arg(1)
        .arg(lock_key(key))
        .arg(owner.to_string())
        .arg(QUERY_RESULT_CACHE_LOCK_TTL_SECONDS)
        .query_async(&mut conn)
        .await
        .context("redis EVAL query result cache lock renewal")?;
    Ok(renewed > 0)
}

fn update_uuid(hasher: &mut Sha256, value: Uuid) {
    hasher.update(value.as_bytes());
}

fn update_i64(hasher: &mut Sha256, value: i64) {
    hasher.update(value.to_le_bytes());
}

fn update_usize(hasher: &mut Sha256, value: usize) {
    hasher.update(value.to_le_bytes());
}

fn update_str(hasher: &mut Sha256, value: &str) {
    update_usize(hasher, value.len());
    hasher.update(value.as_bytes());
}

fn update_normalized_text(hasher: &mut Sha256, value: &str) {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    update_str(hasher, &normalized);
}

fn hex_digest(digest: sha2::digest::Output<Sha256>) -> String {
    let mut hash = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hash.push_str(&format!("{byte:02x}"));
    }
    hash
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn base_input(top_k: usize) -> QueryResultCacheKeyInput<'static> {
        QueryResultCacheKeyInput {
            workspace_id: Uuid::nil(),
            library_id: Uuid::nil(),
            readable_content_fingerprint: "content:baseline",
            graph_projection_version: 2,
            binding_fingerprint: "query_answer:00000000-0000-0000-0000-000000000001",
            answer_system_prompt: "grounded answer prompt",
            answer_runtime_fingerprint: "answer-runtime:baseline",
            mode_label: "mix",
            top_k,
            source_links_enabled: true,
            user_question: "  TargetName   how ",
            prompt_history_text: None,
        }
    }

    #[test]
    fn cache_key_carries_canonical_namespace_and_context() {
        let key = cache_key(&base_input(8));
        assert!(key.starts_with("query_result:"));
        assert_eq!(key.matches(':').count(), 1);
    }

    #[test]
    fn cache_key_changes_with_top_k() {
        assert_ne!(cache_key(&base_input(8)), cache_key(&base_input(12)));
    }

    #[test]
    fn cache_key_changes_with_readable_content_fingerprint() {
        let mut changed = base_input(8);
        changed.readable_content_fingerprint = "content:changed";
        assert_ne!(cache_key(&base_input(8)), cache_key(&changed));
    }

    #[test]
    fn cache_key_changes_with_graph_projection_version() {
        let mut changed = base_input(8);
        changed.graph_projection_version += 1;
        assert_ne!(cache_key(&base_input(8)), cache_key(&changed));
    }

    #[test]
    fn cache_key_changes_with_prompt_history() {
        let mut with_history = base_input(8);
        with_history.prompt_history_text = Some("assistant: target details");
        assert_ne!(cache_key(&base_input(8)), cache_key(&with_history));
    }

    #[test]
    fn cache_key_changes_with_answer_system_prompt() {
        let mut changed = base_input(8);
        changed.answer_system_prompt = "grounded answer prompt with updated semantics";
        assert_ne!(cache_key(&base_input(8)), cache_key(&changed));
    }

    #[test]
    fn cache_key_changes_with_answer_runtime_fingerprint() {
        let mut changed = base_input(8);
        changed.answer_runtime_fingerprint = "answer-runtime:changed";
        assert_ne!(cache_key(&base_input(8)), cache_key(&changed));
    }

    #[test]
    fn answer_runtime_fingerprint_covers_deterministic_answer_builders() {
        let paths =
            ANSWER_RUNTIME_FINGERPRINT_SOURCES.iter().map(|(path, _)| *path).collect::<Vec<_>>();
        let unique_paths = paths.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(paths.len(), unique_paths.len());
        for required in [
            "execution/answer.rs",
            "execution/endpoint_answer.rs",
            "execution/endpoint_chunk_answer.rs",
            "execution/focused_document_answer.rs",
            "execution/question_intent.rs",
            "execution/technical_answer.rs",
            "execution/technical_literal_focus.rs",
            "execution/technical_url_answer.rs",
            "execution/verification.rs",
        ] {
            assert!(
                unique_paths.contains(required),
                "answer result cache fingerprint must include {required}"
            );
        }
    }
}
