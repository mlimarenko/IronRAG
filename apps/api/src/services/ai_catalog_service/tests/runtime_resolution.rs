use super::*;

#[test]
fn runtime_binding_batch_deduplicates_purposes_in_first_seen_order() {
    let purposes = deduplicate_binding_purposes(&[
        AiBindingPurpose::QueryAnswer,
        AiBindingPurpose::EmbedChunk,
        AiBindingPurpose::QueryAnswer,
        AiBindingPurpose::ExtractText,
        AiBindingPurpose::EmbedChunk,
    ]);

    assert_eq!(
        purposes,
        vec![
            AiBindingPurpose::QueryAnswer,
            AiBindingPurpose::EmbedChunk,
            AiBindingPurpose::ExtractText,
        ]
    );
    assert!(deduplicate_binding_purposes(&[]).is_empty());
}

#[test]
fn binding_embedding_dimensions_take_precedence_over_catalog_metadata() {
    let dimensions = resolve_effective_embedding_dimensions(
        AiBindingPurpose::EmbedChunk,
        &serde_json::json!({"dimensions": 384}),
        &serde_json::json!({"dimensions": 768}),
    )
    .expect("valid embedding dimensions")
    .expect("configured embedding dimensions");

    assert_eq!(dimensions.get(), 384);
}

#[test]
fn catalog_embedding_dimensions_are_used_without_mutating_request_parameters() {
    let binding_parameters = serde_json::json!({"encoding_format": "float"});
    let dimensions = resolve_effective_embedding_dimensions(
        AiBindingPurpose::EmbedChunk,
        &binding_parameters,
        &serde_json::json!({"dimensions": 768}),
    )
    .expect("valid catalog embedding dimensions")
    .expect("catalog embedding dimensions");

    assert_eq!(dimensions.get(), 768);
    assert!(binding_parameters.get("dimensions").is_none());
}

#[test]
fn invalid_explicit_embedding_dimensions_fail_closed_instead_of_falling_back() {
    for invalid_dimensions in [
        serde_json::json!(0),
        serde_json::json!(-1),
        serde_json::json!("384"),
        serde_json::json!(4_001),
    ] {
        let result = resolve_effective_embedding_dimensions(
            AiBindingPurpose::EmbedChunk,
            &serde_json::json!({"dimensions": invalid_dimensions}),
            &serde_json::json!({"dimensions": 768}),
        );

        assert!(matches!(result, Err(ApiError::BadRequest(_))));
    }
}

#[test]
fn invalid_catalog_embedding_dimensions_fail_closed() {
    for invalid_dimensions in [
        serde_json::json!(0),
        serde_json::json!(-1),
        serde_json::json!("384"),
        serde_json::json!(4_001),
    ] {
        let result = resolve_effective_embedding_dimensions(
            AiBindingPurpose::EmbedChunk,
            &serde_json::json!({}),
            &serde_json::json!({"dimensions": invalid_dimensions}),
        );

        assert!(matches!(result, Err(ApiError::BadRequest(_))));
    }
}

#[test]
fn disabled_provider_or_model_cannot_be_resolved_or_bound() {
    let provider = sample_provider("provider-alpha");
    let model = catalog_model(
        Uuid::now_v7(),
        provider.id,
        "sample-model",
        vec![AiBindingPurpose::QueryAnswer],
    );
    let account = AiAccount {
        id: Uuid::now_v7(),
        scope_kind: AiScopeKind::Instance,
        workspace_id: None,
        library_id: None,
        provider_catalog_id: provider.id,
        label: "sample-account".to_string(),
        api_key: None,
        base_url: None,
        credential_state: "active".to_string(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };

    for disable_provider in [false, true] {
        let mut candidate_provider = provider.clone();
        let mut candidate_model = model.clone();
        if disable_provider {
            candidate_provider.lifecycle_state = "disabled".to_string();
        } else {
            candidate_model.lifecycle_state = "disabled".to_string();
        }

        assert!(matches!(
            validate_binding_target(BindingTarget {
                purpose: AiBindingPurpose::QueryAnswer,
                provider: &candidate_provider,
                model: &candidate_model,
                account_credential_state: "active",
            }),
            Err(ApiError::BadRequest(_))
        ));
        assert!(matches!(
            validate_binding_target(BindingTarget {
                purpose: AiBindingPurpose::QueryAnswer,
                provider: &candidate_provider,
                model: &candidate_model,
                account_credential_state: &account.credential_state,
            }),
            Err(ApiError::BadRequest(_))
        ));
    }
}
