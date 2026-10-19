//! `QueryCompiler` — natural language → typed [`QueryIR`].
//!
//! This is the primary entry point for the whole query pipeline. Every
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
//!   `ApiError::ProviderFailure`. The authoritative answer boundary may recover
//!   with `provider_free_fallback_query_ir`, whose low-confidence IR keeps
//!   the full retrieval query and formal evidence visible in diagnostics
//!   without guessing semantic intent or language.
//! - Cache hits are allowed only after the active `QueryCompile` binding has
//!   resolved successfully. The cache key includes the resolved binding
//!   fingerprint, so model/provider/preset changes do not replay stale IR.
//! - Only successful live compiles and binding-validated cache hits produce
//!   `CompileQueryOutcome`.

mod fingerprint;
mod ir_cache;
mod provider_free;

pub use fingerprint::query_ir_runtime_fingerprint;
use fingerprint::*;
pub use ir_cache::*;
pub(crate) use provider_free::*;

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

/// Borrowed inputs for [`QueryCompilerService::compile_with_cache_and_gateway`],
/// the cache + gateway test seam that skips `AppState`.
#[derive(Debug, Clone, Copy)]
pub struct CompileInput<'a> {
    pub binding: &'a ResolvedRuntimeBinding,
    pub library_id: Uuid,
    pub question: &'a str,
    pub history: &'a [CompileHistoryTurn],
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

/// Stateless service — all dependencies come through `AppState`.
#[derive(Debug, Default, Clone, Copy)]
pub struct QueryCompilerService;

impl QueryCompilerService {
    /// Primary entry point. Lookup order is:
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
        let binding = resolve_compile_binding(state, command.library_id).await?;
        let cache =
            PersistenceQueryIrCache::new(&state.persistence.postgres, &state.persistence.redis);
        let question_hash = hash_compile_request(
            &command.question,
            &command.history,
            QUERY_IR_SCHEMA_VERSION,
            &binding,
        );

        if let Some(outcome) = load_cached_compile(&cache, &command, &question_hash).await {
            return Ok(outcome);
        }

        let outcome = self.compile_live(state, &binding, &command).await?;
        store_compile_cache(&cache, &command, &question_hash, &outcome).await;
        Ok(outcome)
    }

    /// Run the billed compile loop. Each attempt is its own reservation: a
    /// retried compile is a second paid call and must be accounted as one.
    async fn compile_live(
        &self,
        state: &AppState,
        binding: &ResolvedRuntimeBinding,
        command: &CompileQueryCommand,
    ) -> Result<CompileQueryOutcome, ApiError> {
        let mut attempt = 0;
        loop {
            match self.run_single_compile_attempt(state, binding, command).await {
                Ok(outcome) => return Ok(outcome),
                Err(error) => {
                    if !retry_compile_attempt(binding, attempt, &error) {
                        return Err(error);
                    }
                    attempt += 1;
                }
            }
        }
    }

    /// Reserve, call the provider, account the paid response, then
    /// parse/validate it. A transport-level provider failure is already
    /// retried by the gateway; retrying it here would double that.
    async fn run_single_compile_attempt(
        &self,
        state: &AppState,
        binding: &ResolvedRuntimeBinding,
        command: &CompileQueryCommand,
    ) -> Result<CompileQueryOutcome, ApiError> {
        let mut provider_call = QueryProviderCallReservation::reserve(
            state,
            command.execution_context,
            binding,
            AiBindingPurpose::QueryCompile,
            "query_compile",
        )
        .await?;

        let response = match crate::observability::with_provider_call_id(
            provider_call.provider_call_id(),
            self.request_compile_response(
                state.llm_gateway.as_ref(),
                binding,
                &command.question,
                &command.history,
            ),
        )
        .await
        {
            Ok(response) => response,
            Err(error) => {
                terminalize_failed_provider_call(&mut provider_call).await;
                return Err(error);
            }
        };
        // Account for the paid response before parsing or semantic
        // validation: a malformed/rejected QueryIR is still a real provider
        // call.
        provider_call.complete(&response.usage_json).await?;
        self.compile_response(binding, &command.question, &command.history, response)
    }

    /// Testable variant that takes an explicit cache handle and gateway.
    /// Mirrors the public `compile` path but skips `AppState` so unit
    /// tests can substitute an in-memory cache and a stub gateway.
    pub async fn compile_with_cache_and_gateway(
        &self,
        cache: &dyn QueryIrCache,
        gateway: &dyn LlmGateway,
        input: CompileInput<'_>,
    ) -> Result<CompileQueryOutcome, ApiError> {
        let CompileInput { binding, library_id, question, history } = input;
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
    /// Production callers use the query compiler path.
    pub async fn compile_with_gateway(
        &self,
        gateway: &dyn LlmGateway,
        binding: &ResolvedRuntimeBinding,
        question: &str,
        history: &[CompileHistoryTurn],
    ) -> Result<CompileQueryOutcome, ApiError> {
        let mut attempt = 0;
        loop {
            let response =
                self.request_compile_response(gateway, binding, question, history).await?;
            match self.compile_response(binding, question, history, response) {
                Ok(outcome) => return Ok(outcome),
                Err(error) => {
                    if !retry_compile_attempt(binding, attempt, &error) {
                        return Err(error);
                    }
                    attempt += 1;
                }
            }
        }
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
        let ir: QueryIR = serde_json::from_str(&response.output_text).map_err(|error| {
            log_compile_output_parse_failure(binding, &response.output_text, &error);
            compile_provider_failure(binding, "QueryCompile provider returned invalid QueryIR JSON")
        })?;
        if let Err(error) = validate_compiled_ir_for_request(question, history, &ir) {
            log_compile_validation_failure(binding, &error);
            return Err(compile_provider_failure(
                binding,
                "QueryCompile provider returned invalid QueryIR",
            ));
        }

        log_compiled_query_ir(&ir, &response, history);

        Ok(CompileQueryOutcome {
            ir,
            provider_kind: response.provider_kind,
            model_name: response.model_name,
            usage_json: response.usage_json,
            served_from_cache: false,
        })
    }
}

/// Resolve the active `QueryCompile` binding, failing loud when the operator
/// has not configured one for the library.
async fn resolve_compile_binding(
    state: &AppState,
    library_id: Uuid,
) -> Result<ResolvedRuntimeBinding, ApiError> {
    match state
        .application_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::QueryCompile)
        .await?
    {
        Some(binding) => Ok(binding),
        None => {
            log_missing_query_compile_binding(library_id);
            Err(ApiError::ProviderFailure(
                "QueryCompile binding is not configured for this library".to_string(),
            ))
        }
    }
}

fn log_missing_query_compile_binding(library_id: Uuid) {
    tracing::error!(
        library_id = %library_id,
        "query_compile binding is not configured"
    );
}

/// Return a validated cache hit, or `None` when the cache misses or the warm
/// entry no longer passes typed request validation (the caller then compiles
/// live).
async fn load_cached_compile(
    cache: &PersistenceQueryIrCache<'_>,
    command: &CompileQueryCommand,
    question_hash: &str,
) -> Option<CompileQueryOutcome> {
    let entry = cache.get(command.library_id, question_hash).await?;
    validated_cached_outcome(entry, &command.question, &command.history)
}

async fn store_compile_cache(
    cache: &PersistenceQueryIrCache<'_>,
    command: &CompileQueryCommand,
    question_hash: &str,
    outcome: &CompileQueryOutcome,
) {
    cache
        .put(
            command.library_id,
            question_hash,
            &CachedIrEntry {
                ir: outcome.ir.clone(),
                provider_kind: outcome.provider_kind.clone(),
                model_name: outcome.model_name.clone(),
                usage_json: outcome.usage_json.clone(),
            },
        )
        .await;
}

/// Best-effort terminalization of a reservation whose provider call failed.
/// A failure to terminalize does not replace the original provider error.
async fn terminalize_failed_provider_call(provider_call: &mut QueryProviderCallReservation) {
    if let Err(billing_error) = provider_call.fail().await {
        log_provider_call_fail_failure(provider_call.provider_call_id(), &billing_error);
    }
}

fn log_provider_call_fail_failure(provider_call_id: Uuid, billing_error: &ApiError) {
    tracing::error!(
        provider_call_id = %provider_call_id,
        billing_error = %billing_error,
        "failed to terminalize query compiler provider-call reservation"
    );
}

fn compile_provider_failure(binding: &ResolvedRuntimeBinding, prefix: &str) -> ApiError {
    ApiError::ProviderFailure(format!(
        "{prefix} for provider `{}` model `{}`",
        binding.provider_kind, binding.model_name
    ))
}

fn log_compile_output_parse_failure(
    binding: &ResolvedRuntimeBinding,
    output_text: &str,
    error: &serde_json::Error,
) {
    tracing::error!(
        provider = %binding.provider_kind,
        model = %binding.model_name,
        output_preview = %preview(output_text, 200),
        ?error,
        "query compile output is not valid QueryIR JSON"
    );
}

fn log_compile_validation_failure(
    binding: &ResolvedRuntimeBinding,
    error: &CompileRequestValidationError,
) {
    tracing::error!(
        provider = %binding.provider_kind,
        model = %binding.model_name,
        ?error,
        "query compile output failed typed request validation"
    );
}

fn log_compiled_query_ir(ir: &QueryIR, response: &ChatResponse, history: &[CompileHistoryTurn]) {
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
}

/// A QueryIR that fails to parse or fails typed validation is a stochastic
/// formatting slip by the model, not a statement about the question: the same
/// prompt usually compiles on the next attempt. Without a retry a single slip
/// terminates the turn, and because every provider transient escalates to an
/// HTTP 500 the caller sees a server error for what is a recoverable event.
/// One extra attempt is deliberate — a model that fails twice on the same
/// prompt is failing systematically, and burning further paid calls on it
/// would trade a fast error for a slow one.
const COMPILE_IR_RETRY_LIMIT: u32 = 1;

/// Whether a failed compile attempt should be retried, and the one line of
/// evidence that says why. Kept separate from the two call sites so the policy
/// cannot drift between the billed production path and the gateway path.
fn retry_compile_attempt(binding: &ResolvedRuntimeBinding, attempt: u32, error: &ApiError) -> bool {
    let retrying = attempt < COMPILE_IR_RETRY_LIMIT;
    tracing::warn!(
        target: "ironrag::query_compile",
        provider = %binding.provider_kind,
        model = %binding.model_name,
        attempt = attempt + 1,
        max_attempts = COMPILE_IR_RETRY_LIMIT + 1,
        retrying,
        %error,
        "query compile produced an unusable QueryIR"
    );
    retrying
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

#[cfg(test)]
mod tests;
