//! Opt-in measurement of authored candidates through the production selected verifier.
//! See `CONTROLLED_SEMANTIC.md`. A successful measurement is not semantic acceptance.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use anyhow::{Context, Result, ensure};
use axum::{
    extract::FromRequestParts,
    http::{HeaderValue, Request, header::COOKIE},
};
use chrono::{DateTime, Utc};
use ironrag_contracts::assistant::semantic_verification::{
    SEMANTIC_VERIFICATION_PROTOCOL_VERSION, SemanticEvidenceScope, SemanticVerificationOutcome,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use crate::{
    app::{config::Settings, state::AppState},
    domains::{
        agent_runtime::RuntimeSurfaceKind,
        ai::AiBindingPurpose,
        query::{QueryAnswerDisposition, QueryVerificationState},
    },
    infra::{
        persistence::Persistence,
        repositories::{query_repository, runtime_repository},
    },
    integrations::{
        llm::{
            ChatRequest, ChatResponse, EmbeddingBatchRequest, EmbeddingBatchResponse,
            EmbeddingRequest, EmbeddingResponse, LlmGateway, ToolUseRequest, ToolUseResponse,
            VisionRequest, VisionResponse,
        },
        retry::with_single_provider_attempt,
    },
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_QUERY_RUN, load_library_and_authorize},
        mcp::tools::{ToolCallContext, call_named_tool},
    },
    mcp_types::{McpReadDocumentResponse, McpReadMode, McpReadabilityState},
    services::query::{
        agent_loop::native_document_read_reference,
        assistant_grounding::AssistantGroundingEvidence,
        execution::{
            QueryVerificationPersistence, RuntimeAnswerVerification, persist_query_verification,
            semantic_verification::{
                SemanticCandidateContext, load_semantic_sources, prepare,
                verify_selected_candidate, with_controlled_binding,
            },
        },
        provider_billing::QueryProviderExecutionContext,
    },
};

const MANIFEST_ENV: &str = "IRONRAG_SEMANTIC_CONTROLS_MANIFEST";
const ADMISSION_ENV: &str = "IRONRAG_SEMANTIC_CONTROLS_ADMISSION";
const MAX_INPUT_BYTES: u64 = 262_144;
const CONVERSATION_CAP: usize = 1000;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    protocol: String,
    protocol_version: u32,
    source_sha: String,
    case_file: PathBuf,
    case_file_sha256: String,
    selected_case_ids: Vec<String>,
    workspace_id: Uuid,
    library_id: Uuid,
    database_url_sha256: String,
    redis_url_sha256: String,
    session_file: PathBuf,
    session_sha256: String,
    session_cookie_json_pointer: String,
    preservation_file: PathBuf,
    preservation_sha256: String,
    sources: Vec<SourcePin>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SourcePin {
    label: String,
    source_file: PathBuf,
    source_file_sha256: String,
    document_id: Uuid,
    revision_id: Uuid,
    canonical_text_sha256: String,
    canonical_utf8_bytes: usize,
}

// The admission file is an externally reviewed operational input, not a grant
// issued by this test. Real lifetime/cost enforcement remains the native broker's.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Admission {
    protocol: String,
    lifecycle_id: Uuid,
    ledger_root: PathBuf,
    manifest_sha256: String,
    runner_sha256: String,
    binary_sha256: String,
    build_receipt_file: PathBuf,
    build_receipt_sha256: String,
    independent_reviewer: String,
    fresh_grant_reference: String,
    not_before: DateTime<Utc>,
    hard_stop_at: DateTime<Utc>,
    max_physical_calls: usize,
    binding_id: Uuid,
    account_id: Uuid,
    provider_catalog_id: Uuid,
    model_catalog_id: Uuid,
    provider_kind: String,
    model_name: String,
    gateway_base_url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BuildReceipt {
    protocol: String,
    source_sha: String,
    binary_sha256: String,
    runner_sha256: String,
    independent_reviewer: String,
    reproducible_source_binding_verified: bool,
    evidence_file: PathBuf,
    evidence_sha256: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Preservation {
    lifecycle_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    independent_reviewer: String,
    sources: Vec<PreservedSource>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreservedSource {
    label: String,
    document_id: Uuid,
    revision_id: Uuid,
    source_file_sha256: String,
    canonical_text_sha256: String,
    canonical_utf8_bytes: usize,
    complete_source_coverage_verified: bool,
    coverage_evidence_file: PathBuf,
    coverage_evidence_sha256: String,
}

// Expected labels, basis and other annotations have no representation in the
// evaluator input. The exact original file bytes are nevertheless pinned.
#[derive(Deserialize)]
struct CaseFile {
    sources: BTreeMap<String, SourceDefinition>,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceDefinition {
    sha256: String,
    complete_document_lines: Option<usize>,
}

#[derive(Deserialize)]
struct Case {
    id: String,
    source: String,
    question: String,
    answer: String,
}

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    let file = File::open(path)?;
    ensure!(file.metadata()?.is_file(), "regular input file required");
    let mut bytes = Vec::new();
    file.take(MAX_INPUT_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= MAX_INPUT_BYTES, "input exceeds byte limit");
    Ok(bytes)
}

fn pinned(path: &Path, digest: &str) -> Result<Vec<u8>> {
    let bytes = read_bounded(path)?;
    ensure!(hash(&bytes) == digest, "input digest mismatch");
    Ok(bytes)
}

fn executable_hash() -> Result<String> {
    let mut file = File::open(std::env::current_exe()?)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 65_536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn select_cases(file: CaseFile, selected: &[String]) -> Result<Vec<Case>> {
    let mut ids = BTreeSet::new();
    ensure!(!file.cases.is_empty() && file.cases.len() <= 32, "invalid case count");
    for case in &file.cases {
        ensure!(!case.id.is_empty() && ids.insert(case.id.as_str()), "duplicate or empty case ID");
        ensure!(file.sources.contains_key(&case.source), "unknown source label");
        ensure!(
            !case.question.trim().is_empty()
                && case.question.len() <= 4000
                && !case.answer.trim().is_empty()
                && case.answer.len() <= 16000,
            "invalid candidate input"
        );
    }
    let requested: BTreeSet<_> = selected.iter().map(String::as_str).collect();
    ensure!(
        !requested.is_empty()
            && requested.len() == selected.len()
            && requested.iter().all(|id| ids.contains(id)),
        "invalid case selection"
    );
    Ok(file.cases.into_iter().filter(|case| requested.contains(case.id.as_str())).collect())
}

fn explicit_paths(
    manifest: Option<String>,
    admission: Option<String>,
) -> Result<(PathBuf, PathBuf)> {
    let manifest =
        manifest.filter(|value| !value.is_empty()).context("explicit manifest required")?;
    let admission =
        admission.filter(|value| !value.is_empty()).context("fresh explicit admission required")?;
    Ok((PathBuf::from(manifest), PathBuf::from(admission)))
}

fn admission_window(admission: &Admission) -> Result<()> {
    admission_window_at(admission, Utc::now())
}

fn admission_window_at(admission: &Admission, now: DateTime<Utc>) -> Result<()> {
    ensure!(
        now >= admission.not_before && now + chrono::Duration::seconds(12) < admission.hard_stop_at,
        "fresh admission window cannot cover another bounded operation"
    );
    Ok(())
}

fn validate_inputs(manifest: &Manifest, bytes: &[u8], admission: &Admission) -> Result<Vec<Case>> {
    ensure!(
        manifest.protocol == "ironrag-semantic-controls/1"
            && manifest.protocol_version == SEMANTIC_VERIFICATION_PROTOCOL_VERSION,
        "incompatible control or semantic protocol"
    );
    ensure!(
        admission.protocol == "ironrag-semantic-control-admission/1"
            && admission.manifest_sha256 == hash(bytes)
            && admission.runner_sha256 == hash(include_bytes!("controlled_semantic.rs"))
            && !admission.independent_reviewer.trim().is_empty()
            && !admission.fresh_grant_reference.trim().is_empty(),
        "missing exact independently reviewed admission"
    );
    admission_window(admission)?;
    require_build_receipt(manifest, admission)?;
    let case_bytes = pinned(&manifest.case_file, &manifest.case_file_sha256)?;
    let file: CaseFile = serde_json::from_slice(&case_bytes)?;
    let mut labels = BTreeSet::new();
    for pin in &manifest.sources {
        ensure!(labels.insert(pin.label.as_str()), "duplicate source pin");
        let definition = file.sources.get(&pin.label).context("source pin not in case file")?;
        let source = pinned(&pin.source_file, &pin.source_file_sha256)?;
        ensure!(
            definition.sha256 == pin.source_file_sha256
                && pin.canonical_utf8_bytes > 0
                && pin.canonical_utf8_bytes <= 22000,
            "source definition or canonical limit mismatch"
        );
        if let Some(lines) = definition.complete_document_lines {
            ensure!(
                std::str::from_utf8(&source)?.lines().count() == lines,
                "complete source-file line coverage changed"
            );
        }
    }
    let cases = select_cases(file, &manifest.selected_case_ids)?;
    ensure!(
        cases.iter().all(|case| labels.contains(case.source.as_str()))
            && admission.max_physical_calls == cases.len(),
        "source coverage or exact physical allowance mismatch"
    );
    let preservation: Preservation = serde_json::from_slice(&pinned(
        &manifest.preservation_file,
        &manifest.preservation_sha256,
    )?)?;
    ensure!(
        preservation.lifecycle_id == admission.lifecycle_id
            && preservation.workspace_id == manifest.workspace_id
            && preservation.library_id == manifest.library_id
            && !preservation.independent_reviewer.trim().is_empty()
            && preservation.sources.len() == manifest.sources.len(),
        "preservation scope mismatch"
    );
    for pin in &manifest.sources {
        let matches: Vec<_> =
            preservation.sources.iter().filter(|source| source.label == pin.label).collect();
        ensure!(matches.len() == 1, "unique independent preservation source required");
        let source = matches[0];
        ensure!(
            source.document_id == pin.document_id
                && source.revision_id == pin.revision_id
                && source.source_file_sha256 == pin.source_file_sha256
                && source.canonical_text_sha256 == pin.canonical_text_sha256
                && source.canonical_utf8_bytes == pin.canonical_utf8_bytes
                && source.complete_source_coverage_verified,
            "complete hash-matched preservation not accepted"
        );
        pinned(&source.coverage_evidence_file, &source.coverage_evidence_sha256)?;
    }
    Ok(cases)
}

fn require_build_receipt(manifest: &Manifest, admission: &Admission) -> Result<()> {
    let receipt: BuildReceipt = serde_json::from_slice(&pinned(
        &admission.build_receipt_file,
        &admission.build_receipt_sha256,
    )?)?;
    ensure!(
        receipt.protocol == "ironrag-semantic-control-build/1"
            && receipt.source_sha == manifest.source_sha
            && receipt.binary_sha256 == admission.binary_sha256
            && receipt.runner_sha256 == admission.runner_sha256
            && !receipt.independent_reviewer.trim().is_empty()
            && receipt.reproducible_source_binding_verified,
        "independently verified binary/source binding required"
    );
    pinned(&receipt.evidence_file, &receipt.evidence_sha256)?;
    Ok(())
}

fn write_new(root: &Path, name: &str, value: &Value) -> Result<()> {
    let mut file =
        OpenOptions::new().write(true).create_new(true).mode(0o600).open(root.join(name))?;
    file.write_all(&serde_json::to_vec(value)?)?;
    file.sync_all()?;
    File::open(root)?.sync_all()?;
    Ok(())
}

fn observation_key(
    lifecycle: Uuid,
    manifest: &Manifest,
    case: &Case,
    pin: &SourcePin,
) -> Result<String> {
    // Mutable output paths, selected subset/order and newly allocated execution
    // IDs are excluded: they must not reopen an interrupted observation.
    Ok(hash(&serde_json::to_vec(&json!({"lifecycleId":lifecycle,
        "questionSha256":hash(case.question.as_bytes()),"answerSha256":hash(case.answer.as_bytes()),
        "documentId":pin.document_id,"revisionId":pin.revision_id,
        "sourceFileSha256":pin.source_file_sha256,"canonicalTextSha256":pin.canonical_text_sha256,
        "protocolVersion":manifest.protocol_version}))?))
}

#[derive(Default)]
struct ProviderReadGuard(AtomicUsize);

impl ProviderReadGuard {
    fn deny<T>(&self) -> Result<T> {
        self.0.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("provider dispatch prohibited during native preparation")
    }
}

#[async_trait::async_trait]
impl LlmGateway for ProviderReadGuard {
    async fn generate(&self, _: ChatRequest) -> Result<ChatResponse> {
        self.deny()
    }
    async fn generate_with_tools(&self, _: ToolUseRequest) -> Result<ToolUseResponse> {
        self.deny()
    }
    async fn embed(&self, _: EmbeddingRequest) -> Result<EmbeddingResponse> {
        self.deny()
    }
    async fn embed_many(&self, _: EmbeddingBatchRequest) -> Result<EmbeddingBatchResponse> {
        self.deny()
    }
    async fn vision_extract(&self, _: VisionRequest) -> Result<VisionResponse> {
        self.deny()
    }
}

async fn build_state(manifest: &Manifest, admission: &Admission) -> Result<AppState> {
    ensure!(executable_hash()? == admission.binary_sha256, "admitted binary changed");
    let source = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()?;
    ensure!(
        source.status.success()
            && std::str::from_utf8(&source.stdout)?.trim() == manifest.source_sha,
        "admitted source HEAD changed"
    );
    ensure!(
        std::process::Command::new("git")
            .args(["diff", "--quiet", "HEAD", "--"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .status()?
            .success(),
        "clean admitted source required"
    );
    let database = std::env::var("IRONRAG_DATABASE_URL").context("explicit database required")?;
    let redis = std::env::var("IRONRAG_REDIS_URL").context("explicit redis required")?;
    ensure!(
        hash(database.as_bytes()) == manifest.database_url_sha256
            && hash(redis.as_bytes()) == manifest.redis_url_sha256,
        "dependency identity changed"
    );
    let settings = Settings::from_env()?;
    ensure!(
        settings.database_url == database
            && settings.redis_url == redis
            && settings.provider_retry_max_attempts == 1,
        "dependency or single-attempt settings changed"
    );
    let pool = PgPoolOptions::new().max_connections(4).connect(&database).await?;
    AppState::from_dependencies(settings, Persistence::for_tests(pool, redis::Client::open(redis)?))
}

async fn authorize(state: &AppState, manifest: &Manifest) -> Result<AuthContext> {
    let session: Value =
        serde_json::from_slice(&pinned(&manifest.session_file, &manifest.session_sha256)?)?;
    let cookie = session
        .pointer(&manifest.session_cookie_json_pointer)
        .and_then(Value::as_str)
        .context("session cookie missing")?;
    ensure!(!cookie.is_empty() && !cookie.contains(';'), "invalid session cookie");
    let mut header = HeaderValue::from_str(&format!("ironrag_ui_session={cookie}"))?;
    header.set_sensitive(true);
    let (mut parts, _) = Request::builder().header(COOKIE, header).body(())?.into_parts();
    let auth = AuthContext::from_request_parts(&mut parts, state)
        .await
        .map_err(|_| anyhow::anyhow!("actual native session authorization failed"))?;
    ensure!(auth.token_kind.is_session(), "actual UI session required");
    let library = load_library_and_authorize(&auth, state, manifest.library_id, POLICY_QUERY_RUN)
        .await
        .map_err(|_| anyhow::anyhow!("native query authorization failed"))?;
    ensure!(library.workspace_id == manifest.workspace_id, "authorized workspace mismatch");
    Ok(auth)
}

async fn check_binding(
    state: &AppState,
    manifest: &Manifest,
    admission: &Admission,
) -> Result<crate::services::ai_catalog_service::ResolvedRuntimeBinding> {
    let binding = state
        .application_services
        .ai_catalog
        .resolve_active_runtime_binding(state, manifest.library_id, AiBindingPurpose::QueryAnswer)
        .await
        .map_err(|_| anyhow::anyhow!("binding resolution failed"))?
        .context("binding missing")?;
    ensure!(
        binding.workspace_id == manifest.workspace_id
            && binding.library_id == manifest.library_id
            && binding.binding_id == admission.binding_id
            && binding.account_id == admission.account_id
            && binding.provider_catalog_id == admission.provider_catalog_id
            && binding.model_catalog_id == admission.model_catalog_id
            && binding.provider_kind == admission.provider_kind
            && binding.model_name == admission.model_name
            && binding.provider_base_url.as_deref() == Some(admission.gateway_base_url.as_str()),
        "native selected provider differs from admission"
    );
    use crate::domains::provider_profiles::{
        OPENAI_COMPATIBLE_RUNTIME_KIND, ProviderStructuredOutputMode,
    };
    ensure!(
        binding.provider_runtime_kind() == Some(OPENAI_COMPATIBLE_RUNTIME_KIND)
            && binding.provider_structured_output_mode()
                == Some(ProviderStructuredOutputMode::JsonSchema),
        "strict native semantic transport required"
    );
    Ok(binding)
}

async fn read_source(
    state: &AppState,
    auth: &AuthContext,
    manifest: &Manifest,
    pin: &SourcePin,
    request_id: &str,
) -> Result<AssistantGroundingEvidence> {
    let revision =
        state.document_store.get_revision(pin.revision_id).await?.context("revision missing")?;
    let canonical = revision.normalized_text.as_deref().context("canonical text missing")?;
    ensure!(
        revision.workspace_id == manifest.workspace_id
            && revision.library_id == manifest.library_id
            && revision.document_id == pin.document_id
            && revision.mime_type == "text/markdown"
            && revision.checksum == format!("sha256:{}", pin.source_file_sha256)
            && revision.text_checksum.as_deref() == Some(pin.canonical_text_sha256.as_str())
            && hash(canonical.as_bytes()) == pin.canonical_text_sha256
            && canonical.len() == pin.canonical_utf8_bytes,
        "entire canonical source identity changed"
    );
    let result = call_named_tool(
        "read_document",
        ToolCallContext { auth, state, request_id, surface_kind: RuntimeSurfaceKind::Ui },
        &json!({"documentId":pin.document_id,"mode":"full","includeReferences":true}),
    )
    .await
    .context("native full read unavailable")?;
    let response: McpReadDocumentResponse =
        serde_json::from_value(result.structured_content.clone())?;
    ensure!(
        !result.is_error
            && matches!(response.read_mode, McpReadMode::Full)
            && matches!(response.readability_state, McpReadabilityState::Readable)
            && response.workspace_id == manifest.workspace_id
            && response.library_id == manifest.library_id
            && response.document_id == pin.document_id
            && response.latest_revision_id == Some(pin.revision_id)
            && response.slice_start_offset == 0
            && response.slice_end_offset == canonical.chars().count()
            && response.total_content_length == Some(canonical.chars().count())
            && !response.has_more
            && response.continuation_token.is_none()
            && response.content.as_deref() == Some(canonical),
        "native full read is incomplete or changed"
    );
    let reference = native_document_read_reference("read_document", &result)
        .context("native full-read provenance rejected")?;
    ensure!(
        reference.document_id == pin.document_id
            && reference.revision_id == Some(pin.revision_id)
            && reference.slice_start_offset == 0
            && reference.slice_end_offset == canonical.chars().count()
            && reference.excerpt == canonical,
        "native reference changed the complete source"
    );
    let audits: i64 = sqlx::query_scalar("select count(*) from audit_event e join audit_event_subject s on s.audit_event_id=e.id where e.request_id=$1 and e.actor_principal_id=$2 and e.action_kind='agent.memory.read' and e.result_kind='succeeded' and s.subject_kind='knowledge_document' and s.document_id=$3 and s.workspace_id=$4 and s.library_id=$5")
        .bind(request_id).bind(auth.principal_id).bind(pin.document_id)
        .bind(manifest.workspace_id).bind(manifest.library_id).fetch_one(&state.persistence.postgres).await?;
    ensure!(audits == 1, "native full-read audit not retained exactly once");
    Ok(AssistantGroundingEvidence {
        verification_corpus: Vec::new(),
        document_references: vec![reference],
    })
}

async fn create_context(
    state: &AppState,
    auth: &AuthContext,
    manifest: &Manifest,
    case: &Case,
    execution_id: Uuid,
    runtime_id: Uuid,
    bundle_id: Uuid,
) -> Result<query_repository::QueryExecutionRow> {
    let conversations: i64 = sqlx::query_scalar(
        "select count(*) from query_conversation where library_id=$1 and request_surface='ui'",
    )
    .bind(manifest.library_id)
    .fetch_one(&state.persistence.postgres)
    .await?;
    ensure!(
        conversations < (CONVERSATION_CAP - 1) as i64,
        "controlled preparation may trigger retention"
    );
    let conversation = query_repository::create_conversation(
        &state.persistence.postgres,
        &query_repository::NewQueryConversation {
            workspace_id: manifest.workspace_id,
            library_id: manifest.library_id,
            created_by_principal_id: Some(auth.principal_id),
            title: Some("Controlled semantic observation (not a generated final)"),
            conversation_state: "active",
            request_surface: "ui",
        },
        CONVERSATION_CAP,
    )
    .await?;
    runtime_repository::create_runtime_execution(
        &state.persistence.postgres,
        &runtime_repository::NewRuntimeExecution {
            id: runtime_id,
            owner_kind: "query_execution",
            owner_id: execution_id,
            task_kind: "query_answer",
            surface_kind: "ui",
            contract_name: "controlled_semantic_observation",
            contract_version: "1",
            lifecycle_state: "accepted",
            active_stage: None,
            turn_budget: 1,
            turn_count: 0,
            parallel_action_limit: 1,
            failure_code: None,
            failure_summary_redacted: None,
            parent_execution_id: None,
        },
    )
    .await?;
    Ok(query_repository::create_execution(
        &state.persistence.postgres,
        &query_repository::NewQueryExecution {
            execution_id,
            context_bundle_id: bundle_id,
            workspace_id: manifest.workspace_id,
            library_id: manifest.library_id,
            conversation_id: conversation.id,
            request_turn_id: None,
            response_turn_id: None,
            binding_id: None,
            runtime_execution_id: runtime_id,
            query_text: &case.question,
            failure_code: None,
        },
    )
    .await?)
}

async fn observe(
    state: &AppState,
    manifest: &Manifest,
    admission: &Admission,
    case: &Case,
    pin: &SourcePin,
    ledger: &Path,
    key: &str,
) -> Result<()> {
    admission_window(admission)?;
    let execution_id = Uuid::now_v7();
    let runtime_id = Uuid::now_v7();
    let bundle_id = Uuid::now_v7();
    let request_id = Uuid::now_v7().to_string();
    write_new(
        ledger,
        &format!("{key}.started.json"),
        &json!({"lifecycleId":admission.lifecycle_id,
        "manifestSha256":admission.manifest_sha256,"caseFileSha256":manifest.case_file_sha256,
        "caseId":case.id,"sourceSha":manifest.source_sha,"runnerSha256":admission.runner_sha256,
        "binarySha256":admission.binary_sha256,"protocolVersion":manifest.protocol_version,
        "questionSha256":hash(case.question.as_bytes()),"checkedAnswerSha256":hash(case.answer.as_bytes()),
        "requestId":request_id,"queryExecutionId":execution_id,"runtimeExecutionId":runtime_id,
        "contextBundleId":bundle_id,"documentId":pin.document_id,"revisionId":pin.revision_id,
        "sourceFileSha256":pin.source_file_sha256,"canonicalTextSha256":pin.canonical_text_sha256,
        "state":"started_not_observed","automaticReplayPermitted":false}),
    )?;
    let result = observe_inner(Observation {
        state,
        manifest,
        admission,
        case,
        pin,
        execution_id,
        runtime_id,
        bundle_id,
        request_id: &request_id,
        ledger,
        key,
    })
    .await;
    if result.is_err() {
        write_new(
            ledger,
            &format!("{key}.failed.json"),
            &json!({"state":"failed_or_unknown",
            "caseId":case.id,"automaticReplayPermitted":false,"semanticAcceptance":"not_established"}),
        )?;
    }
    result
}

fn lifecycle_identity(manifest: &Manifest, admission: &Admission) -> Result<String> {
    Ok(serde_json::to_string(&json!({"protocol":"controlled-semantic-lifecycle/1",
        "lifecycleId":admission.lifecycle_id,"workspaceId":manifest.workspace_id,
        "libraryId":manifest.library_id,"ledgerRoot":admission.ledger_root}))?)
}

fn require_lifecycle_identity(actual: &str, expected: &str) -> Result<()> {
    ensure!(actual == expected, "native lifecycle already owns a different ledger root");
    Ok(())
}

async fn anchor_lifecycle(
    state: &AppState,
    manifest: &Manifest,
    admission: &Admission,
) -> Result<()> {
    // The admitted database, not a caller-selected output directory, is the
    // durable lifecycle owner. Serialize its first native context creation.
    let mut transaction = state.persistence.postgres.begin().await?;
    let lock = i64::from_be_bytes(admission.lifecycle_id.as_bytes()[..8].try_into()?);
    sqlx::query("select pg_advisory_xact_lock($1)").bind(lock).execute(&mut *transaction).await?;
    let expected = lifecycle_identity(manifest, admission)?;
    if let Some(existing) =
        query_repository::get_execution_by_id(&state.persistence.postgres, admission.lifecycle_id)
            .await?
    {
        ensure!(
            existing.workspace_id == manifest.workspace_id
                && existing.library_id == manifest.library_id,
            "lifecycle belongs to another native scope"
        );
        require_lifecycle_identity(&existing.query_text, &expected)?;
    } else {
        let auth = authorize(state, manifest).await?;
        let context = Case {
            id: "lifecycle-owner".into(),
            source: String::new(),
            question: expected,
            answer: String::new(),
        };
        create_context(
            state,
            &auth,
            manifest,
            &context,
            admission.lifecycle_id,
            Uuid::new_v5(&admission.lifecycle_id, b"runtime"),
            Uuid::new_v5(&admission.lifecycle_id, b"bundle"),
        )
        .await?;
    }
    transaction.commit().await?;
    Ok(())
}

struct Observation<'a> {
    state: &'a AppState,
    manifest: &'a Manifest,
    admission: &'a Admission,
    case: &'a Case,
    pin: &'a SourcePin,
    execution_id: Uuid,
    runtime_id: Uuid,
    bundle_id: Uuid,
    request_id: &'a str,
    ledger: &'a Path,
    key: &'a str,
}

async fn observe_inner(input: Observation<'_>) -> Result<()> {
    let Observation {
        state,
        manifest,
        admission,
        case,
        pin,
        execution_id,
        runtime_id,
        bundle_id,
        request_id,
        ledger,
        key,
    } = input;
    let guard = Arc::new(ProviderReadGuard::default());
    let mut preparation_state = state.clone();
    preparation_state.llm_gateway = guard.clone();
    let auth = authorize(&preparation_state, manifest).await?;
    let grounding = read_source(&preparation_state, &auth, manifest, pin, request_id).await?;
    let execution = create_context(
        &preparation_state,
        &auth,
        manifest,
        case,
        execution_id,
        runtime_id,
        bundle_id,
    )
    .await?;
    let mut stage = super::agent_grounding::prepare_agent_verification_stage(
        super::agent_grounding::AgentAnswerVerificationRequest {
            state: &preparation_state,
            execution: &execution,
            context_bundle_id: bundle_id,
            answer_text: &case.answer,
            assistant_grounding: &grounding,
        },
    )
    .await?;
    let not_run = RuntimeAnswerVerification {
        semantic_verification: None,
        state: QueryVerificationState::NotRun,
        warnings: Vec::new(),
        unsupported_literals: Vec::new(),
    };
    persist_query_verification(QueryVerificationPersistence {
        state: &preparation_state,
        execution_id,
        verification: &not_run,
        answer_disposition: QueryAnswerDisposition::NonTerminal,
        clarification: &Default::default(),
        authoritative_evidence: &stage.generation.authoritative_evidence,
        assistant_grounding: &grounding,
        answer_kind: None,
    })
    .await?;
    let bundle = state
        .context_store
        .get_bundle_by_query_execution(execution_id)
        .await?
        .context("persisted source context disappeared")?;
    let retained = bundle
        .assembly_diagnostics
        .pointer("/assistantGrounding/documentReferences")
        .context("native document provenance was not persisted")?;
    ensure!(
        *retained == serde_json::to_value(&grounding.document_references)?,
        "persisted provenance changed"
    );
    let context = QueryProviderExecutionContext {
        workspace_id: manifest.workspace_id,
        library_id: manifest.library_id,
        query_execution_id: execution_id,
        runtime_execution_id: runtime_id,
    };
    let sources = load_semantic_sources(
        &preparation_state,
        context,
        &stage.generation.primary_answer_chunks,
        &stage.generation.authoritative_evidence,
        &stage.generation.assistant_grounding,
    )
    .await?;
    let prepared = prepare(&case.question, &case.answer, sources)?;
    ensure!(
        prepared.sources().len() == 1
            && prepared.sources().iter().all(|source| source.document_id == pin.document_id
                && source.document_revision_id == pin.revision_id
                && source.scope == SemanticEvidenceScope::CompleteDocument
                && source.text_sha256 == pin.canonical_text_sha256
                && source.text.len() == pin.canonical_utf8_bytes
                && hash(source.text.as_bytes()) == pin.canonical_text_sha256),
        "complete native source proof not established"
    );
    ensure!(guard.0.load(Ordering::SeqCst) == 0, "preparation attempted provider work");
    write_new(
        ledger,
        &format!("{key}.prepared.json"),
        &json!({"questionSha256":prepared.question_sha256(),
        "checkedAnswerSha256":prepared.answer_sha256(),"evidenceManifestSha256":prepared.manifest_sha256(),
        "sources":prepared.sources(),"providerPreparationCalls":0}),
    )?;
    let binding = check_binding(state, manifest, admission).await?;
    admission_window(admission)?;
    let mut calls = Vec::new();
    let mut iterations = Vec::new();
    let outcome = with_single_provider_attempt(with_controlled_binding(
        binding,
        verify_selected_candidate(
            SemanticCandidateContext {
                state,
                execution: context,
                question: &case.question,
                reuse: None,
            },
            &mut stage,
            &mut calls,
            &mut iterations,
        ),
    ))
    .await;
    // Retain paid/unknown results before checking any post-dispatch assertion.
    write_new(
        ledger,
        &format!("{key}.observed.json"),
        &json!({"caseId":case.id,
        "orchestrationReturnedOk":outcome.is_ok(),"report":stage.verification.semantic_verification,
        "providerCalls":calls,"iterations":iterations,"formalState":stage.verification.state,
        "warnings":stage.verification.warnings,"semanticAcceptance":"not_evaluated",
        "generatedFinal":false,"automaticReplayPermitted":false}),
    )?;
    outcome?;
    let report =
        stage.verification.semantic_verification.as_ref().context("semantic report missing")?;
    ensure!(
        report.question_sha256 == prepared.question_sha256()
            && report.checked_answer_sha256 == prepared.answer_sha256()
            && report.evidence_manifest_sha256 == prepared.manifest_sha256()
            && matches!(report.outcome, SemanticVerificationOutcome::Completed { .. }),
        "semantic operation failed, refused, not-run or lost identity"
    );
    ensure!(calls.len() == 1 && report.provider_call_id == calls.first().map(|call| call.provider_call_id())
        && calls.iter().all(|call| call.binding_id() == admission.binding_id
            && call.provider().provider_kind == admission.provider_kind
            && call.provider().model_name == admission.model_name
            && call.binding_purpose() == AiBindingPurpose::QueryAnswer
            && call.call_kind() == crate::agent_runtime::tasks::query_answer::QueryProviderCallKind::QueryAnswerSemanticVerification),
        "native provider attribution not established");
    Ok(())
}

#[tokio::test]
#[ignore = "fresh independently accepted native financial/target admission required; never ordinary CI"]
async fn measure_authored_candidates() -> Result<()> {
    let result = async {
        // This guard precedes settings, credentials, clients, DB and provider construction.
        let (manifest_path, admission_path) =
            explicit_paths(std::env::var(MANIFEST_ENV).ok(), std::env::var(ADMISSION_ENV).ok())?;
        let manifest_bytes = read_bounded(&manifest_path)?;
        let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
        let admission: Admission = serde_json::from_slice(&read_bounded(&admission_path)?)?;
        let cases = validate_inputs(&manifest, &manifest_bytes, &admission)?;
        let parent = admission.ledger_root.canonicalize()?;
        ensure!(
            parent == admission.ledger_root && parent.is_absolute(),
            "canonical admitted ledger root required"
        );
        ensure!(
            parent.metadata()?.permissions().mode() & 0o077 == 0,
            "private admission directory required"
        );
        // One canonical ledger per immutable lifecycle beside its accepted admission.
        let ledger = parent.join(format!("semantic-observations-{}", admission.lifecycle_id));
        match std::fs::DirBuilder::new().mode(0o700).create(&ledger) {
            Ok(()) => File::open(&parent)?.sync_all()?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                ensure!(
                    ledger.is_dir() && !ledger.symlink_metadata()?.file_type().is_symlink(),
                    "invalid ledger"
                );
            }
            Err(error) => return Err(error.into()),
        }
        let state = build_state(&manifest, &admission).await?;
        anchor_lifecycle(&state, &manifest, &admission).await?;
        for case in cases {
            let pin = manifest
                .sources
                .iter()
                .find(|source| source.label == case.source)
                .context("source pin missing")?;
            let key = observation_key(admission.lifecycle_id, &manifest, &case, pin)?;
            observe(&state, &manifest, &admission, &case, pin, &ledger, &key).await?;
        }
        Ok(())
    }
    .await;
    // Do not print underlying DSN, cookie, provider or settings diagnostics.
    result.map_err(|_: anyhow::Error| anyhow::anyhow!(
        "controlled semantic observation refused or incomplete; retain the first-observation ledger; no automatic replay"))
}

#[test]
fn controlled_semantic_requires_admission_before_initialization() {
    assert!(explicit_paths(None, None).is_err());
    assert!(explicit_paths(Some("unread-manifest".into()), None).is_err());
    assert!(explicit_paths(None, Some("unread-admission".into())).is_err());
}

#[test]
fn controlled_semantic_binds_case_bytes_and_exact_selection() {
    let bytes = br#"{"sources":{"s":{"sha256":"digest"}},"cases":[{"id":"a","source":"s","question":"q","answer":"a","expected":{"support":"supported"}},{"id":"b","source":"s","question":"q2","answer":"a2"}]}"#;
    let file = || serde_json::from_slice::<CaseFile>(bytes).unwrap();
    let cases = select_cases(file(), &["b".into()]).unwrap();
    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].question, "q2");
    assert!(select_cases(file(), &["b".into(), "b".into()]).is_err());
    assert!(select_cases(file(), &["missing".into()]).is_err());
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("cases.json");
    std::fs::write(&path, bytes).unwrap();
    assert_eq!(pinned(&path, &hash(bytes)).unwrap(), bytes);
    assert!(pinned(&path, "changed").is_err());
}

#[test]
fn controlled_semantic_rejects_duplicate_cases() {
    let bytes = br#"{"sources":{"s":{"sha256":"digest"}},"cases":[{"id":"a","source":"s","question":"q","answer":"a"},{"id":"a","source":"s","question":"q2","answer":"a2"}]}"#;
    let file = serde_json::from_slice(bytes).unwrap();
    assert!(select_cases(file, &["a".into()]).is_err());
}

#[test]
fn controlled_semantic_started_record_survives_interruption() {
    let root = tempfile::tempdir().unwrap();
    write_new(root.path(), "identity.started.json", &json!({"state":"started_not_observed"}))
        .unwrap();
    assert!(write_new(root.path(), "identity.started.json", &json!({"state":"retry"})).is_err());
    assert!(!root.path().join("identity.observed.json").exists());
}

#[test]
fn controlled_semantic_pins_inputs_and_rejects_changed_preservation() {
    let root = tempfile::tempdir().unwrap();
    let (mut manifest, bytes, admission) = offline_inputs(root.path());
    let cases = validate_inputs(&manifest, &bytes, &admission).unwrap();
    assert_eq!(cases.len(), 1);
    assert!(validate_inputs(&manifest, b"changed manifest", &admission).is_err());
    std::fs::write(&manifest.sources[0].source_file, b"changed source").unwrap();
    assert!(validate_inputs(&manifest, &bytes, &admission).is_err());
    std::fs::write(&manifest.sources[0].source_file, b"complete source").unwrap();
    let preservation_bytes =
        pinned(&manifest.preservation_file, &manifest.preservation_sha256).unwrap();
    let mut preservation: Value = serde_json::from_slice(&preservation_bytes).unwrap();
    preservation["sources"][0]["completeSourceCoverageVerified"] = json!(false);
    let changed = serde_json::to_vec(&preservation).unwrap();
    std::fs::write(&manifest.preservation_file, &changed).unwrap();
    manifest.preservation_sha256 = hash(&changed);
    assert!(validate_inputs(&manifest, &bytes, &admission).is_err());
}

#[test]
fn controlled_semantic_identity_cannot_reopen_by_annotation_or_subset_change() {
    let root = tempfile::tempdir().unwrap();
    let (mut manifest, bytes, admission) = offline_inputs(root.path());
    let mut cases = validate_inputs(&manifest, &bytes, &admission).unwrap();
    let first = observation_key(admission.lifecycle_id, &manifest, &cases[0], &manifest.sources[0])
        .unwrap();
    manifest.case_file_sha256 = "different annotations".into();
    cases[0].id = "renamed annotation".into();
    assert_eq!(
        first,
        observation_key(admission.lifecycle_id, &manifest, &cases[0], &manifest.sources[0])
            .unwrap()
    );
    cases[0].answer.push_str("changed");
    assert_ne!(
        first,
        observation_key(admission.lifecycle_id, &manifest, &cases[0], &manifest.sources[0])
            .unwrap()
    );
}

#[test]
fn controlled_semantic_window_stops_before_the_provider_deadline() {
    let root = tempfile::tempdir().unwrap();
    let (_, _, mut admission) = offline_inputs(root.path());
    let now: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
    admission.not_before = now;
    admission.hard_stop_at = now + chrono::Duration::seconds(13);
    assert!(admission_window_at(&admission, now).is_ok());
    assert!(admission_window_at(&admission, now - chrono::Duration::seconds(1)).is_err());
    assert!(admission_window_at(&admission, now + chrono::Duration::seconds(1)).is_err());
}

#[test]
fn controlled_semantic_native_lifecycle_rejects_another_ledger_root() {
    let root = tempfile::tempdir().unwrap();
    let (manifest, _, mut admission) = offline_inputs(root.path());
    let original = lifecycle_identity(&manifest, &admission).unwrap();
    assert!(require_lifecycle_identity(&original, &original).is_ok());
    admission.ledger_root = root.path().join("different-output-root");
    assert!(
        require_lifecycle_identity(&original, &lifecycle_identity(&manifest, &admission).unwrap())
            .is_err()
    );
}

#[test]
fn controlled_semantic_requires_independent_binary_source_binding() {
    let root = tempfile::tempdir().unwrap();
    let (mut manifest, _, mut admission) = offline_inputs(root.path());
    assert!(require_build_receipt(&manifest, &admission).is_ok());
    manifest.source_sha = "changed-source".into();
    assert!(require_build_receipt(&manifest, &admission).is_err());
    manifest.source_sha = "offline fixture".into();
    admission.binary_sha256 = "changed-binary".into();
    assert!(require_build_receipt(&manifest, &admission).is_err());
}

fn offline_inputs(root: &Path) -> (Manifest, Vec<u8>, Admission) {
    let id = Uuid::from_u128(1);
    let source = b"complete source";
    let digest = hash(source);
    let source_path = root.join("source.md");
    let case_path = root.join("cases.json");
    let coverage_path = root.join("coverage.json");
    let preservation_path = root.join("preservation.json");
    let build_receipt_path = root.join("build-receipt.json");
    let build_evidence_path = root.join("build-evidence.json");
    std::fs::write(&build_evidence_path, b"offline binding fixture, not a real build proof")
        .unwrap();
    let build_receipt = serde_json::to_vec(&json!({"protocol":"ironrag-semantic-control-build/1",
        "sourceSha":"offline fixture","binarySha256":"not executed",
        "runnerSha256":hash(include_bytes!("controlled_semantic.rs")),
        "independentReviewer":"offline fixture","reproducibleSourceBindingVerified":true,
        "evidenceFile":build_evidence_path,
        "evidenceSha256":hash(b"offline binding fixture, not a real build proof")}))
    .unwrap();
    std::fs::write(&build_receipt_path, &build_receipt).unwrap();
    std::fs::write(&source_path, source).unwrap();
    std::fs::write(&coverage_path, b"offline coverage fixture").unwrap();
    let case_bytes = serde_json::to_vec(&json!({"sources":{"source-a":{"sha256":digest}},
        "cases":[{"id":"a","source":"source-a","question":"q","answer":"a"}]}))
    .unwrap();
    std::fs::write(&case_path, &case_bytes).unwrap();
    let preservation =
        serde_json::to_vec(&json!({"lifecycleId":id,"workspaceId":id,"libraryId":id,
        "independentReviewer":"offline fixture, not an acceptance receipt","sources":[{
            "label":"source-a","documentId":id,"revisionId":id,"sourceFileSha256":digest,
            "canonicalTextSha256":digest,"canonicalUtf8Bytes":source.len(),
            "completeSourceCoverageVerified":true,"coverageEvidenceFile":coverage_path,
            "coverageEvidenceSha256":hash(b"offline coverage fixture")}]}))
        .unwrap();
    std::fs::write(&preservation_path, &preservation).unwrap();
    let manifest_bytes = serde_json::to_vec(&json!({"protocol":"ironrag-semantic-controls/1",
        "protocolVersion":SEMANTIC_VERIFICATION_PROTOCOL_VERSION,"sourceSha":"offline fixture",
        "caseFile":case_path,"caseFileSha256":hash(&case_bytes),"selectedCaseIds":["a"],
        "workspaceId":id,"libraryId":id,"databaseUrlSha256":"not read","redisUrlSha256":"not read",
        "sessionFile":root.join("not-read-session"),"sessionSha256":"not read",
        "sessionCookieJsonPointer":"/cookie","preservationFile":preservation_path,
        "preservationSha256":hash(&preservation),"sources":[{"label":"source-a",
            "sourceFile":source_path,"sourceFileSha256":digest,"documentId":id,"revisionId":id,
            "canonicalTextSha256":digest,"canonicalUtf8Bytes":source.len()}]}))
    .unwrap();
    let admission: Admission =
        serde_json::from_value(json!({"protocol":"ironrag-semantic-control-admission/1",
        "lifecycleId":id,"ledgerRoot":root,"manifestSha256":hash(&manifest_bytes),
        "runnerSha256":hash(include_bytes!("controlled_semantic.rs")),"binarySha256":"not executed",
        "buildReceiptFile":build_receipt_path,"buildReceiptSha256":hash(&build_receipt),
        "independentReviewer":"offline fixture","freshGrantReference":"not a grant",
        "notBefore":Utc::now() - chrono::Duration::seconds(30),
        "hardStopAt":Utc::now() + chrono::Duration::minutes(5),"maxPhysicalCalls":1,
        "bindingId":id,"accountId":id,"providerCatalogId":id,"modelCatalogId":id,
        "providerKind":"not executed","modelName":"not executed","gatewayBaseUrl":"not executed"}))
        .unwrap();
    (serde_json::from_slice(&manifest_bytes).unwrap(), manifest_bytes, admission)
}
