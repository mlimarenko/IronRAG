use std::{sync::LazyLock, time::Duration};

use anyhow::{Context, Result};
use futures::StreamExt as _;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;
use tracing::warn;
use uuid::Uuid;

use crate::app::state::{
    BulkIngestHardeningSettings, RetrievalIntelligenceSettings, SemanticRerankRuntimeSettings,
};

pub(crate) const QUERY_RESULT_CACHE_TTL_SECONDS: u64 = 300;
const QUERY_RESULT_CACHE_KEY_SCHEMA_VERSION: u8 = 3;
pub(crate) const QUERY_RESULT_CACHE_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const QUERY_RESULT_CACHE_WAIT_INTERVAL: Duration = Duration::from_millis(250);
pub(crate) const QUERY_RESULT_CACHE_NOTIFICATION_FALLBACK_INTERVAL: Duration =
    Duration::from_secs(2);
const QUERY_RESULT_CACHE_LOCK_TTL_SECONDS: u64 = 180;
const QUERY_RESULT_CACHE_LOCK_RENEW_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub(crate) struct QueryResultCacheKeyInput<'a> {
    pub(crate) workspace_id: Uuid,
    pub(crate) library_id: Uuid,
    pub(crate) source_truth_version: i64,
    pub(crate) library_answer_config_fingerprint: &'a str,
    pub(crate) readable_content_fingerprint: &'a str,
    pub(crate) graph_projection_version: i64,
    pub(crate) graph_topology_generation: i64,
    pub(crate) binding_fingerprint: &'a str,
    pub(crate) retrieval_policy_fingerprint: &'a str,
    pub(crate) answer_system_prompt: &'a str,
    pub(crate) answer_runtime_fingerprint: &'a str,
    pub(crate) mode_label: &'static str,
    pub(crate) top_k: usize,
    pub(crate) user_question: &'a str,
    pub(crate) effective_question: &'a str,
    pub(crate) answer_history_text: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
struct CachedQueryResult {
    source_execution_id: Uuid,
}

const ANSWER_RUNTIME_FINGERPRINT_SOURCES: &[(&str, &str)] = &[
    ("domains/query.rs", include_str!("../../domains/query.rs")),
    ("domains/query_ir.rs", include_str!("../../domains/query_ir.rs")),
    ("assistant_prompt.rs", include_str!("assistant_prompt.rs")),
    ("compiler.rs", include_str!("compiler.rs")),
    ("completion_policy.rs", include_str!("completion_policy.rs")),
    ("latest_versions.rs", include_str!("latest_versions.rs")),
    ("execution/answer.rs", include_str!("execution/answer.rs")),
    ("execution/answer_kind.rs", include_str!("execution/answer_kind.rs")),
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
    ("execution/fusion.rs", include_str!("execution/fusion.rs")),
    ("execution/graph_retrieval.rs", include_str!("execution/graph_retrieval.rs")),
    ("execution/hyde.rs", include_str!("execution/hyde.rs")),
    ("execution/port_answer.rs", include_str!("execution/port_answer.rs")),
    ("execution/preflight.rs", include_str!("execution/preflight.rs")),
    ("execution/question_intent.rs", include_str!("execution/question_intent.rs")),
    ("execution/rerank.rs", include_str!("execution/rerank.rs")),
    ("execution/retrieval_plan.rs", include_str!("execution/retrieval_plan.rs")),
    ("execution/semantic_rerank.rs", include_str!("execution/semantic_rerank.rs")),
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
    ("execution/tuning.rs", include_str!("execution/tuning.rs")),
    ("execution/types.rs", include_str!("execution/types.rs")),
    ("execution/verification.rs", include_str!("execution/verification.rs")),
    ("execution/verification_claims.rs", include_str!("execution/verification_claims.rs")),
    ("execution/verification_policy.rs", include_str!("execution/verification_policy.rs")),
    ("execution/verification_support.rs", include_str!("execution/verification_support.rs")),
    ("execution/mod.rs", include_str!("execution/mod.rs")),
    ("planner.rs", include_str!("planner.rs")),
    ("search.rs", include_str!("search.rs")),
    ("i18n/mod.rs", include_str!("i18n/mod.rs")),
    ("service/context.rs", include_str!("service/context.rs")),
    ("service/formatting.rs", include_str!("service/formatting.rs")),
    ("service/turn.rs", include_str!("service/turn.rs")),
    ("text_match.rs", include_str!("text_match.rs")),
    ("result_cache.rs", include_str!("result_cache.rs")),
];

#[must_use]
pub(crate) fn cache_key(input: &QueryResultCacheKeyInput<'_>) -> String {
    let mut hasher = Sha256::new();
    update_str(
        &mut hasher,
        &format!("query-result-cache-key:v{QUERY_RESULT_CACHE_KEY_SCHEMA_VERSION}"),
    );
    update_uuid(&mut hasher, input.workspace_id);
    update_uuid(&mut hasher, input.library_id);
    update_i64(&mut hasher, input.source_truth_version);
    update_str(&mut hasher, input.library_answer_config_fingerprint);
    update_str(&mut hasher, input.readable_content_fingerprint);
    update_i64(&mut hasher, input.graph_projection_version);
    update_i64(&mut hasher, input.graph_topology_generation);
    update_str(&mut hasher, input.binding_fingerprint);
    update_str(&mut hasher, input.retrieval_policy_fingerprint);
    update_str(&mut hasher, input.answer_system_prompt);
    update_str(&mut hasher, input.answer_runtime_fingerprint);
    update_str(&mut hasher, input.mode_label);
    update_usize(&mut hasher, input.top_k);
    // Question bytes are deliberately exact and length-framed. Case and
    // whitespace can be semantically significant (identifiers, literals,
    // quoted source text), so normalization here can replay an answer for a
    // different request.
    update_str(&mut hasher, input.user_question);
    update_str(&mut hasher, input.effective_question);
    match input.answer_history_text {
        Some(history) => {
            hasher.update([1]);
            update_str(&mut hasher, history);
        }
        None => hasher.update([0]),
    }
    let hash = hex_digest(hasher.finalize());
    format!("query_result:v{QUERY_RESULT_CACHE_KEY_SCHEMA_VERSION}:{hash}")
}

pub(crate) fn answer_runtime_fingerprint() -> &'static str {
    static FINGERPRINT: LazyLock<String> = LazyLock::new(|| {
        let mut hasher = Sha256::new();
        update_str(&mut hasher, "cargo-package-version");
        update_str(&mut hasher, env!("CARGO_PKG_VERSION"));
        update_str(&mut hasher, "database-migrations");
        update_str(&mut hasher, env!("IRONRAG_MIGRATIONS_FINGERPRINT"));
        for (path, source) in ANSWER_RUNTIME_FINGERPRINT_SOURCES {
            update_str(&mut hasher, path);
            update_str(&mut hasher, source);
        }
        hex_digest(hasher.finalize())
    });
    FINGERPRINT.as_str()
}

#[must_use]
pub(crate) fn semantic_rerank_runtime_fingerprint(
    settings: SemanticRerankRuntimeSettings,
) -> String {
    let settings = settings.bounded();
    format!(
        "semantic-rerank:v1:{}:{}:{}:{}:{}",
        settings.mode.as_str(),
        settings.timeout_ms,
        settings.candidate_limit,
        settings.candidate_text_chars,
        settings.total_text_chars,
    )
}

/// Fingerprint every runtime setting currently read by the response-producing
/// retrieval/rerank pipeline. Ingest/reconciliation settings intentionally do
/// not participate because they are already represented by the readable
/// content and graph generation fingerprints.
#[must_use]
pub(crate) fn retrieval_policy_fingerprint(
    settings: &RetrievalIntelligenceSettings,
    graph_policy: &BulkIngestHardeningSettings,
) -> String {
    format!(
        "retrieval-policy:v3:rerank={}:candidate-limit={}:balanced-context={}:graph-empty={}:graph-self-loops={}:graph-warning-backlog={}:{}",
        settings.rerank_enabled,
        settings.rerank_candidate_limit,
        settings.balanced_context_enabled,
        graph_policy.graph_filter_empty_relations,
        graph_policy.graph_filter_degenerate_self_loops,
        graph_policy.graph_convergence_warning_backlog_threshold,
        semantic_rerank_runtime_fingerprint(settings.semantic_rerank),
    )
}

#[must_use]
pub(crate) fn lock_key(cache_key: &str) -> String {
    format!("query_result_lock:{cache_key}")
}

#[must_use]
pub(crate) fn notification_channel(cache_key: &str) -> String {
    format!("query_result_ready:{cache_key}")
}

pub(crate) struct QueryResultCacheWaiter {
    pubsub: redis::aio::PubSub,
}

impl QueryResultCacheWaiter {
    /// Waits for a cache-fill notification without treating timeout as an
    /// error. Callers always re-read the canonical cache after a wakeup.
    pub(crate) async fn wait_for_notification(&mut self, timeout: Duration) -> Result<bool> {
        match tokio::time::timeout(timeout, self.pubsub.on_message().next()).await {
            Ok(Some(_message)) => Ok(true),
            Ok(None) => anyhow::bail!("redis query result cache notification stream ended"),
            Err(_) => Ok(false),
        }
    }
}

pub(crate) async fn subscribe_fill_notifications(
    client: &redis::Client,
    key: &str,
) -> Result<QueryResultCacheWaiter> {
    let mut pubsub = client
        .get_async_pubsub()
        .await
        .context("connect to redis for query result cache notifications")?;
    pubsub
        .subscribe(notification_channel(key))
        .await
        .context("subscribe to query result cache notifications")?;
    Ok(QueryResultCacheWaiter { pubsub })
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

#[cfg(test)]
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
    ttl_seconds: u64,
) -> Result<()> {
    anyhow::ensure!(ttl_seconds > 0, "query result cache TTL must be positive");
    let ttl_seconds = ttl_seconds.min(QUERY_RESULT_CACHE_TTL_SECONDS);
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .context("connect to redis for query result cache write")?;
    let bytes = serde_json::to_vec(&CachedQueryResult { source_execution_id })
        .context("encode query result cache payload")?;
    let _: () =
        conn.set_ex(key, bytes, ttl_seconds).await.context("redis SET EX query result cache")?;
    let _: usize = conn
        .publish(notification_channel(key), source_execution_id.to_string())
        .await
        .context("redis PUBLISH query result cache notification")?;
    Ok(())
}

/// Bounds the remaining lifetime computed by PostgreSQL. Returning that
/// original lifetime when warming Redis prevents an early Redis eviction from
/// extending a persistent row, without trusting an API replica's wall clock.
#[must_use]
pub(crate) fn bounded_db_remaining_ttl_seconds(remaining_seconds: i64) -> Option<u64> {
    if remaining_seconds <= 0 {
        return None;
    }
    Some(u64::try_from(remaining_seconds).ok()?.min(QUERY_RESULT_CACHE_TTL_SECONDS))
}

/// Deletes a Redis winner only when it still points at the stale execution the
/// caller inspected. A concurrent replacement must survive old-reader cleanup.
pub(crate) async fn delete_cached_execution_id_if_matches(
    client: &redis::Client,
    key: &str,
    expected_source_execution_id: Uuid,
) -> Result<bool> {
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .context("connect to redis for conditional query result cache delete")?;
    let expected = serde_json::to_vec(&CachedQueryResult {
        source_execution_id: expected_source_execution_id,
    })
    .context("encode expected query result cache payload")?;
    let deleted: i64 = redis::cmd("EVAL")
        .arg(
            "if redis.call('GET', KEYS[1]) == ARGV[1] \
             then return redis.call('DEL', KEYS[1]) \
             else return 0 end",
        )
        .arg(1)
        .arg(key)
        .arg(expected)
        .query_async(&mut conn)
        .await
        .context("redis EVAL conditional query result cache delete")?;
    Ok(deleted > 0)
}

/// Structural test for a Redis *availability* failure (server down,
/// unreachable, connection dropped, or timed out) as opposed to a
/// protocol/server error that signals something genuinely wrong.
///
/// The fill-lock fast path must degrade to cache-miss latency when Redis
/// is simply unavailable, never surface a 5xx — the answer pipeline does
/// not need Redis to compute. We classify on the typed [`redis::RedisError`]
/// error kind, never on message-string matching.
#[must_use]
fn redis_error_is_connectivity(error: &redis::RedisError) -> bool {
    error.is_io_error()
        || error.is_connection_refusal()
        || error.is_connection_dropped()
        || error.is_timeout()
}

/// Decide whether a fill-lock acquisition error should fail *open* (proceed
/// without the lock, degrading to cache-miss behaviour) or fail *closed*
/// (surface coordination-unavailable to the caller).
///
/// Errors returned by [`try_acquire_fill_guard`] are always Redis
/// connect/command failures — genuine lock contention is reported as
/// `Ok(None)`, not `Err`. We still distinguish the two error *classes*
/// structurally: a connectivity failure (Redis down) fails open; any other
/// Redis error (e.g. an unexpected server/protocol fault) keeps the
/// conservative fail-closed semantics so a real malfunction stays visible.
#[must_use]
pub(crate) fn fill_lock_error_fails_open(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<redis::RedisError>().is_some_and(redis_error_is_connectivity)
    })
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
            source_truth_version: 7,
            library_answer_config_fingerprint: "library-answer:v1:include-hint=true:retrieval=baseline",
            readable_content_fingerprint: "content:baseline",
            graph_projection_version: 2,
            graph_topology_generation: 5,
            binding_fingerprint: "query_answer:00000000-0000-0000-0000-000000000001",
            retrieval_policy_fingerprint: "retrieval-policy:v2:rerank=true:candidate-limit=24:balanced-context=true:semantic-rerank:v1:off:1500:16:1200:18000",
            answer_system_prompt: "grounded answer prompt",
            answer_runtime_fingerprint: "answer-runtime:baseline",
            mode_label: "mix",
            top_k,
            user_question: "TargetName   how",
            effective_question: "scope: target details\nquestion: TargetName how",
            answer_history_text: None,
        }
    }

    #[test]
    fn cache_key_carries_canonical_namespace_and_context() {
        let key = cache_key(&base_input(8));
        assert!(key.starts_with("query_result:v3:"));
        assert_eq!(key.matches(':').count(), 2);
        assert_eq!(notification_channel(&key), format!("query_result_ready:{key}"));
    }

    #[test]
    fn cache_key_keeps_question_case_exact() {
        let mut changed = base_input(8);
        changed.user_question = "targetname   how";

        assert_ne!(cache_key(&base_input(8)), cache_key(&changed));
    }

    #[test]
    fn cache_key_keeps_question_whitespace_exact() {
        let mut changed = base_input(8);
        changed.user_question = "TargetName how";

        assert_ne!(cache_key(&base_input(8)), cache_key(&changed));
    }

    #[test]
    fn cache_key_length_frames_adjacent_question_fields() {
        let mut left = base_input(8);
        left.user_question = "ab";
        left.effective_question = "c";
        let mut right = base_input(8);
        right.user_question = "a";
        right.effective_question = "bc";

        assert_ne!(cache_key(&left), cache_key(&right));
    }

    #[test]
    fn cache_key_changes_with_top_k() {
        assert_ne!(cache_key(&base_input(8)), cache_key(&base_input(12)));
    }

    #[test]
    fn cache_key_changes_with_effective_question() {
        let mut changed = base_input(8);
        changed.effective_question = "scope: different details\nquestion: TargetName how";
        assert_ne!(cache_key(&base_input(8)), cache_key(&changed));
    }

    #[test]
    fn cache_key_changes_with_readable_content_fingerprint() {
        let mut changed = base_input(8);
        changed.readable_content_fingerprint = "content:changed";
        assert_ne!(cache_key(&base_input(8)), cache_key(&changed));
    }

    #[test]
    fn cache_key_changes_with_committed_source_truth_version() {
        let mut changed = base_input(8);
        changed.source_truth_version += 1;

        assert_ne!(cache_key(&base_input(8)), cache_key(&changed));
    }

    #[test]
    fn cache_key_changes_with_query_visible_library_config() {
        let mut changed = base_input(8);
        changed.library_answer_config_fingerprint =
            "library-answer:v1:include-hint=false:retrieval=baseline";

        assert_ne!(cache_key(&base_input(8)), cache_key(&changed));
    }

    #[test]
    fn cache_key_changes_with_graph_projection_version() {
        let mut changed = base_input(8);
        changed.graph_projection_version += 1;
        assert_ne!(cache_key(&base_input(8)), cache_key(&changed));
    }

    #[test]
    fn cache_key_changes_with_graph_topology_generation() {
        let mut changed = base_input(8);
        changed.graph_topology_generation += 1;
        assert_ne!(cache_key(&base_input(8)), cache_key(&changed));
    }

    #[test]
    fn cache_key_changes_with_answer_history() {
        let mut with_history = base_input(8);
        with_history.answer_history_text = Some("assistant: target details");
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
    fn semantic_rerank_fingerprint_tracks_mode_and_every_policy_budget() {
        use crate::{
            app::state::SemanticRerankRuntimeSettings, domains::query::SemanticRerankMode,
        };

        let base = SemanticRerankRuntimeSettings {
            mode: SemanticRerankMode::Off,
            timeout_ms: 1_500,
            candidate_limit: 16,
            candidate_text_chars: 1_200,
            total_text_chars: 18_000,
        };
        let baseline = semantic_rerank_runtime_fingerprint(base);
        for changed in [
            SemanticRerankRuntimeSettings { mode: SemanticRerankMode::Shadow, ..base },
            SemanticRerankRuntimeSettings { mode: SemanticRerankMode::Active, ..base },
            SemanticRerankRuntimeSettings { timeout_ms: 1_499, ..base },
            SemanticRerankRuntimeSettings { candidate_limit: 15, ..base },
            SemanticRerankRuntimeSettings { candidate_text_chars: 1_199, ..base },
            SemanticRerankRuntimeSettings { total_text_chars: 17_999, ..base },
        ] {
            assert_ne!(baseline, semantic_rerank_runtime_fingerprint(changed));
        }
    }

    #[test]
    fn semantic_rerank_fingerprint_uses_effective_hard_bounded_policy() {
        use crate::{
            app::state::SemanticRerankRuntimeSettings, domains::query::SemanticRerankMode,
        };

        let hard_max = SemanticRerankRuntimeSettings {
            mode: SemanticRerankMode::Active,
            timeout_ms: 3_000,
            candidate_limit: 32,
            candidate_text_chars: 2_400,
            total_text_chars: 32_000,
        };
        let oversized = SemanticRerankRuntimeSettings {
            timeout_ms: u64::MAX,
            candidate_limit: usize::MAX,
            candidate_text_chars: usize::MAX,
            total_text_chars: usize::MAX,
            ..hard_max
        };

        assert_eq!(
            semantic_rerank_runtime_fingerprint(hard_max),
            semantic_rerank_runtime_fingerprint(oversized)
        );
    }

    fn retrieval_settings() -> RetrievalIntelligenceSettings {
        use crate::domains::query::SemanticRerankMode;

        RetrievalIntelligenceSettings {
            query_intent_cache_ttl_hours: 24,
            query_intent_cache_max_entries_per_library: 500,
            rerank_enabled: true,
            rerank_candidate_limit: 24,
            semantic_rerank: SemanticRerankRuntimeSettings {
                mode: SemanticRerankMode::Off,
                timeout_ms: 1_500,
                candidate_limit: 16,
                candidate_text_chars: 1_200,
                total_text_chars: 18_000,
            },
            balanced_context_enabled: true,
            extraction_recovery_enabled: true,
            extraction_recovery_max_attempts: 2,
            summary_refresh_batch_size: 10,
            targeted_reconciliation_enabled: true,
            targeted_reconciliation_max_targets: 10,
        }
    }

    fn graph_policy() -> BulkIngestHardeningSettings {
        BulkIngestHardeningSettings {
            document_activity_freshness_seconds: 60,
            document_stalled_after_seconds: 300,
            graph_filter_empty_relations: true,
            graph_filter_degenerate_self_loops: true,
            graph_convergence_warning_backlog_threshold: 100,
        }
    }

    #[test]
    fn retrieval_policy_fingerprint_tracks_every_response_setting() {
        use crate::domains::query::SemanticRerankMode;

        let baseline_settings = retrieval_settings();
        let baseline = retrieval_policy_fingerprint(&baseline_settings, &graph_policy());

        let mut rerank_enabled = retrieval_settings();
        rerank_enabled.rerank_enabled = false;
        let mut candidate_limit = retrieval_settings();
        candidate_limit.rerank_candidate_limit = 23;
        let mut balanced_context = retrieval_settings();
        balanced_context.balanced_context_enabled = false;
        let mut semantic_mode = retrieval_settings();
        semantic_mode.semantic_rerank.mode = SemanticRerankMode::Active;
        let mut semantic_timeout = retrieval_settings();
        semantic_timeout.semantic_rerank.timeout_ms = 1_499;
        let mut semantic_candidate_limit = retrieval_settings();
        semantic_candidate_limit.semantic_rerank.candidate_limit = 15;
        let mut semantic_candidate_chars = retrieval_settings();
        semantic_candidate_chars.semantic_rerank.candidate_text_chars = 1_199;
        let mut semantic_total_chars = retrieval_settings();
        semantic_total_chars.semantic_rerank.total_text_chars = 17_999;

        for changed in [
            rerank_enabled,
            candidate_limit,
            balanced_context,
            semantic_mode,
            semantic_timeout,
            semantic_candidate_limit,
            semantic_candidate_chars,
            semantic_total_chars,
        ] {
            assert_ne!(baseline, retrieval_policy_fingerprint(&changed, &graph_policy()));
        }
    }

    #[test]
    fn retrieval_policy_ignores_ingest_only_settings() {
        let baseline_settings = retrieval_settings();
        let baseline = retrieval_policy_fingerprint(&baseline_settings, &graph_policy());
        let mut changed = retrieval_settings();
        changed.extraction_recovery_enabled = false;
        changed.extraction_recovery_max_attempts += 1;
        changed.summary_refresh_batch_size += 1;
        changed.targeted_reconciliation_enabled = false;
        changed.targeted_reconciliation_max_targets += 1;

        assert_eq!(baseline, retrieval_policy_fingerprint(&changed, &graph_policy()));
    }

    #[test]
    fn retrieval_policy_tracks_effective_graph_filter_configuration() {
        let settings = retrieval_settings();
        let baseline = retrieval_policy_fingerprint(&settings, &graph_policy());
        let mut empty_relations = graph_policy();
        empty_relations.graph_filter_empty_relations = false;
        let mut self_loops = graph_policy();
        self_loops.graph_filter_degenerate_self_loops = false;
        let mut warning_backlog = graph_policy();
        warning_backlog.graph_convergence_warning_backlog_threshold += 1;

        for changed in [empty_relations, self_loops, warning_backlog] {
            assert_ne!(baseline, retrieval_policy_fingerprint(&settings, &changed));
        }
    }

    #[test]
    fn cache_key_changes_with_semantic_rerank_policy_and_binding() {
        let baseline = base_input(8);
        let mut policy = baseline.clone();
        policy.retrieval_policy_fingerprint = "retrieval-policy:v3:changed";
        let mut binding = baseline.clone();
        binding.binding_fingerprint = "rerank:00000000-0000-0000-0000-000000000002";

        assert_ne!(cache_key(&baseline), cache_key(&policy));
        assert_ne!(cache_key(&baseline), cache_key(&binding));
    }

    #[test]
    fn fill_lock_connectivity_error_fails_open() {
        // An IO-class Redis error (server down / unreachable) must degrade to
        // cache-miss behaviour rather than surface a coordination 5xx.
        let io = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "connection refused");
        let redis_error = redis::RedisError::from(io);
        assert!(redis_error.is_connection_refusal() || redis_error.is_io_error());
        let wrapped =
            anyhow::Error::new(redis_error).context("connect to redis for query result cache lock");
        assert!(fill_lock_error_fails_open(&wrapped));
    }

    #[test]
    fn database_ttl_is_absolute_and_bounded() {
        assert_eq!(bounded_db_remaining_ttl_seconds(300), Some(QUERY_RESULT_CACHE_TTL_SECONDS));
        assert_eq!(bounded_db_remaining_ttl_seconds(1), Some(1));
        assert_eq!(bounded_db_remaining_ttl_seconds(301), Some(300));
        assert_eq!(bounded_db_remaining_ttl_seconds(0), None);
        assert_eq!(bounded_db_remaining_ttl_seconds(-1), None);
    }

    #[test]
    fn fill_lock_protocol_error_fails_closed() {
        // A non-connectivity Redis error (e.g. a server/extension fault) is not
        // a routine outage; keep conservative fail-closed semantics so a real
        // malfunction stays visible instead of silently skipping coordination.
        let redis_error =
            redis::RedisError::from((redis::ErrorKind::Extension, "unexpected server response"));
        assert!(!redis_error.is_io_error());
        assert!(!redis_error.is_connection_refusal());
        assert!(!redis_error.is_connection_dropped());
        assert!(!redis_error.is_timeout());
        let wrapped =
            anyhow::Error::new(redis_error).context("redis SET NX query result cache lock");
        assert!(!fill_lock_error_fails_open(&wrapped));
    }

    #[test]
    fn fill_lock_non_redis_error_fails_closed() {
        // An anyhow error with no Redis cause in the chain is not a Redis
        // availability signal and must not fail open.
        let wrapped = anyhow::anyhow!("unrelated failure");
        assert!(!fill_lock_error_fails_open(&wrapped));
    }

    #[tokio::test]
    #[ignore = "requires local redis service"]
    async fn waiter_rechecks_winner_after_acquiring_released_fill_lock() -> Result<()> {
        let redis_url = std::env::var("IRONRAG_REDIS_URL")
            .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
        let client = redis::Client::open(redis_url)?;
        let key = format!("query_result:v3:post-lock-recheck-test:{}", Uuid::now_v7());
        let first_owner = Uuid::now_v7();
        let first_guard = try_acquire_fill_guard(&client, &key, first_owner)
            .await?
            .expect("first owner must acquire the fill lock");
        assert!(
            try_acquire_fill_guard(&client, &key, Uuid::now_v7()).await?.is_none(),
            "waiter must first observe contention",
        );

        let source_execution_id = Uuid::now_v7();
        put_cached_execution_id(&client, &key, source_execution_id, QUERY_RESULT_CACHE_TTL_SECONDS)
            .await?;
        drop(first_guard);

        let second_guard = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(guard) = try_acquire_fill_guard(&client, &key, Uuid::now_v7()).await? {
                    return Ok::<_, anyhow::Error>(guard);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .context("timed out waiting for released query result fill lock")??;

        assert_eq!(get_cached_execution_id(&client, &key).await?, Some(source_execution_id));
        drop(second_guard);
        assert!(delete_cached_execution_id_if_matches(&client, &key, source_execution_id).await?);
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires local redis service"]
    async fn stale_reader_cannot_delete_replaced_redis_winner() -> Result<()> {
        let redis_url = std::env::var("IRONRAG_REDIS_URL")
            .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
        let client = redis::Client::open(redis_url)?;
        let key = format!("query_result:v3:conditional-delete-test:{}", Uuid::now_v7());
        let old_winner = Uuid::now_v7();
        let new_winner = Uuid::now_v7();
        put_cached_execution_id(&client, &key, old_winner, QUERY_RESULT_CACHE_TTL_SECONDS).await?;
        put_cached_execution_id(&client, &key, new_winner, QUERY_RESULT_CACHE_TTL_SECONDS).await?;

        assert!(!delete_cached_execution_id_if_matches(&client, &key, old_winner).await?);
        assert_eq!(get_cached_execution_id(&client, &key).await?, Some(new_winner));
        assert!(delete_cached_execution_id_if_matches(&client, &key, new_winner).await?);
        assert_eq!(get_cached_execution_id(&client, &key).await?, None);
        Ok(())
    }

    #[test]
    fn answer_runtime_fingerprint_covers_deterministic_answer_builders() {
        let paths =
            ANSWER_RUNTIME_FINGERPRINT_SOURCES.iter().map(|(path, _)| *path).collect::<Vec<_>>();
        let unique_paths = paths.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(paths.len(), unique_paths.len());
        for required in [
            "domains/query.rs",
            "domains/query_ir.rs",
            "completion_policy.rs",
            "execution/answer.rs",
            "execution/endpoint_answer.rs",
            "execution/endpoint_chunk_answer.rs",
            "execution/focused_document_answer.rs",
            "execution/fusion.rs",
            "execution/question_intent.rs",
            "execution/retrieval_plan.rs",
            "execution/semantic_rerank.rs",
            "execution/technical_answer.rs",
            "execution/technical_literal_focus.rs",
            "execution/technical_url_answer.rs",
            "execution/verification.rs",
            "execution/verification_claims.rs",
            "execution/verification_policy.rs",
            "execution/verification_support.rs",
            "i18n/mod.rs",
        ] {
            assert!(
                unique_paths.contains(required),
                "answer result cache fingerprint must include {required}"
            );
        }
    }
}
