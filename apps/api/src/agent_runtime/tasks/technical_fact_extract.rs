use crate::{
    agent_runtime::task::{RuntimeTask, RuntimeTaskSpec, StructuredRuntimeTask},
    domains::agent_runtime::{
        RuntimeOutputMode, RuntimeRecoveryPolicy, RuntimeStageKind, RuntimeSurfaceKind,
        RuntimeTaskKind,
    },
    services::ingest::technical_facts::{
        ExtractTechnicalFactsCommand, ExtractTechnicalFactsResult, TechnicalFactExtractionFailure,
    },
};

const TECHNICAL_FACT_EXTRACT_STAGE_CATALOG: &[RuntimeStageKind] =
    &[RuntimeStageKind::TechnicalFactExtract];

pub struct TechnicalFactExtractTask;

impl RuntimeTask for TechnicalFactExtractTask {
    type Input = ExtractTechnicalFactsCommand;
    type Success = ExtractTechnicalFactsResult;
    type Failure = TechnicalFactExtractionFailure;

    const CONTRACT_NAME: &'static str = "technical_fact_extract";
    const CONTRACT_VERSION: &'static str = "1";

    fn spec() -> RuntimeTaskSpec {
        RuntimeTaskSpec {
            task_kind: RuntimeTaskKind::TechnicalFactExtract,
            surface_kind: RuntimeSurfaceKind::Internal,
            binding_purpose: None,
            machine_consumed: true,
            max_turns: 1,
            max_parallel_actions: 1,
            stage_catalog: TECHNICAL_FACT_EXTRACT_STAGE_CATALOG,
            recovery_policy: RuntimeRecoveryPolicy::None,
            output_mode: RuntimeOutputMode::Structured,
        }
    }

    fn policy_failure(reason_code: &str, reason_summary_redacted: &str) -> Self::Failure {
        TechnicalFactExtractionFailure {
            code: reason_code.to_string(),
            summary: reason_summary_redacted.to_string(),
        }
    }
}

impl StructuredRuntimeTask for TechnicalFactExtractTask {}
