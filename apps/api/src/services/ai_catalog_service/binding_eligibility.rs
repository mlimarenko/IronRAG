//! Canonical purpose-eligibility gate for AI bindings (IRG-182).
//!
//! `validate_binding_target` is the one validation both the write path
//! (`binding_operations`) and the read path (`runtime_resolution`) call
//! before a binding may be stored or resolved. A decision provider
//! (`capabilities.decision` supported) may store or activate a binding only
//! for a decision purpose (`AiBindingPurpose::is_decision`); a chat model
//! stays eligible for a decision purpose through its own
//! `allowed_binding_purposes`. Both decision purposes are shipped:
//! `query_rerank` by IRG-187 and `graph_entity_type` by IRG-185, so no
//! decision purpose fails closed here any more.

use super::*;

/// The provider account, model and purpose a binding write or read targets.
pub(super) struct BindingTarget<'a> {
    pub(super) purpose: AiBindingPurpose,
    pub(super) provider: &'a ProviderCatalogEntry,
    pub(super) model: &'a ModelCatalogEntry,
    pub(super) account_credential_state: &'a str,
}

/// Validates a binding target in the fixed order the plan specifies: model
/// ownership, provider/model lifecycle, account credential state, the
/// purpose-capability rule, and the model's own allowed-purpose contract.
pub(super) fn validate_binding_target(target: BindingTarget<'_>) -> Result<(), ApiError> {
    let BindingTarget { purpose, provider, model, account_credential_state } = target;
    if model.provider_catalog_id != provider.id {
        return Err(ApiError::BadRequest(
            "binding links a provider account to a model from another provider".to_string(),
        ));
    }
    if provider.lifecycle_state == "disabled" {
        return Err(ApiError::BadRequest("provider is disabled".to_string()));
    }
    if model.lifecycle_state == "disabled" {
        return Err(ApiError::BadRequest("model is disabled".to_string()));
    }
    if account_credential_state != "active" {
        return Err(ApiError::BadRequest("provider account is not active".to_string()));
    }
    validate_purpose_capability(purpose, provider)?;
    validate_model_binding_purpose(purpose, model)?;
    Ok(())
}

/// A decision-only provider (`capabilities.decision` supported) may serve
/// only a decision purpose. A generative provider is unaffected: nothing here
/// blocks it from serving any purpose its models allow, decision purposes
/// included — a fast chat model stays a valid alternative for
/// `graph_entity_type`/`query_rerank` through its own
/// `allowed_binding_purposes`.
fn validate_purpose_capability(
    purpose: AiBindingPurpose,
    provider: &ProviderCatalogEntry,
) -> Result<(), ApiError> {
    if provider.capabilities.decision == ProviderCapabilityState::Supported
        && !purpose.is_decision()
    {
        return Err(ApiError::BadRequest(format!(
            "a decision-only provider cannot serve {}; bind it to query_rerank or graph_entity_type",
            binding_purpose_key(purpose)
        )));
    }
    Ok(())
}

/// The same purpose-capability rule as `validate_binding_target`, applied to
/// a model's full allowed-purpose list on create/update: a decision provider
/// must not gain a model whose allowed purposes include a generative one.
pub(super) fn validate_model_purposes_for_provider(
    provider: &ProviderCatalogEntry,
    purposes: &[AiBindingPurpose],
) -> Result<(), ApiError> {
    for purpose in purposes {
        validate_purpose_capability(*purpose, provider)?;
    }
    Ok(())
}
