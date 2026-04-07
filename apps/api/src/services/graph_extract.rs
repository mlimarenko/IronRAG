use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::time::Instant;
use uuid::Uuid;

use crate::{
    agent_runtime::{
        builder::StructuredRequestBuilder,
        executor::{RuntimeExecutionError, RuntimeExecutionSession},
        persistence as runtime_persistence,
        request::build_provider_request,
        response::{RuntimeFailureSummary, RuntimeRecoveryOutcome, RuntimeTerminalOutcome},
        task::RuntimeTask,
        tasks::graph_extract::{GraphExtractTask, GraphExtractTaskInput},
    },
    app::state::AppState,
    domains::{
        agent_runtime::{RuntimeExecutionOwner, RuntimeStageKind, RuntimeStageState},
        ai::AiBindingPurpose,
        graph_quality::{ExtractionOutcomeStatus, ExtractionRecoverySummary},
        provider_profiles::EffectiveProviderProfile,
        runtime_graph::RuntimeNodeType,
        runtime_ingestion::{RuntimeProviderFailureClass, RuntimeProviderFailureDetail},
    },
    infra::repositories::{self, ChunkRow, DocumentRow, RuntimeGraphExtractionRecordRow},
    integrations::llm::{ChatRequestSeed, LlmGateway},
    services::{
        ai_catalog_service::ResolvedRuntimeBinding, extraction_recovery::ExtractionRecoveryService,
        graph_identity, runtime_ingestion::RuntimeTaskExecutionContext,
    },
    shared::technical_facts::TechnicalFactQualifier,
};

const GRAPH_EXTRACTION_VERSION: &str = "graph_extract_v6";
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
    pub library_id: uuid::Uuid,
    pub document: DocumentRow,
    pub chunk: ChunkRow,
    pub structured_chunk: GraphExtractionStructuredChunkContext,
    pub technical_facts: Vec<GraphExtractionTechnicalFact>,
    pub revision_id: Option<uuid::Uuid>,
    pub activated_by_attempt_id: Option<uuid::Uuid>,
    pub resume_hint: Option<GraphExtractionResumeHint>,
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
    recovery_kind: Option<String>,
    trigger_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct GraphExtractionRecoveryTrace {
    provider_attempt_count: usize,
    reask_count: usize,
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
    provider_failure: Option<RuntimeProviderFailureDetail>,
    normalized: GraphExtractionCandidateSet,
    lifecycle: GraphExtractionLifecycle,
    recovery: GraphExtractionRecoveryTrace,
    recovery_summary: ExtractionRecoverySummary,
    recovery_attempts: Vec<GraphExtractionRecoveryRecord>,
}

#[derive(Debug, Clone)]
struct GraphExtractionFailureOutcome {
    request_shape_key: String,
    request_size_bytes: usize,
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
    _request: &GraphExtractionRequest,
    error_message: impl Into<String>,
) -> GraphExtractionFailureOutcome {
    GraphExtractionFailureOutcome {
        request_shape_key: "graph_extract_v6:unconfigured".to_string(),
        request_size_bytes: 0,
        error_message: error_message.into(),
        provider_failure: None,
        recovery_summary: ExtractionRecoverySummary {
            status: ExtractionOutcomeStatus::Failed,
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
}

#[derive(Debug, Clone)]
struct FailedNormalizationAttempt {
    parse_error: String,
}

#[derive(Debug, Clone)]
struct ParsedGraphExtractionCandidate {
    raw: RawGraphExtractionResponse,
    normalized: GraphExtractionCandidateSet,
    normalization_path: &'static str,
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

#[cfg(test)]
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
    let mut sections: Vec<(String, String)> = Vec::new();
    sections.push((
        "task".to_string(),
        "You are a knowledge graph extraction expert. Your job is to extract structured entities and relationships from a document chunk to build a rich, queryable knowledge graph.\n\n\
Extract ALL meaningful entities: named things (people, organizations, artifacts, natural phenomena), typed concepts (algorithms, patterns, paradigms), processes (methods, workflows), and measurable attributes (metrics, parameters, configuration values) that appear in the text.\n\n\
For each entity, determine the single best type from the entity type reference below.\n\n\
Extract ALL relationships between entities. Use the most specific relation type from the catalog. NEVER use \"mentions\" when a more specific type exists. For example:\n\
- \"X uses Y\" → uses (not mentions)\n\
- \"X depends on Y\" → depends_on (not mentions)\n\
- \"X is built with Y\" → uses or builds_on (not mentions)\n\
- \"X authenticates via Y\" → authenticates (not mentions)\n\
- \"X returns Y\" → returns (not mentions)\n\
- \"X implements Y\" → implements (not mentions)\n\
- \"X contains Y\" → contains (not mentions)\n\
- \"X is a type of Y\" → is_a (not mentions)\n\
Only use \"mentions\" for truly tangential references where the text names something without describing any functional relationship.\n\n\
Resolve coreferences: when the text says \"it\", \"this system\", \"the API\", \"the framework\", or uses abbreviations, resolve them to the full canonical entity name. Do not extract pronouns or abbreviations as separate entities.".to_string(),
    ));
    sections.push((
        "entity_types".to_string(),
        "Entity type reference (choose the single best type for each entity):\n\
- person: A named individual human being (Linus Torvalds, Marie Curie, Warren Buffett, Hippocrates)\n\
- organization: A company, institution, government body, team, or standards body (Google, WHO, SEC, IETF, Supreme Court, Red Cross)\n\
- location: A named geographic place, region, facility, or site (Silicon Valley, Wall Street, Chernobyl, Amazon rainforest)\n\
- event: A named occurrence, incident, milestone, or time-bounded happening (COVID-19 pandemic, Log4Shell, 2008 financial crisis, Roe v. Wade, Apollo 11)\n\
- artifact: Anything created, built, or designed by humans — software, tools, products, drugs, devices, standards, protocols, laws, licenses, code functions, APIs, documents (PostgreSQL, Aspirin, TCP/IP, GDPR, MIT License, build_router(), GET /api/users, Basel III, React, insulin pump)\n\
- natural: Anything existing in nature without human creation — species, organisms, diseases, genes, proteins, elements, minerals, natural phenomena (SARS-CoV-2, BRCA1 gene, malaria, silicon, photosynthesis, earthquake, DNA)\n\
- process: A named procedure, method, algorithm, workflow, or repeatable sequence of steps (Agile methodology, PCR testing, IPO process, judicial review, gradient descent, fermentation)\n\
- concept: An abstract idea, theory, principle, pattern, paradigm, theme, or field of study (dependency injection, herd immunity, supply and demand, due process, machine learning, oncology, relativity)\n\
- attribute: A named measurable property, metric, indicator, parameter, status, threshold, or configuration value (p99 latency, blood pressure, GDP, APP_PORT, credit score, HTTP 200, melting point, pH level)\n\
- entity: Catch-all for named things that do not fit any other type. Always prefer a more specific type above.".to_string(),
    ));
    sections.push((
        "examples".to_string(),
        "Example 1 - API documentation chunk:\n\
Input: \"FastAPI uses Pydantic for data validation. When you declare a parameter with a type annotation, FastAPI automatically validates the input and returns a 422 status code if validation fails.\"\n\
Output: {\"entities\":[{\"label\":\"FastAPI\",\"node_type\":\"artifact\",\"aliases\":[],\"summary\":\"Python web framework with automatic data validation\"},{\"label\":\"Pydantic\",\"node_type\":\"artifact\",\"aliases\":[],\"summary\":\"Data validation library used by FastAPI\"},{\"label\":\"422\",\"node_type\":\"attribute\",\"aliases\":[\"422 Unprocessable Entity\"],\"summary\":\"HTTP status code returned when validation fails\"}],\"relations\":[{\"source_label\":\"FastAPI\",\"target_label\":\"Pydantic\",\"relation_type\":\"uses\",\"summary\":\"FastAPI uses Pydantic for data validation\"},{\"source_label\":\"FastAPI\",\"target_label\":\"422\",\"relation_type\":\"returns\",\"summary\":\"Returns 422 when input validation fails\"}]}\n\n\
Example 2 - Infrastructure chunk:\n\
Input: \"The auth-service runs on port 8001 and depends on PostgreSQL for session storage. It authenticates users via JWT tokens signed with RS256.\"\n\
Output: {\"entities\":[{\"label\":\"auth-service\",\"node_type\":\"artifact\",\"aliases\":[],\"summary\":\"Authentication service on port 8001\"},{\"label\":\"PostgreSQL\",\"node_type\":\"artifact\",\"aliases\":[\"Postgres\"],\"summary\":\"Database used for session storage\"},{\"label\":\"JWT\",\"node_type\":\"artifact\",\"aliases\":[\"JSON Web Token\"],\"summary\":\"Token format for authentication\"},{\"label\":\"RS256\",\"node_type\":\"artifact\",\"aliases\":[],\"summary\":\"RSA-SHA256 signing algorithm\"}],\"relations\":[{\"source_label\":\"auth-service\",\"target_label\":\"PostgreSQL\",\"relation_type\":\"depends_on\",\"summary\":\"Uses PostgreSQL for session storage\"},{\"source_label\":\"auth-service\",\"target_label\":\"JWT\",\"relation_type\":\"authenticates\",\"summary\":\"Authenticates users via JWT tokens\"},{\"source_label\":\"JWT\",\"target_label\":\"RS256\",\"relation_type\":\"uses\",\"summary\":\"Tokens signed with RS256 algorithm\"}]}".to_string(),
    ));
    sections.push((
        "schema".to_string(),
        format!(
            "Return strict JSON with keys `entities` and `relations`. Each entity must include `label`, `node_type` (one of: `person`, `organization`, `location`, `event`, `artifact`, `natural`, `process`, `concept`, `attribute`, `entity`), `aliases`, and `summary`. Each relation must include `source_label`, `target_label`, `relation_type`, and `summary`. `relation_type` must be copied verbatim from this catalog: {}. Use lowercase ASCII snake_case only. Never translate, localize, paraphrase, or invent a new relation_type. If no concise summary is available, emit an empty string. If none fit exactly, omit the relation.",
            graph_identity::canonical_relation_type_catalog().join(", ")
        ),
    ));
    sections.push((
        "rules".to_string(),
        "Do not include markdown fences or prose. If no grounded graph evidence exists, return {\"entities\":[],\"relations\":[]}.\n\
Critical rules:\n\
1. ALWAYS provide a non-empty summary for every entity and relation.\n\
2. NEVER use 'mentions' when any specific relation type fits. Audit each relation: could it be uses, depends_on, contains, implements, returns, configures, extends, calls, or another specific type?\n\
3. Extract the entity's PRIMARY role or purpose in the summary, not just its name.\n\
4. When the text describes a capability, feature, or behavior, model it as a relation (enables, provides, supports) not just a mention.".to_string(),
    ));
    sections.push((
        "document".to_string(),
        format!("Document: {document_label}\nChunk ordinal: {}", request.chunk.ordinal),
    ));
    {
        let section_path_text = if request.structured_chunk.section_path.is_empty() {
            String::new()
        } else {
            format!("\nSection: {}", request.structured_chunk.section_path.join(" > "))
        };
        sections.push((
            "domain_context".to_string(),
            format!("Document domain: {document_label}{section_path_text}"),
        ));
    }
    sections.push((
        "structured_chunk".to_string(),
        render_structured_chunk_context(&request.structured_chunk),
    ));
    if let Some(technical_fact_section) =
        render_graph_extraction_technical_facts(&request.technical_facts, safe_limit / 5)
    {
        sections.push(("technical_facts".to_string(), technical_fact_section));
    }

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

    let reserved_bytes = sections
        .iter()
        .map(|(title, body)| title.len().saturating_add(body.len()).saturating_add(8))
        .sum::<usize>();
    let chunk_text_budget =
        safe_limit.saturating_sub(reserved_bytes).max(GRAPH_EXTRACTION_REQUEST_OVERHEAD_BYTES / 4);
    let chunk_segments = segment_chunk_text_for_prompt(
        &request.chunk.content,
        chunk_text_budget,
        downgraded_max_segments(downgrade_level),
    );
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
        "graph_extract_v6:{}:segments_{}:downgrade_{}:{}",
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
        return vec!["Prepared chunk text:".to_string()];
    }

    if content.len() <= max_total_bytes {
        return vec![format!("Prepared chunk text:\n{content}")];
    }

    let segment_count = max_segments.max(1);
    let segment_budget = (max_total_bytes / segment_count).max(256);
    let chars = content.chars().collect::<Vec<_>>();
    let total_chars = chars.len();
    let approx_chars_per_segment = segment_budget / 4;
    let edge_chars = approx_chars_per_segment.min(total_chars);
    let head = chars[..edge_chars].iter().collect::<String>();
    if segment_count == 1 {
        return vec![format!("Prepared chunk text segment 1/1:\n{head}")];
    }

    if segment_count == 2 {
        let tail = chars[total_chars.saturating_sub(edge_chars)..].iter().collect::<String>();
        return vec![
            "Prepared chunk text segment 1/2:\n".to_string() + &head,
            "Prepared chunk text segment 2/2:\n".to_string() + &tail,
        ];
    }

    let middle_start = total_chars.saturating_sub(approx_chars_per_segment) / 2;
    let middle_end = (middle_start + approx_chars_per_segment).min(total_chars);
    let middle = chars[middle_start..middle_end].iter().collect::<String>();
    let tail = chars[total_chars.saturating_sub(edge_chars)..].iter().collect::<String>();

    vec![
        format!("Prepared chunk text segment 1/{segment_count}:\n{head}"),
        format!("Prepared chunk text segment 2/{segment_count}:\n{middle}"),
        format!("Prepared chunk text segment 3/{segment_count}:\n{tail}"),
    ]
}

fn render_structured_chunk_context(context: &GraphExtractionStructuredChunkContext) -> String {
    let mut lines = Vec::new();
    lines.push(format!("Chunk kind: {}", context.chunk_kind.as_deref().unwrap_or("unknown")));
    if !context.section_path.is_empty() {
        lines.push(format!("Section path: {}", context.section_path.join(" > ")));
    }
    if !context.heading_trail.is_empty() {
        lines.push(format!("Heading trail: {}", context.heading_trail.join(" > ")));
    }
    if !context.support_block_ids.is_empty() {
        lines.push(format!("Support block count: {}", context.support_block_ids.len()));
    }
    if let Some(literal_digest) = &context.literal_digest {
        lines.push(format!("Literal digest: {literal_digest}"));
    }
    lines.join("\n")
}

fn render_graph_extraction_technical_facts(
    facts: &[GraphExtractionTechnicalFact],
    max_bytes: usize,
) -> Option<String> {
    if facts.is_empty() {
        return None;
    }

    let mut rendered = String::new();
    for fact in facts {
        let qualifiers = if fact.qualifiers.is_empty() {
            String::new()
        } else {
            format!(
                " | qualifiers: {}",
                fact.qualifiers
                    .iter()
                    .map(|qualifier| format!("{}={}", qualifier.key, qualifier.value))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let line = format!(
            "- {}: {} | display: {}{}",
            fact.fact_kind, fact.canonical_value, fact.display_value, qualifiers
        );
        let next_len = rendered.len().saturating_add(line.len()).saturating_add(1);
        if !rendered.is_empty() && next_len > max_bytes.max(256) {
            break;
        }
        if !rendered.is_empty() {
            rendered.push('\n');
        }
        rendered.push_str(&line);
    }

    (!rendered.is_empty()).then_some(rendered)
}

#[cfg(test)]
fn graph_extraction_response_format(provider_kind: &str) -> serde_json::Value {
    if provider_kind == "deepseek" {
        return serde_json::json!({
            "type": "json_object"
        });
    }

    serde_json::json!({
        "type": "json_schema",
        "json_schema": {
            "name": "graph_extraction",
            "strict": true,
            "schema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "entities": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "label": { "type": "string" },
                                "node_type": {
                                    "type": "string",
                                    "enum": ["person", "organization", "location", "event", "artifact", "natural_kind", "process", "concept", "topic", "metric", "regulation", "code_symbol", "entity"]
                                },
                                "aliases": {
                                    "type": "array",
                                    "items": { "type": "string" }
                                },
                                "summary": { "type": "string" }
                            },
                            "required": ["label", "node_type", "aliases", "summary"]
                        }
                    },
                    "relations": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "source_label": { "type": "string" },
                                "target_label": { "type": "string" },
                                "relation_type": {
                                    "type": "string",
                                    "enum": graph_identity::canonical_relation_type_catalog()
                                },
                                "summary": { "type": "string" }
                            },
                            "required": ["source_label", "target_label", "relation_type", "summary"]
                        }
                    }
                },
                "required": ["entities", "relations"]
            }
        }
    })
}

pub async fn extract_chunk_graph_candidates(
    state: &AppState,
    runtime_context: &RuntimeTaskExecutionContext,
    request: &GraphExtractionRequest,
) -> std::result::Result<GraphExtractionOutcome, GraphExtractionExecutionError> {
    let extraction_record_id = uuid::Uuid::now_v7();
    let mut runtime_session =
        seed_graph_extract_runtime_session(state, extraction_record_id, request, runtime_context)
            .await
            .map_err(|error| map_graph_runtime_execution_error(request, None, error))?;

    let initial_selection = runtime_context
        .provider_profile
        .selection_for_binding_purpose(AiBindingPurpose::ExtractGraph);
    repositories::create_runtime_graph_extraction_record(
        &state.persistence.postgres,
        &repositories::CreateRuntimeGraphExtractionRecordInput {
            id: extraction_record_id,
            runtime_execution_id: runtime_session.execution.id,
            library_id: request.library_id,
            document_id: request.document.id,
            chunk_id: request.chunk.id,
            provider_kind: initial_selection.provider_kind.as_str().to_string(),
            model_name: initial_selection.model_name.clone(),
            extraction_version: GRAPH_EXTRACTION_VERSION.to_string(),
            prompt_hash: "pending".to_string(),
            status: "processing".to_string(),
            raw_output_json: serde_json::json!({}),
            normalized_output_json: serde_json::json!({ "entities": [], "relations": [] }),
            glean_pass_count: 0,
            error_message: None,
        },
    )
    .await
    .map_err(|error| {
        graph_extraction_execution_error(
            request,
            format!("failed to create graph extraction owner record: {error:#}"),
            None,
            ExtractionRecoverySummary {
                status: ExtractionOutcomeStatus::Failed,
                second_pass_applied: false,
                warning: None,
            },
            Vec::new(),
        )
    })?;

    let execution_result = run_graph_extraction_runtime(
        state,
        &runtime_context.provider_profile,
        request,
        extraction_record_id,
        &mut runtime_session,
    )
    .await;

    match execution_result {
        Ok((runtime_outcome, extraction_outcome)) => {
            let runtime_result = state
                .agent_runtime
                .executor()
                .finalize_session::<GraphExtractTask>(runtime_session, runtime_outcome)
                .await;
            runtime_persistence::persist_runtime_result(
                &state.persistence.postgres,
                &runtime_result.execution,
                &runtime_result.trace,
            )
            .await
            .map_err(|error| {
                graph_extraction_execution_error(
                    request,
                    format!("failed to persist graph extraction runtime trace: {error:#}"),
                    extraction_outcome.provider_failure.clone(),
                    extraction_outcome.recovery_summary.clone(),
                    extraction_outcome.recovery_attempts.clone(),
                )
            })?;
            repositories::update_runtime_graph_extraction_record(
                &state.persistence.postgres,
                extraction_record_id,
                &repositories::UpdateRuntimeGraphExtractionRecordInput {
                    provider_kind: extraction_outcome.provider_kind.clone(),
                    model_name: extraction_outcome.model_name.clone(),
                    prompt_hash: extraction_outcome.prompt_hash.clone(),
                    status: "ready".to_string(),
                    raw_output_json: extraction_outcome.raw_output_json.clone(),
                    normalized_output_json: serde_json::to_value(&extraction_outcome.normalized)
                        .unwrap_or_else(|_| serde_json::json!({})),
                    glean_pass_count: i32::try_from(extraction_outcome.usage_calls.len())
                        .unwrap_or(i32::MAX),
                    error_message: None,
                },
            )
            .await
            .map_err(|error| {
                graph_extraction_execution_error(
                    request,
                    format!("failed to update graph extraction owner record: {error:#}"),
                    extraction_outcome.provider_failure.clone(),
                    extraction_outcome.recovery_summary.clone(),
                    extraction_outcome.recovery_attempts.clone(),
                )
            })?
            .ok_or_else(|| {
                graph_extraction_execution_error(
                    request,
                    format!(
                        "graph extraction owner record {} was not found during update",
                        extraction_record_id
                    ),
                    extraction_outcome.provider_failure.clone(),
                    extraction_outcome.recovery_summary.clone(),
                    extraction_outcome.recovery_attempts.clone(),
                )
            })?;
            Ok(GraphExtractionOutcome {
                graph_extraction_id: Some(extraction_record_id),
                runtime_execution_id: Some(runtime_result.execution.id),
                ..extraction_outcome
            })
        }
        Err((runtime_outcome, error)) => {
            let runtime_result = state
                .agent_runtime
                .executor()
                .finalize_session::<GraphExtractTask>(runtime_session, runtime_outcome)
                .await;
            runtime_persistence::persist_runtime_result(
                &state.persistence.postgres,
                &runtime_result.execution,
                &runtime_result.trace,
            )
            .await
            .map_err(|persist_error| GraphExtractionExecutionError {
                message: format!(
                    "failed to persist graph extraction runtime trace: {persist_error:#}"
                ),
                request_shape_key: error.request_shape_key.clone(),
                request_size_bytes: error.request_size_bytes,
                provider_failure: error.provider_failure.clone(),
                recovery_summary: error.recovery_summary.clone(),
                recovery_attempts: error.recovery_attempts.clone(),
                resume_state: error.resume_state.clone(),
            })?;
            repositories::update_runtime_graph_extraction_record(
                &state.persistence.postgres,
                extraction_record_id,
                &repositories::UpdateRuntimeGraphExtractionRecordInput {
                    provider_kind: error
                        .provider_failure
                        .as_ref()
                        .and_then(|failure| failure.provider_kind.clone())
                        .unwrap_or_else(|| "unknown".to_string()),
                    model_name: error
                        .provider_failure
                        .as_ref()
                        .and_then(|failure| failure.model_name.clone())
                        .unwrap_or_else(|| "unknown".to_string()),
                    prompt_hash: "unknown".to_string(),
                    status: graph_async_operation_status(&runtime_result.outcome).to_string(),
                    raw_output_json: serde_json::json!({}),
                    normalized_output_json: serde_json::json!({ "entities": [], "relations": [] }),
                    glean_pass_count: i32::try_from(error.resume_state.replay_count)
                        .unwrap_or(i32::MAX),
                    error_message: Some(error.message.clone()),
                },
            )
            .await
            .map_err(|persist_error| GraphExtractionExecutionError {
                message: format!(
                    "failed to update graph extraction failure record: {persist_error:#}"
                ),
                request_shape_key: error.request_shape_key.clone(),
                request_size_bytes: error.request_size_bytes,
                provider_failure: error.provider_failure.clone(),
                recovery_summary: error.recovery_summary.clone(),
                recovery_attempts: error.recovery_attempts.clone(),
                resume_state: error.resume_state.clone(),
            })?
            .ok_or_else(|| GraphExtractionExecutionError {
                message: format!(
                    "graph extraction owner record {} was not found during failure update",
                    extraction_record_id
                ),
                request_shape_key: error.request_shape_key.clone(),
                request_size_bytes: error.request_size_bytes,
                provider_failure: error.provider_failure.clone(),
                recovery_summary: error.recovery_summary.clone(),
                recovery_attempts: error.recovery_attempts.clone(),
                resume_state: error.resume_state.clone(),
            })?;
            append_graph_runtime_policy_audit(
                state,
                request,
                extraction_record_id,
                &runtime_result,
            )
            .await;
            Err(error)
        }
    }
}

async fn seed_graph_extract_runtime_session(
    state: &AppState,
    graph_extraction_id: uuid::Uuid,
    request: &GraphExtractionRequest,
    runtime_context: &RuntimeTaskExecutionContext,
) -> std::result::Result<RuntimeExecutionSession, RuntimeExecutionError> {
    let runtime_request = StructuredRequestBuilder::<GraphExtractTask>::new(
        GraphExtractTaskInput {
            library_id: request.library_id,
            document_id: request.document.id,
            chunk_id: request.chunk.id,
            revision_id: request.revision_id,
            normalized_text: request.chunk.content.clone(),
            technical_facts: request.technical_facts.clone(),
        },
        RuntimeExecutionOwner::graph_extraction_attempt(graph_extraction_id),
    )
    .with_budget_limits(
        runtime_context.runtime_overrides.max_turns,
        runtime_context.runtime_overrides.max_parallel_actions,
    )
    .build();

    state
        .agent_runtime
        .seed_and_persist_session(&state.persistence.postgres, &runtime_request)
        .await
}

async fn run_graph_extraction_runtime(
    state: &AppState,
    provider_profile: &EffectiveProviderProfile,
    request: &GraphExtractionRequest,
    graph_extraction_id: uuid::Uuid,
    runtime_session: &mut RuntimeExecutionSession,
) -> std::result::Result<
    (
        RuntimeTerminalOutcome<GraphExtractionCandidateSet, GraphExtractionTaskFailure>,
        GraphExtractionOutcome,
    ),
    (
        RuntimeTerminalOutcome<GraphExtractionCandidateSet, GraphExtractionTaskFailure>,
        GraphExtractionExecutionError,
    ),
> {
    if let Err(failure) = begin_graph_runtime_stage(
        state.agent_runtime.executor(),
        runtime_session,
        RuntimeStageKind::ExtractGraph,
    )
    .await
    {
        record_graph_runtime_stage(
            state.agent_runtime.executor(),
            runtime_session,
            RuntimeStageKind::ExtractGraph,
            RuntimeStageState::Failed,
            false,
            Some(&failure),
        );
        let error = graph_extraction_execution_error(
            request,
            failure.summary.clone(),
            None,
            ExtractionRecoverySummary {
                status: ExtractionOutcomeStatus::Failed,
                second_pass_applied: false,
                warning: None,
            },
            Vec::new(),
        );
        return Err((make_graph_terminal_failure_outcome(failure.clone()), error));
    }

    match resolve_graph_extraction(state, provider_profile, request).await {
        Ok(resolved) => {
            record_graph_runtime_stage(
                state.agent_runtime.executor(),
                runtime_session,
                RuntimeStageKind::ExtractGraph,
                RuntimeStageState::Completed,
                false,
                None,
            );

            let runtime_execution_id = runtime_session.execution.id;
            let recovery_status = resolved.recovery_summary.status.clone();
            if matches!(
                recovery_status,
                ExtractionOutcomeStatus::Recovered | ExtractionOutcomeStatus::Partial
            ) {
                if let Err(failure) = begin_graph_runtime_stage(
                    state.agent_runtime.executor(),
                    runtime_session,
                    RuntimeStageKind::Recovery,
                )
                .await
                {
                    record_graph_runtime_stage(
                        state.agent_runtime.executor(),
                        runtime_session,
                        RuntimeStageKind::Recovery,
                        RuntimeStageState::Failed,
                        false,
                        Some(&failure),
                    );
                    let error = graph_extraction_execution_error(
                        request,
                        failure.summary.clone(),
                        resolved.provider_failure.clone(),
                        resolved.recovery_summary.clone(),
                        resolved.recovery_attempts.clone(),
                    );
                    return Err((make_graph_terminal_failure_outcome(failure.clone()), error));
                }
                record_graph_runtime_stage(
                    state.agent_runtime.executor(),
                    runtime_session,
                    RuntimeStageKind::Recovery,
                    RuntimeStageState::Recovered,
                    false,
                    None,
                );
            }

            let normalized = resolved.normalized.clone();
            let outcome = GraphExtractionOutcome {
                graph_extraction_id: Some(graph_extraction_id),
                runtime_execution_id: Some(runtime_execution_id),
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
                provider_failure: resolved.provider_failure.clone(),
                recovery_summary: resolved.recovery_summary.clone(),
                recovery_attempts: resolved.recovery_attempts.clone(),
            };

            let runtime_outcome = match recovery_status {
                ExtractionOutcomeStatus::Clean => {
                    RuntimeTerminalOutcome::Completed { success: normalized }
                }
                ExtractionOutcomeStatus::Recovered | ExtractionOutcomeStatus::Partial => {
                    RuntimeTerminalOutcome::Recovered {
                        success: normalized,
                        recovery: RuntimeRecoveryOutcome {
                            attempts: u8::try_from(outcome.recovery_attempts.len())
                                .unwrap_or(u8::MAX),
                            summary_redacted: outcome.recovery_summary.warning.clone(),
                        },
                    }
                }
                ExtractionOutcomeStatus::Failed => RuntimeTerminalOutcome::Failed {
                    failure: GraphExtractionTaskFailure {
                        code: GraphExtractionTaskFailureCode::MalformedOutput.as_str().to_string(),
                        summary: "graph extraction resolved with failed recovery status"
                            .to_string(),
                    },
                    summary: make_graph_runtime_failure_summary(
                        GraphExtractionTaskFailureCode::MalformedOutput.as_str(),
                        "graph extraction resolved with failed recovery status",
                    ),
                },
            };
            match runtime_outcome {
                RuntimeTerminalOutcome::Failed { failure, summary } => Err((
                    RuntimeTerminalOutcome::Failed { failure, summary },
                    graph_extraction_execution_error(
                        request,
                        "graph extraction resolved with failed recovery status",
                        outcome.provider_failure.clone(),
                        outcome.recovery_summary.clone(),
                        outcome.recovery_attempts.clone(),
                    ),
                )),
                _ => Ok((runtime_outcome, outcome)),
            }
        }
        Err(failure) => {
            record_graph_runtime_stage(
                state.agent_runtime.executor(),
                runtime_session,
                RuntimeStageKind::ExtractGraph,
                RuntimeStageState::Failed,
                false,
                Some(&GraphExtractionTaskFailure {
                    code: graph_failure_code_from_outcome(&failure).to_string(),
                    summary: failure.error_message.clone(),
                }),
            );
            let task_failure = GraphExtractionTaskFailure {
                code: graph_failure_code_from_outcome(&failure).to_string(),
                summary: failure.error_message.clone(),
            };
            Err((
                make_graph_terminal_failure_outcome(task_failure.clone()),
                GraphExtractionExecutionError {
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
                },
            ))
        }
    }
}

fn map_graph_runtime_execution_error(
    request: &GraphExtractionRequest,
    _runtime_execution_id: Option<uuid::Uuid>,
    error: RuntimeExecutionError,
) -> GraphExtractionExecutionError {
    let message = match error {
        RuntimeExecutionError::InvalidTaskSpec(message) => message,
        RuntimeExecutionError::UnregisteredTask(task_kind) => {
            format!("runtime task is not registered: {}", task_kind.as_str())
        }
        RuntimeExecutionError::TurnBudgetExhausted => {
            "runtime execution budget exhausted".to_string()
        }
        RuntimeExecutionError::PolicyBlocked { reason_code, reason_summary_redacted, .. } => {
            format!("{reason_code}: {reason_summary_redacted}")
        }
    };
    graph_extraction_execution_error(
        request,
        message,
        None,
        ExtractionRecoverySummary {
            status: ExtractionOutcomeStatus::Failed,
            second_pass_applied: false,
            warning: None,
        },
        Vec::new(),
    )
}

fn graph_extraction_execution_error(
    request: &GraphExtractionRequest,
    message: impl Into<String>,
    provider_failure: Option<RuntimeProviderFailureDetail>,
    recovery_summary: ExtractionRecoverySummary,
    recovery_attempts: Vec<GraphExtractionRecoveryRecord>,
) -> GraphExtractionExecutionError {
    GraphExtractionExecutionError {
        message: message.into(),
        request_shape_key: "graph_extract_v6:runtime".to_string(),
        request_size_bytes: request.chunk.content.len(),
        provider_failure,
        recovery_summary,
        recovery_attempts,
        resume_state: GraphExtractionResumeState {
            resumed_from_checkpoint: false,
            replay_count: request.resume_hint.as_ref().map(|hint| hint.replay_count).unwrap_or(0),
            downgrade_level: normalized_downgrade_level(request),
        },
    }
}

fn make_graph_terminal_failure_outcome(
    failure: GraphExtractionTaskFailure,
) -> RuntimeTerminalOutcome<GraphExtractionCandidateSet, GraphExtractionTaskFailure> {
    let summary = make_graph_runtime_failure_summary(&failure.code, &failure.summary);
    if matches!(
        failure.code.as_str(),
        "runtime_policy_rejected" | "runtime_policy_terminated" | "runtime_policy_blocked"
    ) {
        RuntimeTerminalOutcome::Canceled { failure, summary }
    } else {
        RuntimeTerminalOutcome::Failed { failure, summary }
    }
}

fn graph_async_operation_status(
    outcome: &RuntimeTerminalOutcome<GraphExtractionCandidateSet, GraphExtractionTaskFailure>,
) -> &'static str {
    match outcome {
        RuntimeTerminalOutcome::Completed { .. } | RuntimeTerminalOutcome::Recovered { .. } => {
            "ready"
        }
        RuntimeTerminalOutcome::Canceled { .. } => "canceled",
        RuntimeTerminalOutcome::Failed { .. } => "failed",
    }
}

fn graph_policy_action_kind(failure_code: &str) -> Option<&'static str> {
    match failure_code {
        "runtime_policy_rejected" => Some("graph_extract.runtime.policy.rejected"),
        "runtime_policy_terminated" => Some("graph_extract.runtime.policy.terminated"),
        "runtime_policy_blocked" => Some("graph_extract.runtime.policy.blocked"),
        _ => None,
    }
}

async fn append_graph_runtime_policy_audit(
    state: &AppState,
    request: &GraphExtractionRequest,
    graph_extraction_id: Uuid,
    runtime_result: &crate::agent_runtime::task::RuntimeTaskResult<GraphExtractTask>,
) {
    let RuntimeTerminalOutcome::Canceled { summary, .. } = &runtime_result.outcome else {
        return;
    };
    let Some(action_kind) = graph_policy_action_kind(&summary.code) else {
        return;
    };
    let _ = state
        .canonical_services
        .audit
        .append_event(
            state,
            crate::services::audit_service::AppendAuditEventCommand {
                actor_principal_id: None,
                surface_kind: "worker".to_string(),
                action_kind: action_kind.to_string(),
                request_id: None,
                trace_id: None,
                result_kind: "rejected".to_string(),
                redacted_message: summary.summary_redacted.clone(),
                internal_message: Some(format!(
                    "runtime policy canceled graph extraction {} for document {} via runtime execution {} with code {}",
                    graph_extraction_id, request.document.id, runtime_result.execution.id, summary.code
                )),
                subjects: vec![state.canonical_services.audit.runtime_execution_subject(
                    runtime_result.execution.id,
                    None,
                    None,
                )],
            },
        )
        .await;
}

async fn begin_graph_runtime_stage(
    executor: &crate::agent_runtime::executor::RuntimeExecutor,
    session: &mut RuntimeExecutionSession,
    stage_kind: RuntimeStageKind,
) -> std::result::Result<(), GraphExtractionTaskFailure> {
    executor.begin_stage(session, stage_kind).await.map_err(|error| match error {
        RuntimeExecutionError::TurnBudgetExhausted => GraphExtractionTaskFailure {
            code: "runtime_budget_exhausted".to_string(),
            summary: "runtime execution budget exhausted".to_string(),
        },
        RuntimeExecutionError::InvalidTaskSpec(message) => GraphExtractionTaskFailure {
            code: "invalid_runtime_task_spec".to_string(),
            summary: message,
        },
        RuntimeExecutionError::UnregisteredTask(task_kind) => GraphExtractionTaskFailure {
            code: "unregistered_runtime_task".to_string(),
            summary: format!("runtime task is not registered: {}", task_kind.as_str()),
        },
        RuntimeExecutionError::PolicyBlocked {
            decision_kind,
            reason_code,
            reason_summary_redacted,
        } => GraphExtractionTaskFailure {
            code: match decision_kind {
                crate::domains::agent_runtime::RuntimeDecisionKind::Reject => {
                    "runtime_policy_rejected".to_string()
                }
                crate::domains::agent_runtime::RuntimeDecisionKind::Terminate => {
                    "runtime_policy_terminated".to_string()
                }
                crate::domains::agent_runtime::RuntimeDecisionKind::Allow => {
                    "runtime_policy_blocked".to_string()
                }
            },
            summary: format!("{reason_code}: {reason_summary_redacted}"),
        },
    })
}

fn record_graph_runtime_stage(
    executor: &crate::agent_runtime::executor::RuntimeExecutor,
    session: &mut RuntimeExecutionSession,
    stage_kind: RuntimeStageKind,
    stage_state: RuntimeStageState,
    deterministic: bool,
    failure: Option<&GraphExtractionTaskFailure>,
) {
    executor.complete_stage(
        session,
        stage_kind,
        stage_state,
        deterministic,
        failure.map(|value| value.code.clone()),
        failure.map(|value| truncate_failure_code(&value.summary).to_string()),
    );
}

fn make_graph_runtime_failure_summary(code: &str, summary: &str) -> RuntimeFailureSummary {
    RuntimeFailureSummary {
        code: code.to_string(),
        summary_redacted: Some(truncate_failure_code(summary).to_string()),
    }
}

fn graph_failure_code_from_outcome(failure: &GraphExtractionFailureOutcome) -> &'static str {
    match failure.provider_failure.as_ref().map(|value| value.failure_class.clone()) {
        Some(RuntimeProviderFailureClass::InvalidModelOutput) => {
            GraphExtractionTaskFailureCode::MalformedOutput.as_str()
        }
        _ => "graph_extract_failed",
    }
}

fn truncate_failure_code(message: &str) -> &str {
    const MAX_LEN: usize = 160;
    if message.len() <= MAX_LEN {
        return message;
    }
    let mut end = MAX_LEN;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    &message[..end]
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

async fn resolve_graph_extraction(
    state: &AppState,
    provider_profile: &EffectiveProviderProfile,
    request: &GraphExtractionRequest,
) -> std::result::Result<ResolvedGraphExtraction, GraphExtractionFailureOutcome> {
    let library_id = request.library_id;
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
                    let raw_issue_summary =
                        extraction_recovery.redact_recovery_summary(&error_context);
                    pending_recovery_records.push(PendingRecoveryRecord {
                        recovery_kind: "provider_retry".to_string(),
                        trigger_reason: trigger_reason.to_string(),
                        raw_issue_summary: Some(raw_issue_summary.clone()),
                        recovered_summary: Some(
                            extraction_recovery.redact_recovery_summary(recovered_summary),
                        ),
                    });
                    pending_follow_up = Some(RecoveryFollowUpRequest::ProviderRetry {
                        trigger_reason: trigger_reason.to_string(),
                        issue_summary: raw_issue_summary,
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
                    pending_recovery_records.iter().any(|record: &PendingRecoveryRecord| {
                        record.recovery_kind == "second_pass"
                    }),
                    false,
                    true,
                );
                return Err(GraphExtractionFailureOutcome {
                    request_shape_key: prompt_plan.request_shape_key,
                    request_size_bytes: prompt_plan.request_size_bytes,
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
        match normalize_graph_extraction_output(&raw.output_text) {
            Ok(normalized_attempt) => {
                trace.provider_attempt_count = provider_attempt_no;
                trace.reask_count = provider_attempt_no.saturating_sub(1);
                trace.attempts.push(GraphExtractionRecoveryAttempt {
                    provider_attempt_no,
                    prompt_hash: raw.prompt_hash.clone(),
                    output_text: raw.output_text.clone(),
                    usage_json: raw.usage_json.clone(),
                    timing: raw.timing.clone(),
                    parse_error: None,
                    normalization_path: normalized_attempt.normalization_path.to_string(),
                    recovery_kind: None,
                    trigger_reason: None,
                });

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
                };

                if second_pass.should_attempt {
                    let second_pass_decision = second_pass.decision.clone().unwrap_or_else(|| {
                        crate::services::extraction_recovery::RecoveryDecisionSummary {
                            reason_code: "sparse_extraction".to_string(),
                            reason_summary_redacted: extraction_recovery.redact_recovery_summary(
                                "The extraction result looked too sparse for the chunk content.",
                            ),
                        }
                    });
                    best_partial_candidate = select_better_partial_candidate(
                        best_partial_candidate,
                        current_candidate.clone(),
                    );
                    pending_recovery_records.push(PendingRecoveryRecord {
                        recovery_kind: "second_pass".to_string(),
                        trigger_reason: second_pass_decision.reason_code.clone(),
                        raw_issue_summary: Some(second_pass_decision.reason_summary_redacted.clone()),
                        recovered_summary: Some(
                            extraction_recovery.redact_recovery_summary(
                                "Requested a second extraction pass because the first result looked sparse or inconsistent.",
                            ),
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
                        recovery_kind: Some("second_pass".to_string()),
                        trigger_reason: Some(second_pass_decision.reason_code.clone()),
                    });
                    pending_follow_up = Some(RecoveryFollowUpRequest::SecondPass {
                        trigger_reason: second_pass_decision.reason_code,
                        issue_summary: second_pass_decision.reason_summary_redacted,
                        previous_output: raw.output_text.clone(),
                    });
                    continue;
                }

                let recovery_summary = extraction_recovery.classify_outcome(
                    trace.provider_attempt_count,
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
                    recovery_kind: (provider_attempt_no < max_provider_attempts)
                        .then_some("provider_retry".to_string()),
                    trigger_reason: (provider_attempt_no < max_provider_attempts)
                        .then_some("malformed_output".to_string()),
                });
                trace.provider_attempt_count = provider_attempt_no;
                trace.reask_count = provider_attempt_no.saturating_sub(1);
                if provider_attempt_no < max_provider_attempts {
                    let parse_error_redacted =
                        extraction_recovery.redact_recovery_summary(&parse_error);
                    pending_recovery_records.push(PendingRecoveryRecord {
                        recovery_kind: "provider_retry".to_string(),
                        trigger_reason: "malformed_output".to_string(),
                        raw_issue_summary: Some(parse_error_redacted.clone()),
                        recovered_summary: Some(extraction_recovery.redact_recovery_summary(
                            "Requested a stricter retry after malformed extraction output.",
                        )),
                    });
                    pending_follow_up = Some(RecoveryFollowUpRequest::ProviderRetry {
                        trigger_reason: "malformed_output".to_string(),
                        issue_summary: parse_error_redacted,
                        previous_output: raw.output_text.clone(),
                    });
                    continue;
                }

                if let Some(candidate) = best_partial_candidate.clone() {
                    let recovery_summary = extraction_recovery.classify_outcome(
                        trace.provider_attempt_count,
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
                        pending_recovery_records
                            .iter()
                            .any(|record| record.recovery_kind == "second_pass"),
                        false,
                        true,
                    );
                    return Err(GraphExtractionFailureOutcome {
                        request_shape_key: raw.request_shape_key.clone(),
                        request_size_bytes: raw.request_size_bytes,
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

    Err(GraphExtractionFailureOutcome {
        request_shape_key: "graph_extract_v6:unknown".to_string(),
        request_size_bytes: 0,
        recovery_summary: extraction_recovery.classify_outcome(
            trace.provider_attempt_count,
            pending_recovery_records.iter().any(|record| record.recovery_kind == "second_pass"),
            false,
            true,
        ),
        error_message: "graph extraction retry loop ended without a terminal outcome".to_string(),
        provider_failure: None,
        recovery_attempts: finalize_recovery_attempt_records(
            &pending_recovery_records,
            &extraction_recovery.classify_outcome(
                trace.provider_attempt_count,
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
    let task_spec = GraphExtractTask::spec();
    let request = build_provider_request(
        &task_spec,
        ChatRequestSeed {
            provider_kind: runtime_binding.provider_kind.clone(),
            model_name: runtime_binding.model_name.clone(),
            api_key_override: Some(runtime_binding.api_key.clone()),
            base_url_override: runtime_binding.provider_base_url.clone(),
            system_prompt: runtime_binding.system_prompt.clone(),
            temperature: runtime_binding.temperature,
            top_p: runtime_binding.top_p,
            max_output_tokens_override: runtime_binding.max_output_tokens_override,
            extra_parameters_json: runtime_binding.extra_parameters_json.clone(),
        },
        prompt_plan.prompt.clone(),
    );
    let response =
        gateway.generate(request).await.context("graph extraction provider call failed")?;
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
    _request_shape_key: String,
    _request_size_bytes: usize,
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

fn normalize_graph_extraction_output(
    output_text: &str,
) -> std::result::Result<NormalizedGraphExtractionAttempt, FailedNormalizationAttempt> {
    parse_graph_extraction_output(output_text)
        .map(|normalized| NormalizedGraphExtractionAttempt {
            normalized,
            normalization_path: "direct",
        })
        .map_err(|error| FailedNormalizationAttempt { parse_error: error.to_string() })
}

pub fn parse_graph_extraction_output(output_text: &str) -> Result<GraphExtractionCandidateSet> {
    let parsed = extract_json_payload(output_text).map_err(|error| {
        anyhow!("{}: {}", GraphExtractionTaskFailureCode::MalformedOutput.as_str(), error)
    })?;
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

    // Post-extraction: refine "mentions" relations using summary heuristics
    let relations = relations
        .into_iter()
        .map(|mut rel| {
            let refined = refine_mentions_relation(
                &rel.relation_type,
                rel.summary.as_deref(),
                &RuntimeNodeType::Entity, // placeholder — full type-aware refinement in graph_merge
                &RuntimeNodeType::Entity,
            );
            if refined != rel.relation_type && graph_identity::is_canonical_relation_type(&refined)
            {
                rel.relation_type = refined;
            }
            rel
        })
        .collect::<Vec<_>>();

    let candidate_set = GraphExtractionCandidateSet { entities, relations };
    validate_graph_extraction_candidate_set(&candidate_set)
        .map_err(|failure| anyhow!(failure.summary.clone()))?;
    Ok(candidate_set)
}

pub fn validate_graph_extraction_candidate_set(
    candidate_set: &GraphExtractionCandidateSet,
) -> Result<(), GraphExtractionTaskFailure> {
    if candidate_set.entities.iter().any(|entity| entity.label.trim().is_empty())
        || candidate_set.relations.iter().any(|relation| {
            relation.source_label.trim().is_empty()
                || relation.target_label.trim().is_empty()
                || relation.relation_type.trim().is_empty()
        })
    {
        return Err(GraphExtractionTaskFailure {
            code: GraphExtractionTaskFailureCode::InvalidCandidateSet.as_str().to_string(),
            summary: "graph extraction candidate set contains empty labels or relation fields"
                .to_string(),
        });
    }

    Ok(())
}

fn refine_entity_type(label: &str, current_type: RuntimeNodeType) -> RuntimeNodeType {
    // Only refine generic "entity" types
    if current_type != RuntimeNodeType::Entity {
        return current_type;
    }

    let label_trimmed = label.trim();

    // Environment variables: ALL_CAPS_WITH_UNDERSCORES → Attribute (configuration parameters)
    if label_trimmed.len() > 2
        && label_trimmed.chars().all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit())
        && label_trimmed.contains('_')
    {
        return RuntimeNodeType::Attribute;
    }

    // URL paths: /api/v1/users → Artifact (human-made endpoints)
    if label_trimmed.starts_with('/') && label_trimmed.len() > 1 {
        return RuntimeNodeType::Artifact;
    }

    // HTTP methods → Artifact
    if matches!(label_trimmed, "GET" | "POST" | "PUT" | "DELETE" | "PATCH" | "OPTIONS" | "HEAD") {
        return RuntimeNodeType::Artifact;
    }

    // HTTP status codes: 3 digits 100-599 → Attribute (status indicators)
    if label_trimmed.len() == 3 {
        if let Ok(code) = label_trimmed.parse::<u16>() {
            if (100..600).contains(&code) {
                return RuntimeNodeType::Attribute;
            }
        }
    }

    // File paths: ends with known extension → Artifact (human-made files)
    if label_trimmed.contains('.') {
        let ext = label_trimmed.rsplit('.').next().unwrap_or("");
        if matches!(
            ext,
            "py" | "rs"
                | "ts"
                | "tsx"
                | "js"
                | "go"
                | "java"
                | "kt"
                | "sql"
                | "md"
                | "json"
                | "yaml"
                | "yml"
                | "toml"
                | "xml"
                | "html"
                | "css"
                | "tf"
                | "pdf"
                | "docx"
                | "pptx"
                | "pkl"
                | "csv"
        ) {
            return RuntimeNodeType::Artifact;
        }
    }

    // URLs → Artifact
    if label_trimmed.starts_with("http://") || label_trimmed.starts_with("https://") {
        return RuntimeNodeType::Artifact;
    }

    current_type
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
    let node_type = match value.get("node_type").and_then(serde_json::Value::as_str) {
        None => RuntimeNodeType::Entity,
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                RuntimeNodeType::Entity
            } else {
                match trimmed.to_ascii_lowercase().as_str() {
                    "document" => RuntimeNodeType::Document,
                    "person" => RuntimeNodeType::Person,
                    "organization" => RuntimeNodeType::Organization,
                    "location" => RuntimeNodeType::Location,
                    "event" => RuntimeNodeType::Event,
                    "artifact" => RuntimeNodeType::Artifact,
                    "natural" => RuntimeNodeType::Natural,
                    "process" => RuntimeNodeType::Process,
                    "concept" => RuntimeNodeType::Concept,
                    "attribute" => RuntimeNodeType::Attribute,
                    "entity" => RuntimeNodeType::Entity,
                    // Backward compatibility
                    "topic" => RuntimeNodeType::Concept,
                    "technology" => RuntimeNodeType::Artifact,
                    "api" => RuntimeNodeType::Artifact,
                    "code_symbol" => RuntimeNodeType::Artifact,
                    "natural_kind" => RuntimeNodeType::Natural,
                    "metric" => RuntimeNodeType::Attribute,
                    "regulation" => RuntimeNodeType::Artifact,
                    _ => RuntimeNodeType::Entity,
                }
            }
        }
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

    let node_type = refine_entity_type(label, node_type);

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
    let relation_slug = graph_identity::normalize_graph_identity_component(relation_type);
    if graph_identity::is_noise_relation_type(&relation_slug) {
        return None;
    }
    let normalized_relation_type = normalize_relation_candidate_type(relation_type)?;

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
    let normalized = graph_identity::normalize_relation_type(relation_type);
    if normalized.is_empty()
        || !relation_type_is_canonical_ascii(&normalized)
        || !graph_identity::is_canonical_relation_type(&normalized)
    {
        return None;
    }
    Some(normalized)
}

fn relation_type_is_canonical_ascii(normalized_relation_type: &str) -> bool {
    normalized_relation_type.bytes().all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_'))
}

/// Post-extraction heuristic to reduce "mentions" overuse.
/// When the LLM outputs "mentions" but the summary text suggests a more specific relation,
/// upgrade to the more specific type.
fn refine_mentions_relation(
    relation_type: &str,
    summary: Option<&str>,
    source_type: &crate::domains::runtime_graph::RuntimeNodeType,
    _target_type: &crate::domains::runtime_graph::RuntimeNodeType,
) -> String {
    if relation_type != "mentions" {
        return relation_type.to_string();
    }

    // Check summary for action verbs that suggest a more specific relation
    if let Some(summary) = summary {
        let s = summary.to_ascii_lowercase();
        if s.contains("depends on") || s.contains("requires") || s.contains("needs") {
            return "depends_on".to_string();
        }
        if s.contains("uses") || s.contains("utilizes") || s.contains("leverages") {
            return "uses".to_string();
        }
        if s.contains("contains") || s.contains("includes") || s.contains("consists of") {
            return "contains".to_string();
        }
        if s.contains("implements") || s.contains("implementation of") {
            return "implements".to_string();
        }
        if s.contains("extends") || s.contains("inherits") {
            return "extends".to_string();
        }
        if s.contains("returns") || s.contains("produces") || s.contains("outputs") {
            return "returns".to_string();
        }
        if s.contains("configures") || s.contains("configuration") {
            return "configures".to_string();
        }
        if s.contains("calls") || s.contains("invokes") {
            return "calls".to_string();
        }
        if s.contains("authenticat") || s.contains("authoriz") {
            return "authenticates".to_string();
        }
        if s.contains("defines") || s.contains("declares") || s.contains("specifies") {
            return "defines".to_string();
        }
        if s.contains("provides") || s.contains("exposes") || s.contains("offers") {
            return "provides".to_string();
        }
        if s.contains("deployed") || s.contains("runs on") || s.contains("hosted") {
            return "deployed_on".to_string();
        }
    }

    // Type-based heuristic: document → entity/code_symbol is usually "describes" not "mentions"
    use crate::domains::runtime_graph::RuntimeNodeType;
    if *source_type == RuntimeNodeType::Document {
        return "describes".to_string();
    }

    relation_type.to_string()
}

fn extract_json_payload(output_text: &str) -> Result<serde_json::Value> {
    let trimmed = output_text.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("graph extraction output is empty"));
    }
    serde_json::from_str::<serde_json::Value>(trimmed).context("invalid graph extraction json")
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
            library_id: uuid::Uuid::nil(),
            source_id: None,
            external_key: "spec.md".to_string(),
            title: Some("Spec".to_string()),
            mime_type: Some("text/markdown".to_string()),
            checksum: None,
            active_revision_id: None,
            document_state: "active".to_string(),
            mutation_kind: None,
            mutation_status: None,
            deleted_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn sample_chunk() -> ChunkRow {
        ChunkRow {
            id: uuid::Uuid::nil(),
            document_id: uuid::Uuid::nil(),
            library_id: uuid::Uuid::nil(),
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
                model_name: "gpt-5.4-mini".to_string(),
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
                model_name: "gpt-5.4-mini".to_string(),
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
            model_name: "gpt-5.4-mini".to_string(),
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens_override: None,
            extra_parameters_json: serde_json::json!({}),
        }
    }

    fn sample_request() -> GraphExtractionRequest {
        GraphExtractionRequest {
            library_id: uuid::Uuid::nil(),
            document: sample_document(),
            chunk: sample_chunk(),
            structured_chunk: GraphExtractionStructuredChunkContext {
                chunk_kind: Some("endpoint_block".to_string()),
                section_path: vec!["REST API".to_string(), "Status".to_string()],
                heading_trail: vec!["REST API".to_string()],
                support_block_ids: vec![uuid::Uuid::now_v7()],
                literal_digest: Some("digest".to_string()),
            },
            technical_facts: vec![
                GraphExtractionTechnicalFact {
                    fact_kind: "http_method".to_string(),
                    canonical_value: "GET".to_string(),
                    display_value: "GET".to_string(),
                    qualifiers: Vec::new(),
                },
                GraphExtractionTechnicalFact {
                    fact_kind: "endpoint_path".to_string(),
                    canonical_value: "/annual-report/graph".to_string(),
                    display_value: "/annual-report/graph".to_string(),
                    qualifiers: vec![TechnicalFactQualifier {
                        key: "method".to_string(),
                        value: "GET".to_string(),
                    }],
                },
            ],
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

    #[test]
    fn prompt_mentions_json_contract_and_chunk_text() {
        let prompt = build_graph_extraction_prompt(&sample_request());

        assert!(prompt.contains("strict JSON"));
        assert!(prompt.contains("entities"));
        assert!(prompt.contains("annual report graph"));
        assert!(prompt.contains("Chunk kind"));
        assert!(prompt.contains("technical_facts"));
        assert!(prompt.contains("copied verbatim from this catalog"));
        assert!(!prompt.contains("`topic`, or `document`"));
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
    fn response_format_enum_matches_canonical_relation_catalog() {
        let response_format = graph_extraction_response_format("openai");
        let enum_values = response_format
            .get("json_schema")
            .and_then(|value| value.get("schema"))
            .and_then(|value| value.get("properties"))
            .and_then(|value| value.get("relations"))
            .and_then(|value| value.get("items"))
            .and_then(|value| value.get("properties"))
            .and_then(|value| value.get("relation_type"))
            .and_then(|value| value.get("enum"))
            .and_then(serde_json::Value::as_array)
            .expect("relation_type enum");
        let rendered = enum_values
            .iter()
            .map(|value| value.as_str().expect("enum string"))
            .collect::<Vec<_>>();

        assert_eq!(rendered, graph_identity::canonical_relation_type_catalog());
    }

    #[test]
    fn deepseek_uses_json_object_response_format() {
        let response_format = graph_extraction_response_format("deepseek");

        assert_eq!(
            response_format.get("type").and_then(serde_json::Value::as_str),
            Some("json_object")
        );
        assert!(response_format.get("json_schema").is_none());
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
        assert_eq!(normalized.entities[1].node_type, RuntimeNodeType::Concept);
        assert_eq!(normalized.relations[0].relation_type, "mentions");
    }

    #[test]
    fn accepts_expanded_node_type_values() {
        let normalized = parse_graph_extraction_output(
            r#"{
              "entities": [
                { "label": "Valid", "node_type": "topic", "aliases": [], "summary": "" },
                { "label": "Google", "node_type": "organization", "aliases": [], "summary": "" }
              ],
              "relations": []
            }"#,
        )
        .expect("parse graph extraction");

        assert_eq!(normalized.entities.len(), 2);
        assert_eq!(normalized.entities[0].label, "Valid");
        assert_eq!(normalized.entities[0].node_type, RuntimeNodeType::Concept);
        assert_eq!(normalized.entities[1].label, "Google");
        assert_eq!(normalized.entities[1].node_type, RuntimeNodeType::Organization);
    }

    #[test]
    fn falls_back_unknown_node_type_to_entity() {
        let normalized = parse_graph_extraction_output(
            r#"{
              "entities": [
                { "label": "Something", "node_type": "invented_type", "aliases": [], "summary": "" }
              ],
              "relations": []
            }"#,
        )
        .expect("parse graph extraction");

        assert_eq!(normalized.entities.len(), 1);
        assert_eq!(normalized.entities[0].label, "Something");
        assert_eq!(normalized.entities[0].node_type, RuntimeNodeType::Entity);
    }

    #[test]
    fn rejects_json_inside_markdown_fence() {
        let error =
            parse_graph_extraction_output("```json\n{\"entities\":[],\"relations\":[]}\n```")
                .expect_err("fenced output must fail");

        assert!(error.to_string().contains("invalid graph extraction json"));
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
    fn drops_non_canonical_non_ascii_relation_types_at_parse_time() {
        let normalized = parse_graph_extraction_output(
            r#"{
              "entities": [],
              "relations": [
                { "source_label": "Alpha", "target_label": "Beta", "relation_type": "включает" },
                { "source_label": "Alpha", "target_label": "Beta", "relation_type": "supports" }
              ]
            }"#,
        )
        .expect("normalize graph extraction");

        assert_eq!(normalized.relations.len(), 1);
        assert_eq!(normalized.relations[0].relation_type, "supports");
    }

    #[test]
    fn rejects_non_json_payloads() {
        let error = parse_graph_extraction_output("not valid json").expect_err("invalid json");

        assert!(error.to_string().contains("invalid graph extraction json"));
    }

    #[test]
    fn rejects_json_object_surrounded_by_prose() {
        let error = parse_graph_extraction_output(
            "Here is the result:\n{\"entities\":[\"OpenAI\"],\"relations\":[]}\nThanks.",
        )
        .expect_err("prose wrapper must fail");

        assert!(error.to_string().contains("invalid graph extraction json"));
    }

    #[test]
    fn rejects_json5_style_payloads() {
        let error = parse_graph_extraction_output(
            "{entities:[{label:'OpenAI', node_type:'entity', aliases:['Open AI'], summary:'provider',},], relations:[]}",
        )
        .expect_err("json5 payload must fail");

        assert!(error.to_string().contains("invalid graph extraction json"));
    }

    #[test]
    fn rejects_truncated_json_payloads() {
        let error = parse_graph_extraction_output(
            r#"{"entities":[{"label":"OpenAI","node_type":"entity","aliases":[],"summary":"provider"}],"relations":[{"source_label":"OpenAI","target_label":"Graph","relation_type":"mentions","summary":"link"}"#,
        )
        .expect_err("truncated payload must fail");

        assert!(error.to_string().contains("invalid graph extraction json"));
    }

    #[test]
    fn rejects_named_sections_without_outer_object() {
        let error = normalize_graph_extraction_output(
            r#"
            entities:
            [{"label":"OpenAI","node_type":"entity","aliases":[],"summary":"provider"}]
            relations:
            [{"source_label":"OpenAI","target_label":"Annual report","relation_type":"mentions","summary":"citation"}]
            "#,
        )
        .expect_err("named sections must fail");

        assert!(error.parse_error.contains("malformed_output"));
    }

    #[tokio::test]
    async fn retries_after_terminal_parse_failure_and_aggregates_usage() {
        let gateway = FakeGateway {
            responses: Mutex::new(vec![
                Ok(ChatResponse {
                    provider_kind: "openai".to_string(),
                    model_name: "gpt-5.4-mini".to_string(),
                    output_text: "this is not json".to_string(),
                    usage_json: serde_json::json!({
                        "prompt_tokens": 11,
                        "completion_tokens": 4,
                        "total_tokens": 15,
                    }),
                }),
                Ok(ChatResponse {
                    provider_kind: "openai".to_string(),
                    model_name: "gpt-5.4-mini".to_string(),
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
                    model_name: "gpt-5.4-mini".to_string(),
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
                    model_name: "gpt-5.4-mini".to_string(),
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
                    model_name: "gpt-5.4-mini".to_string(),
                    output_text: "broken payload".to_string(),
                    usage_json: serde_json::json!({ "prompt_tokens": 5 }),
                }),
                Ok(ChatResponse {
                    provider_kind: "openai".to_string(),
                    model_name: "gpt-5.4-mini".to_string(),
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
        assert_eq!(
            failure.provider_failure.as_ref().map(|detail| detail.failure_class.clone()),
            Some(RuntimeProviderFailureClass::InvalidModelOutput)
        );
    }

    #[test]
    fn provider_usage_payload_keeps_provider_metadata() {
        let usage = build_provider_usage_json(
            "openai",
            "gpt-5.4-mini",
            serde_json::json!({
                "prompt_tokens": 21,
                "completion_tokens": 9,
            }),
        );

        assert_eq!(usage.get("provider_kind").and_then(serde_json::Value::as_str), Some("openai"));
        assert_eq!(
            usage.get("model_name").and_then(serde_json::Value::as_str),
            Some("gpt-5.4-mini")
        );
        assert_eq!(usage.get("prompt_tokens").and_then(serde_json::Value::as_i64), Some(21));
    }
}
