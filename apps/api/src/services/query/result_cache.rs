use std::{sync::LazyLock, time::Duration};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::app::state::{
    BulkIngestHardeningSettings, RetrievalIntelligenceSettings, SemanticRerankRuntimeSettings,
};

mod redis_cache;
#[cfg(test)]
mod tests;

pub(crate) use redis_cache::{
    QueryResultCacheFillGuard, QueryResultCacheWaiter, bounded_db_remaining_ttl_seconds,
    delete_cached_execution_id_if_matches, fill_lock_error_fails_open, put_cached_execution_id,
    subscribe_fill_notifications, try_acquire_fill_guard,
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
    ("domains/query_ir/schema.rs", include_str!("../../domains/query_ir/schema.rs")),
    ("domains/query_ir/tests.rs", include_str!("../../domains/query_ir/tests.rs")),
    ("domains/query_ir/types.rs", include_str!("../../domains/query_ir/types.rs")),
    (
        "domains/query_ir/validation_tests.rs",
        include_str!("../../domains/query_ir/validation_tests.rs"),
    ),
    ("assistant_prompt.rs", include_str!("assistant_prompt.rs")),
    ("compiler.rs", include_str!("compiler.rs")),
    ("compiler/fingerprint.rs", include_str!("compiler/fingerprint.rs")),
    ("compiler/ir_cache.rs", include_str!("compiler/ir_cache.rs")),
    ("compiler/provider_free.rs", include_str!("compiler/provider_free.rs")),
    ("compiler/tests.rs", include_str!("compiler/tests.rs")),
    ("compiler/tests/cache.rs", include_str!("compiler/tests/cache.rs")),
    (
        "compiler/tests/fallback_and_prompt.rs",
        include_str!("compiler/tests/fallback_and_prompt.rs"),
    ),
    ("compiler/tests/provider_behavior.rs", include_str!("compiler/tests/provider_behavior.rs")),
    ("completion_policy.rs", include_str!("completion_policy.rs")),
    ("latest_versions.rs", include_str!("latest_versions.rs")),
    ("execution/answer/mod.rs", include_str!("execution/answer/mod.rs")),
    (
        "execution/answer/context_rendering.rs",
        include_str!("execution/answer/context_rendering.rs"),
    ),
    (
        "execution/answer/context_rendering_tests.rs",
        include_str!("execution/answer/context_rendering_tests.rs"),
    ),
    ("execution/answer/deterministic.rs", include_str!("execution/answer/deterministic.rs")),
    ("execution/answer/procedure_intent.rs", include_str!("execution/answer/procedure_intent.rs")),
    (
        "execution/answer/procedure_intent_tests.rs",
        include_str!("execution/answer/procedure_intent_tests.rs"),
    ),
    (
        "execution/answer/procedure_parsing.rs",
        include_str!("execution/answer/procedure_parsing.rs"),
    ),
    (
        "execution/answer/procedure_parsing_tests.rs",
        include_str!("execution/answer/procedure_parsing_tests.rs"),
    ),
    (
        "execution/answer/procedure_projection.rs",
        include_str!("execution/answer/procedure_projection.rs"),
    ),
    (
        "execution/answer/procedure_selection.rs",
        include_str!("execution/answer/procedure_selection.rs"),
    ),
    ("execution/answer/prompt.rs", include_str!("execution/answer/prompt.rs")),
    (
        "execution/answer/setup_configuration.rs",
        include_str!("execution/answer/setup_configuration.rs"),
    ),
    (
        "execution/answer/setup_configuration_tests.rs",
        include_str!("execution/answer/setup_configuration_tests.rs"),
    ),
    ("execution/answer/setup_literals.rs", include_str!("execution/answer/setup_literals.rs")),
    ("execution/answer/source_slice.rs", include_str!("execution/answer/source_slice.rs")),
    (
        "execution/answer/source_slice_tests.rs",
        include_str!("execution/answer/source_slice_tests.rs"),
    ),
    (
        "execution/answer/source_unit_focus.rs",
        include_str!("execution/answer/source_unit_focus.rs"),
    ),
    (
        "execution/answer/source_unit_inventory.rs",
        include_str!("execution/answer/source_unit_inventory.rs"),
    ),
    (
        "execution/answer/source_unit_inventory_tests.rs",
        include_str!("execution/answer/source_unit_inventory_tests.rs"),
    ),
    (
        "execution/answer/structural_identifiers.rs",
        include_str!("execution/answer/structural_identifiers.rs"),
    ),
    ("execution/answer/structured_list.rs", include_str!("execution/answer/structured_list.rs")),
    (
        "execution/answer/structured_list_tests.rs",
        include_str!("execution/answer/structured_list_tests.rs"),
    ),
    (
        "execution/answer/test_support_tests.rs",
        include_str!("execution/answer/test_support_tests.rs"),
    ),
    ("execution/answer/version_changes.rs", include_str!("execution/answer/version_changes.rs")),
    (
        "execution/answer/version_changes_tests.rs",
        include_str!("execution/answer/version_changes_tests.rs"),
    ),
    ("execution/answer_kind.rs", include_str!("execution/answer_kind.rs")),
    ("execution/answer_pipeline/mod.rs", include_str!("execution/answer_pipeline/mod.rs")),
    (
        "execution/answer_pipeline/clarification.rs",
        include_str!("execution/answer_pipeline/clarification.rs"),
    ),
    (
        "execution/answer_pipeline/clarification_tests.rs",
        include_str!("execution/answer_pipeline/clarification_tests.rs"),
    ),
    (
        "execution/answer_pipeline/context_probes.rs",
        include_str!("execution/answer_pipeline/context_probes.rs"),
    ),
    (
        "execution/answer_pipeline/context_probes_tests.rs",
        include_str!("execution/answer_pipeline/context_probes_tests.rs"),
    ),
    (
        "execution/answer_pipeline/coverage.rs",
        include_str!("execution/answer_pipeline/coverage.rs"),
    ),
    (
        "execution/answer_pipeline/coverage_tests.rs",
        include_str!("execution/answer_pipeline/coverage_tests.rs"),
    ),
    (
        "execution/answer_pipeline/deterministic.rs",
        include_str!("execution/answer_pipeline/deterministic.rs"),
    ),
    (
        "execution/answer_pipeline/disposition.rs",
        include_str!("execution/answer_pipeline/disposition.rs"),
    ),
    (
        "execution/answer_pipeline/disposition_tests.rs",
        include_str!("execution/answer_pipeline/disposition_tests.rs"),
    ),
    (
        "execution/answer_pipeline/literal_coverage.rs",
        include_str!("execution/answer_pipeline/literal_coverage.rs"),
    ),
    (
        "execution/answer_pipeline/literal_revision.rs",
        include_str!("execution/answer_pipeline/literal_revision.rs"),
    ),
    (
        "execution/answer_pipeline/literal_revision_tests.rs",
        include_str!("execution/answer_pipeline/literal_revision_tests.rs"),
    ),
    (
        "execution/answer_pipeline/output_boundary.rs",
        include_str!("execution/answer_pipeline/output_boundary.rs"),
    ),
    (
        "execution/answer_pipeline/output_boundary_tests.rs",
        include_str!("execution/answer_pipeline/output_boundary_tests.rs"),
    ),
    (
        "execution/answer_pipeline/preflight/logging.rs",
        include_str!("execution/answer_pipeline/preflight/logging.rs"),
    ),
    (
        "execution/answer_pipeline/preflight/mod.rs",
        include_str!("execution/answer_pipeline/preflight/mod.rs"),
    ),
    (
        "execution/answer_pipeline/single_shot/logging.rs",
        include_str!("execution/answer_pipeline/single_shot/logging.rs"),
    ),
    (
        "execution/answer_pipeline/single_shot/mod.rs",
        include_str!("execution/answer_pipeline/single_shot/mod.rs"),
    ),
    (
        "execution/answer_pipeline/single_shot/preflight_stages.rs",
        include_str!("execution/answer_pipeline/single_shot/preflight_stages.rs"),
    ),
    (
        "execution/answer_pipeline/single_shot_tests.rs",
        include_str!("execution/answer_pipeline/single_shot_tests.rs"),
    ),
    (
        "execution/answer_pipeline/test_support.rs",
        include_str!("execution/answer_pipeline/test_support.rs"),
    ),
    (
        "execution/answer_pipeline/variant_selection.rs",
        include_str!("execution/answer_pipeline/variant_selection.rs"),
    ),
    (
        "execution/answer_pipeline/variant_selection_tests.rs",
        include_str!("execution/answer_pipeline/variant_selection_tests.rs"),
    ),
    (
        "execution/answer_pipeline/verification.rs",
        include_str!("execution/answer_pipeline/verification.rs"),
    ),
    (
        "execution/authoritative_answer_context.rs",
        include_str!("execution/authoritative_answer_context.rs"),
    ),
    (
        "execution/authoritative_answer_context/answer_context.rs",
        include_str!("execution/authoritative_answer_context/answer_context.rs"),
    ),
    (
        "execution/authoritative_answer_context/image_deprioritization_tests.rs",
        include_str!("execution/authoritative_answer_context/image_deprioritization_tests.rs"),
    ),
    (
        "execution/authoritative_answer_context/runtime_context_merge_tests.rs",
        include_str!("execution/authoritative_answer_context/runtime_context_merge_tests.rs"),
    ),
    (
        "execution/authoritative_answer_context/source_coverage.rs",
        include_str!("execution/authoritative_answer_context/source_coverage.rs"),
    ),
    ("execution/consolidation/focus.rs", include_str!("execution/consolidation/focus.rs")),
    ("execution/consolidation/mod.rs", include_str!("execution/consolidation/mod.rs")),
    (
        "execution/consolidation/orchestration.rs",
        include_str!("execution/consolidation/orchestration.rs"),
    ),
    ("execution/consolidation/selection.rs", include_str!("execution/consolidation/selection.rs")),
    ("execution/context.rs", include_str!("execution/context.rs")),
    ("execution/context/answer_context.rs", include_str!("execution/context/answer_context.rs")),
    ("execution/context/assembly.rs", include_str!("execution/context/assembly.rs")),
    ("execution/context/chunk_ranking.rs", include_str!("execution/context/chunk_ranking.rs")),
    (
        "execution/context/document_rendering.rs",
        include_str!("execution/context/document_rendering.rs"),
    ),
    ("execution/context/entity_context.rs", include_str!("execution/context/entity_context.rs")),
    ("execution/context/library_context.rs", include_str!("execution/context/library_context.rs")),
    ("execution/context/references.rs", include_str!("execution/context/references.rs")),
    ("execution/document_target.rs", include_str!("execution/document_target.rs")),
    (
        "execution/document_target/comparison.rs",
        include_str!("execution/document_target/comparison.rs"),
    ),
    (
        "execution/document_target/focused_answer.rs",
        include_str!("execution/document_target/focused_answer.rs"),
    ),
    (
        "execution/document_target/matching.rs",
        include_str!("execution/document_target/matching.rs"),
    ),
    (
        "execution/document_target/scope_resolution.rs",
        include_str!("execution/document_target/scope_resolution.rs"),
    ),
    ("execution/embed.rs", include_str!("execution/embed.rs")),
    ("execution/endpoint_answer.rs", include_str!("execution/endpoint_answer.rs")),
    ("execution/endpoint_chunk_answer.rs", include_str!("execution/endpoint_chunk_answer.rs")),
    ("execution/fact_lookup.rs", include_str!("execution/fact_lookup.rs")),
    ("execution/focused_document_answer.rs", include_str!("execution/focused_document_answer.rs")),
    ("execution/fusion.rs", include_str!("execution/fusion.rs")),
    ("execution/graph_retrieval/mod.rs", include_str!("execution/graph_retrieval/mod.rs")),
    (
        "execution/graph_retrieval/relevance.rs",
        include_str!("execution/graph_retrieval/relevance.rs"),
    ),
    (
        "execution/graph_retrieval/retrieval.rs",
        include_str!("execution/graph_retrieval/retrieval.rs"),
    ),
    (
        "execution/graph_retrieval/target_profiles.rs",
        include_str!("execution/graph_retrieval/target_profiles.rs"),
    ),
    ("execution/hyde.rs", include_str!("execution/hyde.rs")),
    ("execution/port_answer.rs", include_str!("execution/port_answer.rs")),
    ("execution/preflight.rs", include_str!("execution/preflight.rs")),
    (
        "execution/preflight/answer_construction.rs",
        include_str!("execution/preflight/answer_construction.rs"),
    ),
    (
        "execution/preflight/document_presence.rs",
        include_str!("execution/preflight/document_presence.rs"),
    ),
    (
        "execution/preflight/document_presence_tests.rs",
        include_str!("execution/preflight/document_presence_tests.rs"),
    ),
    (
        "execution/preflight/document_scope.rs",
        include_str!("execution/preflight/document_scope.rs"),
    ),
    (
        "execution/preflight/setup_literals.rs",
        include_str!("execution/preflight/setup_literals.rs"),
    ),
    ("execution/preflight/tests.rs", include_str!("execution/preflight/tests.rs")),
    ("execution/question_intent.rs", include_str!("execution/question_intent.rs")),
    ("execution/rerank.rs", include_str!("execution/rerank.rs")),
    ("execution/retrieval_plan.rs", include_str!("execution/retrieval_plan.rs")),
    ("execution/semantic_rerank.rs", include_str!("execution/semantic_rerank.rs")),
    (
        "execution/semantic_rerank/binding_tests.rs",
        include_str!("execution/semantic_rerank/binding_tests.rs"),
    ),
    (
        "execution/semantic_rerank/preparation.rs",
        include_str!("execution/semantic_rerank/preparation.rs"),
    ),
    (
        "execution/semantic_rerank/provider_call.rs",
        include_str!("execution/semantic_rerank/provider_call.rs"),
    ),
    (
        "execution/semantic_rerank/provider_dispatch.rs",
        include_str!("execution/semantic_rerank/provider_dispatch.rs"),
    ),
    (
        "execution/semantic_rerank/provider_dispatch_tests.rs",
        include_str!("execution/semantic_rerank/provider_dispatch_tests.rs"),
    ),
    (
        "execution/semantic_rerank/rerank_bench_tests.rs",
        include_str!("execution/semantic_rerank/rerank_bench_tests.rs"),
    ),
    (
        "execution/semantic_rerank/sampling_tests.rs",
        include_str!("execution/semantic_rerank/sampling_tests.rs"),
    ),
    ("execution/semantic_rerank/scoring.rs", include_str!("execution/semantic_rerank/scoring.rs")),
    (
        "execution/semantic_rerank/shadow_lease.rs",
        include_str!("execution/semantic_rerank/shadow_lease.rs"),
    ),
    ("execution/semantic_rerank/tests.rs", include_str!("execution/semantic_rerank/tests.rs")),
    ("execution/retrieve/mod.rs", include_str!("execution/retrieve/mod.rs")),
    ("execution/retrieve/chunk_budget.rs", include_str!("execution/retrieve/chunk_budget.rs")),
    (
        "execution/retrieve/chunk_reservations.rs",
        include_str!("execution/retrieve/chunk_reservations.rs"),
    ),
    (
        "execution/retrieve/companion_lanes.rs",
        include_str!("execution/retrieve/companion_lanes.rs"),
    ),
    (
        "execution/retrieve/content_anchors.rs",
        include_str!("execution/retrieve/content_anchors.rs"),
    ),
    (
        "execution/retrieve/document_expansion.rs",
        include_str!("execution/retrieve/document_expansion.rs"),
    ),
    ("execution/retrieve/final_shaping.rs", include_str!("execution/retrieve/final_shaping.rs")),
    ("execution/retrieve/fusion.rs", include_str!("execution/retrieve/fusion.rs")),
    (
        "execution/retrieve/graph_evidence_context.rs",
        include_str!("execution/retrieve/graph_evidence_context.rs"),
    ),
    (
        "execution/retrieve/graph_evidence_loading.rs",
        include_str!("execution/retrieve/graph_evidence_loading.rs"),
    ),
    (
        "execution/retrieve/graph_evidence_search.rs",
        include_str!("execution/retrieve/graph_evidence_search.rs"),
    ),
    ("execution/retrieve/indexes.rs", include_str!("execution/retrieve/indexes.rs")),
    ("execution/retrieve/lane_execution.rs", include_str!("execution/retrieve/lane_execution.rs")),
    (
        "execution/retrieve/latest_version_loading.rs",
        include_str!("execution/retrieve/latest_version_loading.rs"),
    ),
    (
        "execution/retrieve/latest_version_selection.rs",
        include_str!("execution/retrieve/latest_version_selection.rs"),
    ),
    ("execution/retrieve/primary_lanes.rs", include_str!("execution/retrieve/primary_lanes.rs")),
    ("execution/retrieve/query_builders.rs", include_str!("execution/retrieve/query_builders.rs")),
    ("execution/retrieve/query_focus.rs", include_str!("execution/retrieve/query_focus.rs")),
    (
        "execution/retrieve/retrieval_execution.rs",
        include_str!("execution/retrieve/retrieval_execution.rs"),
    ),
    (
        "execution/retrieve/setup_candidates.rs",
        include_str!("execution/retrieve/setup_candidates.rs"),
    ),
    ("execution/retrieve/setup_focus.rs", include_str!("execution/retrieve/setup_focus.rs")),
    ("execution/retrieve/vector_context.rs", include_str!("execution/retrieve/vector_context.rs")),
    (
        "execution/retrieve/versioned_update.rs",
        include_str!("execution/retrieve/versioned_update.rs"),
    ),
    (
        "execution/source_context/budgeting.rs",
        include_str!("execution/source_context/budgeting.rs"),
    ),
    (
        "execution/source_context/candidates.rs",
        include_str!("execution/source_context/candidates.rs"),
    ),
    (
        "execution/source_context/companions.rs",
        include_str!("execution/source_context/companions.rs"),
    ),
    ("execution/source_context/loading.rs", include_str!("execution/source_context/loading.rs")),
    ("execution/source_context/mod.rs", include_str!("execution/source_context/mod.rs")),
    (
        "execution/source_context/orchestration.rs",
        include_str!("execution/source_context/orchestration.rs"),
    ),
    (
        "execution/source_context/rendering.rs",
        include_str!("execution/source_context/rendering.rs"),
    ),
    (
        "execution/source_context/selection.rs",
        include_str!("execution/source_context/selection.rs"),
    ),
    (
        "execution/source_context/source_slice.rs",
        include_str!("execution/source_context/source_slice.rs"),
    ),
    ("execution/source_context/types.rs", include_str!("execution/source_context/types.rs")),
    ("execution/source_profile.rs", include_str!("execution/source_profile.rs")),
    (
        "execution/structured_query_pipeline.rs",
        include_str!("execution/structured_query_pipeline.rs"),
    ),
    (
        "execution/structured_query_pipeline/planning.rs",
        include_str!("execution/structured_query_pipeline/planning.rs"),
    ),
    (
        "execution/structured_query_pipeline/retrieval.rs",
        include_str!("execution/structured_query_pipeline/retrieval.rs"),
    ),
    (
        "execution/structured_query_pipeline/tests.rs",
        include_str!("execution/structured_query_pipeline/tests.rs"),
    ),
    ("execution/table_retrieval.rs", include_str!("execution/table_retrieval.rs")),
    ("execution/table_row_answer.rs", include_str!("execution/table_row_answer.rs")),
    ("execution/table_summary_answer.rs", include_str!("execution/table_summary_answer.rs")),
    ("execution/technical_answer.rs", include_str!("execution/technical_answer.rs")),
    (
        "execution/technical_answer/error_code_mapping.rs",
        include_str!("execution/technical_answer/error_code_mapping.rs"),
    ),
    (
        "execution/technical_answer/module_configuration.rs",
        include_str!("execution/technical_answer/module_configuration.rs"),
    ),
    (
        "execution/technical_answer/transport_config.rs",
        include_str!("execution/technical_answer/transport_config.rs"),
    ),
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
    ("search/chunk_rebuild.rs", include_str!("search/chunk_rebuild.rs")),
    ("search/embedding.rs", include_str!("search/embedding.rs")),
    ("search/evidence.rs", include_str!("search/evidence.rs")),
    ("search/failures.rs", include_str!("search/failures.rs")),
    ("search/graph_rebuild.rs", include_str!("search/graph_rebuild.rs")),
    ("search/mod.rs", include_str!("search/mod.rs")),
    ("search/persistence.rs", include_str!("search/persistence.rs")),
    ("search/rebuild.rs", include_str!("search/rebuild.rs")),
    ("search/reuse.rs", include_str!("search/reuse.rs")),
    ("i18n/mod.rs", include_str!("i18n/mod.rs")),
    ("service/context.rs", include_str!("service/context.rs")),
    ("service/context/bundle_references.rs", include_str!("service/context/bundle_references.rs")),
    (
        "service/context/prepared_references.rs",
        include_str!("service/context/prepared_references.rs"),
    ),
    ("service/formatting.rs", include_str!("service/formatting.rs")),
    ("service/turn/mod.rs", include_str!("service/turn/mod.rs")),
    ("service/turn/agent_answer.rs", include_str!("service/turn/agent_answer.rs")),
    ("service/turn/agent_grounding.rs", include_str!("service/turn/agent_grounding.rs")),
    ("service/turn/agent_verification.rs", include_str!("service/turn/agent_verification.rs")),
    ("service/turn/cache_coordination.rs", include_str!("service/turn/cache_coordination.rs")),
    ("service/turn/cache_policy.rs", include_str!("service/turn/cache_policy.rs")),
    ("service/turn/execution_detail.rs", include_str!("service/turn/execution_detail.rs")),
    ("service/turn/grounded_answer.rs", include_str!("service/turn/grounded_answer.rs")),
    ("service/turn/pipeline.rs", include_str!("service/turn/pipeline.rs")),
    ("service/turn/preparation.rs", include_str!("service/turn/preparation.rs")),
    ("service/turn/readable_snapshot.rs", include_str!("service/turn/readable_snapshot.rs")),
    ("service/turn/runtime.rs", include_str!("service/turn/runtime.rs")),
    ("text_match.rs", include_str!("text_match.rs")),
    ("result_cache.rs", include_str!("result_cache.rs")),
    ("result_cache/redis_cache.rs", include_str!("result_cache/redis_cache.rs")),
    ("result_cache/tests.rs", include_str!("result_cache/tests.rs")),
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
