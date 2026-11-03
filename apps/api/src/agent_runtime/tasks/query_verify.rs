use serde::{Deserialize, Serialize};

use crate::{
    agent_runtime::task::{RuntimeTask, RuntimeTaskSpec, StructuredRuntimeTask},
    domains::agent_runtime::{
        RuntimeOutputMode, RuntimeRecoveryPolicy, RuntimeStageKind, RuntimeSurfaceKind,
        RuntimeTaskKind,
    },
};

const QUERY_VERIFY_STAGE_CATALOG: &[RuntimeStageKind] = &[RuntimeStageKind::Verify];

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
/// Correlation contract for the selected QueryAnswer semantic suboperation.
/// This unused task registration does not introduce an independent model route.
pub struct QueryVerifyTaskInput {
    pub query_execution_id: uuid::Uuid,
    pub question_sha256: String,
    pub checked_answer_sha256: String,
    pub evidence_manifest_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueryVerifyTaskSuccess {
    pub semantic_verification:
        ironrag_contracts::assistant::semantic_verification::SemanticVerificationReport,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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
            binding_purpose: RuntimeTaskKind::QueryVerify.binding_purpose(),
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
