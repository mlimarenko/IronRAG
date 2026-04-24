//! `QueryCompiler` — natural language → typed [`QueryIR`].
//!
//! This is the canonical entry point for the whole query pipeline. Every
//! downstream stage (planner, retrieval, ranking, verification, answer
//! generation, session follow-up) must read its routing signals from the IR
//! this service produces, never by re-classifying the raw question with
//! hardcoded keyword lists.
//!
//! The service calls the LLM bound to `AiBindingPurpose::QueryCompile` via the
//! same `UnifiedGateway` / provider abstraction that powers every other
//! pipeline stage. The operator picks which provider/model compiles queries
//! exactly the way they pick `QueryAnswer` or `ExtractGraph` — through
//! `/ai/bindings` at instance / workspace / library scope. No model is
//! hardcoded in this file.
//!
//! Robustness guarantees:
//! - If provider binding resolution fails (no binding configured) or the
//!   provider call itself fails, the compiler returns a **fallback IR**
//!   (`QueryAct::Describe` / `QueryScope::SingleDocument` / `confidence: 0.0`)
//!   so the rest of the pipeline keeps working in a degraded but safe mode.
//!   Callers inspect `fallback_reason` to decide whether to surface a
//!   warning.
//! - Provider output that fails JSON-schema validation also returns a
//!   fallback IR with the parse error recorded in `fallback_reason`.

use std::sync::Arc;

use async_trait::async_trait;
use redis::{AsyncCommands, Client as RedisClient};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        ai::AiBindingPurpose,
        query_ir::{
            QUERY_IR_SCHEMA_VERSION, QueryAct, QueryIR, QueryLanguage, QueryScope,
            VerificationLevel, query_ir_json_schema,
        },
    },
    infra::repositories::query_ir_cache_repository::{get_query_ir_cache, upsert_query_ir_cache},
    integrations::llm::{ChatRequestSeed, LlmGateway, build_structured_chat_request},
    interfaces::http::router_support::ApiError,
    services::ai_catalog_service::ResolvedRuntimeBinding,
};

/// Canonical Redis key prefix for the hot IR cache. The trailing `v1`
/// distinguishes this cache namespace from unrelated Redis key families —
/// it is NOT the IR `schema_version` (that travels in the value / hash).
const REDIS_IR_CACHE_PREFIX: &str = "ir_cache:v1";

/// Hot-tier TTL. Chosen so even low-traffic libraries see regular warm
/// reads without pinning stale IR past a day.
pub const REDIS_IR_CACHE_TTL_SECS: u64 = 24 * 60 * 60;

/// Sentinel `provider_kind` values for cache hits so downstream logging /
/// usage aggregation can tell compiled-by-LLM apart from served-from-cache
/// without a separate field on `CompileQueryOutcome`.
pub const CACHE_HIT_REDIS_PROVIDER_KIND: &str = "cache:redis";
pub const CACHE_HIT_POSTGRES_PROVIDER_KIND: &str = "cache:postgres";

/// Turn the conversation resolver feeds in so the compiler can spot
/// anaphora / deixis across turns. Kept deliberately thin — only the last
/// few turns matter and the compiler will not crawl full history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompileHistoryTurn {
    /// `"user"` or `"assistant"`.
    pub role: String,
    /// Short excerpt (caller is responsible for trimming to a reasonable
    /// length — ~500 chars per turn is plenty).
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct CompileQueryCommand {
    pub library_id: Uuid,
    pub question: String,
    /// Last N turns of conversation, ordered oldest → newest. Empty for the
    /// first turn in a session. The compiler only uses this to detect
    /// unresolved references — it is NOT fed to downstream retrieval.
    pub history: Vec<CompileHistoryTurn>,
}

#[derive(Debug, Clone)]
pub struct CompileQueryOutcome {
    pub ir: QueryIR,
    pub provider_kind: String,
    pub model_name: String,
    pub usage_json: serde_json::Value,
    /// `None` on the canonical success path. `Some(reason)` when the
    /// compiler fell back to a default IR (binding missing, provider
    /// outage, invalid model output). Callers should surface this as a
    /// non-fatal diagnostic in the query execution record.
    pub fallback_reason: Option<String>,
    /// `true` when this outcome was served from the two-tier cache
    /// (Redis or Postgres) instead of a live LLM call. Billing must
    /// skip cache hits so repeat questions do not double-charge the
    /// same token usage.
    pub served_from_cache: bool,
}

impl CompileQueryOutcome {
    /// Convenience for logging / diagnostics.
    #[must_use]
    pub fn verification_level(&self) -> VerificationLevel {
        self.ir.verification_level()
    }
}

/// Abstraction over the two-tier (Redis + Postgres) compiled-IR cache so
/// unit tests can substitute an in-memory fake while production wires the
/// real `Persistence` handles. The trait is intentionally thin — the
/// compiler only needs a keyed get / put; cache coherence between the
/// tiers (Redis warmup on pg hit, writing to both on miss) belongs to the
/// concrete implementation.
#[async_trait]
pub trait QueryIrCache: Send + Sync {
    /// Return a cached outcome for `(library_id, question_hash)` if one is
    /// available under the current schema version, or `None` on miss /
    /// transient error (errors are logged and treated as misses — the
    /// cache must never fail the compile pipeline).
    async fn get(&self, library_id: Uuid, question_hash: &str) -> Option<CachedIrEntry>;

    /// Write a freshly compiled IR to every tier that can accept it.
    /// Errors are logged inside the implementation; callers continue
    /// regardless so a cache outage never propagates into the query
    /// pipeline.
    async fn put(&self, library_id: Uuid, question_hash: &str, entry: &CachedIrEntry);
}

/// Shape persisted under one cache key. `provider_kind` / `model_name` /
/// `usage_json` are retained so a cache-served outcome can still render
/// accurate diagnostics in the query execution record.
#[derive(Debug, Clone)]
pub struct CachedIrEntry {
    pub ir: QueryIR,
    pub provider_kind: String,
    pub model_name: String,
    pub usage_json: Value,
}

/// Production cache implementation. Redis is the hot tier (24h TTL);
/// Postgres is the persistent (debug) tier. A cache miss on Redis but a
/// hit on Postgres triggers a Redis warmup so subsequent reads stay
/// fast.
pub struct PersistenceQueryIrCache<'a> {
    pub pool: &'a PgPool,
    pub redis: &'a RedisClient,
    pub schema_version: u16,
}

impl<'a> PersistenceQueryIrCache<'a> {
    #[must_use]
    pub fn new(pool: &'a PgPool, redis: &'a RedisClient) -> Self {
        Self { pool, redis, schema_version: QUERY_IR_SCHEMA_VERSION }
    }

    fn schema_version_pg(&self) -> i16 {
        i16::try_from(self.schema_version).unwrap_or(i16::MAX)
    }
}

#[async_trait]
impl<'a> QueryIrCache for PersistenceQueryIrCache<'a> {
    async fn get(&self, library_id: Uuid, question_hash: &str) -> Option<CachedIrEntry> {
        if let Some(entry) = redis_get_ir(self.redis, library_id, question_hash).await {
            return Some(CachedIrEntry {
                ir: entry,
                provider_kind: CACHE_HIT_REDIS_PROVIDER_KIND.to_string(),
                model_name: String::new(),
                usage_json: json!({"source": "redis"}),
            });
        }

        let row = match get_query_ir_cache(
            self.pool,
            library_id,
            question_hash,
            self.schema_version_pg(),
        )
        .await
        {
            Ok(row) => row,
            Err(error) => {
                tracing::warn!(
                    %library_id,
                    question_hash,
                    ?error,
                    "query_ir_cache postgres lookup failed — treating as miss"
                );
                return None;
            }
        };

        let row = row?;
        let ir: QueryIR = match serde_json::from_value(row.query_ir_json.clone()) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(
                    %library_id,
                    question_hash,
                    ?error,
                    "query_ir_cache row failed to parse as QueryIR — treating as miss"
                );
                return None;
            }
        };

        // Warm the hot tier so the next read does not pay the pg round trip.
        redis_set_ir(self.redis, library_id, question_hash, &ir, REDIS_IR_CACHE_TTL_SECS).await;

        Some(CachedIrEntry {
            ir,
            provider_kind: CACHE_HIT_POSTGRES_PROVIDER_KIND.to_string(),
            model_name: String::new(),
            usage_json: json!({
                "source": "postgres",
                "original_provider_kind": row.provider_kind,
                "original_model_name": row.model_name,
            }),
        })
    }

    async fn put(&self, library_id: Uuid, question_hash: &str, entry: &CachedIrEntry) {
        let ir_json = match serde_json::to_value(&entry.ir) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(
                    %library_id,
                    question_hash,
                    ?error,
                    "query_ir_cache failed to serialize IR — skipping cache write"
                );
                return;
            }
        };

        if let Err(error) = upsert_query_ir_cache(
            self.pool,
            library_id,
            question_hash,
            self.schema_version_pg(),
            ir_json,
            Some(entry.provider_kind.as_str()).filter(|v| !v.is_empty()),
            Some(entry.model_name.as_str()).filter(|v| !v.is_empty()),
            entry.usage_json.clone(),
        )
        .await
        {
            tracing::warn!(
                %library_id,
                question_hash,
                ?error,
                "query_ir_cache postgres upsert failed — continuing without persistent cache"
            );
        }

        redis_set_ir(self.redis, library_id, question_hash, &entry.ir, REDIS_IR_CACHE_TTL_SECS)
            .await;
    }
}

/// Compute the canonical cache key hash for one `(question, history,
/// schema_version)` triple. The hash is content-addressed: equal inputs
/// produce equal keys regardless of trailing whitespace or letter case so
/// trivially-reworded repeats share a cache entry.
#[must_use]
pub fn hash_question(
    question: &str,
    history: &[CompileHistoryTurn],
    schema_version: u16,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"v");
    hasher.update(schema_version.to_be_bytes());
    hasher.update(b"|q|");
    hasher.update(normalize(question).as_bytes());
    for turn in history {
        hasher.update(b"|t|");
        hasher.update(normalize(&turn.role).as_bytes());
        hasher.update(b":");
        hasher.update(normalize(&turn.content).as_bytes());
    }
    hex::encode(hasher.finalize())
}

fn normalize(value: &str) -> String {
    value.trim().to_lowercase()
}

async fn redis_get_ir(
    redis: &RedisClient,
    library_id: Uuid,
    question_hash: &str,
) -> Option<QueryIR> {
    let key = redis_key(library_id, question_hash);
    let mut conn = match redis.get_multiplexed_async_connection().await {
        Ok(conn) => conn,
        Err(error) => {
            tracing::warn!(?error, "query_ir_cache redis connect failed — treating as miss");
            return None;
        }
    };
    let raw: Option<String> = match conn.get(&key).await {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(key, ?error, "query_ir_cache redis GET failed — treating as miss");
            return None;
        }
    };
    let raw = raw?;
    match serde_json::from_str::<QueryIR>(&raw) {
        Ok(ir) => Some(ir),
        Err(error) => {
            tracing::warn!(key, ?error, "query_ir_cache redis payload is not valid IR — miss");
            None
        }
    }
}

async fn redis_set_ir(
    redis: &RedisClient,
    library_id: Uuid,
    question_hash: &str,
    ir: &QueryIR,
    ttl_secs: u64,
) {
    let key = redis_key(library_id, question_hash);
    let payload = match serde_json::to_string(ir) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(key, ?error, "query_ir_cache redis serialize failed — skipping");
            return;
        }
    };
    let mut conn = match redis.get_multiplexed_async_connection().await {
        Ok(conn) => conn,
        Err(error) => {
            tracing::warn!(?error, "query_ir_cache redis connect failed — skipping write");
            return;
        }
    };
    if let Err(error) = conn.set_ex::<_, _, ()>(&key, payload, ttl_secs.max(1)).await {
        tracing::warn!(key, ?error, "query_ir_cache redis SET EX failed — skipping");
    }
}

fn redis_key(library_id: Uuid, question_hash: &str) -> String {
    format!("{REDIS_IR_CACHE_PREFIX}:{library_id}:{question_hash}")
}

/// Stateless service — all dependencies come through `AppState`.
#[derive(Debug, Default, Clone, Copy)]
pub struct QueryCompilerService;

impl QueryCompilerService {
    /// Canonical entry point. Lookup order is:
    ///
    /// 1. Hash the `(question, history, schema_version)` triple.
    /// 2. Redis hot tier — on hit, return without touching binding
    ///    resolution or the LLM.
    /// 3. Postgres persistent tier — on hit, warm Redis and return.
    /// 4. Miss: resolve the active `QueryCompile` binding, call the LLM,
    ///    write through to both tiers on the canonical success path.
    ///    Fallback outcomes (`fallback_reason.is_some()`) are NEVER
    ///    cached so a transient binding / provider outage does not
    ///    freeze a degraded IR into both tiers.
    pub async fn compile(
        &self,
        state: &AppState,
        command: CompileQueryCommand,
    ) -> Result<CompileQueryOutcome, ApiError> {
        let cache =
            PersistenceQueryIrCache::new(&state.persistence.postgres, &state.persistence.redis);
        let question_hash =
            hash_question(&command.question, &command.history, QUERY_IR_SCHEMA_VERSION);

        if let Some(entry) = cache.get(command.library_id, &question_hash).await {
            return Ok(cached_outcome(entry));
        }

        let binding = match state
            .canonical_services
            .ai_catalog
            .resolve_active_runtime_binding(
                state,
                command.library_id,
                AiBindingPurpose::QueryCompile,
            )
            .await?
        {
            Some(binding) => binding,
            None => {
                tracing::warn!(
                    library_id = %command.library_id,
                    "query_compile binding is not configured — returning fallback IR"
                );
                return Ok(fallback_outcome("binding_not_configured"));
            }
        };

        let outcome = self
            .compile_with_gateway(
                state.llm_gateway.as_ref(),
                &binding,
                &command.question,
                &command.history,
            )
            .await?;

        if outcome.fallback_reason.is_none() {
            cache
                .put(
                    command.library_id,
                    &question_hash,
                    &CachedIrEntry {
                        ir: outcome.ir.clone(),
                        provider_kind: outcome.provider_kind.clone(),
                        model_name: outcome.model_name.clone(),
                        usage_json: outcome.usage_json.clone(),
                    },
                )
                .await;
        }

        Ok(outcome)
    }

    /// Testable variant that takes an explicit cache handle and gateway.
    /// Mirrors the public `compile` path but skips `AppState` so unit
    /// tests can substitute an in-memory cache and a stub gateway.
    pub async fn compile_with_cache_and_gateway(
        &self,
        cache: &dyn QueryIrCache,
        gateway: &dyn LlmGateway,
        binding: &ResolvedRuntimeBinding,
        library_id: Uuid,
        question: &str,
        history: &[CompileHistoryTurn],
    ) -> Result<CompileQueryOutcome, ApiError> {
        let question_hash = hash_question(question, history, QUERY_IR_SCHEMA_VERSION);

        if let Some(entry) = cache.get(library_id, &question_hash).await {
            return Ok(cached_outcome(entry));
        }

        let outcome = self.compile_with_gateway(gateway, binding, question, history).await?;

        if outcome.fallback_reason.is_none() {
            cache
                .put(
                    library_id,
                    &question_hash,
                    &CachedIrEntry {
                        ir: outcome.ir.clone(),
                        provider_kind: outcome.provider_kind.clone(),
                        model_name: outcome.model_name.clone(),
                        usage_json: outcome.usage_json.clone(),
                    },
                )
                .await;
        }

        Ok(outcome)
    }

    /// Lower-level entry point used by the OpenAI smoke test and by
    /// integration tests that already hold a concrete binding + gateway.
    /// Production callers use [`Self::compile`].
    pub async fn compile_with_gateway(
        &self,
        gateway: &dyn LlmGateway,
        binding: &ResolvedRuntimeBinding,
        question: &str,
        history: &[CompileHistoryTurn],
    ) -> Result<CompileQueryOutcome, ApiError> {
        let schema = query_ir_json_schema();
        let response_format = json!({
            "type": "json_schema",
            "json_schema": {
                "name": "query_ir",
                "strict": true,
                "schema": schema,
            }
        });

        let prompt = build_compile_prompt(question, history);
        let system_prompt = binding
            .system_prompt
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .map_or_else(|| QUERY_COMPILER_SYSTEM_PROMPT.to_string(), ToOwned::to_owned);

        let seed = ChatRequestSeed {
            provider_kind: binding.provider_kind.clone(),
            model_name: binding.model_name.clone(),
            api_key_override: binding.api_key.clone(),
            base_url_override: binding.provider_base_url.clone(),
            system_prompt: Some(system_prompt),
            temperature: binding.temperature,
            top_p: binding.top_p,
            max_output_tokens_override: binding.max_output_tokens_override,
            extra_parameters_json: binding.extra_parameters_json.clone(),
        };
        let request = build_structured_chat_request(seed, prompt, response_format);

        let response = match gateway.generate(request).await {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(
                    provider = %binding.provider_kind,
                    model = %binding.model_name,
                    ?error,
                    "query compile provider call failed — returning fallback IR"
                );
                return Ok(fallback_outcome("provider_call_failed"));
            }
        };

        let ir: QueryIR = match serde_json::from_str(&response.output_text) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(
                    provider = %binding.provider_kind,
                    model = %binding.model_name,
                    output_preview = %preview(&response.output_text, 200),
                    ?error,
                    "query compile output is not valid QueryIR JSON — returning fallback IR"
                );
                return Ok(fallback_outcome("invalid_ir_output"));
            }
        };
        let ir = normalize_compiled_ir(question, history, ir);

        tracing::info!(
            target: "ironrag::query_compile",
            provider = %response.provider_kind,
            model = %response.model_name,
            act = ir.act.as_str(),
            scope = ir.scope.as_str(),
            language = ir.language.as_str(),
            target_types = ?ir.target_types,
            literal_constraints_count = ir.literal_constraints.len(),
            conversation_refs_count = ir.conversation_refs.len(),
            confidence = ir.confidence,
            "query compiled"
        );

        #[cfg(debug_assertions)]
        debug_assert!(
            crate::domains::query_ir::validate_ir(&ir).is_ok(),
            "compiled QueryIR failed self-consistency checks: {:?}",
            crate::domains::query_ir::validate_ir(&ir).err()
        );

        Ok(CompileQueryOutcome {
            ir,
            provider_kind: response.provider_kind,
            model_name: response.model_name,
            usage_json: response.usage_json,
            fallback_reason: None,
            served_from_cache: false,
        })
    }
}

/// Lift a cached entry into the normal success-path outcome shape. The
/// `provider_kind` field carries a `cache:*` sentinel so downstream
/// diagnostics can tell LLM-compiled from cache-served compilations apart
/// without a separate flag.
fn cached_outcome(entry: CachedIrEntry) -> CompileQueryOutcome {
    CompileQueryOutcome {
        ir: entry.ir,
        provider_kind: entry.provider_kind,
        model_name: entry.model_name,
        usage_json: entry.usage_json,
        fallback_reason: None,
        served_from_cache: true,
    }
}

fn fallback_outcome(reason: &str) -> CompileQueryOutcome {
    CompileQueryOutcome {
        ir: fallback_ir(),
        provider_kind: String::new(),
        model_name: String::new(),
        usage_json: json!({
            "aggregation": "none",
            "call_count": 0,
        }),
        fallback_reason: Some(reason.to_string()),
        served_from_cache: false,
    }
}

fn normalize_compiled_ir(
    question: &str,
    history: &[CompileHistoryTurn],
    mut ir: QueryIR,
) -> QueryIR {
    if history.is_empty()
        && matches!(ir.act, QueryAct::FollowUp)
        && stateless_ir_has_explicit_target(&ir)
    {
        tracing::info!(
            target: "ironrag::query_compile",
            question_len = question.len(),
            target_entities_count = ir.target_entities.len(),
            has_document_focus = ir.document_focus.is_some(),
            literal_constraints_count = ir.literal_constraints.len(),
            "query compile repaired stateless follow_up IR"
        );
        // A stateless call has no prior turn to resolve. If the IR still
        // carries an explicit target, it is a standalone question and must
        // stay on the grounded single-shot path; the raw question text still
        // tells the answer model whether the user asked for a procedure.
        ir.act = QueryAct::Describe;
        ir.conversation_refs.clear();
    }
    ir
}

fn stateless_ir_has_explicit_target(ir: &QueryIR) -> bool {
    !ir.target_entities.is_empty()
        || ir.document_focus.as_ref().is_some_and(|hint| !hint.hint.trim().is_empty())
        || !ir.literal_constraints.is_empty()
}

/// Safe default when the compiler cannot run. Chosen so no downstream stage
/// misroutes: `Describe` + `SingleDocument` is the "generic descriptive"
/// path, verification is `Lenient`, and `confidence = 0.0` signals callers
/// to prefer asking the user instead of building on the IR.
fn fallback_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::Describe,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: Vec::new(),
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        confidence: 0.0,
    }
}

const QUERY_COMPILER_SYSTEM_PROMPT: &str = "You are the IronRAG query compiler. Your only job is to \
read the user's natural-language question and, where present, a short window of prior conversation \
turns, and return a typed QueryIR JSON object. The JSON schema is supplied through structured \
outputs; you MUST follow it exactly and MUST NOT add prose, commentary, code fences, or extra \
fields.\n\
\n\
Guiding principles:\n\
1. `act` captures what the user is fundamentally asking: `retrieve_value` (exact value), \
`describe`, `configure_how` (procedure), `compare`, `enumerate`, `meta` (about the library itself), \
or `follow_up` (refers to prior turn).\n\
2. `scope` is `multi_document` ONLY when the user explicitly names or clearly implies two or more \
documents / modules / subjects; `library_meta` when the question is about the library itself. \
Default is `single_document`.\n\
3. `literal_constraints` captures verbatim strings the user quoted — URLs, file paths, parameter \
names, code identifiers, version numbers. If the user did not quote anything verbatim, the array \
is empty.\n\
4. `conversation_refs` lists unresolved anaphora / deixis / ellipsis you observe in the current \
question. `act = follow_up` is typical when the question cannot stand on its own.\n\
5. `target_types` are free-form ontology tags (examples: endpoint, port, parameter, procedure, \
protocol, config_key, error_code, env_var, metric, table_row, document, concept). You may invent a \
new tag if nothing fits — the system grows its ontology from your output.\n\
6. `confidence` ∈ [0.0, 1.0]. Use < 0.6 only when you genuinely cannot pin the question.\n\
7. `language` is `ru` / `en` / `auto`. Detect from the question text.\n\
\n\
Output nothing but the JSON object described by the schema.";

/// Build the user-side prompt: prior turns (if any) plus the current question.
fn build_compile_prompt(question: &str, history: &[CompileHistoryTurn]) -> String {
    let mut buffer = String::new();
    if !history.is_empty() {
        buffer.push_str("# Prior conversation (oldest first)\n");
        for turn in history {
            buffer.push_str("- ");
            buffer.push_str(&turn.role);
            buffer.push_str(": ");
            buffer.push_str(turn.content.trim());
            buffer.push('\n');
        }
        buffer.push('\n');
    }
    buffer.push_str("# Current question\n");
    buffer.push_str(question.trim());
    buffer.push('\n');
    buffer
}

fn preview(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut out = String::new();
        for (index, ch) in text.chars().enumerate() {
            if index >= max {
                break;
            }
            out.push(ch);
        }
        out.push('…');
        out
    }
}

/// Test-only alias exposed so downstream callers can depend on a single
/// concrete type; `Arc` is used only because some call sites keep the
/// service inside `AppState` alongside other canonical services.
#[allow(dead_code)]
pub type SharedQueryCompilerService = Arc<QueryCompilerService>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::llm::{
        ChatRequest, ChatResponse, EmbeddingBatchRequest, EmbeddingBatchResponse, EmbeddingRequest,
        EmbeddingResponse, VisionRequest, VisionResponse,
    };
    use std::collections::HashMap;
    use std::sync::Mutex;

    struct StubGateway {
        output: Mutex<Option<Result<ChatResponse, anyhow::Error>>>,
        last_request: Mutex<Option<ChatRequest>>,
    }

    impl StubGateway {
        fn new(output: Result<ChatResponse, anyhow::Error>) -> Self {
            Self { output: Mutex::new(Some(output)), last_request: Mutex::new(None) }
        }
    }

    #[async_trait]
    impl LlmGateway for StubGateway {
        async fn generate(&self, request: ChatRequest) -> anyhow::Result<ChatResponse> {
            *self.last_request.lock().unwrap() = Some(request);
            self.output.lock().unwrap().take().expect("stub gateway called twice")
        }
        async fn embed(&self, _: EmbeddingRequest) -> anyhow::Result<EmbeddingResponse> {
            unreachable!()
        }
        async fn embed_many(
            &self,
            _: EmbeddingBatchRequest,
        ) -> anyhow::Result<EmbeddingBatchResponse> {
            unreachable!()
        }
        async fn vision_extract(&self, _: VisionRequest) -> anyhow::Result<VisionResponse> {
            unreachable!()
        }
    }

    fn sample_binding() -> ResolvedRuntimeBinding {
        ResolvedRuntimeBinding {
            binding_id: Uuid::now_v7(),
            workspace_id: Uuid::nil(),
            library_id: Uuid::nil(),
            binding_purpose: AiBindingPurpose::QueryCompile,
            provider_catalog_id: Uuid::now_v7(),
            provider_kind: "openai".to_string(),
            provider_base_url: None,
            provider_api_style: "openai".to_string(),
            credential_id: Uuid::now_v7(),
            api_key: Some("sk-test".to_string()),
            model_catalog_id: Uuid::now_v7(),
            model_name: "gpt-5.4-nano".to_string(),
            system_prompt: None,
            temperature: None,
            top_p: None,
            max_output_tokens_override: None,
            extra_parameters_json: serde_json::json!({}),
        }
    }

    fn chat_response_with(output_text: &str) -> ChatResponse {
        ChatResponse {
            provider_kind: "openai".to_string(),
            model_name: "gpt-5.4-nano".to_string(),
            output_text: output_text.to_string(),
            usage_json: json!({"prompt_tokens": 100, "completion_tokens": 40}),
        }
    }

    #[tokio::test]
    async fn compiles_descriptive_question_into_ir() {
        let ir_json = json!({
            "act": "configure_how",
            "scope": "single_document",
            "language": "ru",
            "target_types": ["procedure"],
            "target_entities": [{"label": "платежный модуль", "role": "subject"}],
            "literal_constraints": [],
            "comparison": null,
            "document_focus": null,
            "conversation_refs": [],
            "needs_clarification": null,
            "confidence": 0.9
        })
        .to_string();
        let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
        let service = QueryCompilerService;
        let binding = sample_binding();

        let outcome = service
            .compile_with_gateway(&gateway, &binding, "как настроить платежный модуль?", &[])
            .await
            .expect("compile ok");

        assert!(outcome.fallback_reason.is_none());
        assert_eq!(outcome.ir.act, QueryAct::ConfigureHow);
        assert_eq!(outcome.ir.scope, QueryScope::SingleDocument);
        assert_eq!(outcome.ir.language, QueryLanguage::Ru);
        assert_eq!(outcome.verification_level(), VerificationLevel::Lenient);
        let request = gateway.last_request.lock().unwrap().clone().unwrap();
        assert_eq!(request.provider_kind, "openai");
        assert_eq!(request.model_name, "gpt-5.4-nano");
        assert!(request.response_format.is_some(), "structured response format must be attached");
        assert!(request.prompt.contains("как настроить платежный модуль?"));
    }

    #[tokio::test]
    async fn repairs_stateless_follow_up_with_explicit_target() {
        let ir_json = json!({
            "act": "follow_up",
            "scope": "single_document",
            "language": "en",
            "target_types": ["service"],
            "target_entities": [{"label": "TargetName", "role": "subject"}],
            "literal_constraints": [],
            "comparison": null,
            "document_focus": null,
            "conversation_refs": [{"surface": "how", "kind": "bare_interrogative"}],
            "needs_clarification": "ambiguous_too_short",
            "confidence": 0.35
        })
        .to_string();
        let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
        let service = QueryCompilerService;
        let binding = sample_binding();

        let outcome = service
            .compile_with_gateway(&gateway, &binding, "TargetName how", &[])
            .await
            .expect("compile ok");

        assert_eq!(outcome.ir.act, QueryAct::Describe);
        assert!(outcome.ir.conversation_refs.is_empty());
        assert_eq!(outcome.ir.target_entities.len(), 1);
    }

    #[tokio::test]
    async fn preserves_follow_up_when_history_exists() {
        let ir_json = json!({
            "act": "follow_up",
            "scope": "single_document",
            "language": "en",
            "target_types": ["service"],
            "target_entities": [{"label": "TargetName", "role": "subject"}],
            "literal_constraints": [],
            "comparison": null,
            "document_focus": null,
            "conversation_refs": [{"surface": "how", "kind": "bare_interrogative"}],
            "needs_clarification": null,
            "confidence": 0.75
        })
        .to_string();
        let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
        let service = QueryCompilerService;
        let binding = sample_binding();
        let history = vec![CompileHistoryTurn {
            role: "assistant".to_string(),
            content: "TargetName was mentioned previously.".to_string(),
        }];

        let outcome = service
            .compile_with_gateway(&gateway, &binding, "how", &history)
            .await
            .expect("compile ok");

        assert_eq!(outcome.ir.act, QueryAct::FollowUp);
        assert_eq!(outcome.ir.conversation_refs.len(), 1);
    }

    #[tokio::test]
    async fn returns_fallback_on_provider_error() {
        let gateway = StubGateway::new(Err(anyhow::anyhow!("upstream 503")));
        let service = QueryCompilerService;
        let binding = sample_binding();

        let outcome = service
            .compile_with_gateway(&gateway, &binding, "what is /system/info?", &[])
            .await
            .expect("fallback is a success path");

        assert_eq!(outcome.fallback_reason.as_deref(), Some("provider_call_failed"));
        assert_eq!(outcome.ir.confidence, 0.0);
        assert_eq!(outcome.ir.act, QueryAct::Describe);
        assert_eq!(outcome.verification_level(), VerificationLevel::Lenient);
    }

    #[tokio::test]
    async fn returns_fallback_on_invalid_ir_output() {
        let gateway = StubGateway::new(Ok(chat_response_with("not valid json")));
        let service = QueryCompilerService;
        let binding = sample_binding();

        let outcome = service
            .compile_with_gateway(&gateway, &binding, "anything", &[])
            .await
            .expect("fallback is a success path");

        assert_eq!(outcome.fallback_reason.as_deref(), Some("invalid_ir_output"));
    }

    #[tokio::test]
    async fn history_turns_are_embedded_in_prompt() {
        let ir_json = serde_json::to_string(&fallback_ir()).unwrap();
        let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
        let service = QueryCompilerService;
        let binding = sample_binding();
        let history = vec![
            CompileHistoryTurn {
                role: "user".to_string(),
                content: "у нас есть модуль платежей?".to_string(),
            },
            CompileHistoryTurn {
                role: "assistant".to_string(),
                content: "Да, модуль платежей описан.".to_string(),
            },
        ];

        let _ = service
            .compile_with_gateway(&gateway, &binding, "а как настроить?", &history)
            .await
            .expect("compile ok");

        let prompt = gateway.last_request.lock().unwrap().clone().unwrap().prompt;
        assert!(prompt.contains("Prior conversation"));
        assert!(prompt.contains("модуль платежей"));
        assert!(prompt.contains("а как настроить?"));
    }

    // -----------------------------------------------------------------
    // Two-level cache tests — mirror `StubGateway` with a `StubCache`
    // keyed by `(library_id, question_hash)` so we can assert both the
    // read-through path (hit skips the LLM) and the write-through path
    // (successful compile populates the cache) without any real Redis
    // or Postgres.
    // -----------------------------------------------------------------

    #[derive(Default)]
    struct StubCache {
        store: Mutex<HashMap<(Uuid, String), CachedIrEntry>>,
        get_calls: Mutex<u32>,
        put_calls: Mutex<u32>,
    }

    impl StubCache {
        fn seeded(library_id: Uuid, question_hash: String, entry: CachedIrEntry) -> Self {
            let cache = Self::default();
            cache.store.lock().unwrap().insert((library_id, question_hash), entry);
            cache
        }

        fn len(&self) -> usize {
            self.store.lock().unwrap().len()
        }

        fn put_calls(&self) -> u32 {
            *self.put_calls.lock().unwrap()
        }
    }

    #[async_trait]
    impl QueryIrCache for StubCache {
        async fn get(&self, library_id: Uuid, question_hash: &str) -> Option<CachedIrEntry> {
            *self.get_calls.lock().unwrap() += 1;
            self.store.lock().unwrap().get(&(library_id, question_hash.to_string())).cloned()
        }

        async fn put(&self, library_id: Uuid, question_hash: &str, entry: &CachedIrEntry) {
            *self.put_calls.lock().unwrap() += 1;
            self.store
                .lock()
                .unwrap()
                .insert((library_id, question_hash.to_string()), entry.clone());
        }
    }

    fn canonical_ir() -> QueryIR {
        QueryIR {
            act: QueryAct::ConfigureHow,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Ru,
            target_types: vec!["procedure".to_string()],
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            confidence: 0.9,
        }
    }

    #[tokio::test]
    async fn cache_hit_short_circuits_llm() {
        let library_id = Uuid::now_v7();
        let question = "как настроить платежный модуль?";
        let history: Vec<CompileHistoryTurn> = Vec::new();
        let hash = hash_question(question, &history, QUERY_IR_SCHEMA_VERSION);
        let cached = CachedIrEntry {
            ir: canonical_ir(),
            provider_kind: CACHE_HIT_REDIS_PROVIDER_KIND.to_string(),
            model_name: String::new(),
            usage_json: json!({"source": "redis"}),
        };
        let cache = StubCache::seeded(library_id, hash, cached);
        let gateway =
            StubGateway::new(Err(anyhow::anyhow!("gateway must not be called on cache hit")));
        let service = QueryCompilerService;
        let binding = sample_binding();

        let outcome = service
            .compile_with_cache_and_gateway(
                &cache, &gateway, &binding, library_id, question, &history,
            )
            .await
            .expect("cache hit is a success path");

        assert!(outcome.fallback_reason.is_none());
        assert_eq!(outcome.provider_kind, CACHE_HIT_REDIS_PROVIDER_KIND);
        assert_eq!(outcome.ir.act, QueryAct::ConfigureHow);
        assert!(
            gateway.last_request.lock().unwrap().is_none(),
            "gateway.generate must not be called on cache hit"
        );
        assert_eq!(cache.put_calls(), 0, "cache must not be rewritten on hit");
    }

    #[tokio::test]
    async fn cache_miss_writes_through() {
        let library_id = Uuid::now_v7();
        let question = "what port does the broker listen on?";
        let history: Vec<CompileHistoryTurn> = Vec::new();
        let ir_json = json!({
            "act": "retrieve_value",
            "scope": "single_document",
            "language": "en",
            "target_types": ["port"],
            "target_entities": [],
            "literal_constraints": [],
            "comparison": null,
            "document_focus": null,
            "conversation_refs": [],
            "needs_clarification": null,
            "confidence": 0.85
        })
        .to_string();
        let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
        let cache = StubCache::default();
        let service = QueryCompilerService;
        let binding = sample_binding();

        let outcome = service
            .compile_with_cache_and_gateway(
                &cache, &gateway, &binding, library_id, question, &history,
            )
            .await
            .expect("compile ok");

        assert!(outcome.fallback_reason.is_none());
        assert_eq!(outcome.ir.act, QueryAct::RetrieveValue);
        assert_eq!(cache.put_calls(), 1, "successful compile must write through to cache");
        assert_eq!(cache.len(), 1);

        // A second call with the same inputs must now be served from the cache
        // without touching the gateway (the stub gateway is one-shot and
        // would panic on a second invocation).
        let outcome_two = service
            .compile_with_cache_and_gateway(
                &cache, &gateway, &binding, library_id, question, &history,
            )
            .await
            .expect("cache hit");
        assert_eq!(outcome_two.ir.act, QueryAct::RetrieveValue);
    }

    #[tokio::test]
    async fn fallback_is_not_cached() {
        let library_id = Uuid::now_v7();
        let question = "anything";
        let history: Vec<CompileHistoryTurn> = Vec::new();
        let gateway = StubGateway::new(Err(anyhow::anyhow!("upstream 503")));
        let cache = StubCache::default();
        let service = QueryCompilerService;
        let binding = sample_binding();

        let outcome = service
            .compile_with_cache_and_gateway(
                &cache, &gateway, &binding, library_id, question, &history,
            )
            .await
            .expect("fallback is a success path");

        assert_eq!(outcome.fallback_reason.as_deref(), Some("provider_call_failed"));
        assert_eq!(cache.put_calls(), 0, "fallback outcomes must not be cached");
        assert_eq!(cache.len(), 0);
    }

    #[tokio::test]
    async fn hash_question_is_normalized_and_history_sensitive() {
        let base = hash_question("Hello World", &[], QUERY_IR_SCHEMA_VERSION);
        let variant = hash_question("  hello world  ", &[], QUERY_IR_SCHEMA_VERSION);
        assert_eq!(base, variant, "trim + lowercase must produce the same hash");

        let with_history = hash_question(
            "Hello World",
            &[CompileHistoryTurn {
                role: "user".to_string(),
                content: "prior context".to_string(),
            }],
            QUERY_IR_SCHEMA_VERSION,
        );
        assert_ne!(base, with_history, "history must contribute to the hash");

        let bumped = hash_question("Hello World", &[], QUERY_IR_SCHEMA_VERSION.wrapping_add(1));
        assert_ne!(base, bumped, "schema_version must contribute to the hash");
    }
}
