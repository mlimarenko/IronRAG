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
//! - Missing `QueryCompile` binding, provider call failures, and invalid
//!   provider output fail loudly with `ApiError::ProviderFailure`. A turn must
//!   not continue with a synthetic IR because that hides routing regressions
//!   behind low-quality retrieval.
//! - Cache hits are allowed only after the active `QueryCompile` binding has
//!   resolved successfully. The cache key includes the resolved binding
//!   fingerprint, so model/provider/preset changes do not replay stale IR.
//! - Only successful live compiles and binding-validated cache hits produce
//!   `CompileQueryOutcome`.

use async_trait::async_trait;
use redis::{AsyncCommands, Client as RedisClient};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::{collections::BTreeSet, sync::LazyLock};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        ai::AiBindingPurpose,
        query_ir::{
            ClarificationReason, ConversationRefKind, EntityRole, QUERY_IR_SCHEMA_VERSION,
            QueryAct, QueryIR, QueryScope, SourceSliceDirection, SourceSliceFilter,
            SourceSliceSpec, UnresolvedRef, VerificationLevel, query_ir_json_schema,
        },
    },
    infra::repositories::query_ir_cache_repository::{get_query_ir_cache, upsert_query_ir_cache},
    integrations::llm::{LlmGateway, build_structured_chat_request},
    interfaces::http::router_support::ApiError,
    services::{
        ai_catalog_service::ResolvedRuntimeBinding,
        query::effective_query::current_question_segment,
    },
};

/// Canonical Redis key prefix for the hot IR cache.
const REDIS_IR_CACHE_PREFIX: &str = "ir_cache";
const STRUCTURAL_CONFIGURATION_REPAIR_CONFIDENCE: f32 = 0.65;
const STRUCTURAL_CONFIGURATION_TARGET_TYPES: [&str; 3] =
    ["configuration_file", "config_key", "parameter"];

/// Minimum compiler confidence at which an over-eager single-document focus is
/// trusted enough to downgrade. Below this the IR is treated as genuinely
/// uncertain and the focus is left untouched so a clarify / stub path can run.
const OVEREAGER_FOCUS_DOWNGRADE_MIN_CONFIDENCE: f32 = 0.6;

/// Ontology target tags that, when present, indicate the user is structurally
/// referencing a document object (its title / a heading) rather than merely
/// naming a product or module. Their presence keeps a `document_focus` pin.
const EXPLICIT_DOCUMENT_REFERENCE_TARGET_TYPES: [&str; 3] =
    ["document", "primary_heading", "secondary_heading"];
const CONFIGURATION_FILE_EXTENSIONS: [&str; 8] =
    [".conf", ".ini", ".cfg", ".properties", ".yaml", ".yml", ".toml", ".json"];

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
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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
/// content-addressed: equal inputs under the same compiler runtime and resolved
/// QueryCompile binding produce equal keys regardless of trailing whitespace or
/// letter case so trivially-reworded repeats share a cache entry.
/// Compiler/runtime source files and binding fields are part of the address, so
/// semantic fixes or provider/model changes never serve stale IR rows that were
/// compiled under a different routing contract.
#[must_use]
fn hash_compile_request(
    question: &str,
    history: &[CompileHistoryTurn],
    schema_version: u16,
    binding: &ResolvedRuntimeBinding,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"schema|");
    hasher.update(schema_version.to_be_bytes());
    hasher.update(b"|runtime|");
    hasher.update(query_ir_runtime_fingerprint().as_bytes());
    hasher.update(b"|binding|");
    hasher.update(query_compile_binding_fingerprint(binding).as_bytes());
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
fn query_compile_binding_fingerprint(binding: &ResolvedRuntimeBinding) -> String {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, "binding_id", &binding.binding_id.to_string());
    hash_field(&mut hasher, "workspace_id", &binding.workspace_id.to_string());
    hash_field(&mut hasher, "library_id", &binding.library_id.to_string());
    hash_field(&mut hasher, "binding_purpose", binding.binding_purpose.as_str());
    hash_field(&mut hasher, "provider_catalog_id", &binding.provider_catalog_id.to_string());
    hash_field(&mut hasher, "provider_kind", &binding.provider_kind);
    hash_option_field(&mut hasher, "provider_base_url", binding.provider_base_url.as_deref());
    hash_field(&mut hasher, "provider_api_style", &binding.provider_api_style);
    hash_field(&mut hasher, "credential_id", &binding.credential_id.to_string());
    hash_field(&mut hasher, "model_catalog_id", &binding.model_catalog_id.to_string());
    hash_field(&mut hasher, "model_name", &binding.model_name);
    hash_option_field(&mut hasher, "system_prompt", binding.system_prompt.as_deref());
    hash_optional_display_field(&mut hasher, "temperature", binding.temperature);
    hash_optional_display_field(&mut hasher, "top_p", binding.top_p);
    hash_optional_display_field(
        &mut hasher,
        "max_output_tokens_override",
        binding.max_output_tokens_override,
    );
    hash_json_value(&mut hasher, "extra_parameters_json", &binding.extra_parameters_json);
    hex::encode(hasher.finalize())
}

fn hash_field(hasher: &mut Sha256, name: &str, value: &str) {
    hasher.update(name.as_bytes());
    hasher.update(b"=");
    hasher.update(value.len().to_string().as_bytes());
    hasher.update(b":");
    hasher.update(value.as_bytes());
    hasher.update(b";");
}

fn hash_option_field(hasher: &mut Sha256, name: &str, value: Option<&str>) {
    match value {
        Some(value) => hash_field(hasher, name, value),
        None => hash_field(hasher, name, "<none>"),
    }
}

fn hash_optional_display_field<T: ToString>(hasher: &mut Sha256, name: &str, value: Option<T>) {
    let value = value.map(|value| value.to_string());
    hash_option_field(hasher, name, value.as_deref());
}

fn hash_json_value(hasher: &mut Sha256, name: &str, value: &Value) {
    hasher.update(name.as_bytes());
    hasher.update(b"=");
    hash_json(hasher, value);
    hasher.update(b";");
}

fn hash_json(hasher: &mut Sha256, value: &Value) {
    match value {
        Value::Null => hasher.update(b"null"),
        Value::Bool(value) => {
            if *value {
                hasher.update(b"true");
            } else {
                hasher.update(b"false");
            }
        }
        Value::Number(value) => hasher.update(value.to_string().as_bytes()),
        Value::String(value) => {
            hasher.update(b"str:");
            hasher.update(value.len().to_string().as_bytes());
            hasher.update(b":");
            hasher.update(value.as_bytes());
        }
        Value::Array(values) => {
            hasher.update(b"[");
            for value in values {
                hash_json(hasher, value);
                hasher.update(b",");
            }
            hasher.update(b"]");
        }
        Value::Object(map) => {
            hasher.update(b"{");
            let mut keys = map.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            for key in keys {
                hasher.update(key.len().to_string().as_bytes());
                hasher.update(b":");
                hasher.update(key.as_bytes());
                hasher.update(b"=");
                if let Some(value) = map.get(key) {
                    hash_json(hasher, value);
                }
                hasher.update(b",");
            }
            hasher.update(b"}");
        }
    }
}

#[must_use]
pub fn query_ir_runtime_fingerprint() -> &'static str {
    static FINGERPRINT: LazyLock<String> = LazyLock::new(|| {
        let mut hasher = Sha256::new();
        hasher.update(include_str!("compiler.rs").as_bytes());
        hasher.update(include_str!("latest_versions.rs").as_bytes());
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
    /// 1. Resolve the active `QueryCompile` binding fail-loud.
    /// 2. Hash `(question, history, schema_version, binding fingerprint)`.
    /// 3. Redis hot tier — on hit, return without touching the LLM.
    /// 4. Postgres persistent tier — on hit, warm Redis and return.
    /// 5. Miss: call the LLM with the resolved binding,
    ///    write successful compiles through to both tiers. Missing binding
    ///    or provider failures return `ApiError::ProviderFailure`.
    pub async fn compile(
        &self,
        state: &AppState,
        command: CompileQueryCommand,
    ) -> Result<CompileQueryOutcome, ApiError> {
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
                tracing::error!(
                    library_id = %command.library_id,
                    "query_compile binding is not configured"
                );
                return Err(ApiError::ProviderFailure(
                    "QueryCompile binding is not configured for this library".to_string(),
                ));
            }
        };
        let cache =
            PersistenceQueryIrCache::new(&state.persistence.postgres, &state.persistence.redis);
        let question_hash = hash_compile_request(
            &command.question,
            &command.history,
            QUERY_IR_SCHEMA_VERSION,
            &binding,
        );

        if let Some(entry) = cache.get(command.library_id, &question_hash).await {
            return Ok(cached_outcome(entry));
        }

        let outcome = self
            .compile_with_gateway(
                state.llm_gateway.as_ref(),
                &binding,
                &command.question,
                &command.history,
            )
            .await?;

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
        let question_hash =
            hash_compile_request(question, history, QUERY_IR_SCHEMA_VERSION, binding);

        if let Some(entry) = cache.get(library_id, &question_hash).await {
            return Ok(cached_outcome(entry));
        }

        let outcome = self.compile_with_gateway(gateway, binding, question, history).await?;

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

        let mut seed = binding.chat_request_seed();
        seed.system_prompt = Some(system_prompt);
        let request = build_structured_chat_request(seed, prompt, response_format);

        let response = match gateway.generate(request).await {
            Ok(value) => value,
            Err(error) => {
                tracing::error!(
                    provider = %binding.provider_kind,
                    model = %binding.model_name,
                    ?error,
                    "query compile provider call failed"
                );
                return Err(ApiError::ProviderFailure(format!(
                    "QueryCompile provider call failed for provider `{}` model `{}`",
                    binding.provider_kind, binding.model_name
                )));
            }
        };

        let ir: QueryIR = match serde_json::from_str(&response.output_text) {
            Ok(value) => value,
            Err(error) => {
                tracing::error!(
                    provider = %binding.provider_kind,
                    model = %binding.model_name,
                    output_preview = %preview(&response.output_text, 200),
                    ?error,
                    "query compile output is not valid QueryIR JSON"
                );
                return Err(ApiError::ProviderFailure(format!(
                    "QueryCompile provider returned invalid QueryIR JSON for provider `{}` model `{}`",
                    binding.provider_kind, binding.model_name
                )));
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
            history_turn_count = history.len(),
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
        served_from_cache: true,
    }
}

fn normalize_compiled_ir(
    question: &str,
    history: &[CompileHistoryTurn],
    mut ir: QueryIR,
) -> QueryIR {
    repair_target_entity_labels(question, history, &mut ir);
    repair_history_backed_elliptic_ref(question, history, &mut ir);
    normalize_latest_version_source_slice(&mut ir);
    normalize_source_slice_filter(&mut ir);
    normalize_structural_configuration_request(question, &mut ir);
    downgrade_overeager_single_document_focus(&mut ir);

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

fn normalize_latest_version_source_slice(ir: &mut QueryIR) {
    if ir.source_slice.is_some()
        || !crate::services::query::latest_versions::query_requests_latest_versions(ir)
    {
        return;
    }
    let requested_count =
        crate::services::query::latest_versions::requested_latest_version_count(ir)
            .min(u16::MAX as usize) as u16;
    ir.source_slice = Some(SourceSliceSpec {
        direction: SourceSliceDirection::Tail,
        count: Some(requested_count),
        filter: SourceSliceFilter::ReleaseMarker,
    });
}

fn normalize_source_slice_filter(ir: &mut QueryIR) {
    if ir.source_slice.is_none() {
        return;
    };
    let filter = if crate::services::query::latest_versions::query_requests_latest_versions(ir) {
        SourceSliceFilter::ReleaseMarker
    } else {
        SourceSliceFilter::None
    };
    if let Some(slice) = ir.source_slice.as_mut() {
        slice.filter = filter;
    }
}

fn normalize_structural_configuration_request(question: &str, ir: &mut QueryIR) {
    if !compiled_ir_is_low_confidence_unfocused_description(ir)
        || !question_has_structural_configuration_surface(question)
    {
        return;
    }
    tracing::info!(
        target: "ironrag::query_compile",
        question_len = question.len(),
        confidence = ir.confidence,
        "query compile repaired structural configuration request"
    );
    ir.act = QueryAct::ConfigureHow;
    ir.confidence = ir.confidence.max(STRUCTURAL_CONFIGURATION_REPAIR_CONFIDENCE);
    ir.needs_clarification = None;
    for target_type in STRUCTURAL_CONFIGURATION_TARGET_TYPES {
        push_target_type_once(&mut ir.target_types, target_type);
    }
}

/// Drop an over-eager single-document focus when the IR describes a generic,
/// directly-answerable question that merely *names* a product / module /
/// subsystem rather than *referencing a specific document*.
///
/// Root cause: a question that names a module ("how do I update `<Module>`")
/// makes the compiler emit `scope = single_document` + a `document_focus.hint`
/// equal to that module name, which then hard-locks retrieval onto whichever
/// single document's TITLE matches — often a thin overview stub — so the
/// answer pipeline reports "not in the available context".
///
/// This repair is the conservative, deterministic half of the fix (the
/// retrieval-side coverage fallback re-broadens after the fact). It fires
/// ONLY on a clearly-generic shape and leaves genuinely document-scoped
/// questions untouched. The discriminator is entirely structural / typed —
/// no natural-language keyword list:
///
/// - high compiler confidence (the question is pinned, not a guess);
/// - a concrete answerable act (`describe` / `configure_how` / `enumerate`);
/// - a resolved subject entity (the named component) and no comparison;
/// - NO explicit document reference: no exact technical literal (a literal
///   the user quoted verbatim such as a path / URL / version / identifier),
///   no `document` / heading ontology target type, and a `document_focus.hint`
///   that carries no path separator or known configuration-file extension
///   (i.e. it is a bare name, not a filename / title token).
///
/// When all hold, the focus pin is the product of naming a component, so we
/// drop `document_focus` and widen `scope` to `MultiDocument` — the codebase's
/// canonical broadened state (mirrors the latest-release inference at
/// `retrieve.rs`) — so retrieval can draw from every relevant document.
fn downgrade_overeager_single_document_focus(ir: &mut QueryIR) {
    if !compiled_ir_is_generic_overeager_single_document_focus(ir) {
        return;
    }
    tracing::info!(
        target: "ironrag::query_compile",
        act = ir.act.as_str(),
        scope = ir.scope.as_str(),
        confidence = ir.confidence,
        target_entities_count = ir.target_entities.len(),
        "query compile downgraded over-eager single_document focus to multi_document"
    );
    ir.document_focus = None;
    ir.scope = QueryScope::MultiDocument;
}

/// Structural predicate for [`downgrade_overeager_single_document_focus`].
///
/// All conditions are typed / structural so the guard cannot drift into a
/// natural-language keyword list. Kept as its own function so it is directly
/// unit-testable against synthetic [`QueryIR`] inputs.
fn compiled_ir_is_generic_overeager_single_document_focus(ir: &QueryIR) -> bool {
    matches!(ir.scope, QueryScope::SingleDocument)
        && ir.document_focus.is_some()
        && ir.confidence >= OVEREAGER_FOCUS_DOWNGRADE_MIN_CONFIDENCE
        && matches!(ir.act, QueryAct::Describe | QueryAct::ConfigureHow | QueryAct::Enumerate)
        && ir.comparison.is_none()
        && ir.source_slice.is_none()
        && ir.target_entities.iter().any(|mention| matches!(mention.role, EntityRole::Subject))
        && !ir_has_explicit_document_reference(ir)
}

/// `true` when the IR carries a structural signal that the user referenced a
/// specific document object (a quoted technical literal, a `document` / heading
/// ontology target type, or a `document_focus.hint` shaped like a filename /
/// path token) rather than merely naming a product / module by its bare name.
fn ir_has_explicit_document_reference(ir: &QueryIR) -> bool {
    if ir.has_exact_technical_literal() {
        return true;
    }
    if ir.target_types.iter().any(|target_type| {
        EXPLICIT_DOCUMENT_REFERENCE_TARGET_TYPES.iter().any(|reference| reference == target_type)
    }) {
        return true;
    }
    ir.document_focus
        .as_ref()
        .is_some_and(|focus| document_focus_hint_is_explicit_reference(&focus.hint))
}

/// A `document_focus.hint` is an explicit document reference when it looks like
/// a filename / path rather than a bare component name: it contains a path
/// separator or ends in a known configuration-file extension.
fn document_focus_hint_is_explicit_reference(hint: &str) -> bool {
    let trimmed = hint.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return true;
    }
    let lowered = trimmed.to_lowercase();
    CONFIGURATION_FILE_EXTENSIONS.iter().any(|extension| lowered.ends_with(extension))
}

fn compiled_ir_is_low_confidence_unfocused_description(ir: &QueryIR) -> bool {
    ir.confidence <= 0.35
        && matches!(ir.act, QueryAct::Describe | QueryAct::RetrieveValue)
        && ir.comparison.is_none()
        && ir.source_slice.is_none()
        && ir.document_focus.is_none()
        && ir.target_types.is_empty()
        && ir.target_entities.is_empty()
        && ir.literal_constraints.is_empty()
        && ir.temporal_constraints.is_empty()
        && ir.conversation_refs.is_empty()
}

fn question_has_structural_configuration_surface(question: &str) -> bool {
    let path_count = count_configuration_file_path_literals(question, 8);
    let section_count = count_configuration_section_literals(question, 8);
    let assignment_count = count_configuration_assignment_literals(question, 8);
    let identifier_count = count_identifier_like_backtick_literals(question, 32);
    let has_configuration_surface = path_count > 0 || section_count > 0 || assignment_count > 0;
    has_configuration_surface
        && (path_count.saturating_add(section_count).saturating_add(assignment_count) >= 2
            || identifier_count >= 2
            || assignment_count >= 1)
}

fn count_configuration_file_path_literals(text: &str, limit: usize) -> usize {
    if limit == 0 {
        return 0;
    }
    let mut seen = BTreeSet::new();
    for literal in backtick_literals(text).into_iter().chain(whitespace_literals(text)) {
        let normalized = literal.trim_matches(literal_boundary_char).to_ascii_lowercase();
        if normalized.is_empty() {
            continue;
        }
        if configuration_file_path_literal(&normalized) && seen.insert(normalized) {
            if seen.len() >= limit {
                break;
            }
        }
    }
    seen.len()
}

fn count_configuration_section_literals(text: &str, limit: usize) -> usize {
    if limit == 0 {
        return 0;
    }
    let mut seen = BTreeSet::new();
    for literal in backtick_literals(text).into_iter().chain(whitespace_literals(text)) {
        let cleaned = literal.trim_matches(non_section_literal_boundary_char);
        if cleaned.len() <= 2 || !cleaned.starts_with('[') || !cleaned.ends_with(']') {
            continue;
        }
        if cleaned.chars().any(char::is_alphanumeric) && seen.insert(cleaned.to_string()) {
            if seen.len() >= limit {
                break;
            }
        }
    }
    seen.len()
}

fn count_configuration_assignment_literals(text: &str, limit: usize) -> usize {
    if limit == 0 {
        return 0;
    }
    let mut seen = BTreeSet::new();
    for literal in backtick_literals(text).into_iter().chain(whitespace_literals(text)) {
        let Some((name, _)) = literal.split_once('=') else {
            continue;
        };
        let name = name.trim_matches(literal_boundary_char);
        if identifier_like_literal(name) && seen.insert(name.to_string()) {
            if seen.len() >= limit {
                break;
            }
        }
    }
    seen.len()
}

fn count_identifier_like_backtick_literals(text: &str, limit: usize) -> usize {
    if limit == 0 {
        return 0;
    }
    let mut seen = BTreeSet::new();
    for literal in backtick_literals(text) {
        let cleaned = literal.trim_matches(literal_boundary_char);
        if identifier_like_literal(cleaned) && seen.insert(cleaned.to_string()) {
            if seen.len() >= limit {
                break;
            }
        }
    }
    seen.len()
}

fn configuration_file_path_literal(value: &str) -> bool {
    let has_path_separator = value.contains('/') || value.contains('\\');
    let has_config_extension =
        CONFIGURATION_FILE_EXTENSIONS.iter().any(|extension| value.ends_with(extension));
    has_path_separator && has_config_extension
}

fn identifier_like_literal(value: &str) -> bool {
    let value = value.trim();
    if !(2..=96).contains(&value.chars().count()) {
        return false;
    }
    let mut has_alphanumeric = false;
    let mut has_letter = false;
    let mut has_digit = false;
    let mut has_lowercase = false;
    let mut has_uppercase = false;
    let mut has_structural_separator = false;
    for ch in value.chars() {
        if ch.is_alphanumeric() {
            has_alphanumeric = true;
            has_letter |= ch.is_alphabetic();
            has_digit |= ch.is_numeric();
            has_lowercase |= ch.is_lowercase();
            has_uppercase |= ch.is_uppercase();
            continue;
        }
        if matches!(ch, '_' | '-' | '.' | ':') {
            has_structural_separator = true;
            continue;
        }
        return false;
    }
    has_alphanumeric
        && has_letter
        && (has_digit || has_structural_separator || (has_lowercase && has_uppercase))
}

fn backtick_literals(text: &str) -> Vec<&str> {
    let mut literals = Vec::new();
    let mut start = None;
    for (index, ch) in text.char_indices() {
        if ch != '`' {
            continue;
        }
        if let Some(open) = start.take() {
            let literal = text[open..index].trim();
            if !literal.is_empty() {
                literals.push(literal);
            }
        } else {
            start = Some(index + ch.len_utf8());
        }
    }
    literals
}

fn whitespace_literals(text: &str) -> impl Iterator<Item = &str> {
    text.split_whitespace()
}

fn literal_boundary_char(ch: char) -> bool {
    matches!(ch, '`' | '"' | '\'' | ',' | ';' | ':' | '.' | '(' | ')' | '[' | ']' | '{' | '}')
}

fn non_section_literal_boundary_char(ch: char) -> bool {
    matches!(ch, '`' | '"' | '\'' | ',' | ';' | ':' | '.' | '(' | ')' | '{' | '}')
}

fn push_target_type_once(target_types: &mut Vec<String>, target_type: &str) {
    if !target_types.iter().any(|current| current.trim().eq_ignore_ascii_case(target_type)) {
        target_types.push(target_type.to_string());
    }
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

fn repair_history_backed_elliptic_ref(
    question: &str,
    history: &[CompileHistoryTurn],
    ir: &mut QueryIR,
) {
    // An elliptic ref marks a follow-up whose subject is still *unresolved* and
    // must be recovered from history. If the compiler already folded the prior
    // turn into a concrete typed anchor (subject/object entities or an explicit
    // document focus), the ellipsis is resolved — flagging it would flip
    // `is_follow_up()` and wrongly disqualify focused-document scope and the
    // single-shot coverage path for what is a resolvable single-document query.
    if history.is_empty()
        || !ir.conversation_refs.is_empty()
        || ir.comparison.is_some()
        || ir.source_slice.is_some()
        || !ir.literal_constraints.is_empty()
        || !ir.temporal_constraints.is_empty()
        || !ir.target_entities.is_empty()
        || ir.document_focus.is_some()
    {
        return;
    }
    let current_question = effective_current_question_text(question);
    if !is_short_elliptic_question(current_question) {
        return;
    }
    tracing::info!(
        target: "ironrag::query_compile",
        question_len = current_question.len(),
        act = ir.act.as_str(),
        history_turn_count = history.len(),
        target_entities_count = ir.target_entities.len(),
        has_document_focus = ir.document_focus.is_some(),
        "query compile repaired history-backed elliptic reference"
    );
    ir.conversation_refs.push(UnresolvedRef {
        surface: current_question.to_string(),
        kind: ConversationRefKind::Elliptic,
    });
}

fn effective_current_question_text(question: &str) -> &str {
    current_question_segment(question)
}

fn is_short_elliptic_question(value: &str) -> bool {
    let tokens = normalized_repair_tokens(value);
    let informative_token_count = tokens.iter().filter(|token| token.chars().count() >= 4).count();
    !tokens.is_empty() && tokens.len() <= 2 && informative_token_count <= 1
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
        || !ir.literal_constraints.is_empty()
        || !ir.temporal_constraints.is_empty()
        || ir.source_slice.is_some()
}

const QUERY_COMPILER_SYSTEM_PROMPT: &str = "You are the IronRAG query compiler. Your only job is to \
read the user's natural-language question and, where present, a short window of prior conversation \
turns, and return a typed QueryIR JSON object. The JSON schema is supplied through the runtime \
structured-output contract; when the runtime cannot carry the full schema, the same schema is \
included in the system instructions. You MUST follow it exactly and MUST NOT add prose, commentary, \
code fences, or extra fields.\n\
\n\
Guiding principles:\n\
1. `act` captures what the user is fundamentally asking: `retrieve_value` (exact value), \
`describe`, `configure_how` (procedure), `compare`, `enumerate`, `meta` (about the library itself), \
or `follow_up` (refers to prior turn). When the current question is a short disambiguating \
selection for a prior substantive request, keep the prior request's act and use the current \
selection as the target; do not downgrade the act to `follow_up` just because the selection needs \
prior context. Requests for an artifact-shaped output such as a complete example, template, \
snippet, or file block for configuring a subject are still `configure_how`; do not downgrade them \
to `describe` merely because the user also forbids invented values or asks to use only retrieved \
fragments. For configuration-shaped artifacts, include `configuration_file` and `config_key` in \
`target_types` when the wording asks for files, sections, settings, parameters, or literal \
configuration values.\n\
2. `scope` is `multi_document` ONLY when the user explicitly names or clearly implies two or more \
documents / modules / subjects; `library_meta` when the question is about the library itself. \
Default is `single_document`.\n\
`document_focus` is a separate, narrower signal: set it (and keep `scope` = `single_document`) \
ONLY when the user EXPLICITLY references one specific document — a document title, a file name, a \
path-shaped identifier, a heading, or a phrasing that points inside one named document or page. A \
question that merely NAMES a product / module / subsystem / feature is NOT a document reference: \
emit that name as a `target_entities` mention (role `subject`) and leave `document_focus` null so \
retrieval can draw from every relevant document. Decide structurally, not by topic: a bare \
component name (token `S` alone) is a subject entity; a filename / path / heading / explicit \
in-document pointer (`S.conf`, `/v2/pay`) is a `document_focus`. A \
general how-to / describe / configure / enumerate question about a named component therefore stays \
library-scoped (`single_document` scope, no `document_focus`) unless the user also pins a concrete \
document identifier.\n\
3. `literal_constraints` captures verbatim strings the user quoted — URLs, file paths, parameter \
names, code identifiers, version numbers. If the user did not quote anything verbatim, the array \
is empty.\n\
4. `temporal_constraints` captures date/time or date-range references when present. Preserve the \
surface span exactly as visible to the user. Populate `start` and `end` with ISO-8601 UTC bounds \
whenever the surface contains a self-contained absolute date or date-range (year, year+month, \
year+month+day, quarter, year+week, ISO timestamp, decade). Treat `start` as inclusive and `end` as \
exclusive. Use null bounds ONLY when the reference is genuinely under-determined and has no runtime \
anchor or explicit absolute period. Absolute calendar references must resolve regardless of the \
writing system used in the original surface.\n\
\n\
Worked examples use numeric calendar forms so the rule stays script-agnostic:\n\
\n\
- surface: \"2026-03\" -> start: \"2026-03-01T00:00:00Z\", end: \"2026-04-01T00:00:00Z\"\n\
- surface: \"2026-03-27\" -> start: \"2026-03-27T00:00:00Z\", end: \"2026-03-28T00:00:00Z\"\n\
- surface: \"2026-Q1\" -> start: \"2026-01-01T00:00:00Z\", end: \"2026-04-01T00:00:00Z\"\n\
- surface: \"2026-W13\" -> start: \"2026-03-23T00:00:00Z\", end: \"2026-03-30T00:00:00Z\"\n\
- surface: \"2026-03-27T14:30:00Z\" -> start: \"2026-03-27T14:30:00Z\", end: \"2026-03-27T14:30:01Z\"\n\
5. `conversation_refs` lists unresolved anaphora / deixis / ellipsis that point to prior \
user-assistant turns, not to positions, ranges, neighboring units, or anchors inside the source \
documents being searched. `act = follow_up` is typical when the question cannot stand on its own.\n\
6. `target_types` are canonical ontology tags. Use built-in tags exactly as written when they fit: \
endpoint, url, path, wsdl, base_url, port, parameter, http_method, protocol, config_key, \
configuration_file, filesystem_path, software_module, package, error_code, env_var, version, release, \
procedure, metric, table_row, table_summary, table_average, table_frequency, document, \
primary_heading, secondary_heading, formats_under_test, concept, relationship. For graph facts, \
use runtime node-type tags: person, organization, location, event, artifact, natural, process, \
concept, attribute, entity, software_module. \
Endpoint-like tags (`endpoint`, `url`, `path`, `wsdl`, `base_url`) identify exact network or \
interface identifiers only; do not use them for timing, severity, status, count, metric, or \
outcome attributes unless the user asks for the identifier itself. You may invent a new singular \
snake_case tag only when no built-in tag fits.\n\
7. `source_slice` is null for ordinary summaries, comparisons, procedures, and needle lookups. \
Set it only when the user asks for a positional slice of a sequential source: earliest units \
(`head`), latest units (`tail`), or a bounded representation of the whole sequence (`all`). \
Populate `count` only when the user asks for a concrete number of units. Set \
`source_slice.filter` to `release_marker` only when the slice asks for latest version/release \
records; otherwise set it to `none`.\n\
For ordered change-summary requests over version or release records, include the `release` or \
`version` target type and use a `tail` source slice when the user asks for the latest records.\n\
8. `target_entities[*].label`, `document_focus.hint`, and verbatim literal-like values must \
preserve the exact writing system and spelling visible in the current question or prior turns. \
When a named target appears in that user-visible text, emit that target as a verbatim substring; \
do not translate, transliterate, normalize look-alike glyphs, or substitute visually similar \
characters.\n\
When a scoped follow-up includes a `literal anchors:` line, treat those values as prior-turn \
referents for retrieval and disambiguation. They may populate `literal_constraints` when the \
current question points back to them, but they are not source evidence by themselves.\n\
9. `confidence` ∈ [0.0, 1.0]. Use < 0.6 only when you genuinely cannot pin the question.\n\
10. `language` must use one of the schema enum values; prefer `auto` when the signal is mixed or \
unclear.\n\
11. `retrieval_query` is a self-contained search string a fresh retriever could run with no access \
to the prior turns. When the current question already stands on its own, copy it verbatim. When it \
depends on prior turns — a bare selection, a pronoun, an omitted subject — fold the subject and \
scope recovered FROM THE PRIOR TURNS into one standalone phrasing while keeping the user's intent \
and granularity. Compose it ONLY from terms that literally appear in the current question or the \
prior turns; never invent a subject the conversation did not mention and never add words from \
outside the transcript. Preserve the exact writing system and spelling of every carried-over term; \
never translate, transliterate, or substitute look-alike glyphs. Mechanically: if the prior turns \
established a subject token S and the current turn supplies only a refinement token R, then \
`retrieval_query` is a standalone phrasing whose tokens are drawn verbatim from {S, R} in the \
transcript; if the current turn already carries its own subject token, `retrieval_query` equals the \
current question verbatim.\n\
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
    use crate::domains::query_ir::QueryLanguage;
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
            api_key: Some("test-key".to_string()),
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
    async fn repairs_structural_configuration_request_from_unfocused_provider_ir() {
        let ir_json = json!({
            "act": "describe",
            "scope": "single_document",
            "language": "en",
            "target_types": [],
            "target_entities": [],
            "literal_constraints": [],
            "temporal_constraints": [],
            "comparison": null,
            "document_focus": null,
            "conversation_refs": [],
            "needs_clarification": null,
            "source_slice": null,
            "confidence": 0.25
        })
        .to_string();
        let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
        let service = QueryCompilerService;
        let binding = sample_binding();
        let question = "scope: Provider Alpha setup was selected earlier\nliteral anchors: `/opt/alpha/alpha.conf`, `[Main]`, `alphaEndpoint`, `alphaSecret`, `printSlip`\nquestion: Provider Alpha setup parameter inventory";

        let outcome = service
            .compile_with_gateway(&gateway, &binding, question, &[])
            .await
            .expect("compile ok");

        assert_eq!(outcome.ir.act, QueryAct::ConfigureHow);
        assert!(outcome.ir.confidence >= STRUCTURAL_CONFIGURATION_REPAIR_CONFIDENCE);
        for target_type in STRUCTURAL_CONFIGURATION_TARGET_TYPES {
            assert!(
                outcome.ir.target_types.iter().any(|value| value == target_type),
                "{:?}",
                outcome.ir.target_types
            );
        }
    }

    #[tokio::test]
    async fn low_confidence_plain_question_without_configuration_surface_stays_unfocused() {
        let ir_json = json!({
            "act": "describe",
            "scope": "single_document",
            "language": "en",
            "target_types": [],
            "target_entities": [],
            "literal_constraints": [],
            "temporal_constraints": [],
            "comparison": null,
            "document_focus": null,
            "conversation_refs": [],
            "needs_clarification": null,
            "source_slice": null,
            "confidence": 0.25
        })
        .to_string();
        let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
        let service = QueryCompilerService;
        let binding = sample_binding();

        let outcome = service
            .compile_with_gateway(&gateway, &binding, "Alpha Suite X9 2.3.4 2.3.5", &[])
            .await
            .expect("compile ok");

        assert_eq!(outcome.ir.act, QueryAct::Describe);
        assert!(outcome.ir.target_types.is_empty());
    }

    #[tokio::test]
    async fn downgrades_overeager_single_document_focus_for_generic_named_component() {
        // High-confidence configure_how about a named module that the model
        // over-pinned to a single document by echoing the module name as the
        // focus hint. No quoted literal, no document/heading target type, and
        // the hint is a bare name -> the pin is a false trigger and must drop.
        let ir_json = json!({
            "act": "configure_how",
            "scope": "single_document",
            "language": "en",
            "target_types": ["procedure"],
            "target_entities": [{"label": "S", "role": "subject"}],
            "literal_constraints": [],
            "temporal_constraints": [],
            "comparison": null,
            "document_focus": {"hint": "S"},
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
            .compile_with_gateway(&gateway, &binding, "how do I update S", &[])
            .await
            .expect("compile ok");

        assert_eq!(outcome.ir.scope, QueryScope::MultiDocument);
        assert!(outcome.ir.document_focus.is_none());
        assert_eq!(outcome.ir.act, QueryAct::ConfigureHow);
    }

    #[tokio::test]
    async fn preserves_single_document_focus_when_user_quotes_a_technical_literal() {
        // Same generic shape, but the user quoted a path-shaped literal — an
        // explicit reference to a specific document/object — so the pin stays.
        let ir_json = json!({
            "act": "configure_how",
            "scope": "single_document",
            "language": "en",
            "target_types": ["procedure"],
            "target_entities": [{"label": "S", "role": "subject"}],
            "literal_constraints": [{"text": "/v2/pay", "kind": "path"}],
            "temporal_constraints": [],
            "comparison": null,
            "document_focus": {"hint": "S"},
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
            .compile_with_gateway(&gateway, &binding, "how do I configure /v2/pay", &[])
            .await
            .expect("compile ok");

        assert_eq!(outcome.ir.scope, QueryScope::SingleDocument);
        assert!(outcome.ir.document_focus.is_some());
    }

    #[tokio::test]
    async fn preserves_single_document_focus_when_focus_hint_is_a_filename() {
        // The focus hint itself is a filename / path token — a structural
        // document reference — so it is a genuine single-document pin.
        let ir_json = json!({
            "act": "describe",
            "scope": "single_document",
            "language": "en",
            "target_types": ["procedure"],
            "target_entities": [{"label": "S", "role": "subject"}],
            "literal_constraints": [],
            "temporal_constraints": [],
            "comparison": null,
            "document_focus": {"hint": "S.conf"},
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
            .compile_with_gateway(&gateway, &binding, "describe S.conf", &[])
            .await
            .expect("compile ok");

        assert_eq!(outcome.ir.scope, QueryScope::SingleDocument);
        assert_eq!(
            outcome.ir.document_focus.as_ref().map(|focus| focus.hint.as_str()),
            Some("S.conf")
        );
    }

    #[tokio::test]
    async fn preserves_single_document_focus_when_target_type_is_document() {
        // A `document` ontology target type is an explicit document reference
        // even when the focus hint is a bare name.
        let ir_json = json!({
            "act": "describe",
            "scope": "single_document",
            "language": "en",
            "target_types": ["document"],
            "target_entities": [{"label": "S", "role": "subject"}],
            "literal_constraints": [],
            "temporal_constraints": [],
            "comparison": null,
            "document_focus": {"hint": "S"},
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
            .compile_with_gateway(&gateway, &binding, "describe the S document", &[])
            .await
            .expect("compile ok");

        assert_eq!(outcome.ir.scope, QueryScope::SingleDocument);
        assert!(outcome.ir.document_focus.is_some());
    }

    #[tokio::test]
    async fn keeps_low_confidence_single_document_focus_untouched() {
        // Below the downgrade confidence floor the IR is treated as genuinely
        // uncertain; the focus is left alone for the clarify / stub path.
        let ir_json = json!({
            "act": "describe",
            "scope": "single_document",
            "language": "en",
            "target_types": ["procedure"],
            "target_entities": [{"label": "S", "role": "subject"}],
            "literal_constraints": [],
            "temporal_constraints": [],
            "comparison": null,
            "document_focus": {"hint": "S"},
            "conversation_refs": [],
            "needs_clarification": null,
            "source_slice": null,
            "confidence": 0.4
        })
        .to_string();
        let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
        let service = QueryCompilerService;
        let binding = sample_binding();

        let outcome =
            service.compile_with_gateway(&gateway, &binding, "S", &[]).await.expect("compile ok");

        assert_eq!(outcome.ir.scope, QueryScope::SingleDocument);
        assert!(outcome.ir.document_focus.is_some());
    }

    #[tokio::test]
    async fn normalizes_broad_latest_release_ir_to_release_tail_slice() {
        let ir_json = json!({
            "act": "enumerate",
            "scope": "single_document",
            "language": "en",
            "target_types": ["release", "version"],
            "target_entities": [],
            "literal_constraints": [],
            "temporal_constraints": [],
            "comparison": null,
            "document_focus": null,
            "conversation_refs": [],
            "needs_clarification": null,
            "source_slice": null,
            "confidence": 0.95
        })
        .to_string();
        let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
        let service = QueryCompilerService;
        let binding = sample_binding();

        let outcome = service
            .compile_with_gateway(&gateway, &binding, "list the latest release records", &[])
            .await
            .expect("compile ok");

        assert_eq!(outcome.ir.scope, QueryScope::SingleDocument);
        let slice = outcome.ir.source_slice.expect("latest release requests need ordered context");
        assert_eq!(slice.direction, SourceSliceDirection::Tail);
        assert_eq!(slice.count, Some(10));
        assert_eq!(slice.filter, SourceSliceFilter::ReleaseMarker);
    }

    #[tokio::test]
    async fn latest_release_normalization_preserves_numeric_literal_count() {
        let ir_json = json!({
            "act": "enumerate",
            "scope": "single_document",
            "language": "en",
            "target_types": ["release", "version"],
            "target_entities": [],
            "literal_constraints": [{"text": "3", "kind": "numeric_code"}],
            "temporal_constraints": [],
            "comparison": null,
            "document_focus": null,
            "conversation_refs": [],
            "needs_clarification": null,
            "source_slice": null,
            "confidence": 0.95
        })
        .to_string();
        let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
        let service = QueryCompilerService;
        let binding = sample_binding();

        let outcome = service
            .compile_with_gateway(&gateway, &binding, "list the latest 3 release records", &[])
            .await
            .expect("compile ok");

        assert_eq!(outcome.ir.source_slice.as_ref().and_then(|slice| slice.count), Some(3));
        assert_eq!(
            crate::services::query::latest_versions::requested_latest_version_count(&outcome.ir),
            3
        );
    }

    #[tokio::test]
    async fn exact_version_literal_does_not_synthesize_latest_release_slice() {
        let ir_json = json!({
            "act": "describe",
            "scope": "single_document",
            "language": "en",
            "target_types": ["release", "version"],
            "target_entities": [{"label": "1.2.3", "role": "subject"}],
            "literal_constraints": [{"text": "1.2.3", "kind": "version"}],
            "temporal_constraints": [],
            "comparison": null,
            "document_focus": {"hint": "release 1.2.3"},
            "conversation_refs": [],
            "needs_clarification": null,
            "source_slice": null,
            "confidence": 0.95
        })
        .to_string();
        let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
        let service = QueryCompilerService;
        let binding = sample_binding();

        let outcome = service
            .compile_with_gateway(&gateway, &binding, "what changed in release 1.2.3", &[])
            .await
            .expect("compile ok");

        assert!(outcome.ir.source_slice.is_none());
        assert!(!crate::services::query::latest_versions::query_requests_latest_versions(
            &outcome.ir
        ));
    }

    #[tokio::test]
    async fn preserves_focused_latest_release_scope_when_normalizing_tail_slice() {
        let ir_json = json!({
            "act": "describe",
            "scope": "single_document",
            "language": "en",
            "target_types": ["release"],
            "target_entities": [{"label": "Alpha Suite", "role": "subject"}],
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
            .compile_with_gateway(&gateway, &binding, "latest Alpha Suite release", &[])
            .await
            .expect("compile ok");

        assert_eq!(outcome.ir.scope, QueryScope::SingleDocument);
        assert_eq!(
            outcome.ir.source_slice.as_ref().map(|slice| slice.direction),
            Some(SourceSliceDirection::Tail)
        );
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
    async fn preserves_stateless_document_focus_follow_up_signal() {
        let ir_json = json!({
            "act": "follow_up",
            "scope": "single_document",
            "language": "en",
            "target_types": [],
            "target_entities": [],
            "literal_constraints": [],
            "temporal_constraints": [],
            "comparison": null,
            "document_focus": {"hint": "Alpha Guide"},
            "conversation_refs": [{"surface": "that one", "kind": "deictic"}],
            "needs_clarification": "anaphora_unresolved",
            "source_slice": null,
            "confidence": 0.35
        })
        .to_string();
        let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
        let service = QueryCompilerService;
        let binding = sample_binding();

        let outcome = service
            .compile_with_gateway(&gateway, &binding, "that one", &[])
            .await
            .expect("compile ok");

        assert_eq!(outcome.ir.act, QueryAct::FollowUp);
        assert_eq!(outcome.ir.conversation_refs.len(), 1);
        assert!(outcome.ir.document_focus.is_some());
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
    async fn anchored_short_follow_up_stays_resolved_without_spurious_elliptic_ref() {
        let ir_json = json!({
            "act": "configure_how",
            "scope": "single_document",
            "language": "en",
            "target_types": ["procedure"],
            "target_entities": [{"label": "Beta", "role": "subject"}],
            "literal_constraints": [],
            "temporal_constraints": [],
            "comparison": null,
            "document_focus": {"hint": "Beta Setup Guide"},
            "conversation_refs": [],
            "needs_clarification": null,
            "source_slice": null,
            "confidence": 0.8
        })
        .to_string();
        let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
        let service = QueryCompilerService;
        let binding = sample_binding();
        let history = vec![CompileHistoryTurn {
            role: "user".to_string(),
            content: "How do I configure payment providers?".to_string(),
        }];

        let outcome = service
            .compile_with_gateway(&gateway, &binding, "Beta", &history)
            .await
            .expect("compile ok");

        // A short follow-up the compiler already resolved into a concrete
        // anchor (subject entity + document focus) must NOT be flagged as an
        // unresolved ellipsis, so it stays a resolvable single-document query
        // and keeps focused-document scope available downstream.
        assert_eq!(outcome.ir.act, QueryAct::ConfigureHow);
        assert!(outcome.ir.conversation_refs.is_empty());
        assert!(!outcome.ir.is_follow_up());
    }

    #[tokio::test]
    async fn anchored_effective_query_short_question_stays_resolved() {
        let ir_json = json!({
            "act": "retrieve_value",
            "scope": "single_document",
            "language": "en",
            "target_types": ["parameter"],
            "target_entities": [{"label": "Gamma", "role": "subject"}],
            "literal_constraints": [],
            "temporal_constraints": [],
            "comparison": null,
            "document_focus": null,
            "conversation_refs": [],
            "needs_clarification": null,
            "source_slice": null,
            "confidence": 0.75
        })
        .to_string();
        let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
        let service = QueryCompilerService;
        let binding = sample_binding();
        let history = vec![CompileHistoryTurn {
            role: "user".to_string(),
            content: "Which parameters are supported?".to_string(),
        }];

        let outcome = service
            .compile_with_gateway(
                &gateway,
                &binding,
                "scope: Which parameters are supported?\nquestion: Gamma",
                &history,
            )
            .await
            .expect("compile ok");

        // Subject entity is a concrete anchor: no spurious elliptic ref.
        assert_eq!(outcome.ir.act, QueryAct::RetrieveValue);
        assert!(outcome.ir.conversation_refs.is_empty());
        assert!(!outcome.ir.is_follow_up());
    }

    #[tokio::test]
    async fn anchorless_short_follow_up_still_flags_elliptic_ref() {
        let ir_json = json!({
            "act": "describe",
            "scope": "single_document",
            "language": "en",
            "target_types": ["concept"],
            "target_entities": [],
            "literal_constraints": [],
            "temporal_constraints": [],
            "comparison": null,
            "document_focus": null,
            "conversation_refs": [],
            "needs_clarification": null,
            "source_slice": null,
            "confidence": 0.7
        })
        .to_string();
        let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
        let service = QueryCompilerService;
        let binding = sample_binding();
        let history = vec![CompileHistoryTurn {
            role: "user".to_string(),
            content: "How do I configure payment providers?".to_string(),
        }];

        let outcome = service
            .compile_with_gateway(&gateway, &binding, "details", &history)
            .await
            .expect("compile ok");

        // No concrete anchor was recovered, so the short follow-up remains an
        // unresolved ellipsis that downstream stages must resolve from history.
        assert_eq!(outcome.ir.conversation_refs.len(), 1);
        assert_eq!(outcome.ir.conversation_refs[0].kind, ConversationRefKind::Elliptic);
    }

    #[tokio::test]
    async fn returns_provider_failure_on_provider_error() {
        let gateway = StubGateway::new(Err(anyhow::anyhow!("upstream 503")));
        let service = QueryCompilerService;
        let binding = sample_binding();

        let error = service
            .compile_with_gateway(&gateway, &binding, "what is /system/info?", &[])
            .await
            .expect_err("provider failure must fail the compile");

        assert!(matches!(error, ApiError::ProviderFailure(_)));
    }

    #[tokio::test]
    async fn returns_provider_failure_on_invalid_ir_output() {
        let gateway = StubGateway::new(Ok(chat_response_with("not valid json")));
        let service = QueryCompilerService;
        let binding = sample_binding();

        let error = service
            .compile_with_gateway(&gateway, &binding, "anything", &[])
            .await
            .expect_err("invalid IR must fail the compile");

        assert!(matches!(error, ApiError::ProviderFailure(_)));
    }

    #[tokio::test]
    async fn history_turns_are_embedded_in_prompt() {
        let ir_json = serde_json::to_string(&canonical_ir()).unwrap();
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

    #[test]
    fn compiler_prompt_preserves_prior_act_for_short_disambiguator() {
        assert!(QUERY_COMPILER_SYSTEM_PROMPT.contains("short disambiguating selection"));
        assert!(QUERY_COMPILER_SYSTEM_PROMPT.contains("keep the prior request's act"));
        assert!(QUERY_COMPILER_SYSTEM_PROMPT.contains("do not downgrade the act to `follow_up`"));
    }

    #[test]
    fn compiler_prompt_keeps_configuration_artifact_requests_as_configure_how() {
        assert!(QUERY_COMPILER_SYSTEM_PROMPT.contains("artifact-shaped output"));
        assert!(QUERY_COMPILER_SYSTEM_PROMPT.contains("still `configure_how`"));
        assert!(QUERY_COMPILER_SYSTEM_PROMPT.contains("do not downgrade them to `describe`"));
        assert!(QUERY_COMPILER_SYSTEM_PROMPT.contains("`configuration_file` and `config_key`"));
    }

    // -----------------------------------------------------------------
    // Two-level cache tests — mirror `StubGateway` with a `StubCache`
    // keyed by `(library_id, question_hash)` so we can assert both the
    // read-through path (binding-scoped hit skips the LLM) and the
    // write-through path (successful compile populates the cache) without
    // any real Redis or Postgres.
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
            retrieval_query: None,
            confidence: 0.9,
        }
    }

    #[tokio::test]
    async fn cache_hit_short_circuits_llm() {
        let library_id = Uuid::now_v7();
        let question = "how do I configure the payment module?";
        let history: Vec<CompileHistoryTurn> = Vec::new();
        let service = QueryCompilerService;
        let binding = sample_binding();
        let hash = hash_compile_request(question, &history, QUERY_IR_SCHEMA_VERSION, &binding);
        let cached = CachedIrEntry {
            ir: canonical_ir(),
            provider_kind: CACHE_HIT_REDIS_PROVIDER_KIND.to_string(),
            model_name: String::new(),
            usage_json: json!({"source": "redis"}),
        };
        let cache = StubCache::seeded(library_id, hash, cached);
        let gateway =
            StubGateway::new(Err(anyhow::anyhow!("gateway must not be called on cache hit")));

        let outcome = service
            .compile_with_cache_and_gateway(
                &cache, &gateway, &binding, library_id, question, &history,
            )
            .await
            .expect("cache hit is a success path");

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
    async fn cache_key_is_scoped_to_resolved_binding() {
        let library_id = Uuid::now_v7();
        let question = "how do I configure the payment module?";
        let history: Vec<CompileHistoryTurn> = Vec::new();
        let binding = sample_binding();
        let mut other_binding = binding.clone();
        other_binding.binding_id = Uuid::now_v7();
        other_binding.model_catalog_id = Uuid::now_v7();
        other_binding.model_name = "gpt-5.4-mini".to_string();

        let hash = hash_compile_request(question, &history, QUERY_IR_SCHEMA_VERSION, &binding);
        let cache = StubCache::seeded(
            library_id,
            hash,
            CachedIrEntry {
                ir: canonical_ir(),
                provider_kind: CACHE_HIT_REDIS_PROVIDER_KIND.to_string(),
                model_name: String::new(),
                usage_json: json!({"source": "redis"}),
            },
        );
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
        let service = QueryCompilerService;

        let outcome = service
            .compile_with_cache_and_gateway(
                &cache,
                &gateway,
                &other_binding,
                library_id,
                question,
                &history,
            )
            .await
            .expect("binding-scoped cache miss should compile live");

        assert_eq!(outcome.ir.act, QueryAct::RetrieveValue);
        assert_eq!(
            gateway
                .last_request
                .lock()
                .unwrap()
                .as_ref()
                .map(|request| request.model_name.as_str()),
            Some("gpt-5.4-mini")
        );
        assert_eq!(cache.put_calls(), 1);
    }

    #[tokio::test]
    async fn provider_failure_is_not_cached() {
        let library_id = Uuid::now_v7();
        let question = "anything";
        let history: Vec<CompileHistoryTurn> = Vec::new();
        let gateway = StubGateway::new(Err(anyhow::anyhow!("upstream 503")));
        let cache = StubCache::default();
        let service = QueryCompilerService;
        let binding = sample_binding();

        let error = service
            .compile_with_cache_and_gateway(
                &cache, &gateway, &binding, library_id, question, &history,
            )
            .await
            .expect_err("provider failure must fail the compile");

        assert!(matches!(error, ApiError::ProviderFailure(_)));
        assert_eq!(cache.put_calls(), 0, "failed compiles must not be cached");
        assert_eq!(cache.len(), 0);
    }

    #[tokio::test]
    async fn hash_compile_request_is_normalized_history_and_binding_sensitive() {
        let binding = sample_binding();
        let base = hash_compile_request("Hello World", &[], QUERY_IR_SCHEMA_VERSION, &binding);
        let variant =
            hash_compile_request("  hello world  ", &[], QUERY_IR_SCHEMA_VERSION, &binding);
        assert_eq!(base, variant, "trim + lowercase must produce the same hash");

        let with_history = hash_compile_request(
            "Hello World",
            &[CompileHistoryTurn {
                role: "user".to_string(),
                content: "prior context".to_string(),
            }],
            QUERY_IR_SCHEMA_VERSION,
            &binding,
        );
        assert_ne!(base, with_history, "history must contribute to the hash");

        let bumped = hash_compile_request(
            "Hello World",
            &[],
            QUERY_IR_SCHEMA_VERSION.wrapping_add(1),
            &binding,
        );
        assert_ne!(base, bumped, "schema_version must contribute to the hash");

        let mut other_binding = binding;
        other_binding.model_name = "gpt-5.4-mini".to_string();
        let other_binding_hash =
            hash_compile_request("Hello World", &[], QUERY_IR_SCHEMA_VERSION, &other_binding);
        assert_ne!(base, other_binding_hash, "binding fingerprint must contribute to the hash");

        assert_eq!(
            query_ir_runtime_fingerprint().len(),
            64,
            "runtime fingerprint is a SHA-256 hex digest"
        );
    }
}
