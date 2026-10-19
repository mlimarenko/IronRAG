//! IRG-182 eligibility cases for the one canonical gate,
//! `binding_eligibility::validate_binding_target`.

use super::*;

const GENERATIVE_PURPOSES: [AiBindingPurpose; 6] = [
    AiBindingPurpose::ExtractText,
    AiBindingPurpose::ExtractGraph,
    AiBindingPurpose::EmbedChunk,
    AiBindingPurpose::QueryCompile,
    AiBindingPurpose::QueryAnswer,
    AiBindingPurpose::Agent,
];

const DECISION_PURPOSES: [AiBindingPurpose; 2] =
    [AiBindingPurpose::QueryRerank, AiBindingPurpose::GraphEntityType];

/// A provider that declares the typed decision capability — the TypeSafe
/// System One shape the seed ships.
fn decision_provider() -> ProviderCatalogEntry {
    let mut provider = sample_provider("provider-alpha");
    provider.capabilities.decision = ProviderCapabilityState::Supported;
    provider
}

fn model_allowing(
    provider: &ProviderCatalogEntry,
    purposes: Vec<AiBindingPurpose>,
) -> ModelCatalogEntry {
    catalog_model(Uuid::now_v7(), provider.id, "sample-model", purposes)
}

fn binding_target_error(
    provider: &ProviderCatalogEntry,
    model: &ModelCatalogEntry,
    purpose: AiBindingPurpose,
) -> ApiError {
    validate_binding_target(BindingTarget {
        purpose,
        provider,
        model,
        account_credential_state: "active",
    })
    .expect_err("the binding target must be rejected")
}

#[test]
fn decision_provider_rejects_every_generative_purpose() {
    let provider = decision_provider();
    for purpose in GENERATIVE_PURPOSES {
        let model = model_allowing(&provider, vec![purpose]);
        let error = binding_target_error(&provider, &model, purpose);
        let ApiError::BadRequest(message) = error else {
            panic!("a rejected binding must be a BadRequest");
        };
        assert!(
            message.contains(purpose.as_str()),
            "the rejection must name the purpose `{}`, got: {message}",
            purpose.as_str()
        );
    }
}

#[test]
fn a_decision_binding_is_storable_for_both_decision_purposes() {
    let provider = decision_provider();
    for purpose in DECISION_PURPOSES {
        let model = model_allowing(&provider, vec![purpose]);
        assert!(
            validate_binding_target(BindingTarget {
                purpose,
                provider: &provider,
                model: &model,
                account_credential_state: "active",
            })
            .is_ok(),
            "a decision binding for {} must validate",
            purpose.as_str()
        );
    }
}

#[test]
fn an_active_query_rerank_decision_binding_is_now_allowed() {
    // IRG-187 shipped the TypeSafe SystemOne runtime for semantic rerank, so
    // the purpose is activatable.
    let provider = decision_provider();
    let model = model_allowing(&provider, vec![AiBindingPurpose::QueryRerank]);
    assert!(
        validate_binding_target(BindingTarget {
            purpose: AiBindingPurpose::QueryRerank,
            provider: &provider,
            model: &model,
            account_credential_state: "active",
        })
        .is_ok(),
        "query_rerank ships with IRG-187 and must activate"
    );
}

#[test]
fn an_active_graph_entity_type_decision_binding_is_now_allowed() {
    // IRG-185 shipped the graph entity typing runtime, so the purpose is
    // activatable and no decision purpose fails closed here any more.
    let provider = decision_provider();
    let model = model_allowing(&provider, vec![AiBindingPurpose::GraphEntityType]);
    assert!(
        validate_binding_target(BindingTarget {
            purpose: AiBindingPurpose::GraphEntityType,
            provider: &provider,
            model: &model,
            account_credential_state: "active",
        })
        .is_ok(),
        "graph_entity_type ships with IRG-185 and must activate"
    );
}

#[test]
fn a_chat_model_stays_allowed_on_a_decision_purpose() {
    // A generative provider is not restricted to generative purposes: a fast
    // chat model remains a valid binding target for `graph_entity_type`.
    let provider = sample_provider("provider-alpha");
    let model = model_allowing(&provider, vec![AiBindingPurpose::GraphEntityType]);
    assert!(
        validate_binding_target(BindingTarget {
            purpose: AiBindingPurpose::GraphEntityType,
            provider: &provider,
            model: &model,
            account_credential_state: "active",
        })
        .is_ok(),
        "a chat model allowed for graph_entity_type must stay bindable"
    );
}

#[test]
fn a_decision_providers_model_contract_rejects_generative_purposes() {
    let provider = decision_provider();
    for purpose in GENERATIVE_PURPOSES {
        assert!(
            matches!(
                validate_model_purposes_for_provider(&provider, &[purpose]),
                Err(ApiError::BadRequest(_))
            ),
            "a decision provider must not gain a model allowing {}",
            purpose.as_str()
        );
    }
    assert!(validate_model_purposes_for_provider(&provider, &DECISION_PURPOSES).is_ok());

    let generative = sample_provider("provider-alpha");
    assert!(validate_model_purposes_for_provider(&generative, &GENERATIVE_PURPOSES).is_ok());
}

#[test]
fn a_persisted_model_row_that_dropped_the_purpose_is_rejected_on_read() {
    // The read path resolves a persisted row and must consult the model's own
    // allowed-purpose contract, not the requested purpose: a model whose
    // allowed purposes no longer include the binding's purpose is refused.
    let provider = sample_provider("provider-alpha");
    let model = model_allowing(&provider, vec![AiBindingPurpose::QueryAnswer]);
    assert!(matches!(
        validate_binding_target(BindingTarget {
            purpose: AiBindingPurpose::EmbedChunk,
            provider: &provider,
            model: &model,
            account_credential_state: "active",
        }),
        Err(ApiError::BadRequest(_))
    ));
}

#[test]
fn a_binding_state_outside_the_enum_is_a_bad_request() {
    // Create now takes a client-supplied state; an unknown label must be
    // refused before it reaches the `ai_binding_state` cast (which would 500).
    use super::super::binding_operations::validated_binding_state;
    for state in ["active", "invalid", "disabled"] {
        assert_eq!(validated_binding_state(state).ok(), Some(state));
    }
    for state in ["bogus", "inactive", "", "Active"] {
        assert!(matches!(validated_binding_state(state), Err(ApiError::BadRequest(_))));
    }
}
