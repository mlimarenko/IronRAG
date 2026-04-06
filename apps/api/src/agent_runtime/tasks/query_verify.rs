use serde::{Deserialize, Serialize};

use crate::{
    agent_runtime::task::{RuntimeTask, RuntimeTaskSpec, StructuredRuntimeTask},
    domains::{
        agent_runtime::{
            RuntimeOutputMode, RuntimeRecoveryPolicy, RuntimeStageKind, RuntimeSurfaceKind,
            RuntimeTaskKind,
        },
        ai::AiBindingPurpose,
        query::{QueryVerificationState, QueryVerificationWarning},
    },
};

const QUERY_VERIFY_STAGE_CATALOG: &[RuntimeStageKind] = &[RuntimeStageKind::Verify];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryVerifyTaskInput {
    pub question: String,
    pub answer_text: String,
    pub grounded_context_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryVerifyTaskSuccess {
    pub verification_state: QueryVerificationState,
    pub verification_warnings: Vec<QueryVerificationWarning>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryVerifyTaskFailure {
    pub code: String,
    pub summary: String,
}

pub struct QueryVerifyTask;

impl RuntimeTask for QueryVerifyTask {
    type Input = QueryVerifyTaskInput;
    type Success = QueryVerifyTaskSuccess;
    type Failure = QueryVerifyTaskFailure;

    const CONTRACT_NAME: &'static str = "query_verify";
    const CONTRACT_VERSION: &'static str = "1";

    fn spec() -> RuntimeTaskSpec {
        RuntimeTaskSpec {
            task_kind: RuntimeTaskKind::QueryVerify,
            surface_kind: RuntimeSurfaceKind::Internal,
            binding_purpose: AiBindingPurpose::for_runtime_task_kind(RuntimeTaskKind::QueryVerify),
            machine_consumed: true,
            max_turns: 1,
            max_parallel_actions: 1,
            stage_catalog: QUERY_VERIFY_STAGE_CATALOG,
            recovery_policy: RuntimeRecoveryPolicy::None,
            output_mode: RuntimeOutputMode::Structured,
        }
    }

    fn policy_failure(reason_code: &str, reason_summary_redacted: &str) -> Self::Failure {
        QueryVerifyTaskFailure {
            code: reason_code.to_string(),
            summary: reason_summary_redacted.to_string(),
        }
    }
}

impl StructuredRuntimeTask for QueryVerifyTask {}
