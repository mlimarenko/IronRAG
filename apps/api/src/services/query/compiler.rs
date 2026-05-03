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

use async_trait::async_trait;
use redis::{AsyncCommands, Client as RedisClient};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::sync::LazyLock;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        ai::AiBindingPurpose,
        query_ir::{
            ClarificationReason, QUERY_IR_SCHEMA_VERSION, QueryAct, QueryIR, QueryLanguage,
            QueryScope, VerificationLevel, query_ir_json_schema,
        },
    },
    infra::repositories::query_ir_cache_repository::{get_query_ir_cache, upsert_query_ir_cache},
    integrations::llm::{ChatRequestSeed, LlmGateway, build_structured_chat_request},
    interfaces::http::router_support::ApiError,
    services::ai_catalog_service::ResolvedRuntimeBinding,
};

/// Canonical Redis key prefix for the hot IR cache.
const REDIS_IR_CACHE_PREFIX: &str = "ir_cache";

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

/// Compute the canonical cache key hash for one compile request. The hash is
/// content-addressed: equal inputs under the same compiler runtime produce
/// equal keys regardless of trailing whitespace or letter case so
/// trivially-reworded repeats share a cache entry. Compiler/runtime source
/// files are part of the address, so semantic fixes never serve stale IR rows
/// that were compiled under older routing rules.
#[must_use]
pub fn hash_question(
    question: &str,
    history: &[CompileHistoryTurn],
    schema_version: u16,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"schema|");
    hasher.update(schema_version.to_be_bytes());
    hasher.update(b"|runtime|");
    hasher.update(query_ir_runtime_fingerprint().as_bytes());
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

#[must_use]
pub fn query_ir_runtime_fingerprint() -> &'static str {
    static FINGERPRINT: LazyLock<String> = LazyLock::new(|| {
        let mut hasher = Sha256::new();
        hasher.update(include_str!("compiler.rs").as_bytes());
        hasher.update(include_str!("../../domains/query_ir.rs").as_bytes());
        hex::encode(hasher.finalize())
    });
    FINGERPRINT.as_str()
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
    repair_target_entity_labels(question, history, &mut ir);

    let stateless_with_explicit_target =
        history.is_empty() && stateless_ir_has_explicit_target(&ir);
    if stateless_with_explicit_target && matches!(ir.act, QueryAct::FollowUp) {
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
    if stateless_with_explicit_target && !ir.conversation_refs.is_empty() {
        tracing::info!(
            target: "ironrag::query_compile",
            question_len = question.len(),
            act = ir.act.as_str(),
            target_entities_count = ir.target_entities.len(),
            has_document_focus = ir.document_focus.is_some(),
            literal_constraints_count = ir.literal_constraints.len(),
            conversation_refs_count = ir.conversation_refs.len(),
            "query compile repaired stateless explicit-target refs"
        );
        ir.conversation_refs.clear();
        if ir.needs_clarification.as_ref().is_some_and(|clarification| {
            matches!(clarification.reason, ClarificationReason::AnaphoraUnresolved)
        }) {
            ir.needs_clarification = None;
        }
    }
    ir
}

fn repair_target_entity_labels(question: &str, history: &[CompileHistoryTurn], ir: &mut QueryIR) {
    if ir.target_entities.is_empty() {
        return;
    }

    let sources = target_label_repair_sources(question, history);
    if sources.is_empty() {
        return;
    }

    for mention in &mut ir.target_entities {
        let label = mention.label.trim();
        if label.is_empty()
            || sources.iter().any(|source| contains_label_token_sequence(source, label))
        {
            continue;
        }

        let Some(repaired) = closest_source_label_span(label, &sources) else {
            continue;
        };
        if repaired == label {
            continue;
        }
        tracing::info!(
            target: "ironrag::query_compile",
            original_label_len = label.chars().count(),
            repaired_label_len = repaired.chars().count(),
            "query compile repaired target entity label to user-visible span"
        );
        mention.label = repaired;
    }
}

fn target_label_repair_sources(question: &str, history: &[CompileHistoryTurn]) -> Vec<String> {
    let mut sources = Vec::new();
    let question = question.trim();
    if !question.is_empty() {
        sources.push(question.to_string());
    }
    for turn in history.iter().rev() {
        let content = turn.content.trim();
        if !content.is_empty() {
            sources.push(content.to_string());
        }
    }
    sources
}

fn contains_label_token_sequence(haystack: &str, needle: &str) -> bool {
    let needle_tokens = normalized_repair_tokens(needle);
    if needle_tokens.is_empty() {
        return false;
    }
    let haystack_tokens = normalized_repair_tokens(haystack);
    if haystack_tokens.len() < needle_tokens.len() {
        return false;
    }
    haystack_tokens.windows(needle_tokens.len()).any(|window| window == needle_tokens)
}

fn closest_source_label_span(label: &str, sources: &[String]) -> Option<String> {
    let label_tokens = alnum_token_spans(label);
    let token_count = label_tokens.len();
    if token_count == 0 || token_count > 8 {
        return None;
    }

    let normalized_label = normalize_label_candidate(label);
    if normalized_label.is_empty() {
        return None;
    }
    if token_count == 1
        && let Some(expanded) = closest_containing_source_token(&normalized_label, sources)
    {
        return Some(expanded);
    }
    let allowed_distance = allowed_label_repair_distance(normalized_label.chars().count());
    if allowed_distance == 0 {
        return None;
    }

    let mut best: Option<LabelRepairCandidate> = None;
    for (source_index, source) in sources.iter().enumerate() {
        let spans = alnum_token_spans(source);
        if spans.len() < token_count {
            continue;
        }

        for window in spans.windows(token_count) {
            let Some(first) = window.first() else {
                continue;
            };
            let Some(last) = window.last() else {
                continue;
            };
            let candidate = source[first.start..last.end].trim();
            if candidate.is_empty() {
                continue;
            }
            let normalized_candidate = normalize_label_candidate(candidate);
            let Some(distance) =
                bounded_edit_distance(&normalized_label, &normalized_candidate, allowed_distance)
            else {
                continue;
            };
            let candidate = LabelRepairCandidate {
                text: candidate.to_string(),
                distance,
                source_index,
                char_len: candidate.chars().count(),
            };
            if best.as_ref().is_none_or(|current| candidate.is_better_than(current)) {
                best = Some(candidate);
            }
        }
    }

    best.map(|candidate| candidate.text)
}

fn closest_containing_source_token(label: &str, sources: &[String]) -> Option<String> {
    if label.chars().count() < 3 {
        return None;
    }

    let mut best: Option<LabelRepairCandidate> = None;
    for (source_index, source) in sources.iter().enumerate() {
        for span in alnum_token_spans(source) {
            let candidate = source[span.start..span.end].trim();
            if candidate.is_empty() {
                continue;
            }
            let normalized_candidate = normalize_label_candidate(candidate);
            if normalized_candidate == label || !normalized_candidate.contains(label) {
                continue;
            }
            let candidate = LabelRepairCandidate {
                text: candidate.to_string(),
                distance: normalized_candidate
                    .chars()
                    .count()
                    .saturating_sub(label.chars().count()),
                source_index,
                char_len: candidate.chars().count(),
            };
            if best.as_ref().is_none_or(|current| candidate.is_better_than(current)) {
                best = Some(candidate);
            }
        }
    }

    best.map(|candidate| candidate.text)
}

#[derive(Debug)]
struct LabelRepairCandidate {
    text: String,
    distance: usize,
    source_index: usize,
    char_len: usize,
}

impl LabelRepairCandidate {
    fn is_better_than(&self, other: &Self) -> bool {
        (self.distance, self.source_index, self.char_len)
            < (other.distance, other.source_index, other.char_len)
    }
}

#[derive(Debug)]
struct AlnumTokenSpan {
    start: usize,
    end: usize,
}

fn alnum_token_spans(value: &str) -> Vec<AlnumTokenSpan> {
    let mut spans = Vec::new();
    let mut current_start = None;
    let mut current_end = 0usize;

    for (index, ch) in value.char_indices() {
        if ch.is_alphanumeric() {
            if current_start.is_none() {
                current_start = Some(index);
            }
            current_end = index + ch.len_utf8();
            continue;
        }

        if let Some(start) = current_start.take() {
            spans.push(AlnumTokenSpan { start, end: current_end });
        }
    }

    if let Some(start) = current_start {
        spans.push(AlnumTokenSpan { start, end: current_end });
    }

    spans
}

fn normalize_label_candidate(value: &str) -> String {
    normalized_repair_tokens(value).join(" ")
}

fn normalized_repair_tokens(value: &str) -> Vec<String> {
    value
        .split(|ch: char| !ch.is_alphanumeric())
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn allowed_label_repair_distance(char_count: usize) -> usize {
    if char_count < 5 {
        return 0;
    }
    (char_count / 8).clamp(1, 3)
}

fn bounded_edit_distance(left: &str, right: &str, max_distance: usize) -> Option<usize> {
    let left_chars = left.chars().collect::<Vec<_>>();
    let right_chars = right.chars().collect::<Vec<_>>();
    if left_chars.len().abs_diff(right_chars.len()) > max_distance {
        return None;
    }

    let mut previous = (0..=right_chars.len()).collect::<Vec<_>>();
    let mut current = vec![0; right_chars.len() + 1];
    for (left_index, left_char) in left_chars.iter().enumerate() {
        current[0] = left_index + 1;
        let mut row_min = current[0];
        for (right_index, right_char) in right_chars.iter().enumerate() {
            let substitution_cost = usize::from(left_char != right_char);
            let deletion = previous[right_index + 1] + 1;
            let insertion = current[right_index] + 1;
            let substitution = previous[right_index] + substitution_cost;
            let distance = deletion.min(insertion).min(substitution);
            current[right_index + 1] = distance;
            row_min = row_min.min(distance);
        }
        if row_min > max_distance {
            return None;
        }
        std::mem::swap(&mut previous, &mut current);
    }

    let distance = previous[right_chars.len()];
    (distance <= max_distance).then_some(distance)
}

fn stateless_ir_has_explicit_target(ir: &QueryIR) -> bool {
    !ir.target_entities.is_empty()
        || ir.document_focus.as_ref().is_some_and(|hint| !hint.hint.trim().is_empty())
        || !ir.literal_constraints.is_empty()
        || !ir.temporal_constraints.is_empty()
        || ir.source_slice.is_some()
}

/// Safe default when the compiler cannot run. Chosen so no downstream stage
/// narrows recall from an under-specified IR: `Describe` + `MultiDocument`
/// keeps retrieval broad, verification is `Lenient`, and `confidence = 0.0`
/// signals callers that the compile stage degraded.
fn fallback_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::Describe,
        scope: QueryScope::MultiDocument,
        language: QueryLanguage::Auto,
        target_types: Vec::new(),
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
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
4. `temporal_constraints` captures date/time or date-range references when present. Preserve the \
surface span exactly as visible to the user. Populate `start` and `end` with ISO-8601 UTC bounds \
whenever the surface contains a self-contained absolute date or date-range (year, year+month, \
year+month+day, quarter, year+week, ISO timestamp, decade). Treat `start` as inclusive and `end` as \
exclusive. Use null bounds ONLY when the reference is genuinely under-determined and has no anchor \
the runtime supplies (bare deixis like \"recently\" / \"lately\" / \"long ago\" without an \
explicit period). Phrases like \"in March 2026\", \"27 марта 2026\", \"2026-Q1\", \
\"on 2026-03-27\", \"3月2026\", \"first half of 2025\" are absolute and MUST resolve.\n\
\n\
Worked examples (cover ≥ 2 writing systems and one purely numeric form per CLAUDE.md \
script-agnostic policy):\n\
\n\
- surface: \"March 2026\" → start: \"2026-03-01T00:00:00Z\", end: \"2026-04-01T00:00:00Z\"\n\
- surface: \"в марте 2026\" → start: \"2026-03-01T00:00:00Z\", end: \"2026-04-01T00:00:00Z\"\n\
- surface: \"27 марта 2026\" → start: \"2026-03-27T00:00:00Z\", end: \"2026-03-28T00:00:00Z\"\n\
- surface: \"2026-03-27\" → start: \"2026-03-27T00:00:00Z\", end: \"2026-03-28T00:00:00Z\"\n\
- surface: \"2026-Q1\" → start: \"2026-01-01T00:00:00Z\", end: \"2026-04-01T00:00:00Z\"\n\
- surface: \"в первой половине 2025\" → start: \"2025-01-01T00:00:00Z\", end: \"2025-07-01T00:00:00Z\"\n\
- surface: \"recently\" / \"недавно\" (no anchor) → start: null, end: null\n\
5. `conversation_refs` lists unresolved anaphora / deixis / ellipsis that point to prior \
user-assistant turns, not to positions, ranges, neighboring units, or anchors inside the source \
documents being searched. `act = follow_up` is typical when the question cannot stand on its own.\n\
6. `target_types` are free-form ontology tags (examples: endpoint, port, parameter, procedure, \
protocol, config_key, error_code, env_var, metric, table_row, table_summary, table_average, \
table_frequency, document, concept) and may also use \
runtime graph node-type tags when the user asks for graph inventory or graph facts: person, \
organization, location, event, artifact, process, concept, attribute, entity. You may invent a new \
tag if nothing fits — the system grows its ontology from your output.\n\
7. `source_slice` is null for ordinary summaries, comparisons, procedures, and needle lookups. \
Set it only when the user asks for a positional slice of a sequential source: earliest units \
(`head`), latest units (`tail`), or a bounded representation of the whole sequence (`all`). \
Populate `count` only when the user asks for a concrete number of units.\n\
8. `target_entities[*].label`, `document_focus.hint`, and verbatim literal-like values must \
preserve the exact writing system and spelling visible in the current question or prior turns. \
When a named target appears in that user-visible text, emit that target as a verbatim substring; \
do not translate, transliterate, normalize look-alike glyphs, or substitute visually similar \
characters.\n\
9. `confidence` ∈ [0.0, 1.0]. Use < 0.6 only when you genuinely cannot pin the question.\n\
10. `language` must use one of the schema enum values; prefer `auto` when the signal is mixed or \
unclear.\n\
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
            "target_entities": [{"label": "payment module", "role": "subject"}],
            "literal_constraints": [],
            "temporal_constraints": [],
            "comparison": null,
            "document_focus": null,
            "conversation_refs": [],
            "needs_clarification": null,
            "source_slice": null,
            "confidence": 0.9
        })
        .to_string();
        let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
        let service = QueryCompilerService;
        let binding = sample_binding();

        let outcome = service
            .compile_with_gateway(&gateway, &binding, "how do I configure the payment module?", &[])
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
        assert!(request.prompt.contains("how do I configure the payment module?"));
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
            "temporal_constraints": [],
            "comparison": null,
            "document_focus": null,
            "conversation_refs": [{"surface": "how", "kind": "bare_interrogative"}],
            "needs_clarification": "ambiguous_too_short",
            "source_slice": null,
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
    async fn repairs_target_entity_labels_to_verbatim_question_spans() {
        let substituted_label = format!("Project Om{}ga", '\u{0435}');
        let ir_json = json!({
            "act": "describe",
            "scope": "single_document",
            "language": "en",
            "target_types": ["artifact"],
            "target_entities": [
                {"label": substituted_label, "role": "subject"},
                {"label": "Δelta Meridion", "role": "object"}
            ],
            "literal_constraints": [],
            "temporal_constraints": [],
            "comparison": null,
            "document_focus": null,
            "conversation_refs": [],
            "needs_clarification": null,
            "source_slice": null,
            "confidence": 0.8
        })
        .to_string();
        let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
        let service = QueryCompilerService;
        let binding = sample_binding();

        let outcome = service
            .compile_with_gateway(
                &gateway,
                &binding,
                "Compare Project Omega and Δelta Meridian",
                &[],
            )
            .await
            .expect("compile ok");

        let labels = outcome
            .ir
            .target_entities
            .iter()
            .map(|mention| mention.label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(labels, vec!["Project Omega", "Δelta Meridian"]);
    }

    #[tokio::test]
    async fn expands_embedded_short_target_label_to_source_token() {
        let ir_json = json!({
            "act": "describe",
            "scope": "multi_document",
            "language": "auto",
            "target_types": ["person"],
            "target_entities": [{"label": "OTO", "role": "subject"}],
            "literal_constraints": [],
            "temporal_constraints": [],
            "comparison": null,
            "document_focus": null,
            "conversation_refs": [],
            "needs_clarification": null,
            "source_slice": null,
            "confidence": 0.8
        })
        .to_string();
        let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
        let service = QueryCompilerService;
        let binding = sample_binding();

        let outcome = service
            .compile_with_gateway(&gateway, &binding, "Tell me about Alpha Otoya", &[])
            .await
            .expect("compile ok");

        assert_eq!(outcome.ir.target_entities[0].label, "Otoya");
    }

    #[tokio::test]
    async fn repairs_stateless_explicit_target_refs_without_changing_act() {
        let ir_json = json!({
            "act": "retrieve_value",
            "scope": "single_document",
            "language": "en",
            "target_types": ["person", "conversation_turn"],
            "target_entities": [{"label": "user name", "role": "subject"}],
            "literal_constraints": [],
            "temporal_constraints": [],
            "comparison": null,
            "document_focus": null,
            "conversation_refs": [
                {"surface": "source beginning", "kind": "deictic"},
                {"surface": "neighboring source units", "kind": "elliptic"}
            ],
            "needs_clarification": {
                "reason": "anaphora_unresolved",
                "suggestion": "clarify the prior turn"
            },
            "source_slice": null,
            "confidence": 0.55
        })
        .to_string();
        let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
        let service = QueryCompilerService;
        let binding = sample_binding();

        let outcome = service
            .compile_with_gateway(&gateway, &binding, "Who introduced themself by name?", &[])
            .await
            .expect("compile ok");

        assert_eq!(outcome.ir.act, QueryAct::RetrieveValue);
        assert!(outcome.ir.conversation_refs.is_empty());
        assert!(outcome.ir.needs_clarification.is_none());
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
            "temporal_constraints": [],
            "comparison": null,
            "document_focus": null,
            "conversation_refs": [{"surface": "how", "kind": "bare_interrogative"}],
            "needs_clarification": null,
            "source_slice": null,
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
                content: "do we have a payment module?".to_string(),
            },
            CompileHistoryTurn {
                role: "assistant".to_string(),
                content: "Yes, the payment module is documented.".to_string(),
            },
        ];

        let _ = service
            .compile_with_gateway(&gateway, &binding, "how do I configure it?", &history)
            .await
            .expect("compile ok");

        let prompt = gateway.last_request.lock().unwrap().clone().unwrap().prompt;
        assert!(prompt.contains("Prior conversation"));
        assert!(prompt.contains("payment module"));
        assert!(prompt.contains("how do I configure it?"));
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
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            confidence: 0.9,
        }
    }

    #[tokio::test]
    async fn cache_hit_short_circuits_llm() {
        let library_id = Uuid::now_v7();
        let question = "how do I configure the payment module?";
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
            "temporal_constraints": [],
            "comparison": null,
            "document_focus": null,
            "conversation_refs": [],
            "needs_clarification": null,
            "source_slice": null,
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

        assert_eq!(
            query_ir_runtime_fingerprint().len(),
            64,
            "runtime fingerprint is a SHA-256 hex digest"
        );
    }
}
