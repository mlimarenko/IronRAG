//! LLM turn helpers used by the assistant answer surfaces.
//!
//! The in-app UI assistant mirrors the public MCP answer contract with a
//! deterministic runtime-first entry: the runtime dispatches the automatic
//! `grounded_answer` call before model tool selection. If that result is
//! nonterminal, the model sees the answer-tool registry and returned evidence
//! before choosing any lower-level follow-up tools.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    time::{Duration, Instant},
};

use futures::{StreamExt as _, stream};
use serde_json::Value;
use tokio::sync::mpsc::Sender;
use uuid::Uuid;

use crate::{
    agent_runtime::tasks::query_answer::{
        QueryProviderCall, QueryProviderCallAttribution, QueryProviderCallKind,
    },
    app::state::AppState,
    domains::provider_profiles::ProviderModelSelection,
    domains::query::{
        MAX_TOP_K, QueryAnswerCandidate, QueryAnswerDisposition, QueryClarification, QueryTurnKind,
        resolve_contextual_grounded_answer_top_k,
    },
    domains::query_ir::{QueryLanguage, literal_text_is_identifier_shaped},
    domains::{agent_runtime::RuntimeSurfaceKind, ai::AiBindingPurpose},
    integrations::{
        llm::{ChatMessage, ChatToolCall, ChatToolDef, ToolUseRequest, ToolUseResponse},
        retry::ProviderCallError,
    },
    interfaces::http::{
        auth::AuthContext,
        mcp::{
            McpToolSurface,
            tools::{
                self, ToolCallContext, ToolVisibilityCapabilities,
                documents::{READ_DOCUMENT_TOOL_NAME, SEARCH_DOCUMENTS_TOOL_NAME},
            },
        },
        router_support::ApiError,
    },
    services::ai_catalog_service::ResolvedRuntimeBinding,
    services::query::{
        assistant_grounding::AssistantGroundingEvidence,
        completion_policy::{
            AnswerCompletionAssessment, AnswerCompletionGapReason, GroundedAnswerCompletionEnvelope,
        },
        error::QueryServiceError,
        i18n::grounded_repair_messages,
        llm_context_debug::{
            AgentLoopMetadata, AgentStopReason, LlmIterationDebug, ResponseToolCallDebug,
        },
        provider_billing::{QueryProviderCallReservation, QueryProviderExecutionContext},
        service::ExternalConversationTurn,
    },
    shared::text_tokens::backtick_literal_spans,
};

mod answer_guard;
mod compaction;
mod fixed_context;
mod grounded_state;
mod grounding;
mod iteration;
mod loop_completion;
mod provider_calls;
mod repair_fallback;
mod state_failure;
mod state_machine;
mod tool_argument_defaults;
mod tool_arguments;
mod tool_dispatch;
mod tool_result;
mod tool_scope;
mod types;

use answer_guard::*;
use compaction::*;
#[cfg(test)]
use fixed_context::*;
use grounded_state::*;
use grounding::*;
use iteration::*;
use loop_completion::*;
use provider_calls::*;
use repair_fallback::*;
use state_failure::*;
use state_machine::*;
use tool_argument_defaults::*;
use tool_arguments::*;
use tool_dispatch::*;
use tool_result::*;
use tool_scope::*;
use types::*;

pub(crate) use fixed_context::{
    LiteralRevisionRequest, SingleShotTurnRequest, merge_usage_into, run_literal_revision_turn,
    run_single_shot_turn,
};
#[allow(unused_imports, reason = "used by the HTTP binary target")]
pub(crate) use state_machine::answer_surface_tool_defs;
pub(crate) use state_machine::run_mcp_tool_agent_turn;
pub(crate) use types::{
    AgentAnswerProvenance, AgentGroundedAnswerOutcome, AgentLoopActivityEvent, AgentTurnFailure,
    AgentTurnResult, McpToolAgentTurnInput,
};

#[cfg(test)]
use crate::{
    domains::iam::PrincipalKind,
    interfaces::http::{
        auth::{AuthGrant, AuthTokenKind},
        authorization::{
            POLICY_LIBRARY_READ, POLICY_MCP_MEMORY_READ, POLICY_QUERY_RUN, POLICY_RUNTIME_READ,
        },
    },
};
#[cfg(test)]
mod answer_guard_tests;
#[cfg(test)]
mod compaction_tests;
#[cfg(test)]
mod grounded_state_tests;
#[cfg(test)]
mod iteration_tests;
#[cfg(test)]
mod loop_completion_tests;
#[cfg(test)]
mod provider_calls_tests;
#[cfg(test)]
mod repair_fallback_tests;
#[cfg(test)]
mod state_failure_tests;
#[cfg(test)]
mod state_machine_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tool_argument_defaults_tests;
#[cfg(test)]
mod tool_arguments_tests;
#[cfg(test)]
mod tool_dispatch_tests;
#[cfg(test)]
mod tool_result_tests;
#[cfg(test)]
mod tool_scope_tests;
#[cfg(test)]
use test_support::*;
