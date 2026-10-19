use super::*;

#[test]
fn resolved_ui_bootstrap_admin_is_absent_without_explicit_credentials() {
    let settings = sample_settings();

    assert_eq!(settings.resolved_ui_bootstrap_admin(), None);
    assert!(!settings.has_explicit_ui_bootstrap_admin());
}

#[test]
fn resolved_ui_bootstrap_admin_uses_configured_credentials() {
    let mut settings = sample_settings();
    settings.ui_bootstrap_admin_login = Some(" root ".into());
    settings.ui_bootstrap_admin_email = Some(" admin@example.com ".into());
    settings.ui_bootstrap_admin_name = Some(" Platform Owner ".into());
    settings.ui_bootstrap_admin_password = Some(" secret ".into());
    settings.ui_bootstrap_admin_api_token = Some(" inrg_0123456789abcdef0123456789abcdef ".into());

    assert_eq!(
        settings.resolved_ui_bootstrap_admin(),
        Some(UiBootstrapAdmin {
            login: "root".into(),
            email: "admin@example.com".into(),
            display_name: "Platform Owner".into(),
            password: "secret".into(),
            api_token: Some("inrg_0123456789abcdef0123456789abcdef".into()),
        })
    );
    assert!(settings.has_explicit_ui_bootstrap_admin());
}

#[test]
fn rejects_malformed_bootstrap_admin_api_token() {
    let mut settings = sample_settings();
    settings.ui_bootstrap_admin_login = Some("owner".into());
    settings.ui_bootstrap_admin_email = Some("owner@example.com".into());
    settings.ui_bootstrap_admin_password = Some("secret".into());
    settings.ui_bootstrap_admin_api_token = Some("legacy-token".into());

    assert_eq!(
        validate_ui_bootstrap_admin_settings(&settings),
        Err("ui_bootstrap_admin_api_token must use the inrg_ token format".into())
    );
}

#[test]
fn accepts_well_formed_bootstrap_admin_api_token() {
    let mut settings = sample_settings();
    settings.ui_bootstrap_admin_login = Some("owner".into());
    settings.ui_bootstrap_admin_email = Some("owner@example.com".into());
    settings.ui_bootstrap_admin_password = Some("secret".into());
    settings.ui_bootstrap_admin_api_token = Some("inrg_0123456789abcdef0123456789abcdef".into());

    assert_eq!(validate_ui_bootstrap_admin_settings(&settings), Ok(()));
}

#[test]
fn rejects_bootstrap_admin_without_email() {
    let mut settings = sample_settings();
    settings.ui_bootstrap_admin_login = Some("owner".into());
    settings.ui_bootstrap_admin_password = Some("secret".into());

    assert_eq!(
        validate_ui_bootstrap_admin_settings(&settings),
        Err("ui_bootstrap_admin_login, ui_bootstrap_admin_email, and ui_bootstrap_admin_password must all be set or all unset; ui_bootstrap_admin_email is missing".into())
    );
    assert_eq!(settings.resolved_ui_bootstrap_admin(), None);
}

#[test]
fn resolved_ui_bootstrap_ai_is_absent_without_provider_credentials() {
    let settings = sample_settings();

    assert_eq!(settings.resolved_ui_bootstrap_ai_setup(), None);
}

#[test]
fn removed_provider_secret_env_convention_is_detected_structurally() {
    assert!(is_removed_provider_api_key_env_name("IRONRAG_PROVIDER_7_API_KEY"));
    assert!(!is_removed_provider_api_key_env_name("IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64"));
    assert!(!is_removed_provider_api_key_env_name("IRONRAG_API_KEY"));
    assert!(!is_removed_provider_api_key_env_name("IRONRAG_provider_API_KEY"));
}

#[test]
fn bootstrap_provider_secrets_preserve_arbitrary_provider_kinds_without_collisions() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let encoded_map = STANDARD.encode(
        r#"{
            "zeta-gateway": "  zeta-secret  ",
            "alpha.7": "alpha-secret",
            "zeta_gateway": "underscore-secret",
            "Zeta/Gateway": "value$#\"\\юникод",
            "whitespace-only": "   ",
            "empty": ""
        }"#,
    );
    let secrets = parse_bootstrap_provider_api_keys_b64(&encoded_map)
        .expect("formal provider-key environment entries should parse");

    assert_eq!(
        secrets,
        vec![
            UiBootstrapAiProviderSecret {
                provider_kind: "Zeta/Gateway".into(),
                api_key: "value$#\"\\юникод".into(),
            },
            UiBootstrapAiProviderSecret {
                provider_kind: "alpha.7".into(),
                api_key: "alpha-secret".into(),
            },
            UiBootstrapAiProviderSecret {
                provider_kind: "whitespace-only".into(),
                api_key: "   ".into(),
            },
            UiBootstrapAiProviderSecret {
                provider_kind: "zeta-gateway".into(),
                api_key: "  zeta-secret  ".into(),
            },
            UiBootstrapAiProviderSecret {
                provider_kind: "zeta_gateway".into(),
                api_key: "underscore-secret".into(),
            },
        ]
    );
}

#[test]
fn bootstrap_provider_secret_json_rejects_invalid_kind_without_leaking_value() {
    let secret_value = "must-not-appear-in-diagnostics";
    let error =
        parse_bootstrap_provider_api_keys_json(&format!(r#"{{"invalid kind":"{secret_value}"}}"#))
            .expect_err("non-active provider suffix must fail closed");

    assert!(error.contains("decoded JSON is invalid"));
    assert!(!error.contains(secret_value));
}

#[test]
fn bootstrap_provider_secret_json_rejects_duplicate_kind_without_leaking_values() {
    let error = parse_bootstrap_provider_api_keys_json(
        r#"{"arbitrary-provider":"first-secret","arbitrary-provider":"second-secret"}"#,
    )
    .expect_err("conflicting duplicate provider credentials must fail closed");

    assert!(error.contains("decoded JSON is invalid"));
    assert!(!error.contains("first-secret"));
    assert!(!error.contains("second-secret"));
}

#[test]
fn empty_bootstrap_provider_secret_json_is_absent_without_alias_defaults() {
    let secrets = parse_bootstrap_provider_api_keys_b64("")
        .expect("an empty generic provider map should be accepted");

    assert!(secrets.is_empty());
}

#[test]
fn bootstrap_provider_secret_base64_rejects_surrounding_whitespace() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let encoded_map = STANDARD.encode(r#"{"arbitrary-provider":"secret"}"#);
    let error = parse_bootstrap_provider_api_keys_b64(&format!(" {encoded_map}"))
        .expect_err("strict base64 must not be silently trimmed");

    assert!(error.contains("IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64"));
    assert!(!error.contains(&encoded_map));
}

#[test]
fn bootstrap_provider_secret_payload_limits_fail_closed_without_leaking_values() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let oversized_key = "x".repeat(MAX_BOOTSTRAP_PROVIDER_API_KEY_BYTES + 1);
    let oversized_key_json = serde_json::json!({ "arbitrary-provider": oversized_key }).to_string();
    let key_error = parse_bootstrap_provider_api_keys_json(&oversized_key_json)
        .expect_err("oversized credentials must fail closed");
    assert_eq!(key_error, "IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64 decoded JSON is invalid");

    let too_many_entries = format!(
        "{{{}}}",
        (0..=MAX_BOOTSTRAP_PROVIDER_SECRETS)
            .map(|index| format!(r#""provider-{index}":"value""#))
            .collect::<Vec<_>>()
            .join(",")
    );
    let entry_error = parse_bootstrap_provider_api_keys_json(&too_many_entries)
        .expect_err("too many provider credentials must fail closed");
    assert_eq!(entry_error, "IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64 decoded JSON is invalid");

    let oversized_document = vec![b'x'; MAX_BOOTSTRAP_PROVIDER_MAP_JSON_BYTES + 1];
    let encoded_document = STANDARD.encode(oversized_document);
    let document_error = parse_bootstrap_provider_api_keys_b64(&encoded_document)
        .expect_err("oversized decoded documents must fail before JSON parsing");
    assert!(document_error.contains("exceeds the configured size limit"));
    assert!(!document_error.contains(&encoded_document));
}

#[test]
fn bootstrap_provider_secret_base64_errors_never_include_encoded_value() {
    let encoded_value = "not-valid-base64$#";
    let error = parse_bootstrap_provider_api_keys_b64(encoded_value)
        .expect_err("malformed base64 must fail closed");

    assert!(error.contains("IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64"));
    assert!(!error.contains(encoded_value));
}

#[test]
fn resolved_ui_bootstrap_ai_exposes_binding_defaults_without_provider_credentials() {
    let mut settings = sample_settings();
    settings.ui_bootstrap_extract_text_provider_kind = Some(" provider-alpha ".into());
    settings.ui_bootstrap_extract_text_model_name = Some(" alpha-multimodal ".into());
    settings.ui_bootstrap_extract_graph_provider_kind = Some(" provider-alpha ".into());
    settings.ui_bootstrap_extract_graph_model_name = Some(" alpha-chat-small ".into());
    settings.ui_bootstrap_embed_chunk_provider_kind = Some(" provider-beta ".into());
    settings.ui_bootstrap_embed_chunk_model_name = Some(" beta-embedding-large ".into());
    settings.ui_bootstrap_query_compile_provider_kind = Some(" provider-alpha ".into());
    settings.ui_bootstrap_query_compile_model_name = Some(" alpha-chat-plus ".into());
    settings.ui_bootstrap_query_answer_provider_kind = Some(" provider-alpha ".into());
    settings.ui_bootstrap_query_answer_model_name = Some(" alpha-chat-large ".into());
    settings.ui_bootstrap_agent_provider_kind = Some(" provider-gamma ".into());
    settings.ui_bootstrap_agent_model_name = Some(" gamma-tools ".into());

    assert_eq!(
        settings.resolved_ui_bootstrap_ai_setup(),
        Some(UiBootstrapAiSetup {
            provider_secrets: vec![],
            binding_defaults: vec![
                UiBootstrapAiBindingDefault {
                    binding_purpose: AiBindingPurpose::ExtractText,
                    provider_kind: Some("provider-alpha".into()),
                    model_name: Some("alpha-multimodal".into()),
                },
                UiBootstrapAiBindingDefault {
                    binding_purpose: AiBindingPurpose::ExtractGraph,
                    provider_kind: Some("provider-alpha".into()),
                    model_name: Some("alpha-chat-small".into()),
                },
                UiBootstrapAiBindingDefault {
                    binding_purpose: AiBindingPurpose::EmbedChunk,
                    provider_kind: Some("provider-beta".into()),
                    model_name: Some("beta-embedding-large".into()),
                },
                UiBootstrapAiBindingDefault {
                    binding_purpose: AiBindingPurpose::QueryCompile,
                    provider_kind: Some("provider-alpha".into()),
                    model_name: Some("alpha-chat-plus".into()),
                },
                UiBootstrapAiBindingDefault {
                    binding_purpose: AiBindingPurpose::QueryAnswer,
                    provider_kind: Some("provider-alpha".into()),
                    model_name: Some("alpha-chat-large".into()),
                },
                UiBootstrapAiBindingDefault {
                    binding_purpose: AiBindingPurpose::Agent,
                    provider_kind: Some("provider-gamma".into()),
                    model_name: Some("gamma-tools".into()),
                },
            ],
        }),
    );
}

#[test]
fn bootstrap_settings_expose_configured_boundary() {
    let settings = sample_settings();
    let bootstrap = settings.bootstrap_settings();

    assert_eq!(bootstrap.ui_bootstrap_admin, None);
}

#[test]
fn bootstrap_settings_resolve_explicit_admin_credentials() {
    let mut settings = sample_settings();
    settings.ui_bootstrap_admin_login = Some(" root ".into());
    settings.ui_bootstrap_admin_email = Some(" root@example.com ".into());
    settings.ui_bootstrap_admin_password = Some(" secret ".into());

    let bootstrap = settings.bootstrap_settings();

    assert_eq!(
        bootstrap.ui_bootstrap_admin,
        Some(UiBootstrapAdmin {
            login: "root".into(),
            email: "root@example.com".into(),
            display_name: "Admin".into(),
            password: "secret".into(),
            api_token: None,
        })
    );
}

#[test]
fn public_origin_settings_split_and_trim_allowed_origins() {
    let mut settings = sample_settings();
    settings.frontend_origin = " https://app.example.com , http://localhost:19000 ".into();

    let origins = settings.public_origin_settings();

    assert_eq!(origins.raw_frontend_origin, " https://app.example.com , http://localhost:19000 ");
    assert_eq!(
        origins.allowed_origins,
        vec!["https://app.example.com".to_string(), "http://localhost:19000".to_string()]
    );
    assert!(origins.session_cookie_secure);
}

#[test]
fn public_origin_settings_leave_local_http_session_cookies_non_secure() {
    let settings = sample_settings();

    let origins = settings.public_origin_settings();

    assert!(!origins.session_cookie_secure);
}

#[test]
fn mcp_http_origin_policy_accepts_frontend_and_api_public_origins() {
    let mut settings = sample_settings();
    settings.frontend_origin = " https://console.example , http://localhost:19000 ".into();
    settings.openapi_public_origin = Some("https://api.example".into());

    let policy = settings.mcp_http_origin_policy().expect("synthetic origin policy must be valid");

    assert!(policy.allows("https://console.example"));
    assert!(policy.allows("http://localhost:19000"));
    assert!(policy.allows("https://api.example"));
    assert!(!policy.allows("https://untrusted.example"));
    assert!(!policy.allows("https://console.example/"));
}

#[test]
fn mcp_http_origin_policy_rejects_malformed_configuration() {
    for invalid in [
        "not-an-origin",
        "https://console.example/path",
        "https://user@console.example",
        "https://console.example/",
        "ftp://console.example",
    ] {
        let mut settings = sample_settings();
        settings.frontend_origin = invalid.into();

        assert!(settings.mcp_http_origin_policy().is_err(), "accepted {invalid}");
    }

    let mut settings = sample_settings();
    settings.openapi_public_origin = Some("https://api.example/v1".into());
    assert!(settings.mcp_http_origin_policy().is_err());
}

#[test]
fn destructive_fresh_bootstrap_settings_default_to_disabled() {
    let settings = sample_settings();
    let destructive = settings.destructive_fresh_bootstrap_settings();

    assert!(!destructive.required);
}

#[test]
fn oidc_configuration_requires_all_core_values_and_redacts_secret() {
    let mut settings = sample_settings();
    settings.oidc_issuer_url = Some("https://auth.example.test/realms/main".into());

    let error = validate_oidc_settings(&settings).expect_err("partial OIDC must fail closed");

    assert!(error.contains("IRONRAG_OIDC_CLIENT_SECRET"));

    settings.oidc_client_id = Some("ironrag".into());
    settings.oidc_client_secret = Some("oidc-secret-regression".into());
    settings.oidc_redirect_url = Some("https://graph.example.test/v1/iam/oidc/callback".into());
    settings.oidc_required_group = Some("ironrag".into());
    settings.frontend_origin = "https://graph.example.test".into();

    let oidc = settings
        .resolved_oidc_settings()
        .expect("complete OIDC must resolve")
        .expect("complete OIDC is enabled");
    let debug = format!("{oidc:?}");
    assert!(!debug.contains("oidc-secret-regression"));
    assert_eq!(oidc.required_group, "ironrag");
}

#[test]
fn blank_oidc_environment_values_disable_sso_but_mixed_values_fail_closed() {
    let mut settings = sample_settings();
    settings.oidc_issuer_url = Some(" ".into());
    settings.oidc_client_id = Some("".into());
    settings.oidc_client_secret = Some("\t".into());
    settings.oidc_redirect_url = Some("".into());
    settings.oidc_required_group = Some("\n".into());
    settings.oidc_bootstrap_admin_subject = Some(" ".into());

    assert_eq!(settings.resolved_oidc_settings(), Ok(None));
    validate_oidc_settings(&settings).expect("all blank OIDC values must mean disabled");

    settings.oidc_issuer_url = Some("https://auth.example.test/realms/main".into());
    let error = validate_oidc_settings(&settings).expect_err("mixed blank OIDC values must fail");
    assert!(error.contains("IRONRAG_OIDC_CLIENT_SECRET"));

    settings.oidc_issuer_url = None;
    settings.oidc_bootstrap_admin_subject = Some("configured-subject".into());
    assert!(validate_oidc_settings(&settings).is_err());
}

#[test]
fn oidc_redirect_must_be_same_origin_exact_callback_and_secure_with_secure_sessions() {
    let mut settings = sample_settings();
    settings.frontend_origin = "https://graph.example.test".into();
    settings.oidc_issuer_url = Some("https://auth.example.test/realms/main".into());
    settings.oidc_client_id = Some("ironrag".into());
    settings.oidc_client_secret = Some("synthetic-client-secret".into());
    settings.oidc_required_group = Some("ironrag".into());

    for redirect in [
        "https://attacker.example.test/v1/iam/oidc/callback",
        "https://graph.example.test/v1/iam/oidc/callback/",
        "http://graph.example.test/v1/iam/oidc/callback",
        "https://graph.example.test/v1/iam/oidc/callback?state=bad",
    ] {
        settings.oidc_redirect_url = Some(redirect.into());
        assert!(validate_oidc_settings(&settings).is_err(), "accepted {redirect}");
    }

    settings.oidc_redirect_url = Some("https://graph.example.test/v1/iam/oidc/callback".into());
    validate_oidc_settings(&settings).expect("same-origin exact callback must validate");
}

#[test]
fn oidc_bootstrap_subject_requires_enabled_oidc_and_is_redacted_in_debug() {
    let mut settings = sample_settings();
    settings.oidc_bootstrap_admin_subject = Some("subject-owner".into());
    assert!(validate_oidc_settings(&settings).is_err());

    settings.oidc_issuer_url = Some("https://auth.example.test/realms/main".into());
    settings.oidc_client_id = Some("ironrag".into());
    settings.oidc_client_secret = Some("synthetic-client-secret".into());
    settings.oidc_redirect_url = Some("http://127.0.0.1:19000/v1/iam/oidc/callback".into());
    settings.oidc_required_group = Some("ironrag".into());
    settings.frontend_origin = "http://127.0.0.1:19000".into();

    let oidc = settings
        .resolved_oidc_settings()
        .expect("OIDC settings must resolve")
        .expect("OIDC must be present");
    assert_eq!(oidc.bootstrap_admin_subject.as_deref(), Some("subject-owner"));
    let debug = format!("{oidc:?}");
    assert!(!debug.contains("subject-owner"));
    assert!(!debug.contains("synthetic-client-secret"));
}
