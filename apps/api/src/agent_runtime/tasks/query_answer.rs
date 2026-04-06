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

const QUERY_ANSWER_STAGE_CATALOG: &[RuntimeStageKind] = &[RuntimeStageKind::Answer];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryAnswerTaskInput {
    pub query_execution_id: Uuid,
    pub question: String,
    pub prompt_history_text: Option<String>,
    pub grounded_context_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryAnswerTaskSuccess {
    pub answer_text: String,
    pub provider: ProviderModelSelection,
    pub usage_json: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryAnswerTaskFailure {
    pub code: String,
    pub summary: String,
}

pub struct QueryAnswerTask;

impl RuntimeTask for QueryAnswerTask {
    type Input = QueryAnswerTaskInput;
    type Success = QueryAnswerTaskSuccess;
    type Failure = QueryAnswerTaskFailure;

    const CONTRACT_NAME: &'static str = "query_answer";
    const CONTRACT_VERSION: &'static str = "1";

    fn spec() -> RuntimeTaskSpec {
        RuntimeTaskSpec {
            task_kind: RuntimeTaskKind::QueryAnswer,
            surface_kind: RuntimeSurfaceKind::Internal,
            binding_purpose: AiBindingPurpose::for_runtime_task_kind(RuntimeTaskKind::QueryAnswer),
            machine_consumed: false,
            max_turns: 1,
            max_parallel_actions: 1,
            stage_catalog: QUERY_ANSWER_STAGE_CATALOG,
            recovery_policy: RuntimeRecoveryPolicy::None,
            output_mode: RuntimeOutputMode::Text,
        }
    }

    fn policy_failure(reason_code: &str, reason_summary_redacted: &str) -> Self::Failure {
        QueryAnswerTaskFailure {
            code: reason_code.to_string(),
            summary: reason_summary_redacted.to_string(),
        }
    }
}

impl TextRuntimeTask for QueryAnswerTask {}
