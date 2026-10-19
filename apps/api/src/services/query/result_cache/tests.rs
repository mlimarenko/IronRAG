use std::collections::BTreeSet;

use anyhow::{Context, Result};

use super::redis_cache::{
    bounded_db_remaining_ttl_seconds, delete_cached_execution_id_if_matches,
    fill_lock_error_fails_open, get_cached_execution_id, notification_channel,
    put_cached_execution_id, try_acquire_fill_guard,
};
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
fn cache_key_carries_stable_namespace_and_context() {
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
    use crate::{app::state::SemanticRerankRuntimeSettings, domains::query::SemanticRerankMode};

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
    use crate::{app::state::SemanticRerankRuntimeSettings, domains::query::SemanticRerankMode};

    let hard_max = SemanticRerankRuntimeSettings {
        mode: SemanticRerankMode::Active,
        timeout_ms: 5_000,
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
    let wrapped = anyhow::Error::new(redis_error).context("redis SET NX query result cache lock");
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
    let redis_url =
        std::env::var("IRONRAG_REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
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
    let redis_url =
        std::env::var("IRONRAG_REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
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
        "execution/answer/mod.rs",
        "execution/answer/context_rendering.rs",
        "execution/answer/deterministic.rs",
        "execution/answer/procedure_intent.rs",
        "execution/answer/procedure_parsing.rs",
        "execution/answer/procedure_projection.rs",
        "execution/answer/procedure_selection.rs",
        "execution/answer/prompt.rs",
        "execution/answer/setup_configuration.rs",
        "execution/answer/setup_literals.rs",
        "execution/answer/source_slice.rs",
        "execution/answer/source_unit_focus.rs",
        "execution/answer/source_unit_inventory.rs",
        "execution/answer/structural_identifiers.rs",
        "execution/answer/structured_list.rs",
        "execution/answer/version_changes.rs",
        "execution/consolidation/focus.rs",
        "execution/consolidation/mod.rs",
        "execution/consolidation/orchestration.rs",
        "execution/consolidation/selection.rs",
        "execution/endpoint_answer.rs",
        "execution/endpoint_chunk_answer.rs",
        "execution/focused_document_answer.rs",
        "execution/fusion.rs",
        "execution/question_intent.rs",
        "execution/retrieval_plan.rs",
        "execution/semantic_rerank.rs",
        "execution/source_context/budgeting.rs",
        "execution/source_context/candidates.rs",
        "execution/source_context/companions.rs",
        "execution/source_context/loading.rs",
        "execution/source_context/mod.rs",
        "execution/source_context/orchestration.rs",
        "execution/source_context/rendering.rs",
        "execution/source_context/selection.rs",
        "execution/source_context/source_slice.rs",
        "execution/source_context/types.rs",
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
