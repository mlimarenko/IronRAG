use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use anyhow::Context;
use chrono::Utc;
use tokio::sync::mpsc::Sender;
use tracing::warn;
use uuid::Uuid;

use crate::{
    agent_runtime::{
        builder::TextRequestBuilder,
        executor::{RuntimeExecutionError, RuntimeExecutionSession},
        persistence as runtime_persistence,
        response::{RuntimeFailureSummary, RuntimeTerminalOutcome},
        task::RuntimeTask,
        tasks::query_answer::{
            QueryAnswerTask, QueryAnswerTaskFailure, QueryAnswerTaskInput, QueryAnswerTaskSuccess,
        },
    },
    app::state::AppState,
    domains::catalog::{CatalogLibrary, CatalogLifecycleState},
    domains::query::{
        QueryAnswerDisposition, QueryClarification, QueryConversationState, QueryExecutionDetail,
        QueryTurnKind, QueryVerificationState, QueryVerificationWarning, resolve_top_k,
    },
    domains::query_ir::{
        QueryIR, QueryLanguage, VerificationLevel, literal_text_is_identifier_shaped,
    },
    domains::{
        agent_runtime::{
            RuntimeDecisionKind, RuntimeExecutionOwner, RuntimeStageKind, RuntimeStageState,
            RuntimeSurfaceKind,
        },
        ai::AiBindingPurpose,
    },
    infra::{
        knowledge_rows::{
            KnowledgeBundleChunkEdgeRow, KnowledgeBundleChunkReferenceRow,
            KnowledgeBundleEntityEdgeRow, KnowledgeBundleEntityReferenceRow,
            KnowledgeBundleEvidenceEdgeRow, KnowledgeBundleEvidenceReferenceRow,
            KnowledgeBundleRelationEdgeRow, KnowledgeBundleRelationReferenceRow,
            KnowledgeContextBundleReferenceSetRow, KnowledgeContextBundleRow,
        },
        repositories::{
            ai_repository, catalog_repository, query_repository, query_result_cache_repository,
            runtime_repository,
        },
    },
    integrations::{provider_budget::ProviderBudgetError, retry::ProviderCallError},
    interfaces::http::{auth::AuthContext, router_support::ApiError},
    services::{
        ingest::runtime::bounded_runtime_overrides,
        mcp::access::library_catalog_ref,
        ops::service::CreateAsyncOperationCommand,
        query::{
            agent_loop::{
                AgentAnswerProvenance, AgentGroundedAnswerOutcome, AgentLoopActivityEvent,
                AgentTurnFailure, AgentTurnResult, LiteralRevisionRequest, McpToolAgentTurnInput,
                run_literal_revision_turn, run_mcp_tool_agent_turn,
            },
            assistant_grounding::AssistantGroundingEvidence,
            error::QueryServiceError,
            execution::{
                AnswerKind, AnswerQueryRequest, AnswerVerificationRequest, AnswerVisibilityKind,
                AuthoritativeAnswerEvidence, PrepareAnswerQueryRequest,
                QueryVerificationPersistence, RuntimeAnswerQueryFailure, RuntimeAnswerQueryResult,
                RuntimeAnswerVerification, SemanticRerankExecutionContext,
                finalize_answer_visibility, generate_answer_query, literal_revision_targets,
                persist_query_verification, persisted_answer_is_system_statement,
                persisted_query_answer_outcome, prepare_answer_query,
                verify_answer_against_authoritative_evidence,
            },
            planner::query_intent_profile_from_query_ir,
            provider_billing::QueryProviderExecutionContext,
            result_cache,
        },
    },
    shared::provider_failure::ProviderFailureKind,
};

use super::{
    ConversationRuntimeContext, ExecuteConversationTurnCommand, ExternalConversationTurn,
    GROUNDED_ANSWER_QUERY_MODE, QueryService, QueryTurnExecutionResult,
    bounded_runtime_policy_summary, combined_runtime_failure_summary,
    context::{
        AssembleContextBundleRequest, assemble_context_bundle,
        load_execution_prepared_reference_context, query_ir_from_bundle_diagnostics,
    },
    formatting::{
        build_assistant_document_references, build_prepared_segment_references,
        build_technical_fact_references, hydrate_entity_references, hydrate_relation_references,
        map_chunk_references, map_entity_references, map_execution_runtime_stage_summaries,
        map_execution_runtime_summary, map_relation_references, parse_query_verification_state,
        parse_query_verification_warnings, search_runtime_graph_entity_references,
    },
    is_runtime_policy_failure_code, runtime_mode_label,
    session::{
        build_conversation_runtime_context,
        build_conversation_runtime_context_with_external_history, derive_conversation_title,
        load_prior_grounded_answer_context_messages, map_conversation_row, map_execution_row,
        map_turn_row, normalize_required_text, should_refresh_conversation_title,
        should_replay_prior_grounded_answer_context,
    },
};

const REFERENCE_CONTEXT_HYDRATION_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const ASSISTANT_AGENT_LOOP_DEADLINE_MS: u64 = 180_000;
pub(crate) const ASSISTANT_AGENT_LOOP_TOOL_COLLECTION_TARGET_MS: u64 = 35_000;
const _: () = {
    assert!(ASSISTANT_AGENT_LOOP_TOOL_COLLECTION_TARGET_MS < ASSISTANT_AGENT_LOOP_DEADLINE_MS);
    assert!(ASSISTANT_AGENT_LOOP_TOOL_COLLECTION_TARGET_MS <= 45_000);
};
const ASSISTANT_AGENT_LOOP_MIN_ITERATIONS: usize = 10;
#[cfg(test)]
const ASSISTANT_AGENT_LOOP_MIN_ITERATION_BUDGET_MS: u64 = 15_000;
const ASSISTANT_LITERAL_INVENTORY_REVISION_TIMEOUT: Duration = Duration::from_secs(12);

mod agent_answer;
mod agent_grounding;
mod agent_verification;
mod cache_coordination;
mod cache_policy;
mod chunk_reference_detail;
mod execution_detail;
mod grounded_answer;
mod pipeline;
mod preparation;
mod readable_snapshot;
mod runtime;

use agent_answer::{AgentAnswerStageInput, execute_agent_answer_stages};
use agent_grounding::{
    AgentAnswerVerificationRequest, ensure_agent_tool_context_bundle,
    finalize_agent_system_statement, is_system_statement_passthrough,
    materialize_agent_grounding_from_child_execution, system_statement_kind_from_child_executions,
    verify_agent_answer_against_tool_evidence,
};
use agent_verification::{
    agent_answer_allows_parent_model_revision,
    agent_answer_requires_parent_tool_evidence_verification, agent_has_verifiable_tool_evidence,
    agent_verification_needs_literal_revision, context_reference_set_has_grounding,
    literal_inventory_coverage_revision_targets, literal_inventory_revision_context,
    literal_revision_covers_required_literals, literal_revision_history_literal_coverage,
    no_agent_tool_evidence_warnings, no_verifiable_tool_evidence_warnings, ui_agent_iteration_cap,
    verification_state_storage_label,
};
use cache_coordination::{CacheCoordinationRequest, CacheFillCoordination};
use cache_policy::{
    GroundedAnswerCacheContextRequest, QueryResultCacheContext,
    build_grounded_answer_cache_context, discard_unexecuted_query_request_turn,
    evict_query_result_cache_entry, query_detail_has_grounding_references,
    query_result_cache_source_is_current, store_query_result_cache_winner,
};
use grounded_answer::{
    GroundedAnswerStageInput, run_grounded_answer_stages, run_grounded_retrieve_stage,
};
use pipeline::QueryExecutionInterruptionGuard;
use preparation::{
    PreparedConversationTurn, compiled_query_uses_answer_history, prepare_conversation_turn,
    prior_agent_grounded_answer_context,
};
use readable_snapshot::{
    QueryReadableContentCoverage, persist_query_readable_content_coverage,
    resolve_query_readable_content_coverage,
};
use runtime::{
    QueryFinalization, anyhow_failure_code, begin_query_runtime_stage,
    finalize_agent_runtime_execution, finalize_grounded_retrieve_failure,
    finalize_grounded_runtime_execution, make_query_answer_failure,
    make_query_terminal_failure_outcome, map_runtime_answer_query_failure, persist_agent_answer,
    persist_failed_agent_debug_snapshot, query_service_failure_code, record_query_runtime_stage,
    seed_query_runtime_session,
};

pub(crate) use runtime::query_runtime_stage_label;

#[cfg(test)]
use agent_grounding::finalized_agent_answer_outcome;
#[cfg(test)]
use agent_verification::{latest_dense_history_identifier_literals, text_contains_literal};
#[cfg(test)]
use cache_policy::{
    query_result_binding_purposes, query_result_cache_enabled_for_semantic_rerank,
    query_result_cache_is_allowed,
};
#[cfg(test)]
use execution_detail::{
    persisted_execution_answer_outcome, should_backfill_graph_entity_references,
};
#[cfg(test)]
use readable_snapshot::{READABLE_SNAPSHOT_WARNING_CODE, apply_query_readable_content_coverage};
#[cfg(test)]
use runtime::redacted_failure_detail;
#[cfg(test)]
use runtime::{
    QueryExecutionApiFailure, map_query_execution_failure, map_runtime_execution_error,
    query_execution_api_failure,
};

#[cfg(test)]
mod test_prelude {
    pub(super) use std::sync::Arc;

    pub(super) use uuid::Uuid;

    pub(super) use super::{
        QueryExecutionApiFailure, QueryReadableContentCoverage, READABLE_SNAPSHOT_WARNING_CODE,
        apply_query_readable_content_coverage, query_execution_api_failure,
        query_service_failure_code, resolve_query_readable_content_coverage,
    };

    pub(super) use crate::agent_runtime::{
        default_policy::{DefaultRuntimePolicy, DefaultRuntimePolicyRules},
        executor::RuntimeExecutor,
        hooks::{NoopRuntimeHooks, RuntimeHooks},
        policy::RuntimePolicy,
        registry::RuntimeTaskRegistry,
        response::{RuntimeFailureSummary, RuntimeRecoveryOutcome, RuntimeTerminalOutcome},
        task::RuntimeTaskRequest,
        tasks::query_answer::{
            QueryAnswerTask, QueryAnswerTaskFailure, QueryAnswerTaskInput, QueryAnswerTaskSuccess,
            QueryProviderCall, QueryProviderCallAttribution, QueryProviderCallKind,
        },
    };
    pub(super) use crate::domains::agent_runtime::{RuntimeExecutionOwner, RuntimeStageKind};
    pub(super) use crate::domains::query_ir::{
        EntityMention, EntityRole, LiteralKind, LiteralSpan, QueryAct, QueryIR, QueryLanguage,
        QueryScope, SourceSliceDirection, SourceSliceFilter, SourceSliceSpec, VerificationLevel,
    };
    pub(super) use crate::domains::{
        ai::AiBindingPurpose,
        query::{
            QueryAnswerDisposition, QueryClarification, QueryTurnKind, QueryVerificationState,
            QueryVerificationWarning,
        },
    };
    pub(super) use crate::interfaces::http::router_support::ApiError;
    pub(super) use crate::services::query::assistant_grounding::AssistantGroundingEvidence;
    pub(super) use crate::services::query::error::QueryServiceError;
    pub(super) use crate::services::query::service::ExternalConversationTurn;

    pub(super) use super::{
        ASSISTANT_AGENT_LOOP_DEADLINE_MS, ASSISTANT_AGENT_LOOP_MIN_ITERATION_BUDGET_MS,
        ASSISTANT_AGENT_LOOP_MIN_ITERATIONS, RuntimeAnswerQueryFailure,
        agent_answer_allows_parent_model_revision,
        agent_answer_requires_parent_tool_evidence_verification,
        agent_has_verifiable_tool_evidence, anyhow_failure_code,
        compiled_query_uses_answer_history, finalized_agent_answer_outcome,
        latest_dense_history_identifier_literals, literal_inventory_coverage_revision_targets,
        literal_revision_covers_required_literals, literal_revision_history_literal_coverage,
        make_query_terminal_failure_outcome, map_query_execution_failure,
        map_runtime_answer_query_failure, map_runtime_execution_error,
        no_agent_tool_evidence_warnings, persisted_execution_answer_outcome,
        query_result_binding_purposes, query_result_cache_enabled_for_semantic_rerank,
        record_query_runtime_stage, redacted_failure_detail,
        should_backfill_graph_entity_references, text_contains_literal, ui_agent_iteration_cap,
    };
    pub(super) use crate::services::query::agent_loop::{
        AgentAnswerProvenance, AgentGroundedAnswerOutcome,
    };
}

#[cfg(test)]
mod agent_grounding_tests;
#[cfg(test)]
mod agent_verification_tests;
#[cfg(test)]
mod cache_policy_tests;
#[cfg(test)]
mod execution_detail_tests;
#[cfg(test)]
mod preparation_tests;
#[cfg(test)]
mod readable_snapshot_tests;
#[cfg(test)]
mod runtime_tests;
