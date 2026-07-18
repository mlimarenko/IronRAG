use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    agent_runtime::task::{RuntimeTask, RuntimeTaskSpec, TextRuntimeTask},
    domains::{
        agent_runtime::{
            RuntimeOutputMode, RuntimeRecoveryPolicy, RuntimeStageKind, RuntimeSurfaceKind,
            RuntimeTaskKind,
        },
        ai::AiBindingPurpose,
        provider_profiles::ProviderModelSelection,
    },
};

const QUERY_ANSWER_STAGE_CATALOG: &[RuntimeStageKind] = &[
    RuntimeStageKind::Retrieve,
    RuntimeStageKind::AssembleContext,
    RuntimeStageKind::Answer,
    RuntimeStageKind::Verify,
    RuntimeStageKind::Persist,
];

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryAnswerTaskInput {
    pub query_execution_id: Uuid,
    pub question: String,
    pub prompt_history_text: Option<String>,
    pub grounded_context_text: String,
}

/// Canonical billing classification for one concrete query provider response.
/// Each value represents one network response, never an aggregate of several
/// agent iterations or answer revisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueryProviderCallKind {
    QueryAgent,
    QueryAnswer,
    QueryAnswerLiteralRevision,
    QueryAnswerInventoryRevision,
}

impl QueryProviderCallKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::QueryAgent => "query_agent",
            Self::QueryAnswer => "query_answer",
            Self::QueryAnswerLiteralRevision => "query_answer_literal_revision",
            Self::QueryAnswerInventoryRevision => "query_answer_inventory_revision",
        }
    }

    #[must_use]
    pub const fn binding_purpose(self) -> AiBindingPurpose {
        match self {
            Self::QueryAgent => AiBindingPurpose::Agent,
            Self::QueryAnswer
            | Self::QueryAnswerLiteralRevision
            | Self::QueryAnswerInventoryRevision => AiBindingPurpose::QueryAnswer,
        }
    }
}

/// Immutable attribution for one provider request whose stable billing event
/// id was reserved before network I/O. A zero-call deterministic path carries
/// an empty vector; usage is retained even when later answer selection rejects
/// that response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", try_from = "QueryProviderCallWire")]
pub struct QueryProviderCall {
    provider_call_id: Uuid,
    binding_id: Uuid,
    binding_purpose: AiBindingPurpose,
    provider: ProviderModelSelection,
    call_kind: QueryProviderCallKind,
    usage_json: serde_json::Value,
}

impl QueryProviderCall {
    #[must_use]
    pub const fn provider_call_id(&self) -> Uuid {
        self.provider_call_id
    }

    #[must_use]
    pub const fn binding_id(&self) -> Uuid {
        self.binding_id
    }

    #[must_use]
    pub const fn binding_purpose(&self) -> AiBindingPurpose {
        self.binding_purpose
    }

    #[must_use]
    pub const fn provider(&self) -> &ProviderModelSelection {
        &self.provider
    }

    #[must_use]
    pub const fn call_kind(&self) -> QueryProviderCallKind {
        self.call_kind
    }

    #[must_use]
    pub const fn usage_json(&self) -> &serde_json::Value {
        &self.usage_json
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryProviderCallAttribution {
    binding_id: Uuid,
    binding_purpose: AiBindingPurpose,
    provider: ProviderModelSelection,
    call_kind: QueryProviderCallKind,
}

impl QueryProviderCallAttribution {
    pub fn try_new(
        binding_id: Uuid,
        binding_purpose: AiBindingPurpose,
        provider: ProviderModelSelection,
        call_kind: QueryProviderCallKind,
    ) -> Result<Self, QueryProviderCallAttributionError> {
        let expected_binding_purpose = call_kind.binding_purpose();
        if binding_purpose != expected_binding_purpose {
            return Err(QueryProviderCallAttributionError {
                binding_id,
                binding_purpose,
                expected_binding_purpose,
                call_kind,
            });
        }
        Ok(Self { binding_id, binding_purpose, provider, call_kind })
    }

    /// Attach provider usage only after the already-validated call returns.
    /// Binding-purpose mismatches are therefore rejected before network I/O,
    /// while every returned response can be recorded infallibly.
    #[must_use]
    pub fn record(
        &self,
        provider_call_id: Uuid,
        usage_json: serde_json::Value,
    ) -> QueryProviderCall {
        QueryProviderCall {
            provider_call_id,
            binding_id: self.binding_id,
            binding_purpose: self.binding_purpose,
            provider: self.provider.clone(),
            call_kind: self.call_kind,
            usage_json,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "provider-call binding-purpose mismatch for {binding_id}: {binding_purpose:?} cannot be used for {call_kind:?}; expected {expected_binding_purpose:?}"
)]
pub struct QueryProviderCallAttributionError {
    binding_id: Uuid,
    binding_purpose: AiBindingPurpose,
    expected_binding_purpose: AiBindingPurpose,
    call_kind: QueryProviderCallKind,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueryProviderCallWire {
    provider_call_id: Uuid,
    binding_id: Uuid,
    binding_purpose: AiBindingPurpose,
    provider: ProviderModelSelection,
    call_kind: QueryProviderCallKind,
    usage_json: serde_json::Value,
}

impl TryFrom<QueryProviderCallWire> for QueryProviderCall {
    type Error = QueryProviderCallAttributionError;

    fn try_from(value: QueryProviderCallWire) -> Result<Self, Self::Error> {
        QueryProviderCallAttribution::try_new(
            value.binding_id,
            value.binding_purpose,
            value.provider,
            value.call_kind,
        )
        .map(|attribution| attribution.record(value.provider_call_id, value.usage_json))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryAnswerTaskSuccess {
    pub answer_text: String,
    /// One immutable record per actual provider response. Billing must iterate
    /// this ledger and must not infer calls from aggregate usage.
    #[serde(default)]
    pub provider_calls: Vec<QueryProviderCall>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryAnswerTaskFailure {
    pub code: String,
    pub summary: String,
    /// Provider responses completed before the terminal failure. Canonical
    /// usage was already committed when each entry was created; this ledger is
    /// diagnostic provenance rather than a post-terminal billing projection.
    #[serde(default)]
    pub provider_calls: Vec<QueryProviderCall>,
}

impl QueryAnswerTaskFailure {
    #[must_use]
    pub fn new(code: impl Into<String>, summary: impl Into<String>) -> Self {
        Self { code: code.into(), summary: summary.into(), provider_calls: Vec::new() }
    }

    #[must_use]
    pub fn with_provider_calls(self, provider_calls: Vec<QueryProviderCall>) -> Self {
        Self { provider_calls, ..self }
    }
}

pub struct QueryAnswerTask;

impl RuntimeTask for QueryAnswerTask {
    type Input = QueryAnswerTaskInput;
    type Success = QueryAnswerTaskSuccess;
    type Failure = QueryAnswerTaskFailure;

    const CONTRACT_NAME: &'static str = "query_answer";
    const CONTRACT_VERSION: &'static str = "2";

    fn spec() -> RuntimeTaskSpec {
        RuntimeTaskSpec {
            task_kind: RuntimeTaskKind::QueryAnswer,
            surface_kind: RuntimeSurfaceKind::Internal,
            binding_purpose: RuntimeTaskKind::QueryAnswer.binding_purpose(),
            machine_consumed: false,
            max_turns: 4,
            max_parallel_actions: 3,
            stage_catalog: QUERY_ANSWER_STAGE_CATALOG,
            recovery_policy: RuntimeRecoveryPolicy::None,
            output_mode: RuntimeOutputMode::Text,
        }
    }

    fn policy_failure(reason_code: &str, reason_summary_redacted: &str) -> Self::Failure {
        QueryAnswerTaskFailure::new(reason_code, reason_summary_redacted)
    }
}

impl TextRuntimeTask for QueryAnswerTask {}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use crate::agent_runtime::task::RuntimeTask;

    use super::{
        QueryAnswerTask, QueryAnswerTaskFailure, QueryAnswerTaskSuccess, QueryProviderCall,
        QueryProviderCallKind,
    };

    #[test]
    fn query_answer_runtime_allows_tool_loop_budget() {
        let spec = QueryAnswerTask::spec();

        assert!(spec.max_turns > 1, "query answers must allow multiple model/tool turns");
        assert!(spec.max_parallel_actions > 1, "query answers must allow parallel tool calls");
        assert!(
            spec.stage_catalog.contains(&crate::domains::agent_runtime::RuntimeStageKind::Answer)
        );
        assert!(
            spec.stage_catalog.contains(&crate::domains::agent_runtime::RuntimeStageKind::Verify)
        );
        assert!(
            spec.stage_catalog.contains(&crate::domains::agent_runtime::RuntimeStageKind::Persist)
        );
        assert_eq!(QueryAnswerTask::CONTRACT_VERSION, "2");
    }

    #[test]
    fn deterministic_success_serializes_an_explicit_empty_provider_call_ledger() {
        let success = QueryAnswerTaskSuccess {
            answer_text: "Neutral deterministic answer".to_string(),
            provider_calls: Vec::new(),
        };

        let serialized = serde_json::to_value(success).expect("serialize task success");

        assert_eq!(serialized["providerCalls"], serde_json::json!([]));
        assert!(serialized.get("providerCallPerformed").is_none());
        assert!(serialized.get("provider").is_none());
        assert!(serialized.get("usageJson").is_none());
    }

    #[test]
    fn deterministic_failure_serializes_an_explicit_empty_provider_call_ledger() {
        let failure = QueryAnswerTaskFailure::new("neutral_failure", "Neutral failure summary");

        let serialized = serde_json::to_value(failure).expect("serialize task failure");

        assert_eq!(serialized["providerCalls"], serde_json::json!([]));
    }

    #[test]
    fn mismatched_provider_call_attribution_is_rejected_during_deserialization() {
        let serialized = serde_json::json!({
            "providerCallId": Uuid::now_v7(),
            "bindingId": Uuid::now_v7(),
            "bindingPurpose": crate::domains::ai::AiBindingPurpose::QueryAnswer,
            "provider": crate::domains::provider_profiles::ProviderModelSelection {
                provider_kind: "provider-a".to_string(),
                model_name: "model-a".to_string(),
            },
            "callKind": QueryProviderCallKind::QueryAgent,
            "usageJson": {"input_tokens": 3},
        });

        let result = serde_json::from_value::<QueryProviderCall>(serialized);

        assert!(result.is_err(), "purpose/kind mismatch must fail closed");
    }
}
