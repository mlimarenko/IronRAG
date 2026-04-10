use crate::{
    agent_runtime::task::{RuntimeTask, RuntimeTaskSpec, StructuredRuntimeTask},
    domains::agent_runtime::{
        RuntimeOutputMode, RuntimeRecoveryPolicy, RuntimeStageKind, RuntimeSurfaceKind,
        RuntimeTaskKind,
    },
    services::ingest::structured_preparation::{
        PrepareStructuredRevisionCommand, PreparedStructuredRevision, StructuredPreparationFailure,
    },
};

const STRUCTURED_PREPARE_STAGE_CATALOG: &[RuntimeStageKind] =
    &[RuntimeStageKind::StructuredPrepare];

pub struct StructuredPrepareTask;

impl RuntimeTask for StructuredPrepareTask {
    type Input = PrepareStructuredRevisionCommand;
    type Success = PreparedStructuredRevision;
    type Failure = StructuredPreparationFailure;

    const CONTRACT_NAME: &'static str = "structured_prepare";
    const CONTRACT_VERSION: &'static str = "1";

    fn spec() -> RuntimeTaskSpec {
        RuntimeTaskSpec {
            task_kind: RuntimeTaskKind::StructuredPrepare,
            surface_kind: RuntimeSurfaceKind::Internal,
            binding_purpose: None,
            machine_consumed: true,
            max_turns: 1,
            max_parallel_actions: 1,
            stage_catalog: STRUCTURED_PREPARE_STAGE_CATALOG,
            recovery_policy: RuntimeRecoveryPolicy::None,
            output_mode: RuntimeOutputMode::Structured,
        }
    }

    fn policy_failure(reason_code: &str, reason_summary_redacted: &str) -> Self::Failure {
        StructuredPreparationFailure {
            code: reason_code.to_string(),
            summary: reason_summary_redacted.to_string(),
        }
    }
}

impl StructuredRuntimeTask for StructuredPrepareTask {}
