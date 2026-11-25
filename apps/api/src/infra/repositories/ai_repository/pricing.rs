use base64::Engine as _;
use image::ImageReader;
use rust_decimal::Decimal;
use sqlx::{Executor, FromRow, PgPool, Postgres};
use uuid::Uuid;

use crate::domains::ai::{ModelLongContextPricing, ModelPricing};
pub use crate::domains::provider_profiles::{
    AdmittedCaller, CALLER_BINDING_FORMAT_VERSION, CallerBinding, FinancialAdmission,
    FinancialAdmissionState, FinancialChatMediaAdmission, FinancialModelAllowance,
    FinancialRequestEnvelope,
};
use crate::shared::provider_http::DispatchCaller;

/// Must come from required deployment configuration, never from the provider request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredFinancialScope {
    pub provider_id: Uuid,
    pub scope_id: Uuid,
    pub policy_fingerprint: String,
}

#[derive(Debug, thiserror::Error)]
pub enum FinancialAdmissionError {
    #[error("financial scope missing, corrupt, or mismatched")]
    InvalidScope,
    #[error("financial scope is not active or has expired")]
    NotActive,
    /// The attempt's authenticated caller is not admitted by the grant's
    /// current caller binding. The variant carries nothing about the binding.
    #[error("financial scope does not admit this caller")]
    Caller,
    #[error("financial allowance exhausted")]
    Exhausted,
    #[error("financial request is outside the admitted envelope")]
    Envelope,
    #[error("financial persistence failed or commit outcome is unknown")]
    Persistence(#[from] sqlx::Error),
}

/// Proof of an acknowledged durable consumption; it is never refunded.
#[derive(Debug)]
pub struct FinancialPermit {
    pub scope_id: Uuid,
    pub global_consumed: u64,
}

fn financial_scope_matches(scope: &RequiredFinancialScope, state: &FinancialAdmission) -> bool {
    state.provider_id == scope.provider_id
        && state.scope_id == scope.scope_id
        && state.policy_fingerprint == scope.policy_fingerprint
        && state.validate().is_ok()
        && state
            .immutable_fingerprint()
            .is_ok_and(|fingerprint| fingerprint == scope.policy_fingerprint)
}

fn validate_financial_rows(
    rows: Vec<(Uuid, serde_json::Value)>,
    required: Option<&RequiredFinancialScope>,
) -> Result<(), FinancialAdmissionError> {
    if rows.len() > 1 {
        return Err(FinancialAdmissionError::InvalidScope);
    }
    for (provider_id, value) in rows {
        let scope = required.ok_or(FinancialAdmissionError::InvalidScope)?;
        let state: FinancialAdmission =
            serde_json::from_value(value).map_err(|_| FinancialAdmissionError::InvalidScope)?;
        if state.provider_id != provider_id
            || scope.provider_id != provider_id
            || state.validate().is_err()
            || !state
                .immutable_fingerprint()
                .is_ok_and(|fingerprint| fingerprint == state.policy_fingerprint)
            || (state.state != FinancialAdmissionState::Closed
                && !financial_scope_matches(scope, &state))
        {
            return Err(FinancialAdmissionError::InvalidScope);
        }
    }
    Ok(())
}

/// Refuse startup for invalid durable state or a mismatched non-closed scope.
pub async fn validate_financial_configuration(
    postgres: &PgPool,
    required: Option<&RequiredFinancialScope>,
) -> Result<(), FinancialAdmissionError> {
    let table: Option<String> =
        sqlx::query_scalar("select to_regclass('public.ai_provider_catalog')::text")
            .fetch_one(postgres)
            .await?;
    if table.is_none() {
        return Ok(());
    }
    let rows: Vec<(Uuid, serde_json::Value)> = sqlx::query_as(
        "select id, capability_flags_json -> 'financialAdmission' from ai_provider_catalog where capability_flags_json ? 'financialAdmission'",
    ).fetch_all(postgres).await?;
    validate_financial_rows(rows, required)
}

/// Message keys a chat request may carry. Media never travels as a message
/// field: it can only appear as a `content` part, which the closed reader below
/// either refuses or admits within the envelope's media policy.
const CHAT_MESSAGE_FIELDS: [&str; 6] =
    ["role", "content", "name", "tool_call_id", "tool_calls", "refusal"];

/// Closed reading of a chat `messages` value under one optional media policy.
///
/// The top-level `audio`/`modalities` guards cannot see a media part nested in
/// `messages[].content`, so the same predicate must read the nested layer.
/// Every message is an object with known fields, and its `content` is either a
/// string or a non-empty array of parts:
///
/// - with no media policy the request is text-only and every part must be the
///   exact `{"type":"text","text":<string>}` shape;
/// - with a policy an `image_url` part is read as one bounded inline image and
///   every other part type stays refused.
fn chat_messages_admit(
    messages: Option<&serde_json::Value>,
    media: Option<&FinancialChatMediaAdmission>,
) -> bool {
    let Some(messages) = messages.and_then(serde_json::Value::as_array) else {
        return false;
    };
    if messages.is_empty() {
        return false;
    }
    let mut images = 0u32;
    let mut image_bytes_total = 0u64;
    let mut image_pixels_total = 0u64;
    for message in messages {
        let Some(message) = message.as_object() else { return false };
        if message.keys().any(|field| !CHAT_MESSAGE_FIELDS.contains(&field.as_str()))
            || !message.get("role").is_some_and(serde_json::Value::is_string)
        {
            return false;
        }
        match message.get("content") {
            Some(serde_json::Value::String(_)) => {}
            Some(serde_json::Value::Array(parts)) if !parts.is_empty() => {
                for part in parts {
                    let Some(part) = part.as_object() else { return false };
                    match part.get("type").and_then(serde_json::Value::as_str) {
                        Some("text")
                            if part.len() == 2
                                && part.get("text").is_some_and(serde_json::Value::is_string) => {}
                        Some("image_url") => {
                            let Some(policy) = media else { return false };
                            if !chat_image_part_admits(
                                part,
                                policy,
                                &mut images,
                                &mut image_bytes_total,
                                &mut image_pixels_total,
                            ) {
                                return false;
                            }
                        }
                        _ => return false,
                    }
                }
            }
            _ => return false,
        }
    }
    true
}

/// Admit one bounded inline image and fold it into the running totals.
///
/// Count, decoded bytes, dimensions and pixels are independent ceilings: each
/// one is enforced on its own, and every accumulation uses checked arithmetic
/// so an overflowing request is refused instead of saturating.
fn chat_image_part_admits(
    part: &serde_json::Map<String, serde_json::Value>,
    policy: &FinancialChatMediaAdmission,
    images: &mut u32,
    image_bytes_total: &mut u64,
    image_pixels_total: &mut u64,
) -> bool {
    let Some(bytes) = inline_image_bytes(part) else { return false };
    let Some(next_images) = images.checked_add(1) else { return false };
    if next_images > policy.max_images {
        return false;
    }
    let bytes_len = bytes.len() as u64;
    if bytes_len > policy.max_image_bytes_each {
        return false;
    }
    let Some(next_bytes) = image_bytes_total.checked_add(bytes_len) else { return false };
    if next_bytes > policy.max_image_bytes_total {
        return false;
    }
    let Some((width, height)) = image_dimensions(&bytes) else { return false };
    if width > policy.max_image_dimension_px || height > policy.max_image_dimension_px {
        return false;
    }
    let Some(pixels) = width.checked_mul(height) else { return false };
    let Some(next_pixels) = image_pixels_total.checked_add(u64::from(pixels)) else {
        return false;
    };
    if next_pixels > policy.max_image_pixels_total {
        return false;
    }
    *images = next_images;
    *image_bytes_total = next_bytes;
    *image_pixels_total = next_pixels;
    true
}

/// The exact inline transport one image part may carry: a `data:` URL whose
/// MIME is PNG or JPEG, whose payload is standard base64, and whose decoded
/// bytes carry that same image signature. Everything else is refused.
fn inline_image_bytes(part: &serde_json::Map<String, serde_json::Value>) -> Option<Vec<u8>> {
    if part.len() != 2 {
        return None;
    }
    let image = part.get("image_url")?.as_object()?;
    if image.len() != 1 {
        return None;
    }
    let url = image.get("url")?.as_str()?;
    let (mime, payload) = url.strip_prefix("data:")?.split_once(";base64,")?;
    let format = match mime {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" => image::ImageFormat::Jpeg,
        _ => return None,
    };
    let bytes = base64::engine::general_purpose::STANDARD.decode(payload).ok()?;
    if image::guess_format(&bytes).ok()? != format {
        return None;
    }
    Some(bytes)
}

/// Header-only dimensions; no full pixel decode is performed during admission.
fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.into_dimensions().ok()
}

/// Qualify the actual built JSON body against one typed envelope.
///
/// Everything here is a validity or resource guard, evaluated on the exact
/// bytes that will be transmitted, before dispatch:
///
/// - the model named in the body must equal the allowance's model;
/// - the chat envelope requires an allowed explicit `service_tier`, `n == 1`,
///   an explicit bounded `max_completion_tokens`, no paid built-in tools, and
///   no media beyond the envelope's optional inline-image policy (absent policy
///   means text-only);
/// - byte ceilings bound memory and obviously unacceptable payloads.
///
/// None of this is a token-count proof or a billing bound: the financial unit
/// is the full documented maximum reserved per physical attempt before send and
/// it is never reduced by the measured body size.
pub(crate) fn envelope_admits_body(
    envelope: &FinancialRequestEnvelope,
    model: &str,
    body: &serde_json::Value,
) -> bool {
    let Some(object) = body.as_object() else {
        return false;
    };
    if object.get("model").and_then(serde_json::Value::as_str) != Some(model) {
        return false;
    }
    let serialized_bytes = serde_json::to_vec(body).map(|bytes| bytes.len()).unwrap_or(usize::MAX);
    match envelope {
        FinancialRequestEnvelope::Chat {
            allowed_service_tiers,
            max_input_bytes,
            max_output_tokens,
            max_input_tokens: _,
            media,
        } => {
            if serialized_bytes as u64 > *max_input_bytes
                || object.contains_key("tools")
                || object.contains_key("functions")
                || object.contains_key("audio")
                || object.contains_key("modalities")
                || !chat_messages_admit(object.get("messages"), media.as_ref())
            {
                return false;
            }
            let tier = object.get("service_tier").and_then(serde_json::Value::as_str);
            if !tier.is_some_and(|tier| allowed_service_tiers.iter().any(|allowed| allowed == tier))
            {
                return false;
            }
            if object.get("n").and_then(serde_json::Value::as_u64) != Some(1) {
                return false;
            }
            if !object
                .get("max_completion_tokens")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|limit| limit > 0 && limit <= *max_output_tokens)
            {
                return false;
            }
            true
        }
        FinancialRequestEnvelope::Embeddings {
            max_inputs,
            max_input_bytes_each,
            max_input_bytes_total,
            max_input_tokens: _,
        } => {
            if object.contains_key("service_tier")
                || object.get("stream").and_then(serde_json::Value::as_bool) == Some(true)
            {
                return false;
            }
            // Token-ID arrays are not text: byte reasoning does not apply to
            // them, so they are refused outright rather than measured.
            let inputs: Vec<&str> = match object.get("input") {
                Some(serde_json::Value::String(input)) => vec![input.as_str()],
                Some(serde_json::Value::Array(values)) => {
                    match values.iter().map(|value| value.as_str()).collect::<Option<Vec<_>>>() {
                        Some(inputs) => inputs,
                        None => return false,
                    }
                }
                _ => return false,
            };
            const DOCUMENTED_MAX_INPUTS: u64 = 2_048;
            let count_ceiling = (*max_inputs).min(DOCUMENTED_MAX_INPUTS);
            if inputs.is_empty() || inputs.len() as u64 > count_ceiling {
                return false;
            }
            // Decoded UTF-8 bytes per input, and their sum, are the financial
            // measure for this class; the serialized-body guard above only
            // bounds the resource use.
            let mut decoded_total: u64 = 0;
            for input in &inputs {
                let bytes = input.len() as u64;
                if bytes > *max_input_bytes_each {
                    return false;
                }
                decoded_total = decoded_total.saturating_add(bytes);
            }
            if decoded_total > *max_input_bytes_total {
                return false;
            }
            true
        }
    }
}

/// Test-fixture helper: evaluates and commits one counter-only attempt.
///
/// Production admissions never use this path. The canonical admission owner
/// (`billing_repository::admit_provider_send`) debits the counter and writes the
/// physical send row in one transaction, so no production caller can commit the
/// counter alone. This wrapper exists only for fixtures that exercise the
/// financial policy in isolation and is compiled only with `test-support`.
#[cfg(feature = "test-support")]
pub async fn consume_financial_attempt(
    postgres: &PgPool,
    scope: &RequiredFinancialScope,
    caller: &DispatchCaller,
    model: &str,
    endpoint: &str,
    body: &serde_json::Value,
) -> Result<FinancialPermit, FinancialAdmissionError> {
    let mut transaction = postgres.begin().await?;
    let permit =
        evaluate_financial_attempt(&mut transaction, scope, caller, model, endpoint, body).await?;
    transaction.commit().await?;
    Ok(permit)
}

/// Reserve one complete physical attempt inside a caller-owned transaction.
///
/// This is the single transaction primitive for the financial counter. The
/// provider row is locked for the whole read-validate-increment sequence, so
/// concurrent API and worker registries cannot exceed either the model or the
/// global allowance, and nothing is refunded on any later outcome.
///
/// The same lock orders the caller decision against a binding rotation: the
/// admitted caller list is read from the locked row on every attempt, so an
/// attempt either commits before the rotation update takes the lock or observes
/// the new list and is refused. A caller outside that list, and a grant with no
/// binding at all, consume nothing.
///
/// The caller must hold the transaction until its own durable work commits. The
/// canonical admission owner commits the counter debit together with the
/// `billing_provider_call` row, so the debit and the recorded send share one
/// atomic outcome and no production path commits the counter alone.
pub async fn evaluate_financial_attempt(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    scope: &RequiredFinancialScope,
    caller: &DispatchCaller,
    model: &str,
    endpoint: &str,
    body: &serde_json::Value,
) -> Result<FinancialPermit, FinancialAdmissionError> {
    let value: Option<serde_json::Value> = sqlx::query_scalar(
        "select capability_flags_json -> 'financialAdmission' from ai_provider_catalog where id = $1 for update",
    ).bind(scope.provider_id).fetch_optional(&mut **transaction).await?.flatten();
    let mut state: FinancialAdmission =
        serde_json::from_value(value.ok_or(FinancialAdmissionError::InvalidScope)?)
            .map_err(|_| FinancialAdmissionError::InvalidScope)?;
    if !financial_scope_matches(scope, &state) {
        return Err(FinancialAdmissionError::InvalidScope);
    }
    if state.state != FinancialAdmissionState::Active || state.cutoff <= chrono::Utc::now() {
        return Err(FinancialAdmissionError::NotActive);
    }
    // Before any counter moves: this attempt is only for a caller the grant
    // admits. Absence of a binding is a refusal, never a wildcard.
    if !state.caller_binding.as_ref().is_some_and(|binding| binding.admits(caller)) {
        return Err(FinancialAdmissionError::Caller);
    }
    if state.global_consumed >= state.global_limit {
        return Err(FinancialAdmissionError::Exhausted);
    }
    let allowance = state.models.get_mut(model).ok_or(FinancialAdmissionError::Envelope)?;
    if allowance.endpoint != endpoint || !envelope_admits_body(&allowance.envelope, model, body) {
        return Err(FinancialAdmissionError::Envelope);
    }
    if allowance.consumed >= allowance.limit {
        return Err(FinancialAdmissionError::Exhausted);
    }
    allowance.consumed =
        allowance.consumed.checked_add(1).ok_or(FinancialAdmissionError::Exhausted)?;
    state.global_consumed =
        state.global_consumed.checked_add(1).ok_or(FinancialAdmissionError::Exhausted)?;
    state.validate().map_err(|_| FinancialAdmissionError::InvalidScope)?;
    let value = serde_json::to_value(&state).map_err(|_| FinancialAdmissionError::InvalidScope)?;
    sqlx::query("update ai_provider_catalog set capability_flags_json = jsonb_set(capability_flags_json, '{financialAdmission}', $2::jsonb, false) where id = $1")
        .bind(scope.provider_id).bind(value).execute(&mut **transaction).await?;
    Ok(FinancialPermit { scope_id: state.scope_id, global_consumed: state.global_consumed })
}

/// Minimal typed catalog policy needed to interpret usage counters before
/// pricing them. Provider and model identities are deliberately excluded:
/// token accounting semantics belong to protocol policy, never identifiers.
#[derive(Debug, Clone, FromRow)]
pub struct ProviderBillingUsageProfileRow {
    pub usage_policy_json: Option<serde_json::Value>,
    pub service_tier_multipliers_json: Option<serde_json::Value>,
}

pub async fn get_provider_billing_usage_profile<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    provider_catalog_id: Uuid,
) -> Result<Option<ProviderBillingUsageProfileRow>, sqlx::Error> {
    sqlx::query_as::<_, ProviderBillingUsageProfileRow>(
        "select
            capability_flags_json -> 'usagePolicy' as usage_policy_json,
            capability_flags_json -> 'serviceTierMultipliers' as service_tier_multipliers_json
         from ai_provider_catalog
         where id = $1",
    )
    .bind(provider_catalog_id)
    .fetch_optional(executor)
    .await
}

#[derive(Debug, Clone, FromRow)]
struct ModelPricingRow {
    currency_code: String,
    input_price_per_1m: Decimal,
    cached_input_price_per_1m: Decimal,
    cache_write_input_price_per_1m: Decimal,
    output_price_per_1m: Decimal,
    long_context_threshold_tokens: Option<i32>,
    long_context_input_price_per_1m: Option<Decimal>,
    long_context_cached_input_price_per_1m: Option<Decimal>,
    long_context_output_price_per_1m: Option<Decimal>,
}

/// Vendor rates of one model row, typed for charge computation.
pub async fn get_model_pricing<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    model_catalog_id: Uuid,
) -> Result<Option<ModelPricing>, sqlx::Error> {
    let row = sqlx::query_as::<_, ModelPricingRow>(
        "select
            currency_code,
            input_price_per_1m,
            cached_input_price_per_1m,
            cache_write_input_price_per_1m,
            output_price_per_1m,
            long_context_threshold_tokens,
            long_context_input_price_per_1m,
            long_context_cached_input_price_per_1m,
            long_context_output_price_per_1m
         from ai_model_catalog
         where id = $1",
    )
    .bind(model_catalog_id)
    .fetch_optional(executor)
    .await?;
    Ok(row.map(|row| {
        let long_context = match (
            row.long_context_threshold_tokens,
            row.long_context_input_price_per_1m,
            row.long_context_cached_input_price_per_1m,
            row.long_context_output_price_per_1m,
        ) {
            (Some(threshold_tokens), Some(input), Some(cached_input), Some(output)) => {
                Some(ModelLongContextPricing {
                    threshold_tokens,
                    input_price_per_1m: input,
                    cached_input_price_per_1m: cached_input,
                    output_price_per_1m: output,
                })
            }
            _ => None,
        };
        ModelPricing {
            currency_code: row.currency_code,
            input_price_per_1m: row.input_price_per_1m,
            cached_input_price_per_1m: row.cached_input_price_per_1m,
            cache_write_input_price_per_1m: row.cache_write_input_price_per_1m,
            output_price_per_1m: row.output_price_per_1m,
            long_context,
        }
    }))
}

#[cfg(test)]
mod financial_configuration_tests {
    use super::*;
    use std::collections::BTreeMap;

    fn admission(state: FinancialAdmissionState) -> FinancialAdmission {
        let mut admission = FinancialAdmission {
            version: 1,
            provider_id: Uuid::new_v4(),
            scope_id: Uuid::new_v4(),
            policy_fingerprint: String::new(),
            cutoff: chrono::Utc::now(),
            state,
            grant_microusd: 100,
            global_limit: 2,
            global_consumed: 0,
            models: BTreeMap::from([(
                "model".to_owned(),
                FinancialModelAllowance {
                    endpoint: "embeddings".to_owned(),
                    envelope: FinancialRequestEnvelope::Embeddings {
                        max_inputs: 1,
                        max_input_bytes_each: 100,
                        max_input_bytes_total: 100,
                        max_input_tokens: 100,
                    },
                    attempt_microusd: 10,
                    limit: 2,
                    consumed: 0,
                },
            )]),
            history: Vec::new(),
            caller_binding: Some(CallerBinding {
                format_version: CALLER_BINDING_FORMAT_VERSION,
                generation: 1,
                callers: vec![AdmittedCaller::MutationOrigin {
                    mutation_id: Uuid::from_u128(3),
                    principal_id: Uuid::from_u128(4),
                }],
            }),
        };
        admission.policy_fingerprint = admission.immutable_fingerprint().expect("fingerprint");
        admission
    }

    fn required(admission: &FinancialAdmission) -> RequiredFinancialScope {
        RequiredFinancialScope {
            provider_id: admission.provider_id,
            scope_id: admission.scope_id,
            policy_fingerprint: admission.policy_fingerprint.clone(),
        }
    }

    fn rows(admission: &FinancialAdmission) -> Vec<(Uuid, serde_json::Value)> {
        vec![(admission.provider_id, serde_json::to_value(admission).expect("serialize"))]
    }

    #[test]
    fn accepts_closed_predecessor_and_matching_states() {
        let mut state = admission(FinancialAdmissionState::Closed);
        let matching = required(&state);
        assert!(validate_financial_rows(rows(&state), Some(&matching)).is_ok());
        let mut successor = matching.clone();
        successor.scope_id = Uuid::new_v4();
        successor.policy_fingerprint = "successor".to_owned();
        assert!(validate_financial_rows(rows(&state), Some(&successor)).is_ok());
        for lifecycle in [FinancialAdmissionState::Active, FinancialAdmissionState::Prepared] {
            state.state = lifecycle;
            assert!(validate_financial_rows(rows(&state), Some(&matching)).is_ok());
            assert!(validate_financial_rows(rows(&state), Some(&successor)).is_err());
        }
    }

    #[test]
    fn closed_predecessor_must_match_required_provider() {
        let state = admission(FinancialAdmissionState::Closed);
        let mut scope = required(&state);
        scope.provider_id = Uuid::new_v4();
        assert!(validate_financial_rows(rows(&state), Some(&scope)).is_err());
    }

    #[test]
    fn closed_predecessor_must_have_valid_identity_arithmetic_and_fingerprint() {
        let state = admission(FinancialAdmissionState::Closed);
        let scope = required(&state);
        let mut invalid = state.clone();
        invalid.provider_id = Uuid::new_v4();
        assert!(validate_financial_rows(rows(&invalid), Some(&scope)).is_err());
        let mut invalid = state.clone();
        invalid.scope_id = Uuid::nil();
        assert!(validate_financial_rows(rows(&invalid), Some(&scope)).is_err());
        let mut invalid = state.clone();
        invalid.global_consumed = 1;
        assert!(validate_financial_rows(rows(&invalid), Some(&scope)).is_err());
        let mut invalid = state.clone();
        invalid.policy_fingerprint = "corrupt".to_owned();
        assert!(validate_financial_rows(rows(&invalid), Some(&scope)).is_err());
        let mismatched_row = vec![(Uuid::new_v4(), serde_json::to_value(&state).unwrap())];
        assert!(validate_financial_rows(mismatched_row, Some(&scope)).is_err());
        assert!(
            validate_financial_rows(vec![(state.provider_id, serde_json::json!({}))], Some(&scope))
                .is_err()
        );
    }

    #[test]
    fn no_required_scope_and_multiple_rows_preserve_fail_closed_behavior() {
        let state = admission(FinancialAdmissionState::Closed);
        let scope = required(&state);
        assert!(validate_financial_rows(Vec::new(), None).is_ok());
        assert!(validate_financial_rows(Vec::new(), Some(&scope)).is_ok());
        assert!(validate_financial_rows(rows(&state), None).is_err());
        let mut duplicate = rows(&state);
        duplicate.extend(rows(&state));
        assert!(validate_financial_rows(duplicate, Some(&scope)).is_err());
    }
}

#[cfg(test)]
mod financial_envelope_tests {
    use super::*;

    fn chat_envelope() -> FinancialRequestEnvelope {
        FinancialRequestEnvelope::Chat {
            allowed_service_tiers: vec!["default".to_owned(), "flex".to_owned()],
            max_input_bytes: 4096,
            max_output_tokens: 128_000,
            max_input_tokens: 922_000,
            media: None,
        }
    }

    fn chat_media(
        max_images: u32,
        max_image_bytes_each: u64,
        max_image_bytes_total: u64,
        max_image_dimension_px: u32,
        max_image_pixels_total: u64,
    ) -> FinancialChatMediaAdmission {
        FinancialChatMediaAdmission {
            max_images,
            max_image_bytes_each,
            max_image_bytes_total,
            max_image_dimension_px,
            max_image_pixels_total,
        }
    }

    fn vision_envelope(media: FinancialChatMediaAdmission) -> FinancialRequestEnvelope {
        FinancialRequestEnvelope::Chat {
            allowed_service_tiers: vec!["default".to_owned(), "flex".to_owned()],
            max_input_bytes: 16_384,
            max_output_tokens: 128_000,
            max_input_tokens: 922_000,
            media: Some(media),
        }
    }

    fn vision_body(content: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "model": "gpt-6-luna",
            "messages": [{"role": "user", "content": content}],
            "service_tier": "flex",
            "n": 1,
            "max_completion_tokens": 1024,
        })
    }

    fn admits_vision(media: FinancialChatMediaAdmission, content: serde_json::Value) -> bool {
        envelope_admits_body(&vision_envelope(media), "gpt-6-luna", &vision_body(content))
    }

    fn image_fixture(
        width: u32,
        height: u32,
        format: image::ImageFormat,
        mime: &str,
    ) -> (String, u64) {
        let image = image::RgbImage::from_pixel(width, height, image::Rgb([0, 0, 0]));
        let mut cursor = std::io::Cursor::new(Vec::new());
        image.write_to(&mut cursor, format).expect("encode image fixture");
        let bytes = cursor.into_inner();
        let decoded_len = bytes.len() as u64;
        (
            format!(
                "data:{mime};base64,{}",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            ),
            decoded_len,
        )
    }

    fn png_data_url(width: u32, height: u32) -> String {
        image_fixture(width, height, image::ImageFormat::Png, "image/png").0
    }

    fn image_part(url: impl Into<String>) -> serde_json::Value {
        let url: String = url.into();
        serde_json::json!({"type": "image_url", "image_url": {"url": url}})
    }

    fn embedding_envelope() -> FinancialRequestEnvelope {
        FinancialRequestEnvelope::Embeddings {
            max_inputs: 2_048,
            max_input_bytes_each: 65_536,
            max_input_bytes_total: 1_048_576,
            max_input_tokens: 300_000,
        }
    }

    fn chat_body() -> serde_json::Value {
        serde_json::json!({
            "model": "gpt-6-luna",
            "messages": [{"role": "user", "content": "hello"}],
            "service_tier": "flex",
            "n": 1,
            "max_completion_tokens": 1024,
        })
    }

    fn admits_chat(mutate: impl FnOnce(&mut serde_json::Value)) -> bool {
        let mut body = chat_body();
        mutate(&mut body);
        envelope_admits_body(&chat_envelope(), "gpt-6-luna", &body)
    }

    #[test]
    fn chat_envelope_admits_the_canonical_body() {
        assert!(admits_chat(|_| {}));
    }

    #[test]
    fn chat_envelope_refuses_wrong_model() {
        assert!(!admits_chat(|body| body["model"] = serde_json::json!("gpt-6-sol")));
    }

    #[test]
    fn chat_envelope_refuses_missing_or_unqualified_tier() {
        assert!(!admits_chat(|body| {
            body.as_object_mut().expect("object").remove("service_tier");
        }));
        assert!(!admits_chat(|body| body["service_tier"] = serde_json::json!("auto")));
        assert!(!admits_chat(|body| body["service_tier"] = serde_json::json!("priority")));
        assert!(!admits_chat(|body| body["service_tier"] = serde_json::json!("fast")));
    }

    #[test]
    fn financial_chat_envelope_admits_streaming_and_refuses_wrong_n() {
        assert!(admits_chat(|body| body["stream"] = serde_json::json!(true)));
        assert!(admits_chat(|body| body["stream"] = serde_json::json!(false)));
        assert!(!admits_chat(|body| body["n"] = serde_json::json!(2)));
        assert!(!admits_chat(|body| {
            body["stream"] = serde_json::json!(true);
            body["messages"] = serde_json::json!([{"role": "user", "content": "x".repeat(4096)}]);
        }));
    }

    #[test]
    fn financial_chat_envelope_refuses_missing_or_excessive_completion_cap() {
        assert!(!admits_chat(|body| {
            body.as_object_mut().expect("object").remove("max_completion_tokens");
        }));
        assert!(!admits_chat(|body| {
            body["max_completion_tokens"] = serde_json::json!(128_001);
        }));
    }

    #[test]
    fn chat_envelope_refuses_paid_tools_and_media() {
        for field in ["tools", "functions", "audio", "modalities"] {
            assert!(
                !admits_chat(|body| body[field] = serde_json::json!([{}])),
                "{field} must be refused"
            );
        }
    }

    #[test]
    fn chat_envelope_admits_multipart_text_content() {
        assert!(admits_chat(|body| {
            body["messages"] = serde_json::json!([{
                "role": "user",
                "content": [{"type": "text", "text": "hello"}],
            }]);
        }));
    }

    #[test]
    fn chat_envelope_refuses_image_parts_nested_in_message_content() {
        for part in [
            serde_json::json!({
                "type": "image_url",
                "image_url": {"url": "data:image/png;base64,AAAA"},
            }),
            serde_json::json!({
                "type": "input_image",
                "input_image": {"url": "data:image/png;base64,AAAA"},
            }),
            serde_json::json!({"type": "audio", "audio": {"data": "AAAA"}}),
            serde_json::json!({"type": "file", "file": {"file_id": "file-1"}}),
            serde_json::json!({
                "type": "text",
                "text": "hello",
                "image_url": {"url": "data:image/png;base64,AAAA"},
            }),
        ] {
            assert!(
                !admits_chat(|body| {
                    body["messages"] = serde_json::json!([{
                        "role": "user",
                        "content": [{"type": "text", "text": "describe"}, part],
                    }]);
                }),
                "a nested media part must be refused"
            );
        }
    }

    #[test]
    fn chat_envelope_refuses_media_outside_complete_text_parts() {
        assert!(!admits_chat(|body| {
            body["messages"] = serde_json::json!([{
                "role": "user",
                "content": "hello",
                "image_url": {"url": "data:image/png;base64,AAAA"},
            }]);
        }));
        assert!(!admits_chat(|body| {
            body["messages"] = serde_json::json!([{
                "role": "user",
                "content": {
                    "type": "image_url",
                    "image_url": {"url": "data:image/png;base64,AAAA"},
                },
            }]);
        }));
        assert!(!admits_chat(|body| {
            body["messages"] = serde_json::json!([{
                "role": "user",
                "content": [{"type": "text"}],
            }]);
        }));
        assert!(!admits_chat(|body| {
            body["messages"] = serde_json::json!([{"role": "user", "content": []}]);
        }));
        assert!(!admits_chat(|body| body["messages"] = serde_json::json!([])));
        assert!(!admits_chat(|body| body["messages"] = serde_json::json!(["hello"])));
        assert!(!admits_chat(|body| body["messages"] = serde_json::json!("hello")));
        assert!(!admits_chat(|body| {
            body.as_object_mut().expect("object").remove("messages");
        }));
    }

    #[test]
    fn media_policy_admits_a_bounded_inline_png_and_plain_text() {
        let media = chat_media(2, 1_024, 1_536, 32, 1_024);
        assert!(admits_vision(
            media.clone(),
            serde_json::json!([
                {"type": "text", "text": "transcribe"},
                image_part(png_data_url(8, 8)),
            ]),
        ));
        assert!(admits_vision(media, serde_json::json!("plain text")));
    }

    #[test]
    fn media_policy_admits_a_bounded_inline_jpeg() {
        let (url, decoded_len) = image_fixture(8, 8, image::ImageFormat::Jpeg, "image/jpeg");
        assert!(admits_vision(
            chat_media(1, decoded_len, decoded_len, 8, 64),
            serde_json::json!([
                {"type": "text", "text": "transcribe"},
                image_part(url),
            ]),
        ));
    }

    #[test]
    fn media_policy_refuses_cross_message_aggregate_bytes() {
        let (url, decoded_len) = image_fixture(8, 8, image::ImageFormat::Png, "image/png");
        // Each image alone fits both byte ceilings; only their sum does not.
        let body = serde_json::json!({
            "model": "gpt-6-luna",
            "messages": [
                {"role": "user", "content": [image_part(url.clone())]},
                {"role": "user", "content": [image_part(url)]},
            ],
            "service_tier": "flex",
            "n": 1,
            "max_completion_tokens": 1024,
        });
        assert!(!envelope_admits_body(
            &vision_envelope(chat_media(2, decoded_len, decoded_len * 2 - 1, 8, 128)),
            "gpt-6-luna",
            &body,
        ));
    }

    #[test]
    fn media_policy_refuses_cross_message_aggregate_pixels() {
        let (url, decoded_len) = image_fixture(8, 8, image::ImageFormat::Png, "image/png");
        // Byte ceilings fit the pair; only the summed pixel ceiling does not.
        let body = serde_json::json!({
            "model": "gpt-6-luna",
            "messages": [
                {"role": "user", "content": [image_part(url.clone())]},
                {"role": "user", "content": [image_part(url)]},
            ],
            "service_tier": "flex",
            "n": 1,
            "max_completion_tokens": 1024,
        });
        assert!(!envelope_admits_body(
            &vision_envelope(chat_media(2, decoded_len, decoded_len * 2, 8, 127)),
            "gpt-6-luna",
            &body,
        ));
    }

    #[test]
    fn media_policy_refuses_accumulator_overflow_instead_of_saturating() {
        let part = image_part(png_data_url(4, 4));
        let part = part.as_object().expect("image part object");
        let policy = chat_media(1, 1_024, u64::MAX, 8, u64::MAX);

        let mut images = 0u32;
        let mut bytes_total = u64::MAX - 1;
        let mut pixels_total = 0u64;
        assert!(!chat_image_part_admits(
            part,
            &policy,
            &mut images,
            &mut bytes_total,
            &mut pixels_total,
        ));
        assert_eq!(images, 0);
        assert_eq!(bytes_total, u64::MAX - 1);

        let mut images = 0u32;
        let mut bytes_total = 0u64;
        let mut pixels_total = u64::MAX - 1;
        assert!(!chat_image_part_admits(
            part,
            &policy,
            &mut images,
            &mut bytes_total,
            &mut pixels_total,
        ));
        assert_eq!(pixels_total, u64::MAX - 1);
    }

    #[test]
    fn media_policy_refuses_unbounded_transport_and_format_shapes() {
        let media = chat_media(2, 1_024, 1_536, 32, 1_024);
        let png = png_data_url(8, 8);
        let mismatched_mime = png.replacen("data:image/png;base64,", "data:image/jpeg;base64,", 1);
        for url in [
            "https://example.test/image.png".to_owned(),
            "data:image/webp;base64,AAAA".to_owned(),
            "data:image/PNG;base64,AAAA".to_owned(),
            "data:image/png;base64,not-base64!!".to_owned(),
            mismatched_mime,
        ] {
            assert!(
                !admits_vision(
                    media.clone(),
                    serde_json::json!([
                        {"type": "text", "text": "transcribe"},
                        image_part(url),
                    ]),
                ),
                "an unsupported transport or format must be refused"
            );
        }
        let mut part = image_part(png.clone());
        part["detail"] = serde_json::json!("high");
        assert!(!admits_vision(media.clone(), serde_json::json!([part])));
        assert!(!admits_vision(
            media,
            serde_json::json!([
                {"type": "image_url", "image_url": {"url": png, "detail": "high"}},
            ]),
        ));
    }

    #[test]
    fn media_policy_enforces_each_bound_independently() {
        let image = || {
            serde_json::json!([
                {"type": "text", "text": "transcribe"},
                image_part(png_data_url(8, 8)),
            ])
        };
        assert!(admits_vision(chat_media(1, 1_024, 1_024, 8, 64), image()));
        assert!(!admits_vision(chat_media(0, 1_024, 1_024, 8, 64), image()));
        assert!(!admits_vision(chat_media(1, 32, 1_024, 8, 64), image()));
        assert!(!admits_vision(chat_media(1, 1_024, 32, 8, 64), image()));
        assert!(!admits_vision(chat_media(1, 1_024, 1_024, 4, 64), image()));
        assert!(!admits_vision(chat_media(1, 1_024, 1_024, 8, 63), image()));
        let two_images =
            || serde_json::json!([image_part(png_data_url(4, 4)), image_part(png_data_url(4, 4)),]);
        assert!(!admits_vision(chat_media(1, 1_024, 1_024, 8, 64), two_images()));
    }

    #[test]
    fn media_policy_still_refuses_tools_and_unknown_media_parts() {
        let media = chat_media(1, 1_024, 1_024, 8, 64);
        assert!(!admits_vision(
            media.clone(),
            serde_json::json!([{"type": "audio", "audio": {"data": "AAAA"}}]),
        ));
        assert!(!admits_vision(
            media.clone(),
            serde_json::json!([
                {"type": "input_image", "input_image": {"url": png_data_url(4, 4)}},
            ]),
        ));
        let mut body = vision_body(serde_json::json!([image_part(png_data_url(4, 4))]));
        body["tools"] = serde_json::json!([{}]);
        assert!(!envelope_admits_body(&vision_envelope(media), "gpt-6-luna", &body));
    }

    #[test]
    fn media_policy_keeps_the_full_request_bytes_guard() {
        let mut envelope = vision_envelope(chat_media(1, 1_024, 1_024, 8, 64));
        let FinancialRequestEnvelope::Chat { max_input_bytes, .. } = &mut envelope else {
            panic!("fixture must be a chat envelope");
        };
        *max_input_bytes = 64;
        let body = vision_body(serde_json::json!([image_part(png_data_url(4, 4))]));
        assert!(!envelope_admits_body(&envelope, "gpt-6-luna", &body));
    }

    #[test]
    fn chat_envelope_refuses_oversized_body_as_a_resource_guard() {
        assert!(!admits_chat(|body| {
            body["messages"] = serde_json::json!([{"role": "user", "content": "x".repeat(4096)}]);
        }));
    }

    fn admits_embedding(input: serde_json::Value) -> bool {
        let body = serde_json::json!({"model": "text-embedding-3-large", "input": input});
        envelope_admits_body(&embedding_envelope(), "text-embedding-3-large", &body)
    }

    #[test]
    fn embedding_envelope_admits_string_inputs() {
        assert!(admits_embedding(serde_json::json!(["alpha", "beta"])));
    }

    #[test]
    fn embedding_envelope_refuses_a_chat_style_tier() {
        let body = serde_json::json!({
            "model": "text-embedding-3-large",
            "input": ["alpha"],
            "service_tier": "flex",
        });
        assert!(!envelope_admits_body(&embedding_envelope(), "text-embedding-3-large", &body));
    }

    #[test]
    fn embedding_envelope_refuses_token_id_arrays() {
        assert!(!admits_embedding(serde_json::json!([[1, 2, 3]])));
        assert!(!admits_embedding(serde_json::json!([["alpha", 7]])));
    }

    #[test]
    fn embedding_envelope_enforces_input_count_and_byte_ceilings() {
        let too_many: Vec<String> = (0..2_049).map(|index| format!("input-{index}")).collect();
        assert!(!admits_embedding(serde_json::json!(too_many)));

        assert!(!admits_embedding(serde_json::json!(["x".repeat(65_537)])));

        let aggregate: Vec<String> = (0..20).map(|_| "y".repeat(65_536)).collect();
        assert!(!admits_embedding(serde_json::json!(aggregate)));
    }
}
