use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::time::Instant;

use crate::{
    app::state::AppState,
    domains::{
        ai::AiBindingPurpose,
        graph_quality::{ExtractionOutcomeStatus, ExtractionRecoverySummary},
        provider_profiles::EffectiveProviderProfile,
        runtime_graph::RuntimeNodeType,
        runtime_ingestion::{RuntimeProviderFailureClass, RuntimeProviderFailureDetail},
    },
    infra::repositories::{self, ChunkRow, DocumentRow, RuntimeGraphExtractionRecordRow},
    integrations::llm::{ChatRequest, LlmGateway},
    services::{
        ai_catalog_service::ResolvedRuntimeBinding,
        extraction_recovery::{ExtractionRecoveryService, ParserRepairClassification},
    },
};

const GRAPH_EXTRACTION_VERSION: &str = "graph_extract_v1";
const GRAPH_EXTRACTION_MAX_PROVIDER_ATTEMPTS: usize = 2;
const GRAPH_EXTRACTION_REQUEST_OVERHEAD_BYTES: usize = 8 * 1024;
const GRAPH_EXTRACTION_MAX_SEGMENTS: usize = 3;
const GRAPH_EXTRACTION_MAX_DOWNGRADE_LEVEL: usize = 2;

fn normalized_downgrade_level(request: &GraphExtractionRequest) -> usize {
    request
        .resume_hint
        .as_ref()
        .map(|hint| hint.downgrade_level.min(GRAPH_EXTRACTION_MAX_DOWNGRADE_LEVEL))
        .unwrap_or(0)
}

fn downgraded_request_size_soft_limit_bytes(base_limit: usize, downgrade_level: usize) -> usize {
    match downgrade_level.min(GRAPH_EXTRACTION_MAX_DOWNGRADE_LEVEL) {
        0 => base_limit,
        1 => (base_limit / 2).max(GRAPH_EXTRACTION_REQUEST_OVERHEAD_BYTES + 1024),
        _ => (base_limit / 3).max(GRAPH_EXTRACTION_REQUEST_OVERHEAD_BYTES + 1024),
    }
}

fn downgraded_max_segments(downgrade_level: usize) -> usize {
    match downgrade_level.min(GRAPH_EXTRACTION_MAX_DOWNGRADE_LEVEL) {
        0 => GRAPH_EXTRACTION_MAX_SEGMENTS,
        1 => 2,
        _ => 1,
    }
}

#[derive(Debug, Clone)]
pub struct GraphExtractionRequest {
    pub project_id: uuid::Uuid,
    pub document: DocumentRow,
    pub chunk: ChunkRow,
    pub revision_id: Option<uuid::Uuid>,
    pub activated_by_attempt_id: Option<uuid::Uuid>,
    pub resume_hint: Option<GraphExtractionResumeHint>,
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

#[derive(Debug, Clone)]
pub struct GraphExtractionOutcome {
    pub provider_kind: String,
    pub model_name: String,
    pub prompt_hash: String,
    pub raw_output_json: serde_json::Value,
    pub usage_json: serde_json::Value,
    pub usage_calls: Vec<GraphExtractionUsageCall>,
    pub normalized: GraphExtractionCandidateSet,
    pub request_shape_key: String,
    pub request_size_bytes: usize,
    pub provider_failure: Option<RuntimeProviderFailureDetail>,
    pub recovery_summary: ExtractionRecoverySummary,
    pub recovery_attempts: Vec<GraphExtractionRecoveryRecord>,
    pub resume_state: GraphExtractionResumeState,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct GraphExtractionTelemetrySummary {
    pub provider_call_count: usize,
    pub total_call_elapsed_ms: i64,
    pub avg_call_elapsed_ms: Option<i64>,
    pub avg_chars_per_second: Option<f64>,
    pub avg_tokens_per_second: Option<f64>,
    pub last_provider_call_at: Option<DateTime<Utc>>,
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
struct RawGraphExtractionResponse {
    provider_kind: String,
    model_name: String,
    prompt_hash: String,
    request_shape_key: String,
    request_size_bytes: usize,
    output_text: String,
    usage_json: serde_json::Value,
    lifecycle: GraphExtractionLifecycle,
    timing: GraphExtractionCallTiming,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GraphExtractionRecoveryAttempt {
    provider_attempt_no: usize,
    prompt_hash: String,
    output_text: String,
    usage_json: serde_json::Value,
    timing: GraphExtractionCallTiming,
    parse_error: Option<String>,
    normalization_path: String,
    repair_candidate: Option<String>,
    recovery_kind: Option<String>,
    trigger_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct GraphExtractionRecoveryTrace {
    provider_attempt_count: usize,
    reask_count: usize,
    local_repair_applied: bool,
    attempts: Vec<GraphExtractionRecoveryAttempt>,
}

#[derive(Debug, Clone)]
struct ResolvedGraphExtraction {
    provider_kind: String,
    model_name: String,
    prompt_hash: String,
    output_text: String,
    usage_json: serde_json::Value,
    usage_calls: Vec<GraphExtractionUsageCall>,
    request_shape_key: String,
    request_size_bytes: usize,
    provider_failure: Option<RuntimeProviderFailureDetail>,
    normalized: GraphExtractionCandidateSet,
    lifecycle: GraphExtractionLifecycle,
    recovery: GraphExtractionRecoveryTrace,
    recovery_summary: ExtractionRecoverySummary,
    recovery_attempts: Vec<GraphExtractionRecoveryRecord>,
}

#[derive(Debug, Clone)]
struct GraphExtractionFailureOutcome {
    provider_kind: String,
    model_name: String,
    prompt_hash: String,
    request_shape_key: String,
    request_size_bytes: usize,
    provider_attempt_count: usize,
    raw_output_json: serde_json::Value,
    error_message: String,
    provider_failure: Option<RuntimeProviderFailureDetail>,
    recovery_summary: ExtractionRecoverySummary,
    recovery_attempts: Vec<GraphExtractionRecoveryRecord>,
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

#[derive(Debug, Clone)]
struct GraphExtractionPromptPlan {
    prompt: String,
    request_shape_key: String,
    request_size_bytes: usize,
}

fn unconfigured_graph_extraction_failure(
    request: &GraphExtractionRequest,
    error_message: impl Into<String>,
) -> GraphExtractionFailureOutcome {
    GraphExtractionFailureOutcome {
        provider_kind: "unconfigured".to_string(),
        model_name: "unconfigured".to_string(),
        prompt_hash: sha256_hex(&build_graph_extraction_prompt(request)),
        request_shape_key: "graph_extract_v3:unconfigured".to_string(),
        request_size_bytes: 0,
        provider_attempt_count: 0,
        raw_output_json: serde_json::json!({}),
        error_message: error_message.into(),
        provider_failure: None,
        recovery_summary: ExtractionRecoverySummary {
            status: ExtractionOutcomeStatus::Failed,
            parser_repair_applied: false,
            second_pass_applied: false,
            warning: None,
        },
        recovery_attempts: Vec::new(),
    }
}

impl fmt::Display for GraphExtractionExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for GraphExtractionExecutionError {}

#[derive(Debug, Clone)]
struct NormalizedGraphExtractionAttempt {
    normalized: GraphExtractionCandidateSet,
    normalization_path: &'static str,
    repair_candidate: Option<String>,
    parser_repair: ParserRepairClassification,
}

#[derive(Debug, Clone)]
struct FailedNormalizationAttempt {
    parse_error: String,
    parser_repair: ParserRepairClassification,
}

#[derive(Debug, Clone)]
struct ParsedGraphExtractionCandidate {
    raw: RawGraphExtractionResponse,
    normalized: GraphExtractionCandidateSet,
    normalization_path: &'static str,
    repair_candidate: Option<String>,
}

#[derive(Debug, Clone)]
struct PendingRecoveryRecord {
    recovery_kind: String,
    trigger_reason: String,
    raw_issue_summary: Option<String>,
    recovered_summary: Option<String>,
}

#[derive(Debug, Clone)]
enum RecoveryFollowUpRequest {
    ProviderRetry { trigger_reason: String, issue_summary: String, previous_output: String },
    SecondPass { trigger_reason: String, issue_summary: String, previous_output: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GraphExtractionPromptVariant {
    Initial,
    ProviderRetry,
    SecondPass,
}

#[must_use]
pub fn build_graph_extraction_prompt(request: &GraphExtractionRequest) -> String {
    build_graph_extraction_prompt_plan(
        request,
        GraphExtractionPromptVariant::Initial,
        None,
        None,
        None,
        usize::MAX,
    )
    .prompt
}

#[cfg(test)]
#[must_use]
fn build_graph_extraction_prompt_preview(
    request: &GraphExtractionRequest,
    request_size_soft_limit_bytes: usize,
) -> (String, String, usize) {
    let plan = build_graph_extraction_prompt_plan(
        request,
        GraphExtractionPromptVariant::Initial,
        None,
        None,
        None,
        request_size_soft_limit_bytes,
    );
    (plan.prompt, plan.request_shape_key, plan.request_size_bytes)
}

fn build_graph_extraction_prompt_plan(
    request: &GraphExtractionRequest,
    variant: GraphExtractionPromptVariant,
    trigger_reason: Option<&str>,
    issue_summary: Option<&str>,
    previous_output: Option<&str>,
    request_size_soft_limit_bytes: usize,
) -> GraphExtractionPromptPlan {
    let downgrade_level = normalized_downgrade_level(request);
    let document_label = request
        .document
        .title
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&request.document.external_key);
    let safe_limit = downgraded_request_size_soft_limit_bytes(
        request_size_soft_limit_bytes.max(GRAPH_EXTRACTION_REQUEST_OVERHEAD_BYTES + 1024),
        downgrade_level,
    );
    let chunk_text_budget = safe_limit.saturating_sub(GRAPH_EXTRACTION_REQUEST_OVERHEAD_BYTES);
    let chunk_segments = segment_chunk_text_for_prompt(
        &request.chunk.content,
        chunk_text_budget.max(1024),
        downgraded_max_segments(downgrade_level),
    );

    let mut sections: Vec<(String, String)> = Vec::new();
    sections.push((
        "task".to_string(),
        "You extract graph-ready entities and relationships from one document chunk.".to_string(),
    ));
    sections.push((
        "schema".to_string(),
        "Return strict JSON with keys `entities` and `relations`. Each entity must include `label`, `node_type` (`entity` or `topic`), `aliases`, and `summary`. Each relation must include `source_label`, `target_label`, `relation_type`, and `summary`.".to_string(),
    ));
    sections.push((
        "rules".to_string(),
        "Do not include markdown fences or prose. If no grounded graph evidence exists, return {\"entities\":[],\"relations\":[]}.".to_string(),
    ));
    sections.push((
        "document".to_string(),
        format!("Document: {document_label}\nChunk ordinal: {}", request.chunk.ordinal),
    ));

    if downgrade_level > 0 {
        sections.push((
            "downgrade".to_string(),
            format!(
                "Adaptive downgrade level: {downgrade_level}\nReason: repeated recoverable extraction replay on this chunk."
            ),
        ));
    }

    if variant != GraphExtractionPromptVariant::Initial {
        sections.push((
            "recovery".to_string(),
            format!(
                "Recovery variant: {}\nTrigger: {}\nIssue: {}",
                match variant {
                    GraphExtractionPromptVariant::Initial => "initial",
                    GraphExtractionPromptVariant::ProviderRetry => "provider_retry",
                    GraphExtractionPromptVariant::SecondPass => "second_pass",
                },
                trigger_reason.unwrap_or("unknown"),
                issue_summary.unwrap_or("unspecified"),
            ),
        ));
    }

    if let Some(previous_output) = previous_output {
        sections.push((
            "previous_output".to_string(),
            format!("Previous extraction output:\n{previous_output}"),
        ));
    }

    for (index, segment) in chunk_segments.iter().enumerate() {
        sections.push((format!("chunk_segment_{}", index + 1), segment.clone()));
    }

    let prompt = sections
        .iter()
        .map(|(title, body)| format!("[{title}]\n{body}"))
        .collect::<Vec<_>>()
        .join("\n\n");
    let request_size_bytes = prompt.len();
    let request_shape_key = format!(
        "graph_extract_v3:{}:segments_{}:downgrade_{}:{}",
        match variant {
            GraphExtractionPromptVariant::Initial => "initial",
            GraphExtractionPromptVariant::ProviderRetry => "provider_retry",
            GraphExtractionPromptVariant::SecondPass => "second_pass",
        },
        chunk_segments.len(),
        downgrade_level,
        if request_size_bytes > request_size_soft_limit_bytes { "trimmed" } else { "full" }
    );

    GraphExtractionPromptPlan { prompt, request_shape_key, request_size_bytes }
}

fn segment_chunk_text_for_prompt(
    content: &str,
    max_total_bytes: usize,
    max_segments: usize,
) -> Vec<String> {
    if content.is_empty() {
        return vec!["Chunk text:".to_string()];
    }

    if content.len() <= max_total_bytes {
        return vec![format!("Chunk text:\n{content}")];
    }

    let segment_count = max_segments.max(1);
    let segment_budget = (max_total_bytes / segment_count).max(256);
    let chars = content.chars().collect::<Vec<_>>();
    let total_chars = chars.len();
    let approx_chars_per_segment = segment_budget / 4;
    let edge_chars = approx_chars_per_segment.min(total_chars);
    let head = chars[..edge_chars].iter().collect::<String>();
    if segment_count == 1 {
        return vec![format!("Chunk text segment 1/1:\n{head}")];
    }

    if segment_count == 2 {
        let tail = chars[total_chars.saturating_sub(edge_chars)..].iter().collect::<String>();
        return vec![
            "Chunk text segment 1/2:\n".to_string() + &head,
            "Chunk text segment 2/2:\n".to_string() + &tail,
        ];
    }

    let middle_start = total_chars.saturating_sub(approx_chars_per_segment) / 2;
    let middle_end = (middle_start + approx_chars_per_segment).min(total_chars);
    let middle = chars[middle_start..middle_end].iter().collect::<String>();
    let tail = chars[total_chars.saturating_sub(edge_chars)..].iter().collect::<String>();

    vec![
        format!("Chunk text segment 1/{segment_count}:\n{head}"),
        format!("Chunk text segment 2/{segment_count}:\n{middle}"),
        format!("Chunk text segment 3/{segment_count}:\n{tail}"),
    ]
}

#[must_use]
pub fn summarize_graph_extraction_usage_calls(
    usage_calls: &[GraphExtractionUsageCall],
) -> GraphExtractionTelemetrySummary {
    if usage_calls.is_empty() {
        return GraphExtractionTelemetrySummary::default();
    }

    let provider_call_count = usage_calls.len();
    let total_call_elapsed_ms: i64 =
        usage_calls.iter().map(|call| call.timing.elapsed_ms.max(0)).sum();
    let avg_call_elapsed_ms =
        Some(total_call_elapsed_ms / i64::try_from(provider_call_count).unwrap_or(1));

    let chars_per_second_values =
        usage_calls.iter().filter_map(|call| call.timing.chars_per_second).collect::<Vec<_>>();
    let tokens_per_second_values =
        usage_calls.iter().filter_map(|call| call.timing.tokens_per_second).collect::<Vec<_>>();

    GraphExtractionTelemetrySummary {
        provider_call_count,
        total_call_elapsed_ms,
        avg_call_elapsed_ms,
        avg_chars_per_second: (!chars_per_second_values.is_empty()).then(|| {
            chars_per_second_values.iter().sum::<f64>() / chars_per_second_values.len() as f64
        }),
        avg_tokens_per_second: (!tokens_per_second_values.is_empty()).then(|| {
            tokens_per_second_values.iter().sum::<f64>() / tokens_per_second_values.len() as f64
        }),
        last_provider_call_at: usage_calls.iter().map(|call| call.timing.finished_at).max(),
    }
}

pub async fn extract_and_persist_chunk_graph_result(
    state: &AppState,
    provider_profile: &EffectiveProviderProfile,
    request: &GraphExtractionRequest,
) -> std::result::Result<GraphExtractionOutcome, GraphExtractionExecutionError> {
    match extract_chunk_graph_candidates(state, provider_profile, request).await {
        Ok(outcome) => {
            let raw_output_json = outcome.raw_output_json.clone();
            let normalized_json =
                serde_json::to_value(&outcome.normalized).unwrap_or_else(|_| serde_json::json!({}));
            repositories::create_runtime_graph_extraction_record(
                &state.persistence.postgres,
                request.project_id,
                request.document.id,
                request.chunk.id,
                &outcome.provider_kind,
                &outcome.model_name,
                GRAPH_EXTRACTION_VERSION,
                &outcome.prompt_hash,
                "ready",
                raw_output_json,
                normalized_json,
                i32::try_from(outcome.usage_calls.len()).unwrap_or(i32::MAX),
                None,
            )
            .await
            .map_err(|error| GraphExtractionExecutionError {
                message: format!("failed to persist graph extraction record: {error:#}"),
                request_shape_key: outcome.request_shape_key.clone(),
                request_size_bytes: outcome.request_size_bytes,
                provider_failure: outcome.provider_failure.clone(),
                recovery_summary: outcome.recovery_summary.clone(),
                recovery_attempts: outcome.recovery_attempts.clone(),
                resume_state: outcome.resume_state.clone(),
            })?;
            Ok(outcome)
        }
        Err(error) => {
            repositories::create_runtime_graph_extraction_record(
                &state.persistence.postgres,
                request.project_id,
                request.document.id,
                request.chunk.id,
                error
                    .provider_failure
                    .as_ref()
                    .and_then(|failure| failure.provider_kind.as_deref())
                    .unwrap_or("unknown"),
                error
                    .provider_failure
                    .as_ref()
                    .and_then(|failure| failure.model_name.as_deref())
                    .unwrap_or("unknown"),
                GRAPH_EXTRACTION_VERSION,
                "unknown",
                "failed",
                serde_json::json!({}),
                serde_json::json!({ "entities": [], "relations": [] }),
                i32::try_from(error.resume_state.replay_count).unwrap_or(i32::MAX),
                Some(&error.message),
            )
            .await
            .map_err(|persist_error| GraphExtractionExecutionError {
                message: format!(
                    "failed to persist graph extraction failure record: {persist_error:#}"
                ),
                request_shape_key: error.request_shape_key.clone(),
                request_size_bytes: error.request_size_bytes,
                provider_failure: error.provider_failure.clone(),
                recovery_summary: error.recovery_summary.clone(),
                recovery_attempts: error.recovery_attempts.clone(),
                resume_state: error.resume_state.clone(),
            })?;
            Err(error)
        }
    }
}

pub async fn extract_chunk_graph_candidates(
    state: &AppState,
    provider_profile: &EffectiveProviderProfile,
    request: &GraphExtractionRequest,
) -> std::result::Result<GraphExtractionOutcome, GraphExtractionExecutionError> {
    match resolve_graph_extraction(state, provider_profile, request).await {
        Ok(resolved) => Ok(GraphExtractionOutcome {
            provider_kind: resolved.provider_kind.clone(),
            model_name: resolved.model_name.clone(),
            prompt_hash: resolved.prompt_hash.clone(),
            raw_output_json: build_raw_output_json(
                &resolved.output_text,
                resolved.usage_json.clone(),
                &resolved.lifecycle,
                &resolved.recovery,
                &resolved.recovery_summary,
                &resolved.usage_calls,
            ),
            usage_json: resolved.usage_json.clone(),
            usage_calls: resolved.usage_calls.clone(),
            normalized: resolved.normalized,
            request_shape_key: resolved.request_shape_key.clone(),
            request_size_bytes: resolved.request_size_bytes,
            provider_failure: resolved.provider_failure.clone(),
            recovery_summary: resolved.recovery_summary,
            recovery_attempts: resolved.recovery_attempts,
            resume_state: GraphExtractionResumeState {
                resumed_from_checkpoint: false,
                replay_count: request
                    .resume_hint
                    .as_ref()
                    .map(|hint| hint.replay_count)
                    .unwrap_or(0),
                downgrade_level: normalized_downgrade_level(request),
            },
        }),
        Err(failure) => Err(GraphExtractionExecutionError {
            message: failure.error_message,
            request_shape_key: failure.request_shape_key,
            request_size_bytes: failure.request_size_bytes,
            provider_failure: failure.provider_failure,
            recovery_summary: failure.recovery_summary,
            recovery_attempts: failure.recovery_attempts,
            resume_state: GraphExtractionResumeState {
                resumed_from_checkpoint: false,
                replay_count: request
                    .resume_hint
                    .as_ref()
                    .map(|hint| hint.replay_count.saturating_add(1))
                    .unwrap_or(1),
                downgrade_level: normalized_downgrade_level(request),
            },
        }),
    }
}

#[must_use]
pub fn extraction_lifecycle_from_record(
    record: &RuntimeGraphExtractionRecordRow,
) -> GraphExtractionLifecycle {
    record
        .raw_output_json
        .get("lifecycle")
        .and_then(|value| serde_json::from_value::<GraphExtractionLifecycle>(value.clone()).ok())
        .unwrap_or_default()
}

#[must_use]
pub fn extraction_recovery_summary_from_record(
    record: &RuntimeGraphExtractionRecordRow,
) -> Option<ExtractionRecoverySummary> {
    record
        .raw_output_json
        .get("recovery_summary")
        .and_then(|value| serde_json::from_value::<ExtractionRecoverySummary>(value.clone()).ok())
}

pub fn extraction_outcome_from_resume_state(
    row: &repositories::RuntimeGraphExtractionResumeStateRow,
) -> Result<GraphExtractionOutcome> {
    let normalized =
        serde_json::from_value::<GraphExtractionCandidateSet>(row.normalized_output_json.clone())
            .context("failed to parse resumed graph extraction candidate set")?;
    let recovery_summary =
        serde_json::from_value::<ExtractionRecoverySummary>(row.recovery_summary_json.clone())
            .unwrap_or(ExtractionRecoverySummary {
                status: crate::domains::graph_quality::ExtractionOutcomeStatus::Clean,
                parser_repair_applied: false,
                second_pass_applied: false,
                warning: None,
            });
    let provider_failure = row
        .provider_failure_json
        .clone()
        .and_then(|value| serde_json::from_value::<RuntimeProviderFailureDetail>(value).ok());

    Ok(GraphExtractionOutcome {
        provider_kind: row.provider_kind.clone().unwrap_or_else(|| "unknown".to_string()),
        model_name: row.model_name.clone().unwrap_or_else(|| "unknown".to_string()),
        prompt_hash: row.prompt_hash.clone().unwrap_or_else(|| "unknown".to_string()),
        raw_output_json: row.raw_output_json.clone(),
        usage_json: serde_json::json!({}),
        usage_calls: Vec::new(),
        normalized,
        request_shape_key: row
            .request_shape_key
            .clone()
            .unwrap_or_else(|| "graph_extract_v3:resumed".to_string()),
        request_size_bytes: row
            .request_size_bytes
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(0),
        provider_failure,
        recovery_summary,
        recovery_attempts: Vec::new(),
        resume_state: GraphExtractionResumeState {
            resumed_from_checkpoint: true,
            replay_count: usize::try_from(row.replay_count.max(0)).unwrap_or(usize::MAX),
            downgrade_level: usize::try_from(row.downgrade_level.max(0)).unwrap_or(usize::MAX),
        },
    })
}

async fn resolve_graph_extraction(
    state: &AppState,
    provider_profile: &EffectiveProviderProfile,
    request: &GraphExtractionRequest,
) -> std::result::Result<ResolvedGraphExtraction, GraphExtractionFailureOutcome> {
    let library_id = request.document.project_id;
    let runtime_binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::ExtractGraph)
        .await
        .map_err(|error| unconfigured_graph_extraction_failure(request, error.to_string()))?
        .ok_or_else(|| {
            unconfigured_graph_extraction_failure(
                request,
                "active graph extraction binding is not configured for this library",
            )
        })?;
    resolve_graph_extraction_with_gateway(
        state.llm_gateway.as_ref(),
        &state.retrieval_intelligence_services.extraction_recovery,
        &state.resolve_settle_blockers_services.provider_failure_classification,
        provider_profile,
        &runtime_binding,
        request,
        state.retrieval_intelligence.extraction_recovery_enabled,
        state
            .retrieval_intelligence
            .extraction_recovery_max_attempts
            .clamp(1, GRAPH_EXTRACTION_MAX_PROVIDER_ATTEMPTS),
        state.resolve_settle_blockers.provider_timeout_retry_limit.max(1),
    )
    .await
}

async fn resolve_graph_extraction_with_gateway(
    gateway: &dyn LlmGateway,
    extraction_recovery: &ExtractionRecoveryService,
    provider_failure_classification: &crate::services::provider_failure_classification::ProviderFailureClassificationService,
    provider_profile: &EffectiveProviderProfile,
    runtime_binding: &ResolvedRuntimeBinding,
    request: &GraphExtractionRequest,
    recovery_enabled: bool,
    max_provider_attempts: usize,
    provider_timeout_retry_limit: usize,
) -> std::result::Result<ResolvedGraphExtraction, GraphExtractionFailureOutcome> {
    let provider_kind = runtime_binding.provider_kind.clone();
    let model_name = runtime_binding.model_name.clone();
    let lifecycle = GraphExtractionLifecycle {
        revision_id: request.revision_id,
        activated_by_attempt_id: request.activated_by_attempt_id,
    };
    let mut trace = GraphExtractionRecoveryTrace::default();
    let mut usage_samples = Vec::new();
    let mut usage_calls = Vec::new();
    let mut pending_follow_up = None;
    let mut pending_recovery_records = Vec::new();
    let mut best_partial_candidate = None;
    let request_size_soft_limit_bytes =
        provider_failure_classification.request_size_soft_limit_bytes();

    let max_provider_attempts = if recovery_enabled { max_provider_attempts.max(1) } else { 1 };
    for provider_attempt_no in 1..=max_provider_attempts {
        let retry_decision = (provider_attempt_no > 1).then_some("retrying_provider_call");
        let prompt_plan = match pending_follow_up.take() {
            None => build_graph_extraction_prompt_plan(
                request,
                GraphExtractionPromptVariant::Initial,
                None,
                None,
                None,
                request_size_soft_limit_bytes,
            ),
            Some(RecoveryFollowUpRequest::ProviderRetry {
                trigger_reason,
                issue_summary,
                previous_output,
            }) => build_graph_extraction_prompt_plan(
                request,
                GraphExtractionPromptVariant::ProviderRetry,
                Some(&trigger_reason),
                Some(&issue_summary),
                Some(&previous_output),
                request_size_soft_limit_bytes,
            ),
            Some(RecoveryFollowUpRequest::SecondPass {
                trigger_reason,
                issue_summary,
                previous_output,
            }) => build_graph_extraction_prompt_plan(
                request,
                GraphExtractionPromptVariant::SecondPass,
                Some(&trigger_reason),
                Some(&issue_summary),
                Some(&previous_output),
                request_size_soft_limit_bytes,
            ),
        };
        let raw = match request_graph_extraction_with_prompt_plan(
            gateway,
            provider_profile,
            runtime_binding,
            &prompt_plan,
            lifecycle.clone(),
        )
        .await
        {
            Ok(raw) => raw,
            Err(error) => {
                let error_context = format!("{error:#}");
                let provider_failure = provider_failure_classification.classify_failure(
                    &provider_kind,
                    &model_name,
                    &error_context,
                    &prompt_plan.request_shape_key,
                    prompt_plan.request_size_bytes,
                    Some(1),
                    None,
                    retry_decision.map(str::to_string),
                    !usage_calls.is_empty(),
                );
                let transient_retry_plan = if provider_failure_classification
                    .is_transient_retryable_failure(&provider_failure)
                {
                    match provider_failure.failure_class {
                        RuntimeProviderFailureClass::UpstreamTimeout => Some((
                            "upstream_timeout",
                            "Retrying graph extraction after an upstream timeout.",
                        )),
                        RuntimeProviderFailureClass::UpstreamProtocolFailure => Some((
                            "upstream_protocol_failure",
                            "Retrying graph extraction after an upstream protocol parse failure on a locally valid request.",
                        )),
                        RuntimeProviderFailureClass::UpstreamRejection => Some((
                            "upstream_transient_rejection",
                            "Retrying graph extraction after a transient upstream rejection.",
                        )),
                        _ => None,
                    }
                } else {
                    None
                };
                let allow_transient_retry = transient_retry_plan.is_some()
                    && provider_attempt_no <= provider_timeout_retry_limit
                    && provider_attempt_no < max_provider_attempts;
                if let (true, Some((trigger_reason, recovered_summary))) =
                    (allow_transient_retry, transient_retry_plan)
                {
                    pending_recovery_records.push(PendingRecoveryRecord {
                        recovery_kind: "provider_retry".to_string(),
                        trigger_reason: trigger_reason.to_string(),
                        raw_issue_summary: Some(error_context.clone()),
                        recovered_summary: Some(recovered_summary.to_string()),
                    });
                    pending_follow_up = Some(RecoveryFollowUpRequest::ProviderRetry {
                        trigger_reason: trigger_reason.to_string(),
                        issue_summary: error_context.clone(),
                        previous_output: String::new(),
                    });
                    trace.provider_attempt_count = provider_attempt_no;
                    trace.reask_count = provider_attempt_no.saturating_sub(1);
                    continue;
                }
                trace.provider_attempt_count = provider_attempt_no;
                trace.reask_count = provider_attempt_no.saturating_sub(1);
                if let Some(candidate) = best_partial_candidate.clone() {
                    let recovery_summary = extraction_recovery.classify_outcome(
                        trace.provider_attempt_count,
                        trace.local_repair_applied,
                        pending_recovery_records.iter().any(|record: &PendingRecoveryRecord| {
                            record.recovery_kind == "second_pass"
                        }),
                        true,
                        false,
                    );
                    let recovery_attempts = finalize_recovery_attempt_records(
                        &pending_recovery_records,
                        &recovery_summary,
                    );
                    return Ok(build_resolved_extraction_from_candidate(
                        candidate,
                        &provider_kind,
                        &model_name,
                        &usage_samples,
                        usage_calls,
                        prompt_plan.request_shape_key.clone(),
                        prompt_plan.request_size_bytes,
                        Some(provider_failure),
                        trace,
                        recovery_summary,
                        recovery_attempts,
                    ));
                }
                let recovery_summary = extraction_recovery.classify_outcome(
                    trace.provider_attempt_count,
                    trace.local_repair_applied,
                    pending_recovery_records.iter().any(|record: &PendingRecoveryRecord| {
                        record.recovery_kind == "second_pass"
                    }),
                    false,
                    true,
                );
                return Err(GraphExtractionFailureOutcome {
                    provider_kind: provider_kind.clone(),
                    model_name: model_name.clone(),
                    prompt_hash: sha256_hex(&prompt_plan.prompt),
                    request_shape_key: prompt_plan.request_shape_key,
                    request_size_bytes: prompt_plan.request_size_bytes,
                    provider_attempt_count: trace.provider_attempt_count,
                    raw_output_json: build_raw_output_json(
                        "",
                        aggregate_provider_usage_json(&provider_kind, &model_name, &usage_samples),
                        &lifecycle,
                        &trace,
                        &recovery_summary,
                        &usage_calls,
                    ),
                    error_message: if provider_attempt_no == 1 {
                        format!(
                            "graph extraction provider call failed before normalization retry: {error:#}"
                        )
                    } else {
                        format!(
                            "graph extraction recovery attempt {} failed: {error:#}",
                            provider_attempt_no,
                        )
                    },
                    provider_failure: Some(provider_failure),
                    recovery_summary: recovery_summary.clone(),
                    recovery_attempts: finalize_recovery_attempt_records(
                        &pending_recovery_records,
                        &recovery_summary,
                    ),
                });
            }
        };
        usage_samples.push(raw.usage_json.clone());
        usage_calls.push(GraphExtractionUsageCall {
            provider_call_no: i32::try_from(usage_calls.len() + 1).unwrap_or(i32::MAX),
            provider_attempt_no: i32::try_from(provider_attempt_no).unwrap_or(i32::MAX),
            prompt_hash: raw.prompt_hash.clone(),
            request_shape_key: raw.request_shape_key.clone(),
            request_size_bytes: raw.request_size_bytes,
            usage_json: raw.usage_json.clone(),
            timing: raw.timing.clone(),
        });
        match normalize_graph_extraction_output_with_repair(extraction_recovery, &raw.output_text) {
            Ok(normalized_attempt) => {
                trace.provider_attempt_count = provider_attempt_no;
                trace.reask_count = provider_attempt_no.saturating_sub(1);
                trace.local_repair_applied |= normalized_attempt.normalization_path == "repaired";
                trace.attempts.push(GraphExtractionRecoveryAttempt {
                    provider_attempt_no,
                    prompt_hash: raw.prompt_hash.clone(),
                    output_text: raw.output_text.clone(),
                    usage_json: raw.usage_json.clone(),
                    timing: raw.timing.clone(),
                    parse_error: None,
                    normalization_path: normalized_attempt.normalization_path.to_string(),
                    repair_candidate: normalized_attempt.repair_candidate.clone(),
                    recovery_kind: normalized_attempt
                        .parser_repair
                        .trigger_reason
                        .as_ref()
                        .map(|_| "parser_repair".to_string()),
                    trigger_reason: normalized_attempt.parser_repair.trigger_reason.clone(),
                });
                if normalized_attempt.normalization_path == "repaired" {
                    pending_recovery_records.push(PendingRecoveryRecord {
                        recovery_kind: "parser_repair".to_string(),
                        trigger_reason: normalized_attempt
                            .parser_repair
                            .trigger_reason
                            .clone()
                            .unwrap_or_else(|| "malformed_json".to_string()),
                        raw_issue_summary: normalized_attempt.parser_repair.issue_summary.clone(),
                        recovered_summary: Some(
                            "Recovered malformed extraction output with local parser repair."
                                .to_string(),
                        ),
                    });
                }

                let second_pass = extraction_recovery.classify_second_pass(
                    &request.chunk.content,
                    normalized_attempt.normalized.entities.len(),
                    normalized_attempt.normalized.relations.len(),
                    recovery_enabled,
                    provider_attempt_no,
                    max_provider_attempts,
                );
                let current_candidate = ParsedGraphExtractionCandidate {
                    raw: raw.clone(),
                    normalized: normalized_attempt.normalized,
                    normalization_path: normalized_attempt.normalization_path,
                    repair_candidate: normalized_attempt.repair_candidate,
                };

                if second_pass.should_attempt {
                    best_partial_candidate = select_better_partial_candidate(
                        best_partial_candidate,
                        current_candidate.clone(),
                    );
                    pending_recovery_records.push(PendingRecoveryRecord {
                        recovery_kind: "second_pass".to_string(),
                        trigger_reason: second_pass
                            .trigger_reason
                            .clone()
                            .unwrap_or_else(|| "sparse_extraction".to_string()),
                        raw_issue_summary: second_pass.issue_summary.clone(),
                        recovered_summary: Some(
                            "Requested a second extraction pass because the first result looked sparse or inconsistent."
                                .to_string(),
                        ),
                    });
                    trace.attempts.push(GraphExtractionRecoveryAttempt {
                        provider_attempt_no,
                        prompt_hash: raw.prompt_hash.clone(),
                        output_text: raw.output_text.clone(),
                        usage_json: raw.usage_json.clone(),
                        timing: raw.timing.clone(),
                        parse_error: None,
                        normalization_path: current_candidate.normalization_path.to_string(),
                        repair_candidate: current_candidate.repair_candidate.clone(),
                        recovery_kind: Some("second_pass".to_string()),
                        trigger_reason: second_pass.trigger_reason.clone(),
                    });
                    pending_follow_up = Some(RecoveryFollowUpRequest::SecondPass {
                        trigger_reason: second_pass
                            .trigger_reason
                            .unwrap_or_else(|| "sparse_extraction".to_string()),
                        issue_summary: second_pass.issue_summary.unwrap_or_else(|| {
                            "The extraction result looked too sparse for the chunk content."
                                .to_string()
                        }),
                        previous_output: raw.output_text.clone(),
                    });
                    continue;
                }

                let recovery_summary = extraction_recovery.classify_outcome(
                    trace.provider_attempt_count,
                    trace.local_repair_applied,
                    pending_recovery_records
                        .iter()
                        .any(|record| record.recovery_kind == "second_pass"),
                    false,
                    false,
                );
                let recovery_attempts =
                    finalize_recovery_attempt_records(&pending_recovery_records, &recovery_summary);
                return Ok(build_resolved_extraction_from_candidate(
                    current_candidate,
                    &raw.provider_kind,
                    &raw.model_name,
                    &usage_samples,
                    usage_calls,
                    raw.request_shape_key.clone(),
                    raw.request_size_bytes,
                    (provider_attempt_no > 1).then(|| {
                        provider_failure_classification.summarize(
                            RuntimeProviderFailureClass::RecoveredAfterRetry,
                            Some(raw.provider_kind.clone()),
                            Some(raw.model_name.clone()),
                            Some(raw.request_shape_key.clone()),
                            Some(raw.request_size_bytes),
                            Some(1),
                            None,
                            Some(raw.timing.elapsed_ms),
                            Some("recovered_after_retry".to_string()),
                            true,
                        )
                    }),
                    trace,
                    recovery_summary,
                    recovery_attempts,
                ));
            }
            Err(parse_failure) => {
                let parse_error = parse_failure.parse_error;
                trace.attempts.push(GraphExtractionRecoveryAttempt {
                    provider_attempt_no,
                    prompt_hash: raw.prompt_hash.clone(),
                    output_text: raw.output_text.clone(),
                    usage_json: raw.usage_json.clone(),
                    timing: raw.timing.clone(),
                    parse_error: Some(parse_error.clone()),
                    normalization_path: "failed".to_string(),
                    repair_candidate: parse_failure.parser_repair.repair_candidate.clone(),
                    recovery_kind: parse_failure
                        .parser_repair
                        .trigger_reason
                        .as_ref()
                        .map(|_| "provider_retry".to_string()),
                    trigger_reason: parse_failure.parser_repair.trigger_reason.clone(),
                });
                trace.provider_attempt_count = provider_attempt_no;
                trace.reask_count = provider_attempt_no.saturating_sub(1);
                if provider_attempt_no < max_provider_attempts {
                    pending_recovery_records.push(PendingRecoveryRecord {
                        recovery_kind: "provider_retry".to_string(),
                        trigger_reason: parse_failure
                            .parser_repair
                            .trigger_reason
                            .clone()
                            .unwrap_or_else(|| "malformed_output".to_string()),
                        raw_issue_summary: Some(parse_error.clone()),
                        recovered_summary: Some(
                            "Requested a stricter retry after malformed extraction output."
                                .to_string(),
                        ),
                    });
                    pending_follow_up = Some(RecoveryFollowUpRequest::ProviderRetry {
                        trigger_reason: parse_failure
                            .parser_repair
                            .trigger_reason
                            .unwrap_or_else(|| "malformed_output".to_string()),
                        issue_summary: parse_error,
                        previous_output: raw.output_text.clone(),
                    });
                    continue;
                }

                if let Some(candidate) = best_partial_candidate.clone() {
                    let recovery_summary = extraction_recovery.classify_outcome(
                        trace.provider_attempt_count,
                        trace.local_repair_applied,
                        pending_recovery_records
                            .iter()
                            .any(|record| record.recovery_kind == "second_pass"),
                        true,
                        false,
                    );
                    let recovery_attempts = finalize_recovery_attempt_records(
                        &pending_recovery_records,
                        &recovery_summary,
                    );
                    return Ok(build_resolved_extraction_from_candidate(
                        candidate,
                        &raw.provider_kind,
                        &raw.model_name,
                        &usage_samples,
                        usage_calls,
                        raw.request_shape_key.clone(),
                        raw.request_size_bytes,
                        Some(provider_failure_classification.summarize(
                            RuntimeProviderFailureClass::RecoveredAfterRetry,
                            Some(raw.provider_kind.clone()),
                            Some(raw.model_name.clone()),
                            Some(raw.request_shape_key.clone()),
                            Some(raw.request_size_bytes),
                            Some(1),
                            None,
                            Some(raw.timing.elapsed_ms),
                            Some("recovered_after_retry".to_string()),
                            true,
                        )),
                        trace,
                        recovery_summary,
                        recovery_attempts,
                    ));
                }

                if provider_attempt_no == max_provider_attempts {
                    let provider_attempt_count = trace.provider_attempt_count;
                    let recovery_summary = extraction_recovery.classify_outcome(
                        trace.provider_attempt_count,
                        trace.local_repair_applied,
                        pending_recovery_records
                            .iter()
                            .any(|record| record.recovery_kind == "second_pass"),
                        false,
                        true,
                    );
                    return Err(GraphExtractionFailureOutcome {
                        provider_kind: raw.provider_kind.clone(),
                        model_name: raw.model_name.clone(),
                        prompt_hash: raw.prompt_hash.clone(),
                        request_shape_key: raw.request_shape_key.clone(),
                        request_size_bytes: raw.request_size_bytes,
                        provider_attempt_count: trace.provider_attempt_count,
                        raw_output_json: build_raw_output_json(
                            &raw.output_text,
                            aggregate_provider_usage_json(
                                &raw.provider_kind,
                                &raw.model_name,
                                &usage_samples,
                            ),
                            &raw.lifecycle,
                            &trace,
                            &recovery_summary,
                            &usage_calls,
                        ),
                        error_message: format!(
                            "failed to normalize graph extraction output after {} provider attempt(s): {}",
                            provider_attempt_count, parse_error,
                        ),
                        provider_failure: Some(provider_failure_classification.summarize(
                            RuntimeProviderFailureClass::InvalidModelOutput,
                            Some(raw.provider_kind.clone()),
                            Some(raw.model_name.clone()),
                            Some(raw.request_shape_key.clone()),
                            Some(raw.request_size_bytes),
                            Some(1),
                            None,
                            Some(raw.timing.elapsed_ms),
                            Some("terminal_failure".to_string()),
                            !usage_calls.is_empty(),
                        )),
                        recovery_summary: recovery_summary.clone(),
                        recovery_attempts: finalize_recovery_attempt_records(
                            &pending_recovery_records,
                            &recovery_summary,
                        ),
                    });
                }
            }
        }
    }

    let aggregate_usage =
        aggregate_provider_usage_json(&provider_kind, &model_name, &usage_samples);
    Err(GraphExtractionFailureOutcome {
        provider_kind,
        model_name,
        prompt_hash: sha256_hex(&build_graph_extraction_prompt(request)),
        request_shape_key: "graph_extract_v3:unknown".to_string(),
        request_size_bytes: 0,
        provider_attempt_count: trace.provider_attempt_count,
        recovery_summary: extraction_recovery.classify_outcome(
            trace.provider_attempt_count,
            trace.local_repair_applied,
            pending_recovery_records.iter().any(|record| record.recovery_kind == "second_pass"),
            false,
            true,
        ),
        raw_output_json: build_raw_output_json(
            "",
            aggregate_usage,
            &lifecycle,
            &trace,
            &extraction_recovery.classify_outcome(
                trace.provider_attempt_count,
                trace.local_repair_applied,
                pending_recovery_records.iter().any(|record| record.recovery_kind == "second_pass"),
                false,
                true,
            ),
            &usage_calls,
        ),
        error_message: "graph extraction retry loop ended without a terminal outcome".to_string(),
        provider_failure: None,
        recovery_attempts: finalize_recovery_attempt_records(
            &pending_recovery_records,
            &extraction_recovery.classify_outcome(
                trace.provider_attempt_count,
                trace.local_repair_applied,
                pending_recovery_records.iter().any(|record| record.recovery_kind == "second_pass"),
                false,
                true,
            ),
        ),
    })
}

async fn request_graph_extraction_with_prompt_plan(
    gateway: &dyn LlmGateway,
    _provider_profile: &EffectiveProviderProfile,
    runtime_binding: &ResolvedRuntimeBinding,
    prompt_plan: &GraphExtractionPromptPlan,
    lifecycle: GraphExtractionLifecycle,
) -> Result<RawGraphExtractionResponse> {
    let prompt_hash = sha256_hex(&prompt_plan.prompt);
    let provider_kind = runtime_binding.provider_kind.clone();
    let model_name = runtime_binding.model_name.clone();
    let started_at = Utc::now();
    let started = Instant::now();
    let response = gateway
        .generate(ChatRequest {
            provider_kind: runtime_binding.provider_kind.clone(),
            model_name: runtime_binding.model_name.clone(),
            prompt: prompt_plan.prompt.clone(),
            api_key_override: Some(runtime_binding.api_key.clone()),
            base_url_override: runtime_binding.provider_base_url.clone(),
            system_prompt: runtime_binding.system_prompt.clone(),
            temperature: runtime_binding.temperature,
            top_p: runtime_binding.top_p,
            max_output_tokens_override: runtime_binding.max_output_tokens_override,
            extra_parameters_json: runtime_binding.extra_parameters_json.clone(),
        })
        .await
        .context("graph extraction provider call failed")?;
    let finished_at = Utc::now();
    let output_text = response.output_text;
    let usage_json = build_provider_usage_json(&provider_kind, &model_name, response.usage_json);

    Ok(RawGraphExtractionResponse {
        provider_kind,
        model_name,
        prompt_hash,
        request_shape_key: prompt_plan.request_shape_key.clone(),
        request_size_bytes: prompt_plan.request_size_bytes,
        output_text: output_text.clone(),
        usage_json: usage_json.clone(),
        lifecycle,
        timing: build_graph_extraction_call_timing(
            started_at,
            finished_at,
            started.elapsed(),
            &prompt_plan.prompt,
            &output_text,
            &usage_json,
        ),
    })
}

fn build_raw_output_json(
    output_text: &str,
    usage_json: serde_json::Value,
    lifecycle: &GraphExtractionLifecycle,
    recovery: &GraphExtractionRecoveryTrace,
    recovery_summary: &ExtractionRecoverySummary,
    usage_calls: &[GraphExtractionUsageCall],
) -> serde_json::Value {
    serde_json::json!({
        "output_text": output_text,
        "usage": usage_json,
        "provider_calls": usage_calls,
        "lifecycle": lifecycle,
        "recovery": recovery,
        "recovery_summary": recovery_summary,
    })
}

fn build_graph_extraction_call_timing(
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
    elapsed: std::time::Duration,
    prompt: &str,
    output_text: &str,
    usage_json: &serde_json::Value,
) -> GraphExtractionCallTiming {
    let elapsed_ms = i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX);
    let input_char_count = i32::try_from(prompt.chars().count()).unwrap_or(i32::MAX);
    let output_char_count = i32::try_from(output_text.chars().count()).unwrap_or(i32::MAX);
    let total_tokens =
        usage_json.get("total_tokens").and_then(serde_json::Value::as_i64).or_else(|| {
            let prompt_tokens =
                usage_json.get("prompt_tokens").and_then(serde_json::Value::as_i64)?;
            let completion_tokens = usage_json
                .get("completion_tokens")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            Some(prompt_tokens.saturating_add(completion_tokens))
        });
    let seconds = (elapsed_ms > 0).then_some(elapsed_ms as f64 / 1000.0);

    GraphExtractionCallTiming {
        started_at,
        finished_at,
        elapsed_ms,
        input_char_count,
        output_char_count,
        chars_per_second: seconds.and_then(|value| {
            (value > 0.0)
                .then_some(f64::from(input_char_count.saturating_add(output_char_count)) / value)
        }),
        tokens_per_second: seconds.and_then(|value| {
            total_tokens.filter(|tokens| *tokens > 0).map(|tokens| tokens as f64 / value)
        }),
    }
}

fn build_provider_usage_json(
    provider_kind: &str,
    model_name: &str,
    usage_json: serde_json::Value,
) -> serde_json::Value {
    let mut payload = usage_json;
    match payload.as_object_mut() {
        Some(object) => {
            object
                .entry("provider_kind".to_string())
                .or_insert_with(|| serde_json::Value::String(provider_kind.to_string()));
            object
                .entry("model_name".to_string())
                .or_insert_with(|| serde_json::Value::String(model_name.to_string()));
            payload
        }
        None => serde_json::json!({
            "provider_kind": provider_kind,
            "model_name": model_name,
            "value": payload,
        }),
    }
}

fn aggregate_provider_usage_json(
    provider_kind: &str,
    model_name: &str,
    usage_samples: &[serde_json::Value],
) -> serde_json::Value {
    let prompt_tokens = usage_samples
        .iter()
        .filter_map(|value| value.get("prompt_tokens").and_then(serde_json::Value::as_i64))
        .sum::<i64>();
    let completion_tokens = usage_samples
        .iter()
        .filter_map(|value| value.get("completion_tokens").and_then(serde_json::Value::as_i64))
        .sum::<i64>();
    let explicit_total_tokens = usage_samples
        .iter()
        .filter_map(|value| value.get("total_tokens").and_then(serde_json::Value::as_i64))
        .sum::<i64>();
    let saw_prompt_tokens = usage_samples
        .iter()
        .any(|value| value.get("prompt_tokens").and_then(serde_json::Value::as_i64).is_some());
    let saw_completion_tokens = usage_samples
        .iter()
        .any(|value| value.get("completion_tokens").and_then(serde_json::Value::as_i64).is_some());
    let saw_total_tokens = usage_samples
        .iter()
        .any(|value| value.get("total_tokens").and_then(serde_json::Value::as_i64).is_some());

    serde_json::json!({
        "aggregation": "sum",
        "provider_kind": provider_kind,
        "model_name": model_name,
        "call_count": usage_samples.len(),
        "prompt_tokens": saw_prompt_tokens.then_some(prompt_tokens),
        "completion_tokens": saw_completion_tokens.then_some(completion_tokens),
        "total_tokens": if saw_total_tokens {
            Some(explicit_total_tokens)
        } else if saw_prompt_tokens || saw_completion_tokens {
            Some(prompt_tokens.saturating_add(completion_tokens))
        } else {
            None
        },
    })
}

fn build_resolved_extraction_from_candidate(
    candidate: ParsedGraphExtractionCandidate,
    provider_kind: &str,
    model_name: &str,
    usage_samples: &[serde_json::Value],
    usage_calls: Vec<GraphExtractionUsageCall>,
    request_shape_key: String,
    request_size_bytes: usize,
    provider_failure: Option<RuntimeProviderFailureDetail>,
    recovery: GraphExtractionRecoveryTrace,
    recovery_summary: ExtractionRecoverySummary,
    recovery_attempts: Vec<GraphExtractionRecoveryRecord>,
) -> ResolvedGraphExtraction {
    ResolvedGraphExtraction {
        provider_kind: provider_kind.to_string(),
        model_name: model_name.to_string(),
        prompt_hash: candidate.raw.prompt_hash.clone(),
        output_text: candidate.raw.output_text.clone(),
        usage_json: aggregate_provider_usage_json(provider_kind, model_name, usage_samples),
        usage_calls,
        request_shape_key,
        request_size_bytes,
        provider_failure,
        normalized: candidate.normalized,
        lifecycle: candidate.raw.lifecycle,
        recovery,
        recovery_summary,
        recovery_attempts,
    }
}

fn select_better_partial_candidate(
    existing: Option<ParsedGraphExtractionCandidate>,
    candidate: ParsedGraphExtractionCandidate,
) -> Option<ParsedGraphExtractionCandidate> {
    match existing {
        Some(current)
            if graph_candidate_score(&current.normalized)
                >= graph_candidate_score(&candidate.normalized) =>
        {
            Some(current)
        }
        _ => Some(candidate),
    }
}

fn graph_candidate_score(candidate_set: &GraphExtractionCandidateSet) -> usize {
    candidate_set.entities.len().saturating_mul(2).saturating_add(candidate_set.relations.len())
}

fn finalize_recovery_attempt_records(
    pending_records: &[PendingRecoveryRecord],
    recovery_summary: &ExtractionRecoverySummary,
) -> Vec<GraphExtractionRecoveryRecord> {
    let status = match recovery_summary.status {
        crate::domains::graph_quality::ExtractionOutcomeStatus::Clean => "skipped",
        crate::domains::graph_quality::ExtractionOutcomeStatus::Recovered => "recovered",
        crate::domains::graph_quality::ExtractionOutcomeStatus::Partial => "partial",
        crate::domains::graph_quality::ExtractionOutcomeStatus::Failed => "failed",
    }
    .to_string();

    pending_records
        .iter()
        .map(|record| GraphExtractionRecoveryRecord {
            recovery_kind: record.recovery_kind.clone(),
            trigger_reason: record.trigger_reason.clone(),
            status: status.clone(),
            raw_issue_summary: record.raw_issue_summary.clone(),
            recovered_summary: record.recovered_summary.clone(),
        })
        .collect()
}

fn normalize_graph_extraction_output_with_repair(
    extraction_recovery: &ExtractionRecoveryService,
    output_text: &str,
) -> std::result::Result<NormalizedGraphExtractionAttempt, FailedNormalizationAttempt> {
    match parse_graph_extraction_output(output_text) {
        Ok(normalized) => Ok(NormalizedGraphExtractionAttempt {
            normalized,
            normalization_path: "direct",
            repair_candidate: None,
            parser_repair: ParserRepairClassification {
                should_attempt: false,
                trigger_reason: None,
                repair_candidate: None,
                issue_summary: None,
            },
        }),
        Err(primary_error) => {
            let parser_repair = extraction_recovery.classify_parser_repair(output_text, true);
            if let Some(candidate) = &parser_repair.repair_candidate {
                if let Ok(normalized) = parse_graph_extraction_output(candidate) {
                    return Ok(NormalizedGraphExtractionAttempt {
                        normalized,
                        normalization_path: "repaired",
                        repair_candidate: Some(candidate.clone()),
                        parser_repair,
                    });
                }
            }
            Err(FailedNormalizationAttempt {
                parse_error: primary_error.to_string(),
                parser_repair,
            })
        }
    }
}

pub fn parse_graph_extraction_output(output_text: &str) -> Result<GraphExtractionCandidateSet> {
    let parsed = extract_json_payload(output_text)?;
    let entities = parsed
        .get("entities")
        .and_then(serde_json::Value::as_array)
        .map(|items| items.iter().filter_map(parse_entity_candidate).collect::<Vec<_>>())
        .unwrap_or_default();
    let relations = parsed
        .get("relations")
        .and_then(serde_json::Value::as_array)
        .map(|items| items.iter().filter_map(parse_relation_candidate).collect::<Vec<_>>())
        .unwrap_or_default();

    Ok(GraphExtractionCandidateSet { entities, relations })
}

fn parse_entity_candidate(value: &serde_json::Value) -> Option<GraphEntityCandidate> {
    if let Some(label) = value.as_str().map(str::trim).filter(|value| !value.is_empty()) {
        return Some(GraphEntityCandidate {
            label: label.to_string(),
            node_type: RuntimeNodeType::Entity,
            aliases: Vec::new(),
            summary: None,
        });
    }

    let label = value.get("label").and_then(serde_json::Value::as_str)?.trim();
    if label.is_empty() {
        return None;
    }
    let node_type = match value
        .get("node_type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("entity")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "topic" => RuntimeNodeType::Topic,
        _ => RuntimeNodeType::Entity,
    };
    let aliases = value
        .get("aliases")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Some(GraphEntityCandidate {
        label: label.to_string(),
        node_type,
        aliases,
        summary: value
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(std::string::ToString::to_string),
    })
}

fn parse_relation_candidate(value: &serde_json::Value) -> Option<GraphRelationCandidate> {
    let source_label = value
        .get("source_label")
        .or_else(|| value.get("source"))
        .and_then(serde_json::Value::as_str)?
        .trim();
    let target_label = value
        .get("target_label")
        .or_else(|| value.get("target"))
        .and_then(serde_json::Value::as_str)?
        .trim();
    let relation_type = value
        .get("relation_type")
        .or_else(|| value.get("type"))
        .and_then(serde_json::Value::as_str)?
        .trim();
    if source_label.is_empty() || target_label.is_empty() || relation_type.is_empty() {
        return None;
    }
    let normalized_relation_type = normalize_relation_candidate_type(relation_type)?;
    if relation_type_is_semantically_void(&normalized_relation_type) {
        return None;
    }

    Some(GraphRelationCandidate {
        source_label: source_label.to_string(),
        target_label: target_label.to_string(),
        relation_type: normalized_relation_type,
        summary: value
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(std::string::ToString::to_string),
    })
}

fn normalize_relation_candidate_type(relation_type: &str) -> Option<String> {
    let normalized = relation_type
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|char| if char.is_ascii_alphanumeric() { char } else { '_' })
        .collect::<String>()
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    (!normalized.is_empty()).then_some(normalized)
}

fn relation_type_is_semantically_void(normalized_relation_type: &str) -> bool {
    matches!(
        normalized_relation_type,
        "na" | "n_a"
            | "none"
            | "null"
            | "unknown"
            | "unspecified"
            | "tbd"
            | "relation"
            | "relationship"
    )
}

fn extract_json_payload(output_text: &str) -> Result<serde_json::Value> {
    let trimmed = output_text.trim();
    if trimmed.is_empty() {
        return Ok(serde_json::json!({ "entities": [], "relations": [] }));
    }

    let unfenced = strip_markdown_fence(trimmed);
    let mut candidates = vec![unfenced.to_string()];
    if let Some(object_candidate) = extract_outer_json_object(unfenced) {
        if object_candidate != unfenced {
            candidates.push(object_candidate);
        }
    }

    let repaired_candidates = candidates
        .iter()
        .filter_map(|candidate| {
            let repaired = close_unbalanced_json_containers(candidate);
            (repaired != *candidate).then_some(repaired)
        })
        .collect::<Vec<_>>();
    candidates.extend(repaired_candidates);

    let mut parse_errors = Vec::new();
    for candidate in candidates {
        match serde_json::from_str::<serde_json::Value>(&candidate) {
            Ok(value) => return Ok(value),
            Err(json_error) => {
                parse_errors.push(format!("strict json: {json_error}"));
            }
        }
        match json5::from_str::<serde_json::Value>(&candidate) {
            Ok(value) => return Ok(value),
            Err(json5_error) => {
                parse_errors.push(format!("json5 fallback: {json5_error}"));
            }
        }
    }

    Err(anyhow!(
        "invalid graph extraction json: {}",
        if parse_errors.is_empty() {
            "unknown parse failure".to_string()
        } else {
            parse_errors.join(" | ")
        }
    ))
}

fn strip_markdown_fence(value: &str) -> &str {
    if !value.starts_with("```") {
        return value;
    }

    value
        .trim_start_matches("```json")
        .trim_start_matches("```JSON")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim()
}

fn extract_outer_json_object(value: &str) -> Option<String> {
    let start = value.find('{')?;
    let end = value.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(value[start..=end].trim().to_string())
}

fn close_unbalanced_json_containers(value: &str) -> String {
    let mut stack = Vec::new();
    let mut in_string = false;
    let mut escaped = false;

    for ch in value.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && in_string {
            escaped = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }

        match ch {
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                if stack.last().copied() == Some(ch) {
                    stack.pop();
                }
            }
            _ => {}
        }
    }

    let mut repaired = value.trim_end().to_string();
    while let Some(ch) = stack.pop() {
        repaired.push(ch);
    }
    repaired
}

fn sha256_hex(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use anyhow::Result;
    use async_trait::async_trait;

    use super::*;
    use crate::{
        domains::graph_quality::ExtractionOutcomeStatus,
        domains::provider_profiles::{ProviderModelSelection, SupportedProviderKind},
        integrations::llm::{
            ChatRequest, ChatResponse, EmbeddingBatchRequest, EmbeddingBatchResponse,
            EmbeddingRequest, EmbeddingResponse, VisionRequest, VisionResponse,
        },
    };

    struct FakeGateway {
        responses: Mutex<Vec<Result<ChatResponse>>>,
    }

    #[async_trait]
    impl LlmGateway for FakeGateway {
        async fn generate(&self, _request: ChatRequest) -> Result<ChatResponse> {
            self.responses.lock().expect("lock fake responses").remove(0)
        }

        async fn embed(&self, _request: EmbeddingRequest) -> Result<EmbeddingResponse> {
            unreachable!("embed is not used in graph extraction tests")
        }

        async fn embed_many(
            &self,
            _request: EmbeddingBatchRequest,
        ) -> Result<EmbeddingBatchResponse> {
            unreachable!("embed_many is not used in graph extraction tests")
        }

        async fn vision_extract(&self, _request: VisionRequest) -> Result<VisionResponse> {
            unreachable!("vision_extract is not used in graph extraction tests")
        }
    }

    fn sample_document() -> DocumentRow {
        DocumentRow {
            id: uuid::Uuid::nil(),
            project_id: uuid::Uuid::nil(),
            source_id: None,
            external_key: "spec.md".to_string(),
            title: Some("Spec".to_string()),
            mime_type: Some("text/markdown".to_string()),
            checksum: None,
            current_revision_id: None,
            active_status: "active".to_string(),
            active_mutation_kind: None,
            active_mutation_status: None,
            deleted_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn sample_chunk() -> ChunkRow {
        ChunkRow {
            id: uuid::Uuid::nil(),
            document_id: uuid::Uuid::nil(),
            project_id: uuid::Uuid::nil(),
            ordinal: 0,
            content: "OpenAI supplies embeddings for the annual report graph.".to_string(),
            token_count: None,
            metadata_json: serde_json::json!({}),
            created_at: chrono::Utc::now(),
        }
    }

    fn sample_profile() -> EffectiveProviderProfile {
        EffectiveProviderProfile {
            indexing: ProviderModelSelection {
                provider_kind: SupportedProviderKind::OpenAi,
                model_name: "gpt-5-mini".to_string(),
            },
            embedding: ProviderModelSelection {
                provider_kind: SupportedProviderKind::OpenAi,
                model_name: "text-embedding-3-small".to_string(),
            },
            answer: ProviderModelSelection {
                provider_kind: SupportedProviderKind::OpenAi,
                model_name: "gpt-5.4".to_string(),
            },
            vision: ProviderModelSelection {
                provider_kind: SupportedProviderKind::OpenAi,
                model_name: "gpt-5-mini".to_string(),
            },
        }
    }

    fn sample_runtime_binding() -> ResolvedRuntimeBinding {
        ResolvedRuntimeBinding {
            binding_id: uuid::Uuid::now_v7(),
            workspace_id: uuid::Uuid::nil(),
            library_id: uuid::Uuid::nil(),
            binding_purpose: AiBindingPurpose::ExtractGraph,
            provider_catalog_id: uuid::Uuid::now_v7(),
            provider_kind: "openai".to_string(),
            provider_base_url: None,
            provider_api_style: "openai".to_string(),
            credential_id: uuid::Uuid::now_v7(),
            api_key: "test-api-key".to_string(),
            model_catalog_id: uuid::Uuid::now_v7(),
            model_name: "gpt-5-mini".to_string(),
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens_override: None,
            extra_parameters_json: serde_json::json!({}),
        }
    }

    fn sample_request() -> GraphExtractionRequest {
        GraphExtractionRequest {
            project_id: uuid::Uuid::nil(),
            document: sample_document(),
            chunk: sample_chunk(),
            revision_id: None,
            activated_by_attempt_id: None,
            resume_hint: None,
        }
    }

    fn oversized_request() -> GraphExtractionRequest {
        let mut request = sample_request();
        request.chunk.content = "Alpha ".repeat(20_000);
        request
    }

    fn sample_resume_state_row() -> repositories::RuntimeGraphExtractionResumeStateRow {
        repositories::RuntimeGraphExtractionResumeStateRow {
            ingestion_run_id: uuid::Uuid::now_v7(),
            chunk_ordinal: 0,
            chunk_content_hash: "hash".to_string(),
            status: "ready".to_string(),
            last_attempt_no: 3,
            replay_count: 2,
            resume_hit_count: 1,
            downgrade_level: 1,
            provider_kind: Some("openai".to_string()),
            model_name: Some("gpt-5-mini".to_string()),
            prompt_hash: Some("prompt-hash".to_string()),
            request_shape_key: Some(
                "graph_extract_v3:initial:segments_2:downgrade_1:full".to_string(),
            ),
            request_size_bytes: Some(2048),
            provider_failure_class: None,
            provider_failure_json: None,
            recovery_summary_json: serde_json::json!({
                "status": "recovered",
                "parserRepairApplied": true,
                "secondPassApplied": false,
                "warning": "used persisted chunk result"
            }),
            raw_output_json: serde_json::json!({
                "lifecycle": {
                    "provider_attempt_count": 2,
                    "local_repair_applied": true,
                    "reask_count": 0
                }
            }),
            normalized_output_json: serde_json::json!({
                "entities": [
                    {
                        "label": "OpenAI",
                        "node_type": "entity",
                        "aliases": ["Open AI"],
                        "summary": "Provider"
                    }
                ],
                "relations": []
            }),
            last_successful_at: Some(chrono::Utc::now()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn prompt_mentions_json_contract_and_chunk_text() {
        let prompt = build_graph_extraction_prompt(&GraphExtractionRequest {
            project_id: uuid::Uuid::nil(),
            document: sample_document(),
            chunk: sample_chunk(),
            revision_id: None,
            activated_by_attempt_id: None,
            resume_hint: None,
        });

        assert!(prompt.contains("strict JSON"));
        assert!(prompt.contains("entities"));
        assert!(prompt.contains("annual report graph"));
    }

    #[test]
    fn resume_state_rebuilds_graph_extraction_outcome() {
        let outcome =
            extraction_outcome_from_resume_state(&sample_resume_state_row()).expect("resume state");

        assert!(outcome.resume_state.resumed_from_checkpoint);
        assert_eq!(outcome.resume_state.replay_count, 2);
        assert_eq!(outcome.resume_state.downgrade_level, 1);
        assert_eq!(outcome.normalized.entities.len(), 1);
        assert_eq!(outcome.recovery_summary.status, ExtractionOutcomeStatus::Recovered);
    }

    #[test]
    fn downgraded_prompt_plan_reduces_segment_count_and_marks_shape() {
        let mut request = oversized_request();
        request.resume_hint =
            Some(GraphExtractionResumeHint { replay_count: 4, downgrade_level: 1 });

        let plan = build_graph_extraction_prompt_plan(
            &request,
            GraphExtractionPromptVariant::Initial,
            None,
            None,
            None,
            256 * 1024,
        );

        assert!(plan.request_shape_key.contains("downgrade_1"));
        assert!(plan.request_size_bytes < 256 * 1024);
        assert!(plan.prompt.contains("Adaptive downgrade level: 1"));
    }

    #[test]
    fn normalizes_json_and_string_candidates() {
        let normalized = parse_graph_extraction_output(
            r#"{
              "entities": [
                "Annual report",
                { "label": "OpenAI", "node_type": "topic", "aliases": ["Open AI"], "summary": "provider" }
              ],
              "relations": [
                { "source": "Annual report", "target": "OpenAI", "type": "mentions" }
              ]
            }"#,
        )
        .expect("normalize graph extraction");

        assert_eq!(normalized.entities.len(), 2);
        assert_eq!(normalized.entities[0].label, "Annual report");
        assert_eq!(normalized.entities[1].node_type, RuntimeNodeType::Topic);
        assert_eq!(normalized.relations[0].relation_type, "mentions");
    }

    #[test]
    fn parses_json_inside_markdown_fence() {
        let normalized =
            parse_graph_extraction_output("```json\n{\"entities\":[],\"relations\":[]}\n```")
                .expect("normalize fenced graph extraction");

        assert!(normalized.entities.is_empty());
        assert!(normalized.relations.is_empty());
    }

    #[test]
    fn drops_empty_candidates_and_normalizes_relation_labels() {
        let normalized = parse_graph_extraction_output(
            r#"{
              "entities": [
                { "label": "  ", "node_type": "entity" },
                { "label": "DeepSeek", "aliases": ["", " Deep Seek "] }
              ],
              "relations": [
                { "source_label": "DeepSeek", "target_label": "Knowledge Graph", "relation_type": "Builds On" },
                { "source_label": " ", "target_label": "Ignored", "relation_type": "mentions" }
              ]
            }"#,
        )
        .expect("normalize graph extraction");

        assert_eq!(normalized.entities.len(), 1);
        assert_eq!(normalized.entities[0].label, "DeepSeek");
        assert_eq!(normalized.entities[0].aliases, vec!["Deep Seek".to_string()]);
        assert_eq!(normalized.relations.len(), 1);
        assert_eq!(normalized.relations[0].relation_type, "builds_on");
    }

    #[test]
    fn drops_semantically_void_relation_types_at_parse_time() {
        let normalized = parse_graph_extraction_output(
            r#"{
              "entities": [],
              "relations": [
                { "source_label": "Alpha", "target_label": "Beta", "relation_type": "unknown" },
                { "source_label": "Alpha", "target_label": "Beta", "relation_type": "supports" }
              ]
            }"#,
        )
        .expect("normalize graph extraction");

        assert_eq!(normalized.relations.len(), 1);
        assert_eq!(normalized.relations[0].relation_type, "supports");
    }

    #[test]
    fn extracts_json_object_from_surrounding_prose() {
        let normalized = parse_graph_extraction_output(
            "Here is the result:\n{\"entities\":[\"OpenAI\"],\"relations\":[]}\nThanks.",
        )
        .expect("normalize graph extraction with prose");

        assert_eq!(normalized.entities.len(), 1);
        assert_eq!(normalized.entities[0].label, "OpenAI");
    }

    #[test]
    fn accepts_json5_style_payloads() {
        let normalized = parse_graph_extraction_output(
            "{entities:[{label:'OpenAI', node_type:'entity', aliases:['Open AI'], summary:'provider',},], relations:[]}",
        )
        .expect("normalize json5 graph extraction");

        assert_eq!(normalized.entities.len(), 1);
        assert_eq!(normalized.entities[0].aliases, vec!["Open AI".to_string()]);
    }

    #[test]
    fn closes_missing_json_containers_at_end() {
        let normalized = parse_graph_extraction_output(
            r#"{"entities":[{"label":"OpenAI","node_type":"entity","aliases":[],"summary":"provider"}],"relations":[{"source_label":"OpenAI","target_label":"Graph","relation_type":"mentions","summary":"link"}"#,
        )
        .expect("normalize truncated graph extraction");

        assert_eq!(normalized.entities.len(), 1);
        assert_eq!(normalized.relations.len(), 1);
    }

    #[test]
    fn rejects_non_json_payloads() {
        let error = parse_graph_extraction_output("not valid json").expect_err("invalid json");

        assert!(error.to_string().contains("invalid graph extraction json"));
    }

    #[test]
    fn repairs_named_sections_without_outer_object() {
        let recovery_service = ExtractionRecoveryService;
        let normalized_attempt = normalize_graph_extraction_output_with_repair(
            &recovery_service,
            r#"
            entities:
            [{"label":"OpenAI","node_type":"entity","aliases":[],"summary":"provider"}]
            relations:
            [{"source_label":"OpenAI","target_label":"Annual report","relation_type":"mentions","summary":"citation"}]
            "#,
        )
        .expect("repair malformed extraction payload");

        assert_eq!(normalized_attempt.normalization_path, "repaired");
        assert!(normalized_attempt.repair_candidate.is_some());
        assert_eq!(normalized_attempt.normalized.entities.len(), 1);
        assert_eq!(normalized_attempt.normalized.relations.len(), 1);
    }

    #[tokio::test]
    async fn retries_after_terminal_parse_failure_and_aggregates_usage() {
        let gateway = FakeGateway {
            responses: Mutex::new(vec![
                Ok(ChatResponse {
                    provider_kind: "openai".to_string(),
                    model_name: "gpt-5-mini".to_string(),
                    output_text: "this is not json".to_string(),
                    usage_json: serde_json::json!({
                        "prompt_tokens": 11,
                        "completion_tokens": 4,
                        "total_tokens": 15,
                    }),
                }),
                Ok(ChatResponse {
                    provider_kind: "openai".to_string(),
                    model_name: "gpt-5-mini".to_string(),
                    output_text: r#"{"entities":["OpenAI"],"relations":[]}"#.to_string(),
                    usage_json: serde_json::json!({
                        "prompt_tokens": 7,
                        "completion_tokens": 3,
                        "total_tokens": 10,
                    }),
                }),
            ]),
        };

        let resolved = resolve_graph_extraction_with_gateway(
            &gateway,
            &ExtractionRecoveryService,
            &crate::services::provider_failure_classification::ProviderFailureClassificationService::default(),
            &sample_profile(),
            &sample_runtime_binding(),
            &sample_request(),
            true,
            2,
            1,
        )
        .await
        .expect("retry should recover");

        assert_eq!(resolved.recovery.provider_attempt_count, 2);
        assert_eq!(resolved.recovery.reask_count, 1);
        assert_eq!(
            resolved.usage_json.get("call_count").and_then(serde_json::Value::as_u64),
            Some(2)
        );
        assert_eq!(
            resolved.usage_json.get("total_tokens").and_then(serde_json::Value::as_i64),
            Some(25)
        );
        let raw_output_json = build_raw_output_json(
            &resolved.output_text,
            resolved.usage_json.clone(),
            &resolved.lifecycle,
            &resolved.recovery,
            &resolved.recovery_summary,
            &resolved.usage_calls,
        );
        let provider_calls = raw_output_json
            .get("provider_calls")
            .and_then(serde_json::Value::as_array)
            .expect("provider calls are persisted");
        assert_eq!(provider_calls.len(), 2);
        assert!(
            provider_calls[0]
                .get("timing")
                .and_then(|value| value.get("elapsed_ms"))
                .and_then(serde_json::Value::as_i64)
                .is_some()
        );
    }

    #[tokio::test]
    async fn retries_upstream_protocol_failures_as_transient_provider_errors() {
        let gateway = FakeGateway {
            responses: Mutex::new(vec![
                Err(anyhow::anyhow!(
                    "{}",
                    "provider request failed: provider=openai status=400 body={\"error\":{\"message\":\"We could not parse the JSON body of your request. The OpenAI API expects a JSON payload.\",\"type\":\"invalid_request_error\"}}"
                )),
                Ok(ChatResponse {
                    provider_kind: "openai".to_string(),
                    model_name: "gpt-5-mini".to_string(),
                    output_text: r#"{"entities":["OpenAI"],"relations":[]}"#.to_string(),
                    usage_json: serde_json::json!({
                        "prompt_tokens": 9,
                        "completion_tokens": 3,
                        "total_tokens": 12,
                    }),
                }),
            ]),
        };

        let resolved = resolve_graph_extraction_with_gateway(
            &gateway,
            &ExtractionRecoveryService,
            &crate::services::provider_failure_classification::ProviderFailureClassificationService::default(),
            &sample_profile(),
            &sample_runtime_binding(),
            &sample_request(),
            true,
            2,
            1,
        )
        .await
        .expect("upstream protocol failure should retry");

        assert_eq!(resolved.recovery.provider_attempt_count, 2);
        assert_eq!(
            resolved.provider_failure.as_ref().map(|detail| detail.failure_class.clone()),
            Some(RuntimeProviderFailureClass::RecoveredAfterRetry)
        );
        assert_eq!(
            resolved.recovery_attempts.first().map(|attempt| attempt.trigger_reason.as_str()),
            Some("upstream_protocol_failure")
        );
    }

    #[tokio::test]
    async fn retries_transient_upstream_rejections_as_provider_errors() {
        let gateway = FakeGateway {
            responses: Mutex::new(vec![
                Err(anyhow::anyhow!(
                    "{}",
                    "provider request failed: provider=openai status=520 body={\"raw_body\":\"error code: 520\"}"
                )),
                Ok(ChatResponse {
                    provider_kind: "openai".to_string(),
                    model_name: "gpt-5-mini".to_string(),
                    output_text: r#"{"entities":["OpenAI"],"relations":[]}"#.to_string(),
                    usage_json: serde_json::json!({
                        "prompt_tokens": 11,
                        "completion_tokens": 4,
                        "total_tokens": 15,
                    }),
                }),
            ]),
        };

        let resolved = resolve_graph_extraction_with_gateway(
            &gateway,
            &ExtractionRecoveryService,
            &crate::services::provider_failure_classification::ProviderFailureClassificationService::default(),
            &sample_profile(),
            &sample_runtime_binding(),
            &sample_request(),
            true,
            2,
            1,
        )
        .await
        .expect("transient upstream rejection should retry");

        assert_eq!(resolved.recovery.provider_attempt_count, 2);
        assert_eq!(
            resolved.provider_failure.as_ref().map(|detail| detail.failure_class.clone()),
            Some(RuntimeProviderFailureClass::RecoveredAfterRetry)
        );
        assert_eq!(
            resolved.recovery_attempts.first().map(|attempt| attempt.trigger_reason.as_str()),
            Some("upstream_transient_rejection")
        );
    }

    #[test]
    fn summarizes_provider_call_telemetry_for_checkpointing() {
        let started_at = Utc::now();
        let first = GraphExtractionUsageCall {
            provider_call_no: 1,
            provider_attempt_no: 1,
            prompt_hash: "hash-1".to_string(),
            request_shape_key: "graph_extract_v3:initial:segments_1:full".to_string(),
            request_size_bytes: 120,
            usage_json: serde_json::json!({ "total_tokens": 24 }),
            timing: GraphExtractionCallTiming {
                started_at,
                finished_at: started_at + chrono::Duration::milliseconds(500),
                elapsed_ms: 500,
                input_char_count: 120,
                output_char_count: 48,
                chars_per_second: Some(336.0),
                tokens_per_second: Some(48.0),
            },
        };
        let second = GraphExtractionUsageCall {
            provider_call_no: 2,
            provider_attempt_no: 2,
            prompt_hash: "hash-2".to_string(),
            request_shape_key: "graph_extract_v3:provider_retry:segments_1:full".to_string(),
            request_size_bytes: 132,
            usage_json: serde_json::json!({ "total_tokens": 18 }),
            timing: GraphExtractionCallTiming {
                started_at: started_at + chrono::Duration::milliseconds(600),
                finished_at: started_at + chrono::Duration::milliseconds(1200),
                elapsed_ms: 600,
                input_char_count: 80,
                output_char_count: 32,
                chars_per_second: Some(186.6),
                tokens_per_second: Some(30.0),
            },
        };

        let summary = summarize_graph_extraction_usage_calls(&[first, second]);

        assert_eq!(summary.provider_call_count, 2);
        assert_eq!(summary.total_call_elapsed_ms, 1_100);
        assert_eq!(summary.avg_call_elapsed_ms, Some(550));
        assert!(summary.avg_chars_per_second.is_some());
        assert!(summary.avg_tokens_per_second.is_some());
        assert_eq!(
            summary.last_provider_call_at,
            Some(started_at + chrono::Duration::milliseconds(1200))
        );
    }

    #[test]
    fn prompt_preview_is_deterministic_for_large_chunks() {
        let request = oversized_request();
        let (first_prompt, first_shape, first_size) =
            build_graph_extraction_prompt_preview(&request, 8 * 1024);
        let (second_prompt, second_shape, second_size) =
            build_graph_extraction_prompt_preview(&request, 8 * 1024);

        assert_eq!(first_prompt, second_prompt);
        assert_eq!(first_shape, second_shape);
        assert_eq!(first_size, second_size);
        assert!(first_prompt.contains("[chunk_segment_1]"));
        assert!(first_shape.contains("segments_3"));
        assert!(first_size <= 8 * 1024 + GRAPH_EXTRACTION_REQUEST_OVERHEAD_BYTES);
    }

    #[tokio::test]
    async fn fails_after_retry_exhaustion_with_recovery_trace() {
        let gateway = FakeGateway {
            responses: Mutex::new(vec![
                Ok(ChatResponse {
                    provider_kind: "openai".to_string(),
                    model_name: "gpt-5-mini".to_string(),
                    output_text: "broken payload".to_string(),
                    usage_json: serde_json::json!({ "prompt_tokens": 5 }),
                }),
                Ok(ChatResponse {
                    provider_kind: "openai".to_string(),
                    model_name: "gpt-5-mini".to_string(),
                    output_text: "still broken".to_string(),
                    usage_json: serde_json::json!({ "prompt_tokens": 6 }),
                }),
            ]),
        };

        let failure = resolve_graph_extraction_with_gateway(
            &gateway,
            &ExtractionRecoveryService,
            &crate::services::provider_failure_classification::ProviderFailureClassificationService::default(),
            &sample_profile(),
            &sample_runtime_binding(),
            &sample_request(),
            true,
            2,
            1,
        )
        .await
        .expect_err("malformed output should fail after retry exhaustion");

        assert!(failure.error_message.contains("after 2 provider attempt(s)"));
        assert_eq!(failure.provider_attempt_count, 2);
        assert_eq!(
            failure
                .raw_output_json
                .get("recovery")
                .and_then(|value| value.get("attempts"))
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(2)
        );
        assert_eq!(
            failure
                .raw_output_json
                .get("provider_calls")
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(2)
        );
        assert_eq!(
            failure.provider_failure.as_ref().map(|detail| detail.failure_class.clone()),
            Some(RuntimeProviderFailureClass::InvalidModelOutput)
        );
    }

    #[test]
    fn provider_usage_payload_keeps_provider_metadata() {
        let usage = build_provider_usage_json(
            "openai",
            "gpt-5-mini",
            serde_json::json!({
                "prompt_tokens": 21,
                "completion_tokens": 9,
            }),
        );

        assert_eq!(usage.get("provider_kind").and_then(serde_json::Value::as_str), Some("openai"));
        assert_eq!(usage.get("model_name").and_then(serde_json::Value::as_str), Some("gpt-5-mini"));
        assert_eq!(usage.get("prompt_tokens").and_then(serde_json::Value::as_i64), Some(21));
    }
}
