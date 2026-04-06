use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    agent_runtime::task::{RuntimeTask, RuntimeTaskSpec, StructuredRuntimeTask},
    domains::{
        agent_runtime::{
            RuntimeOutputMode, RuntimeRecoveryPolicy, RuntimeStageKind, RuntimeSurfaceKind,
            RuntimeTaskKind,
        },
        ai::AiBindingPurpose,
    },
    services::graph_extract::{
        GraphExtractionCandidateSet, GraphExtractionTaskFailure, GraphExtractionTechnicalFact,
    },
};

const GRAPH_EXTRACT_STAGE_CATALOG: &[RuntimeStageKind] =
    &[RuntimeStageKind::ExtractGraph, RuntimeStageKind::Recovery];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphExtractTaskInput {
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub chunk_id: Uuid,
    pub revision_id: Option<Uuid>,
    pub normalized_text: String,
    pub technical_facts: Vec<GraphExtractionTechnicalFact>,
}

pub struct GraphExtractTask;

impl RuntimeTask for GraphExtractTask {
    type Input = GraphExtractTaskInput;
    type Success = GraphExtractionCandidateSet;
    type Failure = GraphExtractionTaskFailure;

    const CONTRACT_NAME: &'static str = "graph_extract";
    const CONTRACT_VERSION: &'static str = "1";

    fn spec() -> RuntimeTaskSpec {
        RuntimeTaskSpec {
            task_kind: RuntimeTaskKind::GraphExtract,
            surface_kind: RuntimeSurfaceKind::Worker,
            binding_purpose: AiBindingPurpose::for_runtime_task_kind(RuntimeTaskKind::GraphExtract),
            machine_consumed: true,
            max_turns: 2,
            max_parallel_actions: 1,
            stage_catalog: GRAPH_EXTRACT_STAGE_CATALOG,
            recovery_policy: RuntimeRecoveryPolicy::VisibleBounded { max_attempts: 2 },
            output_mode: RuntimeOutputMode::Structured,
        }
    }

    fn policy_failure(reason_code: &str, reason_summary_redacted: &str) -> Self::Failure {
        GraphExtractionTaskFailure {
            code: reason_code.to_string(),
            summary: reason_summary_redacted.to_string(),
        }
    }
}

impl StructuredRuntimeTask for GraphExtractTask {}
