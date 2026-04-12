use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

use crate::{
    domains::{
        graph_quality::ExtractionRecoverySummary, runtime_graph::RuntimeNodeType,
        runtime_ingestion::RuntimeProviderFailureDetail,
    },
    shared::extraction::technical_facts::TechnicalFactQualifier,
};

#[derive(Debug, Clone)]
pub struct GraphExtractionRequest {
    pub library_id: uuid::Uuid,
    pub document: crate::infra::repositories::DocumentRow,
    pub chunk: crate::infra::repositories::ChunkRow,
    pub structured_chunk: GraphExtractionStructuredChunkContext,
    pub technical_facts: Vec<GraphExtractionTechnicalFact>,
    pub revision_id: Option<uuid::Uuid>,
    pub activated_by_attempt_id: Option<uuid::Uuid>,
    pub resume_hint: Option<GraphExtractionResumeHint>,
    pub library_extraction_prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct GraphExtractionStructuredChunkContext {
    pub chunk_kind: Option<String>,
    pub section_path: Vec<String>,
    pub heading_trail: Vec<String>,
    pub support_block_ids: Vec<uuid::Uuid>,
    pub literal_digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GraphExtractionTechnicalFact {
    pub fact_kind: String,
    pub canonical_value: String,
    pub display_value: String,
    pub qualifiers: Vec<TechnicalFactQualifier>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct GraphExtractionLifecycle {
    pub revision_id: Option<uuid::Uuid>,
    pub activated_by_attempt_id: Option<uuid::Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct GraphExtractionResumeHint {
    pub replay_count: usize,
    pub downgrade_level: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphExtractionResumeState {
    pub resumed_from_checkpoint: bool,
    pub replay_count: usize,
    pub downgrade_level: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphEntityCandidate {
    pub label: String,
    pub node_type: RuntimeNodeType,
    pub sub_type: Option<String>,
    pub aliases: Vec<String>,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphRelationCandidate {
    pub source_label: String,
    pub target_label: String,
    pub relation_type: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct GraphExtractionCandidateSet {
    pub entities: Vec<GraphEntityCandidate>,
    pub relations: Vec<GraphRelationCandidate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphExtractionTaskFailureCode {
    MalformedOutput,
    InvalidCandidateSet,
}

impl GraphExtractionTaskFailureCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MalformedOutput => "malformed_output",
            Self::InvalidCandidateSet => "invalid_candidate_set",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphExtractionTaskFailure {
    pub code: String,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct GraphExtractionOutcome {
    pub graph_extraction_id: Option<uuid::Uuid>,
    pub runtime_execution_id: Option<uuid::Uuid>,
    pub provider_kind: String,
    pub model_name: String,
    pub prompt_hash: String,
    pub raw_output_json: serde_json::Value,
    pub usage_json: serde_json::Value,
    pub usage_calls: Vec<GraphExtractionUsageCall>,
    pub normalized: GraphExtractionCandidateSet,
    pub provider_failure: Option<RuntimeProviderFailureDetail>,
    pub recovery_summary: ExtractionRecoverySummary,
    pub recovery_attempts: Vec<GraphExtractionRecoveryRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphExtractionRecoveryRecord {
    pub recovery_kind: String,
    pub trigger_reason: String,
    pub status: String,
    pub raw_issue_summary: Option<String>,
    pub recovered_summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphExtractionUsageCall {
    pub provider_call_no: i32,
    pub provider_attempt_no: i32,
    pub prompt_hash: String,
    pub request_shape_key: String,
    pub request_size_bytes: usize,
    pub usage_json: serde_json::Value,
    pub timing: GraphExtractionCallTiming,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphExtractionCallTiming {
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub elapsed_ms: i64,
    pub input_char_count: i32,
    pub output_char_count: i32,
    pub chars_per_second: Option<f64>,
    pub tokens_per_second: Option<f64>,
}

#[derive(Debug, Clone)]
pub(crate) struct RawGraphExtractionResponse {
    pub(crate) provider_kind: String,
    pub(crate) model_name: String,
    pub(crate) prompt_hash: String,
    pub(crate) request_shape_key: String,
    pub(crate) request_size_bytes: usize,
    pub(crate) output_text: String,
    pub(crate) usage_json: serde_json::Value,
    pub(crate) lifecycle: GraphExtractionLifecycle,
    pub(crate) timing: GraphExtractionCallTiming,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct GraphExtractionRecoveryAttempt {
    pub(crate) provider_attempt_no: usize,
    pub(crate) prompt_hash: String,
    pub(crate) output_text: String,
    pub(crate) usage_json: serde_json::Value,
    pub(crate) timing: GraphExtractionCallTiming,
    pub(crate) parse_error: Option<String>,
    pub(crate) normalization_path: String,
    pub(crate) recovery_kind: Option<String>,
    pub(crate) trigger_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct GraphExtractionRecoveryTrace {
    pub(crate) provider_attempt_count: usize,
    pub(crate) reask_count: usize,
    pub(crate) attempts: Vec<GraphExtractionRecoveryAttempt>,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedGraphExtraction {
    pub(crate) provider_kind: String,
    pub(crate) model_name: String,
    pub(crate) prompt_hash: String,
    pub(crate) output_text: String,
    pub(crate) usage_json: serde_json::Value,
    pub(crate) usage_calls: Vec<GraphExtractionUsageCall>,
    pub(crate) provider_failure: Option<RuntimeProviderFailureDetail>,
    pub(crate) normalized: GraphExtractionCandidateSet,
    pub(crate) lifecycle: GraphExtractionLifecycle,
    pub(crate) recovery: GraphExtractionRecoveryTrace,
    pub(crate) recovery_summary: ExtractionRecoverySummary,
    pub(crate) recovery_attempts: Vec<GraphExtractionRecoveryRecord>,
}

#[derive(Debug, Clone)]
pub(crate) struct GraphExtractionFailureOutcome {
    pub(crate) request_shape_key: String,
    pub(crate) request_size_bytes: usize,
    pub(crate) error_message: String,
    pub(crate) provider_failure: Option<RuntimeProviderFailureDetail>,
    pub(crate) recovery_summary: ExtractionRecoverySummary,
    pub(crate) recovery_attempts: Vec<GraphExtractionRecoveryRecord>,
}

#[derive(Debug, Clone)]
pub struct GraphExtractionExecutionError {
    pub message: String,
    pub request_shape_key: String,
    pub request_size_bytes: usize,
    pub provider_failure: Option<RuntimeProviderFailureDetail>,
    pub recovery_summary: ExtractionRecoverySummary,
    pub recovery_attempts: Vec<GraphExtractionRecoveryRecord>,
    pub resume_state: GraphExtractionResumeState,
}

impl fmt::Display for GraphExtractionExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for GraphExtractionExecutionError {}

#[derive(Debug, Clone)]
pub(crate) struct GraphExtractionPromptPlan {
    pub(crate) prompt: String,
    pub(crate) request_shape_key: String,
    pub(crate) request_size_bytes: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct NormalizedGraphExtractionAttempt {
    pub(crate) normalized: GraphExtractionCandidateSet,
    pub(crate) normalization_path: &'static str,
}

#[derive(Debug, Clone)]
pub(crate) struct FailedNormalizationAttempt {
    pub(crate) parse_error: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ParsedGraphExtractionCandidate {
    pub(crate) raw: RawGraphExtractionResponse,
    pub(crate) normalized: GraphExtractionCandidateSet,
    pub(crate) normalization_path: &'static str,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingRecoveryRecord {
    pub(crate) recovery_kind: String,
    pub(crate) trigger_reason: String,
    pub(crate) raw_issue_summary: Option<String>,
    pub(crate) recovered_summary: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) enum RecoveryFollowUpRequest {
    ProviderRetry { trigger_reason: String, issue_summary: String, previous_output: String },
    SecondPass { trigger_reason: String, issue_summary: String, previous_output: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GraphExtractionPromptVariant {
    Initial,
    ProviderRetry,
    SecondPass,
}
