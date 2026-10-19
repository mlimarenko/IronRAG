use super::*;

#[test]
fn preserves_provider_alpha_compatible_base_url_path() {
    let provider = sample_provider("provider-beta");

    assert_eq!(
        normalize_provider_base_url(&provider, "http://localhost:11434/v1")
            .expect("provider-beta Provider Alpha-compatible base path should normalize"),
        "http://localhost:11434/v1"
    );
    assert_eq!(
        normalize_provider_base_url(&provider, "http://localhost:11434/api")
            .expect("non-active provider-beta paths should not be rewritten"),
        "http://localhost:11434/api"
    );
}

#[test]
fn provider_base_url_policy_blocks_hosted_overrides_and_allows_local_overrides() {
    let provider = sample_provider("provider-alpha");
    let error = normalize_provider_base_url_input(&provider, Some("https://override.example/v1"))
        .expect_err("hosted providers must reject forbidden baseUrl overrides");
    assert!(matches!(error, ApiError::BadRequest(_)));

    let local_provider = sample_provider("provider-beta");
    let local_normalized =
        normalize_provider_base_url_input(&local_provider, Some("http://localhost:11434/v1"))
            .expect("local providers should still accept explicit baseUrl overrides");
    assert_eq!(local_normalized.as_deref(), Some("http://localhost:11434/v1"));

    let mut private_provider = provider.clone();
    private_provider.base_url_policy.allow_override = true;
    let private_error = normalize_provider_base_url(&private_provider, "https://127.0.0.1/v1")
        .expect_err("hosted providers should reject private network base URLs");
    assert!(matches!(private_error, ApiError::BadRequest(_)));

    let userinfo_error =
        normalize_provider_base_url(&private_provider, "https://userinfo@example.com/v1")
            .expect_err("provider base URLs must not carry userinfo");
    assert!(matches!(userinfo_error, ApiError::BadRequest(_)));

    let query_error =
        normalize_provider_base_url(&private_provider, "https://example.com/v1?marker=opaque")
            .expect_err("provider base URLs must not carry query strings");
    assert!(matches!(query_error, ApiError::BadRequest(_)));
}

#[test]
fn openai_provider_accepts_base_url_override_for_compatible_gateways() {
    let mut provider = sample_provider("provider-alpha");
    provider.provider_kind = "openai".to_string();
    provider.base_url_policy.allow_override = true;

    let normalized =
        normalize_provider_base_url_input(&provider, Some("https://openai.bothub.ru/v1"))
            .expect("openai provider should accept explicit compatible baseUrl override");
    assert_eq!(normalized.as_deref(), Some("https://openai.bothub.ru/v1"));
}

#[test]
fn hosted_base_url_update_clears_legacy_override_and_runtime_rejects_it() {
    let provider = sample_provider("provider-epsilon");

    let created = provider_credential_base_url_for_create(&provider, None)
        .expect("fixed hosted providers should not store provider default on create");
    assert_eq!(created, None);

    let stored = provider_credential_base_url_for_update(
        &provider,
        Some("https://stale-host.example/v1"),
        None,
    )
    .expect("fixed hosted providers should clear stored credential baseUrl during update");
    assert_eq!(stored, None);

    let runtime_error = runtime_provider_base_url(&provider, Some("https://stale-host.example/v1"))
        .expect_err("runtime must fail closed on a forbidden persisted override");
    assert!(matches!(runtime_error, ApiError::BadRequest(_)));
}

#[test]
fn allowed_override_update_preserves_omitted_url_but_not_hidden_key_across_origins() {
    let provider = sample_provider("provider-beta");
    let existing = "http://localhost:11434/v1";

    let preserved = provider_credential_base_url_for_update(&provider, Some(existing), None)
        .expect("omitting an allowed override should preserve it");
    assert_eq!(preserved.as_deref(), Some(existing));

    validate_provider_base_url_key_reuse(
        &provider,
        Some(existing),
        Some("http://localhost:11434/api"),
        true,
        false,
    )
    .expect("the same current authority may reuse the existing key");

    let authority_error = validate_provider_base_url_key_reuse(
        &provider,
        Some(existing),
        Some("https://gateway.example/v1"),
        true,
        false,
    )
    .expect_err("a hidden key must never move to a different authority");
    assert!(matches!(authority_error, ApiError::BadRequest(_)));

    validate_provider_base_url_key_reuse(
        &provider,
        Some(existing),
        Some("https://gateway.example/v1"),
        true,
        true,
    )
    .expect("an explicitly supplied replacement key may be validated on a new authority");
}

#[test]
fn detects_loopback_base_urls() {
    assert!(is_loopback_base_url("http://localhost:11434/v1"));
    assert!(is_loopback_base_url("http://127.0.0.1:11434/v1"));
    assert!(!is_loopback_base_url("http://host.docker.internal:11434/v1"));
}

#[test]
fn vector_dimension_cache_tracks_embed_chunk_purpose() {
    assert!(super::binding_affects_vector_index_dimension(AiBindingPurpose::EmbedChunk));
    assert!(!super::binding_affects_vector_index_dimension(AiBindingPurpose::ExtractGraph));
    assert!(!super::binding_affects_vector_index_dimension(AiBindingPurpose::QueryCompile));
}

#[test]
fn env_provider_credential_bootstrap_skips_typed_provider_validation_failures() {
    for failure_kind in [
        ProviderCredentialValidationFailureKind::Rejected,
        ProviderCredentialValidationFailureKind::Transport,
        ProviderCredentialValidationFailureKind::LoopbackUnreachable,
    ] {
        let error = provider_credential_validation_error(failure_kind, "opaque detail");
        assert!(is_provider_credential_validation_error(&error), "{failure_kind:?}");
    }
}

#[test]
fn env_provider_credential_bootstrap_does_not_classify_error_prose() {
    let prose_only = ApiError::BadRequest(
        "provider credential validation failed for Provider Alpha at /models".to_string(),
    );

    assert!(!is_provider_credential_validation_error(&prose_only));
    assert!(!is_provider_credential_validation_error(&ApiError::Internal));
    assert!(!is_provider_credential_validation_error(&ApiError::Conflict(
        "duplicate credential label".to_string()
    )));
    assert!(!is_provider_credential_validation_error(&ApiError::NotFound(
        "provider_catalog missing".to_string()
    )));
}

#[test]
fn chat_round_trip_rejects_only_typed_authentication_statuses() {
    for status in [reqwest::StatusCode::UNAUTHORIZED, reqwest::StatusCode::FORBIDDEN] {
        let error = anyhow::Error::new(crate::integrations::retry::ProviderCallError::http_status(
            "provider-alpha",
            status,
            &reqwest::header::HeaderMap::new(),
            "opaque",
        ));

        assert_eq!(
            chat_round_trip_validation_failure_kind(&error),
            ChatRoundTripValidationFailureKind::CredentialRejected,
        );
    }
}

#[test]
fn chat_round_trip_maps_typed_unavailability_without_rejecting_credentials() {
    for status in [
        reqwest::StatusCode::REQUEST_TIMEOUT,
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        reqwest::StatusCode::BAD_GATEWAY,
    ] {
        let error = anyhow::Error::new(crate::integrations::retry::ProviderCallError::http_status(
            "provider-alpha",
            status,
            &reqwest::header::HeaderMap::new(),
            "opaque",
        ));

        assert_eq!(
            chat_round_trip_validation_failure_kind(&error),
            ChatRoundTripValidationFailureKind::ProviderUnavailable,
        );
    }

    let source = reqwest::Client::new()
        .get("://")
        .build()
        .expect_err("synthetic malformed URL must create a typed reqwest error");
    let error = anyhow::Error::new(crate::integrations::retry::ProviderCallError::transport(
        "opaque", source,
    ));
    assert_eq!(
        chat_round_trip_validation_failure_kind(&error),
        ChatRoundTripValidationFailureKind::ProviderUnavailable,
    );
}

#[test]
fn chat_round_trip_maps_execution_deadline_timeout_to_provider_unavailable() {
    let error = anyhow::Error::new(crate::integrations::retry::ProviderCallError::timeout(
        "opaque deadline context",
    ));

    assert_eq!(
        chat_round_trip_validation_failure_kind(&error),
        ChatRoundTripValidationFailureKind::ProviderUnavailable,
    );
}

#[test]
fn chat_round_trip_does_not_infer_credentials_from_misleading_prose() {
    let protocol_error =
        anyhow::Error::new(crate::integrations::retry::ProviderCallError::protocol(
            "status=401 credential rejected transport timeout",
        ));
    let untyped_error = anyhow::anyhow!("status=403 invalid api key");
    let non_authentication_status =
        anyhow::Error::new(crate::integrations::retry::ProviderCallError::http_status(
            "provider-alpha",
            reqwest::StatusCode::BAD_REQUEST,
            &reqwest::header::HeaderMap::new(),
            "credential rejected",
        ));

    assert_eq!(
        chat_round_trip_validation_failure_kind(&protocol_error),
        ChatRoundTripValidationFailureKind::Internal,
    );
    assert_eq!(
        chat_round_trip_validation_failure_kind(&untyped_error),
        ChatRoundTripValidationFailureKind::Internal,
    );
    assert_eq!(
        chat_round_trip_validation_failure_kind(&non_authentication_status),
        ChatRoundTripValidationFailureKind::Internal,
    );
}

#[test]
fn env_provider_credential_bootstrap_compares_decrypted_values_not_randomized_envelopes() {
    let encoded_key = STANDARD.encode([61_u8; 32]);
    let cipher = CredentialCipher::from_base64(&encoded_key).expect("valid key");
    let account_id = Uuid::now_v7();
    let encrypted = cipher
        .encrypt(SecretPurpose::AiAccountApiKey, account_id, "current-key")
        .expect("encrypt configured key");

    assert!(
        !bootstrap_env_credential_needs_sync(
            &cipher,
            account_id,
            Some(encrypted.as_str()),
            "current-key",
        )
        .expect("matching encrypted credential should compare")
    );
    assert!(
        bootstrap_env_credential_needs_sync(
            &cipher,
            account_id,
            Some(encrypted.as_str()),
            "rotated-key",
        )
        .expect("rotated encrypted credential should compare")
    );
    assert!(
        bootstrap_env_credential_needs_sync(
            &cipher,
            account_id,
            Some("legacy-plaintext-value"),
            "current-key",
        )
        .is_err(),
        "a non-envelope stored credential must fail closed"
    );
    assert!(
        bootstrap_env_credential_needs_sync(&cipher, account_id, None, "current-key")
            .expect("missing credential requires sync")
    );
    assert!(
        bootstrap_env_credential_needs_sync(
            &cipher,
            Uuid::now_v7(),
            Some(encrypted.as_str()),
            "current-key",
        )
        .is_err(),
        "copying a v3 envelope to another account must fail closed"
    );
}

#[test]
fn a_listing_entry_keeps_only_what_the_provider_declared() {
    // A router publishes context length, output ceiling, modalities and rates
    // alongside the id. Reading them is what lets a discovered model carry a
    // real price instead of a zero nobody chose.
    let entry = serde_json::json!({
        "id": "vendor-x/model-9",
        "context_length": 262_144,
        "architecture": {"input_modalities": ["text", "image"]},
        "top_provider": {"max_completion_tokens": 8_192},
        "pricing": {"prompt": "0.0000004", "completion": "0.0000016"},
    });

    let model =
        discovered_model_from_listing_entry(&entry, Some("USD")).expect("entry carries an id");

    assert_eq!(model.model_name, "vendor-x/model-9");
    assert_eq!(model.context_window, Some(262_144));
    assert_eq!(model.max_output_tokens, Some(8_192));
    assert_eq!(model.input_modalities, vec!["text".to_owned(), "image".to_owned()]);
    let pricing = model.pricing.expect("both rates parse, so the record carries a price");
    // Providers quote per token; the catalog stores per million.
    assert_eq!(pricing.input_price_per_1m.to_string(), "0.4000000");
    assert_eq!(pricing.output_price_per_1m.to_string(), "1.6000000");
    assert_eq!(pricing.currency_code, "USD");
}

#[test]
fn a_bare_listing_entry_claims_nothing_it_was_not_told() {
    // OpenAI's own listing is `{"id": ...}` and nothing else. Absent must stay
    // absent: an inferred modality would block a capable model, and an assumed
    // zero price would report a paid model as free.
    let model =
        discovered_model_from_listing_entry(&serde_json::json!({"id": "model-a"}), Some("USD"))
            .expect("entry carries an id");

    assert_eq!(model.context_window, None);
    assert_eq!(model.max_output_tokens, None);
    assert!(model.input_modalities.is_empty());
    assert!(model.pricing.is_none());
}

#[test]
fn half_a_price_is_no_price() {
    // Recording a listed input rate with a defaulted zero output rate would
    // read as a discount on every answer the model writes.
    let model = discovered_model_from_listing_entry(
        &serde_json::json!({"id": "model-b", "pricing": {"prompt": "0.000001"}}),
        Some("USD"),
    )
    .expect("entry carries an id");

    assert!(model.pricing.is_none());
}

#[test]
fn an_entry_without_an_id_is_not_a_model() {
    assert!(
        discovered_model_from_listing_entry(&serde_json::json!({"name": "model-c"}), Some("USD"))
            .is_none()
    );
    assert!(
        discovered_model_from_listing_entry(&serde_json::json!({"id": "   "}), Some("USD"))
            .is_none()
    );
}

#[test]
fn a_price_with_no_declared_unit_is_not_recorded() {
    // The listing publishes a bare number. A rouble-priced router fills the
    // same field a dollar-priced one does, so without the provider declaring
    // the unit there is no way to record the rate — and recording it anyway
    // multiplied every reported cost by about a hundred.
    let entry = serde_json::json!({
        "id": "model-d",
        "pricing": {"prompt": "0.000001", "completion": "0.000004"},
    });

    assert!(discovered_model_from_listing_entry(&entry, None).expect("id").pricing.is_none());
    assert!(discovered_model_from_listing_entry(&entry, Some("  ")).expect("id").pricing.is_none());
    assert_eq!(
        discovered_model_from_listing_entry(&entry, Some("RUB"))
            .expect("id")
            .pricing
            .expect("a declared unit gives the rate a meaning")
            .currency_code,
        "RUB"
    );
}
