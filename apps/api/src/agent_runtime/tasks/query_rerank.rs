use crate::{
    agent_runtime::task::{RuntimeTask, RuntimeTaskSpec, StructuredRuntimeTask},
    domains::{
        agent_runtime::{
            RuntimeOutputMode, RuntimeRecoveryPolicy, RuntimeStageKind, RuntimeSurfaceKind,
            RuntimeTaskKind,
        },
        ai::AiBindingPurpose,
    },
    services::query_support::{QueryRerankFailure, QueryRerankTaskInput, RerankOutcome},
};

const QUERY_RERANK_STAGE_CATALOG: &[RuntimeStageKind] = &[RuntimeStageKind::Rerank];

pub struct QueryRerankTask;

impl RuntimeTask for QueryRerankTask {
    type Input = QueryRerankTaskInput;
    type Success = RerankOutcome;
    type Failure = QueryRerankFailure;

    const CONTRACT_NAME: &'static str = "query_rerank";
    const CONTRACT_VERSION: &'static str = "1";

    fn spec() -> RuntimeTaskSpec {
        RuntimeTaskSpec {
            task_kind: RuntimeTaskKind::QueryRerank,
            surface_kind: RuntimeSurfaceKind::Internal,
            binding_purpose: AiBindingPurpose::for_runtime_task_kind(RuntimeTaskKind::QueryRerank),
            machine_consumed: true,
            max_turns: 1,
            max_parallel_actions: 1,
            stage_catalog: QUERY_RERANK_STAGE_CATALOG,
            recovery_policy: RuntimeRecoveryPolicy::None,
            output_mode: RuntimeOutputMode::Structured,
        }
    }

    fn policy_failure(reason_code: &str, reason_summary_redacted: &str) -> Self::Failure {
        QueryRerankFailure {
            code: reason_code.to_string(),
            summary: reason_summary_redacted.to_string(),
        }
    }
}

impl StructuredRuntimeTask for QueryRerankTask {}
