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
//!   provider output fail loudly from the compiler service with
//!   `ApiError::ProviderFailure`. The canonical answer boundary may recover
//!   with `provider_free_fallback_query_ir`, whose low-confidence IR keeps
//!   the full retrieval query and formal evidence visible in diagnostics
//!   without guessing semantic intent or language.
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
use std::sync::LazyLock;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        ai::AiBindingPurpose,
        query_ir::{
            ClarificationReason, LiteralKind, LiteralSpan, QUERY_IR_SCHEMA_VERSION, QueryAct,
            QueryIR, QueryLanguage, QueryScope, VerificationLevel, query_ir_json_schema,
        },
    },
    infra::repositories::query_ir_cache_repository::{get_query_ir_cache, upsert_query_ir_cache},
    integrations::llm::{ChatResponse, LlmGateway, build_structured_chat_request},
    interfaces::http::router_support::ApiError,
    services::{
        ai_catalog_service::ResolvedRuntimeBinding,
        query::provider_billing::{QueryProviderCallReservation, QueryProviderExecutionContext},
    },
};

/// Canonical Redis key prefix for the hot IR cache.
const REDIS_IR_CACHE_PREFIX: &str = "ir_cache";
const PROVIDER_FREE_FALLBACK_LITERAL_LIMIT: usize = 8;
const PROVIDER_FREE_FALLBACK_TOKEN_MAX_CHARS: usize = 80;
const PROVIDER_FREE_FALLBACK_QUOTED_LITERAL_MAX_CHARS: usize = 240;
const PROVIDER_FREE_FALLBACK_CONFIDENCE: f32 = 0.25;

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
pub(crate) struct CompileQueryCommand {
    pub(crate) library_id: Uuid,
    pub(crate) execution_context: QueryProviderExecutionContext,
    pub(crate) question: String,
    /// Last N turns of conversation, ordered oldest → newest. Empty for the
    /// first turn in a session. The compiler only uses this to detect
    /// unresolved references — it is NOT fed to downstream retrieval.
    pub(crate) history: Vec<CompileHistoryTurn>,
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
/// content-addressed over the exact question/history bytes under the same
/// compiler runtime and resolved QueryCompile binding. Case, whitespace, and
/// formal literal spelling are source-significant and must never alias.
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
    hash_field(&mut hasher, "question", question);
    hash_field(&mut hasher, "history_len", &history.len().to_string());
    for turn in history {
        hash_field(&mut hasher, "history_role", &turn.role);
        hash_field(&mut hasher, "history_content", &turn.content);
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
    hash_field(&mut hasher, "account_id", &binding.account_id.to_string());
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
    pub(crate) async fn compile(
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

        if let Some(entry) = cache.get(command.library_id, &question_hash).await
            && let Some(outcome) =
                validated_cached_outcome(entry, &command.question, &command.history)
        {
            return Ok(outcome);
        }

        let mut provider_call = QueryProviderCallReservation::reserve(
            state,
            command.execution_context,
            &binding,
            AiBindingPurpose::QueryCompile,
            "query_compile",
        )
        .await?;

        let response = match self
            .request_compile_response(
                state.llm_gateway.as_ref(),
                &binding,
                &command.question,
                &command.history,
            )
            .await
        {
            Ok(response) => response,
            Err(error) => {
                if let Err(billing_error) = provider_call.fail().await {
                    tracing::error!(
                        provider_call_id = %provider_call.provider_call_id(),
                        %billing_error,
                        "failed to terminalize query compiler provider-call reservation"
                    );
                }
                return Err(error);
            }
        };
        // Account for the paid response before parsing or semantic validation:
        // a malformed/rejected QueryIR is still a real provider call.
        provider_call.complete(&response.usage_json).await?;
        let outcome =
            self.compile_response(&binding, &command.question, &command.history, response)?;

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

        if let Some(entry) = cache.get(library_id, &question_hash).await
            && let Some(outcome) = validated_cached_outcome(entry, question, history)
        {
            return Ok(outcome);
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
    /// Production callers use the canonical compiler path.
    pub async fn compile_with_gateway(
        &self,
        gateway: &dyn LlmGateway,
        binding: &ResolvedRuntimeBinding,
        question: &str,
        history: &[CompileHistoryTurn],
    ) -> Result<CompileQueryOutcome, ApiError> {
        let response = self.request_compile_response(gateway, binding, question, history).await?;
        self.compile_response(binding, question, history, response)
    }

    async fn request_compile_response(
        &self,
        gateway: &dyn LlmGateway,
        binding: &ResolvedRuntimeBinding,
        question: &str,
        history: &[CompileHistoryTurn],
    ) -> Result<ChatResponse, ApiError> {
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
        Ok(response)
    }

    fn compile_response(
        &self,
        binding: &ResolvedRuntimeBinding,
        question: &str,
        history: &[CompileHistoryTurn],
        response: ChatResponse,
    ) -> Result<CompileQueryOutcome, ApiError> {
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
        if let Err(error) = validate_compiled_ir_for_request(question, history, &ir) {
            tracing::error!(
                provider = %binding.provider_kind,
                model = %binding.model_name,
                ?error,
                "query compile output failed typed request validation"
            );
            return Err(ApiError::ProviderFailure(format!(
                "QueryCompile provider returned invalid QueryIR for provider `{}` model `{}`",
                binding.provider_kind, binding.model_name
            )));
        }

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

fn validated_cached_outcome(
    entry: CachedIrEntry,
    question: &str,
    history: &[CompileHistoryTurn],
) -> Option<CompileQueryOutcome> {
    if let Err(error) = validate_compiled_ir_for_request(question, history, &entry.ir) {
        tracing::warn!(
            provider = %entry.provider_kind,
            model = %entry.model_name,
            ?error,
            "query compiler cache entry failed typed request validation; recompiling"
        );
        return None;
    }
    Some(cached_outcome(entry))
}

#[derive(Debug, Clone, PartialEq)]
enum CompileRequestValidationError {
    InvalidTypedIr(crate::domains::query_ir::QueryIrValidationError),
    StatelessConversationDependency,
    UngroundedTargetEntity,
    UngroundedLiteral,
    UngroundedTemporalSurface,
    UngroundedDocumentFocus,
    UngroundedConversationReference,
    UngroundedComparisonOperand,
    IncompleteRetrievalQuery,
}

fn validate_compiled_ir_for_request(
    question: &str,
    history: &[CompileHistoryTurn],
    ir: &QueryIR,
) -> Result<(), CompileRequestValidationError> {
    crate::domains::query_ir::validate_ir(ir)
        .map_err(CompileRequestValidationError::InvalidTypedIr)?;

    if history.is_empty()
        && (ir.is_follow_up()
            || ir.needs_clarification.as_ref().is_some_and(|clarification| {
                matches!(clarification.reason, ClarificationReason::AnaphoraUnresolved)
            }))
    {
        return Err(CompileRequestValidationError::StatelessConversationDependency);
    }

    if ir
        .target_entities
        .iter()
        .any(|entity| !request_contains_exact_surface(question, history, &entity.label))
    {
        return Err(CompileRequestValidationError::UngroundedTargetEntity);
    }
    if ir
        .literal_constraints
        .iter()
        .any(|literal| !request_contains_exact_surface(question, history, &literal.text))
    {
        return Err(CompileRequestValidationError::UngroundedLiteral);
    }
    if ir
        .temporal_constraints
        .iter()
        .any(|constraint| !request_contains_exact_surface(question, history, &constraint.surface))
    {
        return Err(CompileRequestValidationError::UngroundedTemporalSurface);
    }
    if ir
        .document_focus
        .as_ref()
        .is_some_and(|focus| !request_contains_exact_surface(question, history, &focus.hint))
    {
        return Err(CompileRequestValidationError::UngroundedDocumentFocus);
    }
    if ir
        .conversation_refs
        .iter()
        .any(|reference| !request_contains_exact_surface(question, history, &reference.surface))
    {
        return Err(CompileRequestValidationError::UngroundedConversationReference);
    }
    if ir.comparison.as_ref().is_some_and(|comparison| {
        comparison
            .a
            .as_deref()
            .into_iter()
            .chain(comparison.b.as_deref())
            .any(|operand| !request_contains_exact_surface(question, history, operand))
    }) {
        return Err(CompileRequestValidationError::UngroundedComparisonOperand);
    }
    let history_target_requires_self_contained_query = ir.target_entities.iter().any(|entity| {
        !question.contains(&entity.label)
            && history.iter().any(|turn| turn.content.contains(&entity.label))
    });
    let retrieval_query = ir.retrieval_query.as_deref();
    if history_target_requires_self_contained_query
        && ir
            .target_entities
            .iter()
            .any(|entity| !retrieval_query.is_some_and(|query| query.contains(&entity.label)))
    {
        return Err(CompileRequestValidationError::IncompleteRetrievalQuery);
    }

    Ok(())
}

fn request_contains_exact_surface(
    question: &str,
    history: &[CompileHistoryTurn],
    surface: &str,
) -> bool {
    !surface.is_empty()
        && surface == surface.trim()
        && (question.contains(surface) || history.iter().any(|turn| turn.content.contains(surface)))
}

/// Build a conservative IR without calling a model after the configured query
/// compiler fails.
///
/// Semantic classification belongs exclusively to the operator-configured
/// compiler model. This fallback therefore preserves the full retrieval query
/// and extracts only language-neutral formal evidence (quoted spans and tokens
/// shaped like paths, URLs, versions, or identifiers). It never infers an act,
/// language, entity, ontology target, or ordered source slice from raw words.
#[must_use]
pub(crate) fn provider_free_fallback_query_ir(question: &str) -> QueryIR {
    let retrieval_query = question.trim();

    QueryIR {
        act: QueryAct::Describe,
        scope: QueryScope::MultiDocument,
        language: QueryLanguage::Auto,
        target_types: Vec::new(),
        target_entities: Vec::new(),
        literal_constraints: provider_free_fallback_literal_constraints(question),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: (!retrieval_query.is_empty()).then(|| retrieval_query.to_string()),
        confidence: PROVIDER_FREE_FALLBACK_CONFIDENCE,
    }
}

fn provider_free_fallback_literal_constraints(question: &str) -> Vec<LiteralSpan> {
    let mut literals = provider_free_quoted_literals(question)
        .into_iter()
        .map(|text| LiteralSpan { kind: LiteralKind::infer(&text), text })
        .collect::<Vec<_>>();
    literals.truncate(PROVIDER_FREE_FALLBACK_LITERAL_LIMIT);
    for token in provider_free_structural_question_tokens(question) {
        if literals.len() >= PROVIDER_FREE_FALLBACK_LITERAL_LIMIT {
            break;
        }
        let kind = LiteralKind::infer(&token);
        if provider_free_literal_kind_is_structural(kind, &token) {
            push_provider_free_fallback_literal(&mut literals, LiteralSpan { text: token, kind });
        }
    }
    literals
}

fn push_provider_free_fallback_literal(literals: &mut Vec<LiteralSpan>, literal: LiteralSpan) {
    if literals.len() < PROVIDER_FREE_FALLBACK_LITERAL_LIMIT
        && !literals.iter().any(|existing| existing.text.eq_ignore_ascii_case(&literal.text))
    {
        literals.push(literal);
    }
}

fn provider_free_literal_kind_is_structural(kind: LiteralKind, text: &str) -> bool {
    matches!(kind, LiteralKind::Url | LiteralKind::Path | LiteralKind::Version)
        || (matches!(kind, LiteralKind::Identifier)
            && text.chars().any(|ch| matches!(ch, '_' | '-' | '.' | '/' | ':') || ch.is_numeric()))
}

fn provider_free_quoted_literals(question: &str) -> Vec<String> {
    let mut literals = Vec::new();
    let mut open: Option<(char, usize)> = None;
    for (index, ch) in question.char_indices() {
        if let Some((closing, start)) = open {
            if ch == closing {
                let literal = question[start..index].trim();
                if !literal.is_empty() {
                    literals.push(
                        literal
                            .chars()
                            .take(PROVIDER_FREE_FALLBACK_QUOTED_LITERAL_MAX_CHARS)
                            .collect(),
                    );
                }
                open = None;
            }
        } else if let Some(closing) = provider_free_quote_closer(ch) {
            open = Some((closing, index + ch.len_utf8()));
        }
    }
    literals
}

fn provider_free_quote_closer(opening: char) -> Option<char> {
    match opening {
        '`' | '"' => Some(opening),
        '«' => Some('»'),
        '“' => Some('”'),
        '„' => Some('“'),
        _ => None,
    }
}

fn provider_free_structural_question_tokens(question: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in question.chars() {
        if ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':') {
            current.push(ch);
        } else if !current.is_empty() {
            tokens.push(provider_free_take_token(&current));
            current.clear();
        }
    }
    if !current.is_empty() {
        tokens.push(provider_free_take_token(&current));
    }
    tokens
}

fn provider_free_take_token(value: &str) -> String {
    value.chars().take(PROVIDER_FREE_FALLBACK_TOKEN_MAX_CHARS).collect()
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
6. `target_types` are finite semantic enum values. Select only values advertised by the supplied \
JSON schema. When no enum value fits, leave `target_types` empty and represent the subject in \
`target_entities`; never invent, translate, or alias a target type.\n\
7. `source_slice` is null for ordinary summaries, comparisons, procedures, and needle lookups. \
Set it only when the user asks for a positional slice of a sequential source: earliest units \
(`head`), latest units (`tail`), or a bounded representation of the whole sequence (`all`). \
Populate `count` only when the user asks for a concrete number of units. Set \
`source_slice.filter` to `release_marker` only when the slice asks for latest version/release \
records; otherwise set it to `none`.\n\
For ordered change-summary requests over version or release records, include the `release` or \
`version` target type and use a `tail` source slice when the user asks for the latest records.\n\
8. Every source-bearing text field — `target_entities[*].label`, `literal_constraints[*].text`, \
`temporal_constraints[*].surface`, `document_focus.hint`, `conversation_refs[*].surface`, and each \
non-null `comparison` operand — must be a non-empty, surrounding-whitespace-free verbatim substring \
of the current question or prior turns. Preserve the exact writing system, spelling, case, \
punctuation, and internal whitespace visible to the user; do not translate, transliterate, \
normalize look-alike glyphs, repair spelling, or \
substitute visually similar characters. Omit a source-bearing field when no exact substring supports \
it. When no prior conversation block is supplied, do not emit a history-dependent `follow_up` act, \
conversation reference, or `anaphora_unresolved` clarification.\n\
When a scoped follow-up includes a `literal anchors:` line, treat those values as prior-turn \
referents for retrieval and disambiguation. They may populate `literal_constraints` when the \
current question points back to them, but they are not source evidence by themselves.\n\
9. `confidence` ∈ [0.0, 1.0]. Use < 0.6 only when you genuinely cannot pin the question.\n\
10. `language` must use one of the schema enum values; prefer `auto` when the signal is mixed or \
unclear.\n\
11. `retrieval_query` is a self-contained search string a fresh retriever could run with no access \
to the prior turns. It is a SEARCH SURFACE, not a canonical label: keep every target entity, \
document hint, and literal exactly as specified above. You may append only short generic \
action/facet words that are already represented by the typed IR fields or visible in the user \
turns. Do not translate, transliterate, normalize the writing system, infer domain vocabulary, or \
invent a new subject, product, provider, version, endpoint, path, or answer value. When the \
question depends on prior turns — a bare selection, a \
pronoun, an omitted subject — fold the subject and scope recovered FROM THE PRIOR TURNS into one \
standalone phrasing while preserving the exact spelling of every carried-over subject or literal. \
Mechanically: if the prior turns established a subject token S and the current turn supplies only a \
refinement token R, `retrieval_query` is a standalone phrasing containing S and R verbatim, plus \
optional source-preserving action/facet terms; if the current turn already carries its own subject \
token, `retrieval_query` starts from the current question and may add only source-preserving \
action/facet terms.\n\
12. `needs_clarification` is a last-resort typed signal. Set it only when a missing user choice makes \
a grounded answer impossible. Do not use it merely because several evidence-backed variants may \
exist. A broad `configure_how`, `describe`, or `enumerate` request remains answerable: retrieve the \
available evidence and explain the evidence-derived variants in the answer. Request clarification \
only when the user must choose one mutually exclusive interpretation before any useful grounded \
answer can be produced.\n\
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
    use crate::domains::query_ir::{
        ComparisonSpec, ConversationRefKind, DocumentHint, EntityMention, EntityRole,
        QueryLanguage, SourceSliceDirection, TemporalConstraint, UnresolvedRef,
    };
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
            account_id: Uuid::now_v7(),
            api_key: Some("test-key".to_string()),
            model_catalog_id: Uuid::now_v7(),
            model_name: "gpt-5.4-nano".to_string(),
            effective_embedding_dimensions: None,
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
    async fn preserves_valid_provider_ir_without_post_llm_semantic_rewrite() {
        let ir_json = json!({
            "act": "describe",
            "scope": "single_document",
            "language": "en",
            "target_types": ["procedure"],
            "target_entities": [{"label": "Subject Alpha", "role": "subject"}],
            "literal_constraints": [],
            "temporal_constraints": [],
            "comparison": null,
            "document_focus": {"hint": "Guide A"},
            "conversation_refs": [],
            "needs_clarification": null,
            "source_slice": null,
            "retrieval_query": "Subject Alpha setup",
            "confidence": 0.25
        })
        .to_string();
        let expected: QueryIR = serde_json::from_str(&ir_json).expect("fixture QueryIR");
        let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));

        let outcome = QueryCompilerService
            .compile_with_gateway(&gateway, &sample_binding(), "Use Guide A for Subject Alpha", &[])
            .await
            .expect("valid provider IR");

        assert_eq!(outcome.ir, expected);
    }

    #[tokio::test]
    async fn rejects_stateless_conversation_dependent_ir_instead_of_rewriting_it() {
        let ir_json = json!({
            "act": "follow_up",
            "scope": "single_document",
            "language": "en",
            "target_types": [],
            "target_entities": [],
            "literal_constraints": [],
            "temporal_constraints": [],
            "comparison": null,
            "document_focus": null,
            "conversation_refs": [{"surface": "that", "kind": "deictic"}],
            "needs_clarification": "anaphora_unresolved",
            "source_slice": null,
            "retrieval_query": "that",
            "confidence": 0.35
        })
        .to_string();
        let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));

        let error = QueryCompilerService
            .compile_with_gateway(&gateway, &sample_binding(), "that", &[])
            .await
            .expect_err("stateless conversation dependency must fail closed");

        assert!(matches!(error, ApiError::ProviderFailure(_)));
    }

    #[tokio::test]
    async fn rejects_ungrounded_provider_entity_instead_of_fuzzy_repairing_it() {
        let ir_json = json!({
            "act": "describe",
            "scope": "single_document",
            "language": "en",
            "target_types": ["artifact"],
            "target_entities": [{"label": "Subject Alpah", "role": "subject"}],
            "literal_constraints": [],
            "temporal_constraints": [],
            "comparison": null,
            "document_focus": null,
            "conversation_refs": [],
            "needs_clarification": null,
            "source_slice": null,
            "retrieval_query": "Subject Alpha",
            "confidence": 0.8
        })
        .to_string();
        let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));

        let error = QueryCompilerService
            .compile_with_gateway(&gateway, &sample_binding(), "Describe Subject Alpha", &[])
            .await
            .expect_err("ungrounded entity must fail closed");

        assert!(matches!(error, ApiError::ProviderFailure(_)));
    }

    #[tokio::test]
    async fn rejects_structurally_invalid_provider_ir_in_release_path() {
        let ir_json = json!({
            "act": "compare",
            "scope": "multi_document",
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
            "retrieval_query": "compare",
            "confidence": 0.8
        })
        .to_string();
        let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));

        let error = QueryCompilerService
            .compile_with_gateway(&gateway, &sample_binding(), "compare", &[])
            .await
            .expect_err("invalid typed IR must fail in every build profile");

        assert!(matches!(error, ApiError::ProviderFailure(_)));
    }

    #[test]
    fn request_validation_rejects_follow_up_retrieval_query_that_drops_prior_target() {
        let history = vec![CompileHistoryTurn {
            role: "user".to_string(),
            content: "Configure Subject Alpha with Variant A or Variant B.".to_string(),
        }];
        let mut ir = canonical_ir();
        ir.act = QueryAct::ConfigureHow;
        ir.target_entities = vec![
            EntityMention { label: "Subject Alpha".to_string(), role: EntityRole::Subject },
            EntityMention { label: "Variant B".to_string(), role: EntityRole::Object },
        ];
        ir.retrieval_query = Some("Variant B".to_string());

        assert_eq!(
            validate_compiled_ir_for_request("Variant B", &history, &ir),
            Err(CompileRequestValidationError::IncompleteRetrievalQuery)
        );

        ir.retrieval_query = Some("Subject Alpha".to_string());
        assert_eq!(
            validate_compiled_ir_for_request("Variant B", &history, &ir),
            Err(CompileRequestValidationError::IncompleteRetrievalQuery)
        );

        ir.retrieval_query = Some("Subject Alpha Variant B".to_string());
        assert_eq!(validate_compiled_ir_for_request("Variant B", &history, &ir), Ok(()));
    }

    #[test]
    fn request_validation_accepts_exact_surfaces_from_question_and_history() {
        let history = vec![CompileHistoryTurn {
            role: "assistant".to_string(),
            content: "EntityH rightH refH".to_string(),
        }];
        let mut ir = canonical_ir();
        ir.target_entities = vec![
            EntityMention { label: "EntityQ".to_string(), role: EntityRole::Subject },
            EntityMention { label: "EntityH".to_string(), role: EntityRole::Object },
        ];
        ir.literal_constraints =
            vec![LiteralSpan { text: "LIT_Q".to_string(), kind: LiteralKind::Identifier }];
        ir.temporal_constraints = vec![TemporalConstraint {
            surface: "2026-03".to_string(),
            start: Some("2026-03-01T00:00:00Z".to_string()),
            end: Some("2026-04-01T00:00:00Z".to_string()),
        }];
        ir.document_focus = Some(DocumentHint { hint: "GuideQ".to_string() });
        ir.conversation_refs =
            vec![UnresolvedRef { surface: "refH".to_string(), kind: ConversationRefKind::Deictic }];
        ir.comparison = Some(ComparisonSpec {
            a: Some("leftQ".to_string()),
            b: Some("rightH".to_string()),
            dimension: "fixture".to_string(),
        });
        ir.retrieval_query = Some("EntityQ EntityH LIT_Q 2026-03 GuideQ leftQ rightH".to_string());

        assert_eq!(
            validate_compiled_ir_for_request("EntityQ LIT_Q 2026-03 GuideQ leftQ", &history, &ir,),
            Ok(())
        );
    }

    #[test]
    fn request_validation_rejects_every_ungrounded_source_field() {
        let history = vec![CompileHistoryTurn {
            role: "user".to_string(),
            content: "prior source".to_string(),
        }];

        let mut entity = canonical_ir();
        entity.target_entities =
            vec![EntityMention { label: "missing entity".to_string(), role: EntityRole::Subject }];

        let mut literal = canonical_ir();
        literal.literal_constraints = vec![LiteralSpan {
            text: "missing_literal".to_string(),
            kind: LiteralKind::Identifier,
        }];

        let mut temporal = canonical_ir();
        temporal.temporal_constraints = vec![TemporalConstraint {
            surface: "missing temporal".to_string(),
            start: None,
            end: None,
        }];

        let mut document = canonical_ir();
        document.document_focus = Some(DocumentHint { hint: "missing document".to_string() });

        let mut reference = canonical_ir();
        reference.conversation_refs = vec![UnresolvedRef {
            surface: "missing reference".to_string(),
            kind: ConversationRefKind::Deictic,
        }];

        let mut comparison = canonical_ir();
        comparison.comparison = Some(ComparisonSpec {
            a: Some("missing operand".to_string()),
            b: None,
            dimension: "fixture".to_string(),
        });

        for (ir, expected) in [
            (entity, CompileRequestValidationError::UngroundedTargetEntity),
            (literal, CompileRequestValidationError::UngroundedLiteral),
            (temporal, CompileRequestValidationError::UngroundedTemporalSurface),
            (document, CompileRequestValidationError::UngroundedDocumentFocus),
            (reference, CompileRequestValidationError::UngroundedConversationReference),
            (comparison, CompileRequestValidationError::UngroundedComparisonOperand),
        ] {
            assert_eq!(
                validate_compiled_ir_for_request("current source", &history, &ir),
                Err(expected)
            );
        }
    }

    #[tokio::test]
    async fn rejects_unknown_provider_target_kind_without_reclassification() {
        let ir_json = json!({
            "act": "describe",
            "scope": "multi_document",
            "language": "auto",
            "target_types": ["fixture_kind"],
            "target_entities": [],
            "literal_constraints": [
                {"text": "urn:fixture:item-17", "kind": "identifier"}
            ],
            "temporal_constraints": [],
            "comparison": null,
            "document_focus": null,
            "conversation_refs": [],
            "needs_clarification": null,
            "source_slice": null,
            "confidence": 0.82
        })
        .to_string();
        let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
        let service = QueryCompilerService;
        let binding = sample_binding();
        let question = "∆ ⟦urn:fixture:item-17⟧";

        let error = service
            .compile_with_gateway(&gateway, &binding, question, &[])
            .await
            .expect_err("unknown target kind must fail the compile boundary");

        assert!(matches!(error, ApiError::ProviderFailure(_)));
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
    async fn preserves_single_document_focus_when_user_quotes_a_technical_literal() {
        // The typed provider output is accepted unchanged because every
        // source-bearing field is present verbatim in the request.
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
            .compile_with_gateway(&gateway, &binding, "how do I configure S using /v2/pay", &[])
            .await
            .expect("compile ok");

        assert_eq!(outcome.ir.scope, QueryScope::SingleDocument);
        assert!(outcome.ir.document_focus.is_some());
    }

    #[tokio::test]
    async fn preserves_single_document_focus_when_focus_hint_is_a_filename() {
        // Exact source grounding validates the filename without inferring
        // whether the filename should or should not select one document.
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
        // Closed target kinds are trusted from the typed compiler output;
        // source-bearing labels and focus hints still require exact grounding.
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
        // Validation is profile- and confidence-independent: valid typed IR is
        // never rewritten after the provider returns it.
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
    async fn does_not_infer_release_tail_slice_from_ontology_words() {
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
        assert!(outcome.ir.source_slice.is_none());
    }

    #[tokio::test]
    async fn preserves_compiler_emitted_twenty_item_release_slice() {
        let ir_json = json!({
            "act": "enumerate",
            "scope": "single_document",
            "language": "en",
            "target_types": ["release", "version"],
            "target_entities": [],
            "literal_constraints": [{"text": "20", "kind": "numeric_code"}],
            "temporal_constraints": [],
            "comparison": null,
            "document_focus": null,
            "conversation_refs": [],
            "needs_clarification": null,
            "source_slice": {"direction": "tail", "count": 20, "filter": "release_marker"},
            "confidence": 0.95
        })
        .to_string();
        let gateway = StubGateway::new(Ok(chat_response_with(&ir_json)));
        let service = QueryCompilerService;
        let binding = sample_binding();

        let outcome = service
            .compile_with_gateway(&gateway, &binding, "list the latest 20 release records", &[])
            .await
            .expect("compile ok");

        assert_eq!(outcome.ir.source_slice.as_ref().and_then(|slice| slice.count), Some(20));
        assert_eq!(
            crate::services::query::latest_versions::requested_latest_version_count(&outcome.ir),
            20
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
    async fn preserves_focused_typed_latest_release_slice() {
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
            "source_slice": {"direction": "tail", "count": 1, "filter": "release_marker"},
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
            "retrieval_query": "TargetName how",
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

    #[test]
    fn provider_free_fallback_is_semantically_neutral() {
        let question = "∆ qlorm vexu 5?";
        let ir = provider_free_fallback_query_ir(question);

        assert_eq!(ir.act, QueryAct::Describe);
        assert_eq!(ir.scope, QueryScope::MultiDocument);
        assert_eq!(ir.language, QueryLanguage::Auto);
        assert!(ir.target_types.is_empty());
        assert!(ir.target_entities.is_empty());
        assert!(ir.source_slice.is_none());
        assert_eq!(ir.effective_retrieval_query("unused"), question);
        assert!(ir.needs_clarification.is_none());
    }

    #[test]
    fn provider_free_fallback_preserves_only_structural_evidence() {
        let question = "∆ `urn:fixture:item-17` /spec/v2.3.4 NODE_9";
        let ir = provider_free_fallback_query_ir(question);

        assert_eq!(ir.act, QueryAct::Describe);
        assert_eq!(ir.language, QueryLanguage::Auto);
        assert!(ir.target_types.is_empty());
        assert!(ir.target_entities.is_empty());
        assert!(ir.source_slice.is_none());
        for expected in ["urn:fixture:item-17", "/spec/v2.3.4", "NODE_9"] {
            assert!(
                ir.literal_constraints.iter().any(|literal| literal.text == expected),
                "missing structural literal {expected:?} in {:?}",
                ir.literal_constraints
            );
        }
    }

    #[test]
    fn provider_free_fallback_never_infers_entities_from_surface_shape() {
        for question in [
            "∆ qlorm Nimbus",
            "NODE_9 status",
            "CamelCaseSubject details",
            "service-name rollout",
            "API:v2 overview",
        ] {
            let ir = provider_free_fallback_query_ir(question);
            assert!(
                ir.target_entities.is_empty(),
                "provider-free fallback must not classify entity semantics from {question:?}"
            );
        }
    }

    #[test]
    fn compiler_production_path_has_no_lexical_fallback_classifiers() {
        let production_source = include_str!("compiler.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("compiler has a production section");

        for forbidden_symbol in [
            "provider_free_fallback_words",
            "provider_free_fallback_source_slice",
            "provider_free_fallback_requests_procedure",
            "provider_free_fallback_requests_remediation",
            "provider_free_fallback_requests_exact_value",
            "normalize_troubleshooting_remediation_request",
            "provider_free_fallback_entity_mentions",
            "provider_free_token_has_entity_signal",
            "provider_free_token_is_plain_titlecase",
        ] {
            assert!(
                !production_source.contains(forbidden_symbol),
                "lexical semantic classifier remains in production: {forbidden_symbol}"
            );
        }
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

        let prompt = gateway.last_request.lock().unwrap().clone().unwrap().prompt.clone();
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
    fn compiler_prompt_declares_exact_source_and_stateless_contract() {
        assert!(QUERY_COMPILER_SYSTEM_PROMPT.contains("Every source-bearing text field"));
        assert!(
            QUERY_COMPILER_SYSTEM_PROMPT.contains("surrounding-whitespace-free verbatim substring")
        );
        assert!(QUERY_COMPILER_SYSTEM_PROMPT.contains("When no prior conversation block"));
        assert!(QUERY_COMPILER_SYSTEM_PROMPT.contains("anaphora_unresolved"));
    }

    #[test]
    fn compiler_prompt_keeps_configuration_artifact_requests_as_configure_how() {
        assert!(QUERY_COMPILER_SYSTEM_PROMPT.contains("artifact-shaped output"));
        assert!(QUERY_COMPILER_SYSTEM_PROMPT.contains("still `configure_how`"));
        assert!(QUERY_COMPILER_SYSTEM_PROMPT.contains("do not downgrade them to `describe`"));
        assert!(QUERY_COMPILER_SYSTEM_PROMPT.contains("`configuration_file` and `config_key`"));
    }

    #[test]
    fn compiler_prompt_reserves_clarification_for_a_blocking_missing_choice() {
        assert!(
            QUERY_COMPILER_SYSTEM_PROMPT
                .contains("missing user choice makes a grounded answer impossible")
        );
        assert!(
            QUERY_COMPILER_SYSTEM_PROMPT
                .contains("broad `configure_how`, `describe`, or `enumerate` request")
        );
        assert!(
            QUERY_COMPILER_SYSTEM_PROMPT
                .contains("explain the evidence-derived variants in the answer")
        );
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
            target_types: vec![crate::domains::query_ir::QueryTargetKind::Procedure],
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
    async fn invalid_cached_ir_is_rejected_and_recompiled() {
        let library_id = Uuid::now_v7();
        let question = "current question";
        let history: Vec<CompileHistoryTurn> = Vec::new();
        let binding = sample_binding();
        let hash = hash_compile_request(question, &history, QUERY_IR_SCHEMA_VERSION, &binding);
        let mut invalid_ir = canonical_ir();
        invalid_ir.act = QueryAct::Compare;
        invalid_ir.comparison = None;
        let cache = StubCache::seeded(
            library_id,
            hash,
            CachedIrEntry {
                ir: invalid_ir,
                provider_kind: CACHE_HIT_REDIS_PROVIDER_KIND.to_string(),
                model_name: String::new(),
                usage_json: json!({"source": "redis"}),
            },
        );
        let live_ir = canonical_ir();
        let gateway = StubGateway::new(Ok(chat_response_with(
            &serde_json::to_string(&live_ir).expect("serialize live fixture"),
        )));

        let outcome = QueryCompilerService
            .compile_with_cache_and_gateway(
                &cache, &gateway, &binding, library_id, question, &history,
            )
            .await
            .expect("invalid cache entry must fall through to a live compile");

        assert_eq!(outcome.ir, live_ir);
        assert!(!outcome.served_from_cache);
        assert!(gateway.last_request.lock().unwrap().is_some());
        assert_eq!(cache.put_calls(), 1);
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
    async fn hash_compile_request_preserves_exact_question_history_and_binding_bytes() {
        let binding = sample_binding();
        let base = hash_compile_request("Hello World", &[], QUERY_IR_SCHEMA_VERSION, &binding);
        let case_variant =
            hash_compile_request("hello world", &[], QUERY_IR_SCHEMA_VERSION, &binding);
        let whitespace_variant =
            hash_compile_request(" Hello World ", &[], QUERY_IR_SCHEMA_VERSION, &binding);
        let formal_literal_variant =
            hash_compile_request("Hello World `A_B`", &[], QUERY_IR_SCHEMA_VERSION, &binding);
        assert_ne!(base, case_variant, "question case is source-significant");
        assert_ne!(base, whitespace_variant, "question whitespace is source-significant");
        assert_ne!(base, formal_literal_variant, "formal literals are source-significant");

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

        let history_case_variant = hash_compile_request(
            "Hello World",
            &[CompileHistoryTurn {
                role: "user".to_string(),
                content: "Prior context".to_string(),
            }],
            QUERY_IR_SCHEMA_VERSION,
            &binding,
        );
        let history_role_variant = hash_compile_request(
            "Hello World",
            &[CompileHistoryTurn {
                role: "assistant".to_string(),
                content: "prior context".to_string(),
            }],
            QUERY_IR_SCHEMA_VERSION,
            &binding,
        );
        assert_ne!(with_history, history_case_variant, "history content bytes are exact");
        assert_ne!(with_history, history_role_variant, "history role bytes are exact");

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
