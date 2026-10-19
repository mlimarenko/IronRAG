//! One canonical provider dispatch for a prepared semantic-rerank shortlist.
//!
//! The resolved binding's typed provider runtime decides the call: a
//! `typesafe_system_one` binding is evaluated through the TypeSafe SystemOne
//! `Score` primitive, and every generative binding goes through the existing
//! chat LLM path. There is no fallback that silently swaps one route for the
//! other — an unavailable decision binding degrades to the deterministic
//! lexical order instead of being treated as chat.
//!
//! Both routes return one provider call's outcome (raw scores plus the exact
//! usage payload the billing normaliser accepts); the caller accounts for the
//! call before it validates the scores, so a paid but malformed ranking is
//! still billed and then degrades the turn.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use uuid::Uuid;

use crate::{
    app::config::Settings,
    domains::provider_profiles::TYPESAFE_SYSTEM_ONE_RUNTIME_KIND,
    integrations::{
        llm::LlmGateway,
        provider_budget::ProviderBudgetRegistry,
        retry::ProviderCallError,
        typesafe::{
            SystemOneAnswer, SystemOneDecisionRequest, SystemOneDecisionResponse, SystemOneError,
            SystemOneQuestion, SystemOneTransportParams, SystemOneUsage,
        },
    },
    services::ai_catalog_service::ResolvedRuntimeBinding,
};

use super::{
    HARD_MAX_USER_MESSAGE_BYTES, PreparedSemanticRerankRequest, SemanticRerankFailure,
    build_semantic_chat_request,
    scoring::{
        ProviderSemanticScore, ValidatedSemanticRanking, parse_semantic_scores,
        validate_semantic_scores,
    },
};

// The rerank score rubric (ordered relevance levels of the `Score` question)
// is provider policy: it arrives with the resolved binding's provider profile,
// never as a table in query code. `binding.decision_score_criteria()` fails
// closed when the profile declares no valid rubric.

/// The provider retry/timeout numbers the chat gateway uses, mirrored for the
/// SystemOne client so both routes read one configuration source.
#[derive(Debug, Clone, Copy)]
pub(super) struct RerankProviderTransport {
    pub(super) http_timeout: Duration,
    pub(super) max_attempts: u32,
    pub(super) initial_backoff: Duration,
    pub(super) max_backoff: Duration,
}

impl RerankProviderTransport {
    pub(super) fn from_settings(settings: &Settings) -> Self {
        Self {
            http_timeout: Duration::from_secs(settings.llm_http_timeout_seconds.max(1)),
            max_attempts: settings.provider_retry_max_attempts,
            initial_backoff: Duration::from_millis(settings.provider_retry_initial_backoff_ms),
            max_backoff: Duration::from_millis(settings.provider_retry_max_backoff_ms),
        }
    }
}

/// One completed, billable provider call. The raw scores are validated by the
/// caller after usage accounting, so a malformed ranking still settles the
/// reservation.
#[derive(Debug)]
pub(super) enum RerankProviderCall {
    Chat {
        output_text: String,
        usage_json: serde_json::Value,
        expected_count: usize,
    },
    Decision {
        /// `None` when the paid response could not be mapped to the shared
        /// score contract: the call still settles its usage and only the
        /// ranking degrades, matching the chat leg's bill-then-validate order.
        scores: Option<Vec<ProviderSemanticScore>>,
        usage_json: serde_json::Value,
        expected_count: usize,
    },
}

impl RerankProviderCall {
    #[must_use]
    pub(super) fn usage_json(&self) -> &serde_json::Value {
        match self {
            Self::Chat { usage_json, .. } | Self::Decision { usage_json, .. } => usage_json,
        }
    }

    /// The typed runtime that served the call, for the existing tracing shape.
    #[must_use]
    pub(super) const fn provider_runtime(&self) -> &'static str {
        match self {
            Self::Chat { .. } => "chat",
            Self::Decision { .. } => TYPESAFE_SYSTEM_ONE_RUNTIME_KIND,
        }
    }

    /// Parses the call into one validated ranking. The chat envelope is decoded
    /// strictly and the decision scores were mapped from validated SystemOne
    /// answers; both end in [`validate_semantic_scores`], the one canonical
    /// ranking contract.
    pub(super) fn into_ranking(self) -> anyhow::Result<ValidatedSemanticRanking> {
        match self {
            Self::Chat { output_text, expected_count, .. } => {
                parse_semantic_scores(&output_text, expected_count)
            }
            Self::Decision { scores, expected_count, .. } => {
                let scores = scores.ok_or_else(|| {
                    anyhow::anyhow!("semantic rerank decision response mapping failed")
                })?;
                validate_semantic_scores(scores, expected_count)
            }
        }
    }
}

/// Everything one provider dispatch needs, resolved by the caller.
///
/// # Errors
/// Returns [`SemanticRerankFailure`] mapped from the transport/timeout/HTTP
/// failure. The caller decides how the durable reservation is settled.
pub(super) struct RerankProviderCallRequest<'a> {
    pub(super) llm_gateway: &'a Arc<dyn LlmGateway>,
    pub(super) provider_budget: &'a Arc<ProviderBudgetRegistry>,
    pub(super) transport: RerankProviderTransport,
    pub(super) binding: &'a ResolvedRuntimeBinding,
    pub(super) prepared: &'a PreparedSemanticRerankRequest,
    pub(super) provider_call_id: Uuid,
    pub(super) provider_timeout: Duration,
}

/// Dispatches one provider call for the resolved binding.
pub(super) async fn call_rerank_provider(
    request: RerankProviderCallRequest<'_>,
) -> Result<RerankProviderCall, SemanticRerankFailure> {
    let RerankProviderCallRequest {
        llm_gateway,
        provider_budget,
        transport,
        binding,
        prepared,
        provider_call_id,
        provider_timeout,
    } = request;
    if binding.provider_runtime_kind() == Some(TYPESAFE_SYSTEM_ONE_RUNTIME_KIND) {
        call_decision_provider(binding, prepared, provider_budget, transport, provider_timeout)
            .await
    } else {
        call_chat_provider(llm_gateway, binding, prepared, provider_call_id, provider_timeout).await
    }
}

async fn call_chat_provider(
    llm_gateway: &Arc<dyn LlmGateway>,
    binding: &ResolvedRuntimeBinding,
    prepared: &PreparedSemanticRerankRequest,
    provider_call_id: Uuid,
    provider_timeout: Duration,
) -> Result<RerankProviderCall, SemanticRerankFailure> {
    let request = build_semantic_chat_request(binding, prepared);
    let response = match tokio::time::timeout(
        provider_timeout,
        crate::observability::with_provider_call_id(
            provider_call_id,
            llm_gateway.generate(request),
        ),
    )
    .await
    {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            tracing::warn!(
                provider = %binding.provider_kind,
                ?error,
                "semantic rerank chat provider failed"
            );
            return Err(SemanticRerankFailure::ProviderFailure);
        }
        Err(_) => return Err(SemanticRerankFailure::TimedOut),
    };
    Ok(RerankProviderCall::Chat {
        output_text: response.output_text,
        usage_json: response.usage_json,
        expected_count: prepared.prepared_candidate_count(),
    })
}

async fn call_decision_provider(
    binding: &ResolvedRuntimeBinding,
    prepared: &PreparedSemanticRerankRequest,
    provider_budget: &Arc<ProviderBudgetRegistry>,
    transport: RerankProviderTransport,
    provider_timeout: Duration,
) -> Result<RerankProviderCall, SemanticRerankFailure> {
    // A decision binding is never treated as chat and never silently swapped:
    // a missing credential or endpoint fails closed to the lexical fallback.
    let Some(levels) = binding.decision_score_criteria() else {
        tracing::warn!(
            provider = %binding.provider_kind,
            "semantic rerank decision binding declares no score rubric; failing closed"
        );
        return Err(SemanticRerankFailure::ProviderFailure);
    };
    let client = binding
        .systemone_client(
            SystemOneTransportParams {
                request_timeout: transport.http_timeout,
                max_attempts: transport.max_attempts,
                initial_backoff: transport.initial_backoff,
                max_backoff: transport.max_backoff,
            },
            provider_budget,
            provider_timeout,
        )
        .map_err(|_| SemanticRerankFailure::ProviderFailure)?;
    let Some((request, submitted_count)) =
        build_systemone_decision_request(binding, prepared, &levels)
    else {
        return Err(SemanticRerankFailure::InvalidResponse);
    };

    let response = match tokio::time::timeout(provider_timeout, client.evaluate(request)).await {
        Ok(Ok(response)) => response,
        // A decodable 200 that fails decision validation was still paid for:
        // its usage settles like any completed call and only the ranking
        // degrades, matching the chat leg's bill-then-validate order.
        Ok(Err(SystemOneError::Response { usage: Some(usage), .. })) => {
            return Ok(RerankProviderCall::Decision {
                scores: None,
                usage_json: systemone_usage_json(usage),
                expected_count: submitted_count,
            });
        }
        Ok(Err(error)) => {
            tracing::warn!(
                provider = %binding.provider_kind,
                ?error,
                "semantic rerank decision provider failed"
            );
            return Err(map_systemone_error(error));
        }
        Err(_) => return Err(SemanticRerankFailure::TimedOut),
    };
    let usage_json = systemone_usage_json(response.usage);
    // The client validated the response shape; a mapping failure below (a
    // score outside the declared rubric) still settles this billable call and
    // degrades the ranking instead of dropping the reservation.
    let scores = semantic_scores_from_systemone(&response, submitted_count, levels.len()).ok();
    Ok(RerankProviderCall::Decision { scores, usage_json, expected_count: submitted_count })
}

/// Usage payload of one SystemOne response in the exact snake_case shape the
/// billing normaliser reads.
fn systemone_usage_json(usage: SystemOneUsage) -> serde_json::Value {
    serde_json::json!({
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
    })
}

/// Maps a typed SystemOne failure onto the rerank failure vocabulary.
fn map_systemone_error(error: SystemOneError) -> SemanticRerankFailure {
    match error {
        SystemOneError::Provider { source } => match source {
            ProviderCallError::Timeout { .. } => SemanticRerankFailure::TimedOut,
            _ => SemanticRerankFailure::ProviderFailure,
        },
        SystemOneError::Response { .. } | SystemOneError::Request(_) => {
            SemanticRerankFailure::InvalidResponse
        }
        SystemOneError::Config(_) => SemanticRerankFailure::ProviderFailure,
    }
}

fn systemone_question_id(index: usize) -> String {
    format!("candidate-{index}")
}

fn systemone_score_question(index: usize, levels: &[String]) -> SystemOneQuestion {
    SystemOneQuestion::Score {
        instructions: format!(
            "Score candidate index {index} from the state by its relevance to the query. Use only \
             the supplied state and return only the requested score."
        ),
        criteria: levels.to_vec(),
    }
}

fn systemone_request(
    binding: &ResolvedRuntimeBinding,
    prepared: &PreparedSemanticRerankRequest,
    levels: &[String],
) -> SystemOneDecisionRequest {
    let mut questions = BTreeMap::new();
    for candidate in &prepared.provider_candidates {
        questions.insert(
            systemone_question_id(candidate.index),
            systemone_score_question(candidate.index, levels),
        );
    }
    SystemOneDecisionRequest {
        state: prepared.provider_payload_json().to_string(),
        model: binding.model_name.clone(),
        questions,
    }
}

/// Builds the SystemOne request, dropping candidate tail entries until the
/// serialized request fits [`HARD_MAX_USER_MESSAGE_BYTES`].
///
/// The same hard byte cap the chat user message obeys bounds the decision
/// request too. Dropping only the tail keeps opaque indices contiguous, exactly
/// as the chat preparation does. Returns `None` when even a single candidate
/// cannot be represented.
fn build_systemone_decision_request(
    binding: &ResolvedRuntimeBinding,
    prepared: &PreparedSemanticRerankRequest,
    levels: &[String],
) -> Option<(SystemOneDecisionRequest, usize)> {
    // The protocol caps one request at `MAX_QUESTIONS`; the rerank's own
    // candidate cap is lower today, but the builder keeps the two bounds
    // coherent instead of relying on that coincidence.
    let total =
        prepared.prepared_candidate_count().min(crate::integrations::typesafe::MAX_QUESTIONS);
    for count in (1..=total).rev() {
        let truncated = prepared.truncated_provider_candidates(count);
        let request = systemone_request(binding, &truncated, levels);
        let Ok(serialized) = serde_json::to_vec(&request) else {
            return None;
        };
        if serialized.len() <= HARD_MAX_USER_MESSAGE_BYTES {
            return Some((request, count));
        }
    }
    None
}

/// Maps the validated SystemOne score answers onto the shared score contract,
/// normalising each `Score` position in `0..=levels-1` to `[0, 1]`.
fn semantic_scores_from_systemone(
    response: &SystemOneDecisionResponse,
    expected_count: usize,
    level_count: usize,
) -> Result<Vec<ProviderSemanticScore>, SemanticRerankFailure> {
    if expected_count == 0 || response.answers.len() != expected_count {
        return Err(SemanticRerankFailure::InvalidResponse);
    }
    if level_count < 2 {
        return Err(SemanticRerankFailure::InvalidResponse);
    }
    let divisor = (level_count - 1) as f64;
    let mut scores = Vec::with_capacity(expected_count);
    for index in 0..expected_count {
        let Some(SystemOneAnswer::Score { score, .. }) =
            response.answers.get(&systemone_question_id(index))
        else {
            return Err(SemanticRerankFailure::InvalidResponse);
        };
        if !score.is_finite() {
            return Err(SemanticRerankFailure::InvalidResponse);
        }
        scores.push(ProviderSemanticScore { index, score: (score / divisor).clamp(0.0, 1.0) });
    }
    Ok(scores)
}

#[cfg(test)]
#[path = "provider_dispatch_tests.rs"]
mod tests;
