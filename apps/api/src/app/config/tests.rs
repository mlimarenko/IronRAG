use super::*;
use config::Map;

fn sample_settings() -> Settings {
    Settings {
        bind_addr: "0.0.0.0:8080".into(),
        service_role: "api".into(),
        database_url: "postgres://postgres:postgres@127.0.0.1:5432/ironrag".into(),
        database_max_connections: 20,
        redis_url: "redis://127.0.0.1:6379".into(),
        arangodb_url: "http://127.0.0.1:8529".into(),
        arangodb_database: "ironrag".into(),
        arangodb_username: "root".into(),
        arangodb_password: "ironrag-dev".into(),
        arangodb_request_timeout_seconds: 15,
        arangodb_bootstrap_collections: true,
        arangodb_bootstrap_views: true,
        arangodb_bootstrap_graph: true,
        arangodb_bootstrap_vector_indexes: true,
        arangodb_vector_dimensions: 3072,
        arangodb_vector_index_n_lists: 100,
        arangodb_vector_index_default_n_probe: 8,
        arangodb_vector_index_training_iterations: 25,
        service_name: "ironrag-backend".into(),
        environment: "local".into(),
        log_filter: "info".into(),
        destructive_fresh_bootstrap_required: false,
        frontend_origin: "http://127.0.0.1:19000,http://localhost:19000".into(),
        openapi_public_origin: None,
        ui_session_secret: "local-ui-session-secret".into(),
        ui_default_locale: "ru".into(),
        ui_bootstrap_admin_login: None,
        ui_bootstrap_admin_email: None,
        ui_bootstrap_admin_name: None,
        ui_bootstrap_admin_password: None,
        ui_bootstrap_admin_api_token: None,
        ui_bootstrap_extract_graph_provider_kind: None,
        ui_bootstrap_extract_graph_model_name: None,
        ui_bootstrap_embed_chunk_provider_kind: None,
        ui_bootstrap_embed_chunk_model_name: None,
        ui_bootstrap_query_answer_provider_kind: None,
        ui_bootstrap_query_answer_model_name: None,
        ui_bootstrap_vision_provider_kind: None,
        ui_bootstrap_vision_model_name: None,
        ui_session_ttl_hours: 720,
        upload_max_size_mb: 50,
        startup_authority_mode: "not_required".into(),
        dependency_postgres_mode: "external".into(),
        dependency_redis_mode: "external".into(),
        dependency_arangodb_mode: "external".into(),
        dependency_object_storage_mode: "disabled".into(),
        content_storage_provider: "filesystem".into(),
        content_storage_topology: "single_node".into(),
        content_storage_key_prefix: "".into(),
        content_storage_root: "/var/lib/ironrag/content-storage".into(),
        content_storage_s3_bucket: None,
        content_storage_s3_endpoint: None,
        content_storage_s3_region: Some("us-east-1".into()),
        content_storage_s3_access_key_id: None,
        content_storage_s3_secret_access_key: None,
        content_storage_s3_session_token: None,
        content_storage_s3_force_path_style: true,
        web_ingest_http_timeout_seconds: 20,
        web_ingest_max_redirects: 10,
        web_ingest_user_agent: "IronRAG-WebIngest/0.1".into(),
        web_ingest_crawl_concurrency: 4,
        ingestion_worker_concurrency: 4,
        ingestion_worker_lease_seconds: 300,
        ingestion_worker_heartbeat_interval_seconds: 15,
        ingestion_max_jobs_per_library: 0,
        ingestion_embedding_parallelism: 4,
        llm_http_timeout_seconds: 120,
        llm_transport_retry_attempts: 3,
        llm_transport_retry_base_delay_ms: 250,
        runtime_agent_max_turns: 4,
        runtime_agent_max_parallel_actions: 4,
        runtime_trace_payload_budget_bytes: DEFAULT_RUNTIME_DIAGNOSTIC_PAYLOAD_BUDGET_BYTES,
        runtime_policy_reason_budget_chars: DEFAULT_RUNTIME_POLICY_REASON_BUDGET_CHARS,
        runtime_policy_reject_task_kinds: None,
        runtime_policy_reject_target_kinds: None,
        query_intent_cache_ttl_hours: 24,
        query_intent_cache_max_entries_per_library: 500,
        query_answer_source_links_enabled: false,
        release_check_repository: "mlimarenko/IronRAG".into(),
        release_check_interval_hours: 12,
        query_rerank_enabled: true,
        query_rerank_candidate_limit: 24,
        query_balanced_context_enabled: true,
        runtime_graph_extract_recovery_enabled: true,
        runtime_graph_extract_recovery_max_attempts: 2,
        runtime_graph_extract_resume_downgrade_level_one_after_replays: 3,
        runtime_graph_extract_resume_downgrade_level_two_after_replays: 5,
        runtime_graph_summary_refresh_batch_size: 64,
        runtime_graph_targeted_reconciliation_enabled: true,
        runtime_graph_targeted_reconciliation_max_targets: 128,
        runtime_document_activity_freshness_seconds: 45,
        runtime_document_stalled_after_seconds: 180,
        runtime_graph_filter_empty_relations: true,
        runtime_graph_filter_degenerate_self_loops: true,
        runtime_graph_convergence_warning_backlog_threshold: 1,
        mcp_memory_default_read_window_chars: 12_000,
        mcp_memory_max_read_window_chars: 50_000,
        mcp_memory_default_search_limit: 10,
        mcp_memory_max_search_limit: 25,
        mcp_memory_idempotency_retention_hours: 72,
        mcp_memory_audit_enabled: true,
        chunking_max_chars: 2800,
        chunking_overlap_chars: 280,
    }
}

fn settings_from_env_entries(entries: &[(&str, &str)]) -> Settings {
    let mut env = Map::new();
    for (key, value) in entries {
        env.insert((*key).to_string(), (*value).to_string());
    }
    let cfg = settings_config_builder()
        .expect("defaults should build")
        .add_source(
            config::Environment::with_prefix("IRONRAG")
                .prefix_separator("_")
                .separator("__")
                .source(Some(env)),
        )
        .build()
        .expect("config should build");
    let mut settings: Settings = cfg.try_deserialize().expect("settings should deserialize");
    settings.service_role = settings.service_role.trim().to_ascii_lowercase();
    validate_service_role(&settings).expect("role should validate");
    validate_service_name(&settings).expect("service name should validate");
    validate_arangodb_settings(&settings).expect("arangodb settings should validate");
    validate_runtime_agent_settings(&settings).expect("runtime settings should validate");
    validate_release_monitor_settings(&settings).expect("release monitor settings should validate");
    validate_mcp_memory_settings(&settings).expect("mcp settings should validate");
    settings
}

#[test]
fn from_env_has_sane_local_defaults() {
    let settings = Settings::from_env().expect("settings should load with defaults");

    assert_eq!(settings.bind_addr, "0.0.0.0:8080");
    assert_eq!(settings.service_role, "api");
    assert_eq!(settings.service_name, "ironrag-backend");
    assert_eq!(settings.environment, "local");
    assert_eq!(settings.database_max_connections, 20);
    assert_eq!(settings.redis_url, "redis://127.0.0.1:6379");
    assert_eq!(settings.arangodb_url, "http://127.0.0.1:8529");
    assert_eq!(settings.arangodb_database, "ironrag");
    assert_eq!(settings.log_filter, "info");
    assert_eq!(settings.ingestion_worker_concurrency, 4);
    assert_eq!(settings.runtime_agent_max_turns, 4);
    assert_eq!(settings.release_check_repository, "mlimarenko/IronRAG");
    assert_eq!(settings.release_check_interval_hours, 12);
    assert_eq!(settings.runtime_agent_max_parallel_actions, 4);
    assert_eq!(
        settings.runtime_trace_payload_budget_bytes,
        DEFAULT_RUNTIME_DIAGNOSTIC_PAYLOAD_BUDGET_BYTES
    );
    assert_eq!(
        settings.runtime_policy_reason_budget_chars,
        DEFAULT_RUNTIME_POLICY_REASON_BUDGET_CHARS
    );
    assert_eq!(settings.query_intent_cache_ttl_hours, 24);
    assert!(settings.query_rerank_enabled);
    assert!(settings.runtime_graph_extract_recovery_enabled);
    assert_eq!(settings.content_storage_root, "/var/lib/ironrag/content-storage");
    assert_eq!(settings.runtime_document_activity_freshness_seconds, 45);
    assert_eq!(settings.runtime_document_stalled_after_seconds, 180);
    assert!(settings.runtime_graph_filter_empty_relations);
    assert!(settings.runtime_graph_filter_degenerate_self_loops);
    assert_eq!(settings.runtime_graph_convergence_warning_backlog_threshold, 1);
    assert_eq!(settings.mcp_memory_default_read_window_chars, 12_000);
    assert_eq!(settings.mcp_memory_max_read_window_chars, 50_000);
    assert_eq!(settings.mcp_memory_default_search_limit, 10);
    assert_eq!(settings.mcp_memory_max_search_limit, 25);
    assert_eq!(settings.mcp_memory_idempotency_retention_hours, 72);
    assert!(settings.mcp_memory_audit_enabled);
}

#[test]
fn from_env_provides_default_database_url() {
    let settings = Settings::from_env().expect("settings should load with defaults");

    assert_eq!(settings.database_url, "postgres://postgres:postgres@127.0.0.1:5432/ironrag");
}

#[test]
fn canonical_prefixed_flat_variables_override_defaults() {
    let settings = settings_from_env_entries(&[
        ("IRONRAG_DATABASE_URL", "postgres://postgres:postgres@postgres:5432/ironrag"),
        ("IRONRAG_SERVICE_ROLE", "API"),
        ("IRONRAG_LOG_FILTER", "debug"),
    ]);

    assert_eq!(settings.database_url, "postgres://postgres:postgres@postgres:5432/ironrag");
    assert_eq!(settings.service_role, "api");
    assert_eq!(settings.log_filter, "debug");
}

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
    settings.ui_bootstrap_admin_api_token = Some(" bootstrap-token ".into());

    assert_eq!(
        settings.resolved_ui_bootstrap_admin(),
        Some(UiBootstrapAdmin {
            login: "root".into(),
            email: "admin@example.com".into(),
            display_name: "Platform Owner".into(),
            password: "secret".into(),
            api_token: Some("bootstrap-token".into()),
        })
    );
    assert!(settings.has_explicit_ui_bootstrap_admin());
}

#[test]
fn resolved_ui_bootstrap_admin_derives_email_when_missing() {
    let mut settings = sample_settings();
    settings.ui_bootstrap_admin_login = Some(" owner ".into());
    settings.ui_bootstrap_admin_password = Some(" secret ".into());

    assert_eq!(
        settings.resolved_ui_bootstrap_admin(),
        Some(UiBootstrapAdmin {
            login: "owner".into(),
            email: "owner@ironrag.local".into(),
            display_name: "Admin".into(),
            password: "secret".into(),
            api_token: None,
        })
    );
}

#[test]
fn resolved_ui_bootstrap_ai_is_absent_without_provider_credentials() {
    let settings = sample_settings();

    assert_eq!(settings.resolved_ui_bootstrap_ai_setup(), None);
}

#[test]
fn resolved_ui_bootstrap_ai_exposes_binding_defaults_without_provider_credentials() {
    let mut settings = sample_settings();
    settings.ui_bootstrap_extract_graph_provider_kind = Some(" deepseek ".into());
    settings.ui_bootstrap_extract_graph_model_name = Some(" deepseek-chat ".into());
    settings.ui_bootstrap_embed_chunk_provider_kind = Some(" openai ".into());
    settings.ui_bootstrap_embed_chunk_model_name = Some(" text-embedding-3-large ".into());
    settings.ui_bootstrap_query_answer_provider_kind = Some(" openai ".into());
    settings.ui_bootstrap_query_answer_model_name = Some(" gpt-5.4 ".into());
    settings.ui_bootstrap_vision_provider_kind = Some(" openai ".into());
    settings.ui_bootstrap_vision_model_name = Some(" gpt-5.4-mini ".into());

    assert_eq!(
        settings.resolved_ui_bootstrap_ai_setup(),
        Some(UiBootstrapAiSetup {
            provider_secrets: vec![],
            binding_defaults: vec![
                UiBootstrapAiBindingDefault {
                    binding_purpose: "extract_graph".into(),
                    provider_kind: Some("deepseek".into()),
                    model_name: Some("deepseek-chat".into()),
                },
                UiBootstrapAiBindingDefault {
                    binding_purpose: "embed_chunk".into(),
                    provider_kind: Some("openai".into()),
                    model_name: Some("text-embedding-3-large".into()),
                },
                UiBootstrapAiBindingDefault {
                    binding_purpose: "query_answer".into(),
                    provider_kind: Some("openai".into()),
                    model_name: Some("gpt-5.4".into()),
                },
                UiBootstrapAiBindingDefault {
                    binding_purpose: "vision".into(),
                    provider_kind: Some("openai".into()),
                    model_name: Some("gpt-5.4-mini".into()),
                },
            ],
        }),
    );
}

#[test]
fn bootstrap_settings_expose_canonical_boundary() {
    let settings = sample_settings();
    let bootstrap = settings.bootstrap_settings();

    assert_eq!(bootstrap.ui_bootstrap_admin, None);
}

#[test]
fn bootstrap_settings_resolve_explicit_admin_credentials() {
    let mut settings = sample_settings();
    settings.ui_bootstrap_admin_login = Some(" root ".into());
    settings.ui_bootstrap_admin_password = Some(" secret ".into());

    let bootstrap = settings.bootstrap_settings();

    assert_eq!(
        bootstrap.ui_bootstrap_admin,
        Some(UiBootstrapAdmin {
            login: "root".into(),
            email: "root@ironrag.local".into(),
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
}

#[test]
fn arango_settings_expose_bootstrap_toggles() {
    let settings = sample_settings();
    let arango = settings.arango_settings();

    assert_eq!(arango.url, "http://127.0.0.1:8529");
    assert_eq!(arango.database, "ironrag");
    assert!(arango.bootstrap_collections);
    assert!(arango.bootstrap_views);
    assert!(arango.bootstrap_graph);
    assert!(arango.bootstrap_vector_indexes);
    assert_eq!(arango.vector_dimensions, 3072);
}

#[test]
fn destructive_fresh_bootstrap_settings_default_to_disabled() {
    let settings = sample_settings();
    let destructive = settings.destructive_fresh_bootstrap_settings();

    assert!(!destructive.required);
}

#[test]
fn rejects_invalid_mcp_memory_ranges() {
    let mut settings = sample_settings();
    settings.mcp_memory_default_read_window_chars = 10_000;
    settings.mcp_memory_max_read_window_chars = 100;

    let error = validate_mcp_memory_settings(&settings).expect_err("range should fail");
    assert!(error.contains("mcp_memory_default_read_window_chars"));
}

#[test]
fn rejects_invalid_runtime_agent_limits() {
    let mut settings = sample_settings();
    settings.runtime_agent_max_turns = 0;

    let error =
        validate_runtime_agent_settings(&settings).expect_err("runtime settings should fail");
    assert!(error.contains("runtime_agent_max_turns"));
}

#[test]
fn service_role_helpers_match_role() {
    let mut settings = sample_settings();

    settings.service_role = "api".into();
    assert!(settings.runs_http_api());
    assert!(!settings.runs_probe_http_api());
    assert!(!settings.runs_ingestion_workers());
    assert!(!settings.runs_startup_authority());

    settings.service_role = "worker".into();
    assert!(!settings.runs_http_api());
    assert!(settings.runs_probe_http_api());
    assert!(settings.runs_ingestion_workers());
    assert!(!settings.runs_startup_authority());

    settings.service_role = "startup".into();
    assert!(!settings.runs_http_api());
    assert!(!settings.runs_probe_http_api());
    assert!(!settings.runs_ingestion_workers());
    assert!(settings.runs_startup_authority());
}

#[test]
fn rejects_invalid_service_roles() {
    let mut settings = sample_settings();
    settings.service_role = "scheduler".into();

    let error = validate_service_role(&settings).expect_err("invalid role should fail");
    assert!(error.contains("service_role"));
}

#[test]
fn rejects_filesystem_cluster_topology() {
    let mut settings = sample_settings();
    settings.content_storage_topology = "shared_cluster".into();

    let error = validate_content_storage_settings(&settings).expect_err("shared cluster must fail");
    assert!(error.contains("content_storage_topology"));
}

#[test]
fn rejects_s3_provider_without_credentials() {
    let mut settings = sample_settings();
    settings.content_storage_provider = "s3".into();
    settings.dependency_object_storage_mode = "bundled".into();

    let error = validate_content_storage_settings(&settings).expect_err("s3 settings must fail");
    assert!(error.contains("content_storage_s3_bucket"));
}

#[test]
fn accepts_service_names_with_identity_safe_characters() {
    let mut settings = sample_settings();
    settings.service_name = "ironrag.worker_01-api".into();

    validate_service_name(&settings).expect("valid service name should pass");
}

#[test]
fn rejects_invalid_service_names() {
    let mut settings = sample_settings();
    settings.service_name = "worker:api".into();

    let error = validate_service_name(&settings).expect_err("invalid service name should fail");
    assert!(error.contains("service_name"));
}

#[test]
fn rejects_invalid_release_check_repository_slug() {
    let mut settings = sample_settings();
    settings.release_check_repository = "https://github.com/mlimarenko/IronRAG".into();

    let error = validate_release_monitor_settings(&settings)
        .expect_err("full urls should fail release repository validation");
    assert!(error.contains("release_check_repository"));
}

#[test]
fn rejects_zero_release_check_interval() {
    let mut settings = sample_settings();
    settings.release_check_interval_hours = 0;

    let error = validate_release_monitor_settings(&settings)
        .expect_err("zero interval should fail release monitor validation");
    assert!(error.contains("release_check_interval_hours"));
}
