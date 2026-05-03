#![allow(clippy::cast_possible_wrap, clippy::missing_const_for_fn, clippy::struct_excessive_bools)]

use serde::Deserialize;

use crate::domains::{
    deployment::{
        ContentStorageProvider, DependencyKind, DependencyMode, DeploymentTopology, ServiceRole,
        StartupAuthorityMode,
    },
    recognition::{LibraryRecognitionPolicy, RecognitionEngine},
};

const DEFAULT_UI_BOOTSTRAP_ADMIN_EMAIL_DOMAIN: &str = "ironrag.local";
const DEFAULT_UI_BOOTSTRAP_ADMIN_NAME: &str = "Admin";
const BOOTSTRAP_PROVIDER_ENV_OPENAI: &str = "IRONRAG_OPENAI_API_KEY";
const BOOTSTRAP_PROVIDER_ENV_DEEPSEEK: &str = "IRONRAG_DEEPSEEK_API_KEY";
const BOOTSTRAP_PROVIDER_ENV_QWEN: &str = "IRONRAG_QWEN_API_KEY";
pub const DEFAULT_RUNTIME_DIAGNOSTIC_PAYLOAD_BUDGET_BYTES: usize = 32_768;
pub const DEFAULT_RUNTIME_POLICY_REASON_BUDGET_CHARS: usize = 2_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeHookBehavior {
    ObserveOnly,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiBootstrapAdmin {
    pub login: String,
    pub email: String,
    pub display_name: String,
    pub password: String,
    pub api_token: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiBootstrapAiSetup {
    pub provider_secrets: Vec<UiBootstrapAiProviderSecret>,
    pub binding_defaults: Vec<UiBootstrapAiBindingDefault>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiBootstrapAiProviderSecret {
    pub provider_kind: String,
    pub api_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiBootstrapAiBindingDefault {
    pub binding_purpose: String,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootstrapSettings {
    pub ui_bootstrap_admin: Option<UiBootstrapAdmin>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicOriginSettings {
    pub raw_frontend_origin: String,
    pub allowed_origins: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArangoSettings {
    pub url: String,
    pub database: String,
    pub username: String,
    pub password: String,
    pub request_timeout_seconds: u64,
    pub bootstrap_collections: bool,
    pub bootstrap_views: bool,
    pub bootstrap_graph: bool,
    pub bootstrap_vector_indexes: bool,
    pub vector_dimensions: u64,
    pub vector_index_n_lists: u64,
    pub vector_index_default_n_probe: u64,
    pub vector_index_training_iterations: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DestructiveFreshBootstrapSettings {
    pub required: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Settings {
    pub bind_addr: String,
    pub service_role: String,
    pub database_url: String,
    pub database_max_connections: u32,
    pub redis_url: String,
    pub arangodb_url: String,
    pub arangodb_database: String,
    pub arangodb_username: String,
    pub arangodb_password: String,
    pub arangodb_request_timeout_seconds: u64,
    pub arangodb_bootstrap_collections: bool,
    pub arangodb_bootstrap_views: bool,
    pub arangodb_bootstrap_graph: bool,
    pub arangodb_bootstrap_vector_indexes: bool,
    pub arangodb_vector_dimensions: u64,
    pub arangodb_vector_index_n_lists: u64,
    pub arangodb_vector_index_default_n_probe: u64,
    pub arangodb_vector_index_training_iterations: u64,
    pub service_name: String,
    pub environment: String,
    pub log_filter: String,
    pub destructive_fresh_bootstrap_required: bool,
    pub frontend_origin: String,
    /// When set, OpenAPI/Swagger uses this value as the only `servers` URL (API origin without a
    /// duplicate `/v1`; paths in the contract already start with `/v1/`). Env: `IRONRAG_OPENAPI_PUBLIC_ORIGIN`.
    pub openapi_public_origin: Option<String>,
    pub ui_session_secret: String,
    pub ui_default_locale: String,
    pub ui_bootstrap_admin_login: Option<String>,
    pub ui_bootstrap_admin_email: Option<String>,
    pub ui_bootstrap_admin_name: Option<String>,
    pub ui_bootstrap_admin_password: Option<String>,
    pub ui_bootstrap_admin_api_token: Option<String>,
    pub ui_bootstrap_extract_graph_provider_kind: Option<String>,
    pub ui_bootstrap_extract_graph_model_name: Option<String>,
    pub ui_bootstrap_embed_chunk_provider_kind: Option<String>,
    pub ui_bootstrap_embed_chunk_model_name: Option<String>,
    pub ui_bootstrap_query_compile_provider_kind: Option<String>,
    pub ui_bootstrap_query_compile_model_name: Option<String>,
    pub ui_bootstrap_query_answer_provider_kind: Option<String>,
    pub ui_bootstrap_query_answer_model_name: Option<String>,
    pub ui_bootstrap_vision_provider_kind: Option<String>,
    pub ui_bootstrap_vision_model_name: Option<String>,
    pub ui_session_ttl_hours: u64,
    pub upload_max_size_mb: u64,
    pub recognition_default_raster_image_engine: String,
    pub startup_authority_mode: String,
    pub dependency_postgres_mode: String,
    pub dependency_redis_mode: String,
    pub dependency_arangodb_mode: String,
    pub dependency_object_storage_mode: String,
    pub content_storage_provider: String,
    pub content_storage_topology: String,
    pub content_storage_key_prefix: String,
    pub content_storage_root: String,
    pub content_storage_s3_bucket: Option<String>,
    pub content_storage_s3_endpoint: Option<String>,
    pub content_storage_s3_region: Option<String>,
    pub content_storage_s3_access_key_id: Option<String>,
    pub content_storage_s3_secret_access_key: Option<String>,
    pub content_storage_s3_session_token: Option<String>,
    pub content_storage_s3_force_path_style: bool,
    pub ingestion_max_parallel_jobs_global: usize,
    pub ingestion_max_parallel_jobs_per_workspace: usize,
    pub ingestion_max_parallel_jobs_per_library: usize,
    /// Soft RSS cap (MiB) the dispatcher watches before claiming a new job.
    /// Set to `0` to auto-derive from the detected cgroup / host memory
    /// ceiling (90%) via `shared::telemetry::resolve_memory_soft_limit_mib`
    /// so any deployment size adapts without manual tuning. A positive
    /// value overrides auto-detection for operators who need a hard-coded
    /// floor. The static per-library parallelism limit is still the ceiling;
    /// this throttle only drops concurrency *below* it under memory
    /// pressure.
    pub ingestion_memory_soft_limit_mib: u64,
    pub ingestion_worker_lease_seconds: u64,
    pub ingestion_worker_heartbeat_interval_seconds: u64,
    /// Number of embedding batches sent in parallel within one job.
    /// Each batch contains EMBEDDING_BATCH_SIZE inputs. Higher values speed up
    /// long documents but may hit provider rate limits.
    pub ingestion_embedding_parallelism: usize,
    /// Max concurrent per-chunk graph-extract LLM calls *within* a single
    /// document. Decoupled from the cross-doc job limit so heavy docs get
    /// full chunk-level parallelism without raising the library cap.
    /// Keep this conservative: provider calls are remote, but prompt
    /// assembly, persistence and reconciliation still compete with worker
    /// heartbeats on CPU-only hosts.
    pub ingestion_graph_extract_parallelism_per_doc: usize,
    pub web_ingest_http_timeout_seconds: u64,
    pub web_ingest_max_redirects: usize,
    pub web_ingest_user_agent: String,
    /// Number of pages fetched in parallel during a web crawl run.
    pub web_ingest_crawl_concurrency: usize,
    pub llm_http_timeout_seconds: u64,
    pub llm_transport_retry_attempts: usize,
    pub llm_transport_retry_base_delay_ms: u64,
    pub runtime_agent_max_turns: u8,
    pub runtime_agent_max_parallel_actions: u8,
    pub runtime_trace_payload_budget_bytes: usize,
    pub runtime_policy_reason_budget_chars: usize,
    pub runtime_policy_reject_task_kinds: Option<String>,
    pub runtime_policy_reject_target_kinds: Option<String>,
    pub query_intent_cache_ttl_hours: u64,
    pub query_intent_cache_max_entries_per_library: usize,
    pub query_answer_source_links_enabled: bool,
    pub release_check_repository: String,
    pub release_check_interval_hours: u64,
    pub query_rerank_enabled: bool,
    pub query_rerank_candidate_limit: usize,
    pub query_balanced_context_enabled: bool,
    pub runtime_graph_extract_recovery_enabled: bool,
    pub runtime_graph_extract_recovery_max_attempts: usize,
    /// Hard wall-clock cap on a single `extract_graph` stage run
    /// (candidate materialization + revision graph reconcile combined).
    /// Guards against stages that silently stall the tokio runtime and
    /// starve the heartbeat loop: when exceeded the attempt finalizes
    /// with `stage_timeout` so the queue can move forward instead of
    /// waiting on a steady-state lease reaper.
    pub runtime_graph_extract_stage_timeout_seconds: u64,
    pub runtime_graph_extract_resume_downgrade_level_one_after_replays: usize,
    pub runtime_graph_extract_resume_downgrade_level_two_after_replays: usize,
    pub runtime_graph_summary_refresh_batch_size: usize,
    pub runtime_graph_targeted_reconciliation_enabled: bool,
    pub runtime_graph_targeted_reconciliation_max_targets: usize,
    pub runtime_document_activity_freshness_seconds: u64,
    pub runtime_document_stalled_after_seconds: u64,
    pub runtime_graph_filter_empty_relations: bool,
    pub runtime_graph_filter_degenerate_self_loops: bool,
    pub runtime_graph_convergence_warning_backlog_threshold: usize,
    pub mcp_memory_default_read_window_chars: usize,
    pub mcp_memory_max_read_window_chars: usize,
    pub mcp_memory_default_search_limit: usize,
    pub mcp_memory_max_search_limit: usize,
    pub mcp_memory_idempotency_retention_hours: u64,
    pub mcp_memory_audit_enabled: bool,
    pub chunking_max_chars: usize,
    pub chunking_overlap_chars: usize,
}

impl Settings {
    /// Loads application settings from canonical `IRONRAG_*` environment variables with defaults.
    ///
    /// # Errors
    /// Returns a [`config::ConfigError`] if configuration defaults cannot be built
    /// or environment values fail deserialization.
    pub fn from_env() -> Result<Self, config::ConfigError> {
        let cfg = settings_config_builder()?
            .add_source(config::Environment::with_prefix("IRONRAG").separator("__"))
            .add_source(
                config::Environment::with_prefix("IRONRAG").prefix_separator("_").separator("__"),
            )
            .build()?;

        let mut settings: Self = cfg.try_deserialize()?;
        settings.service_role = settings.service_role.trim().to_ascii_lowercase();
        settings.startup_authority_mode =
            settings.startup_authority_mode.trim().to_ascii_lowercase();
        settings.dependency_postgres_mode =
            settings.dependency_postgres_mode.trim().to_ascii_lowercase();
        settings.dependency_redis_mode = settings.dependency_redis_mode.trim().to_ascii_lowercase();
        settings.dependency_arangodb_mode =
            settings.dependency_arangodb_mode.trim().to_ascii_lowercase();
        settings.dependency_object_storage_mode =
            settings.dependency_object_storage_mode.trim().to_ascii_lowercase();
        settings.content_storage_provider =
            settings.content_storage_provider.trim().to_ascii_lowercase();
        settings.content_storage_topology =
            settings.content_storage_topology.trim().to_ascii_lowercase();
        settings.service_name = settings.service_name.trim().to_string();
        settings.release_check_repository = settings.release_check_repository.trim().to_string();
        validate_service_role(&settings).map_err(config::ConfigError::Message)?;
        validate_startup_authority_mode(&settings).map_err(config::ConfigError::Message)?;
        validate_dependency_modes(&settings).map_err(config::ConfigError::Message)?;
        validate_content_storage_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_service_name(&settings).map_err(config::ConfigError::Message)?;
        validate_arangodb_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_ingestion_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_recognition_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_runtime_agent_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_release_monitor_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_mcp_memory_settings(&settings).map_err(config::ConfigError::Message)?;

        Ok(settings)
    }

    #[must_use]
    pub const fn runtime_hook_behavior(&self) -> RuntimeHookBehavior {
        RuntimeHookBehavior::ObserveOnly
    }

    #[must_use]
    pub const fn runtime_maximum_diagnostic_payload_bytes(&self) -> usize {
        self.runtime_trace_payload_budget_bytes
    }

    #[must_use]
    pub fn bootstrap_settings(&self) -> BootstrapSettings {
        BootstrapSettings { ui_bootstrap_admin: self.resolved_ui_bootstrap_admin() }
    }

    #[must_use]
    pub fn public_origin_settings(&self) -> PublicOriginSettings {
        PublicOriginSettings {
            raw_frontend_origin: self.frontend_origin.clone(),
            allowed_origins: self
                .frontend_origin
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(std::string::ToString::to_string)
                .collect(),
        }
    }

    #[must_use]
    pub fn default_recognition_policy(&self) -> LibraryRecognitionPolicy {
        // validated at startup; parse failure here is a programming error.
        #[allow(clippy::expect_used)]
        let raster_image_engine = self
            .recognition_default_raster_image_engine
            .parse::<RecognitionEngine>()
            .expect("recognition_default_raster_image_engine must be validated before use");
        LibraryRecognitionPolicy { raster_image_engine }
    }

    #[must_use]
    pub fn arango_settings(&self) -> ArangoSettings {
        ArangoSettings {
            url: self.arangodb_url.clone(),
            database: self.arangodb_database.clone(),
            username: self.arangodb_username.clone(),
            password: self.arangodb_password.clone(),
            request_timeout_seconds: self.arangodb_request_timeout_seconds,
            bootstrap_collections: self.arangodb_bootstrap_collections,
            bootstrap_views: self.arangodb_bootstrap_views,
            bootstrap_graph: self.arangodb_bootstrap_graph,
            bootstrap_vector_indexes: self.arangodb_bootstrap_vector_indexes,
            vector_dimensions: self.arangodb_vector_dimensions,
            vector_index_n_lists: self.arangodb_vector_index_n_lists,
            vector_index_default_n_probe: self.arangodb_vector_index_default_n_probe,
            vector_index_training_iterations: self.arangodb_vector_index_training_iterations,
        }
    }

    #[must_use]
    pub fn destructive_fresh_bootstrap_settings(&self) -> DestructiveFreshBootstrapSettings {
        DestructiveFreshBootstrapSettings { required: self.destructive_fresh_bootstrap_required }
    }

    #[must_use]
    pub fn resolved_ui_bootstrap_admin(&self) -> Option<UiBootstrapAdmin> {
        let login = self
            .ui_bootstrap_admin_login
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_lowercase)?;
        let password = self
            .ui_bootstrap_admin_password
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(std::string::ToString::to_string)?;
        let email = self
            .ui_bootstrap_admin_email
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map_or_else(
                || format!("{login}@{DEFAULT_UI_BOOTSTRAP_ADMIN_EMAIL_DOMAIN}"),
                str::to_lowercase,
            );
        let display_name = self
            .ui_bootstrap_admin_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map_or_else(
                || DEFAULT_UI_BOOTSTRAP_ADMIN_NAME.to_string(),
                std::string::ToString::to_string,
            );
        let api_token = self
            .ui_bootstrap_admin_api_token
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(std::string::ToString::to_string);

        Some(UiBootstrapAdmin { login, email, display_name, password, api_token })
    }

    #[must_use]
    pub fn resolved_ui_bootstrap_ai_setup(&self) -> Option<UiBootstrapAiSetup> {
        let provider_secrets = [
            ("openai", resolved_bootstrap_provider_api_key(BOOTSTRAP_PROVIDER_ENV_OPENAI)),
            ("deepseek", resolved_bootstrap_provider_api_key(BOOTSTRAP_PROVIDER_ENV_DEEPSEEK)),
            ("qwen", resolved_bootstrap_provider_api_key(BOOTSTRAP_PROVIDER_ENV_QWEN)),
        ]
        .into_iter()
        .filter_map(|(provider_kind, api_key)| {
            api_key.map(|api_key| UiBootstrapAiProviderSecret {
                provider_kind: provider_kind.to_string(),
                api_key,
            })
        })
        .collect::<Vec<_>>();

        let binding_defaults = [
            resolved_ui_bootstrap_ai_binding_default(
                "extract_graph",
                self.ui_bootstrap_extract_graph_provider_kind.as_deref(),
                self.ui_bootstrap_extract_graph_model_name.as_deref(),
            ),
            resolved_ui_bootstrap_ai_binding_default(
                "embed_chunk",
                self.ui_bootstrap_embed_chunk_provider_kind.as_deref(),
                self.ui_bootstrap_embed_chunk_model_name.as_deref(),
            ),
            resolved_ui_bootstrap_ai_binding_default(
                "query_compile",
                self.ui_bootstrap_query_compile_provider_kind.as_deref(),
                self.ui_bootstrap_query_compile_model_name.as_deref(),
            ),
            resolved_ui_bootstrap_ai_binding_default(
                "query_answer",
                self.ui_bootstrap_query_answer_provider_kind.as_deref(),
                self.ui_bootstrap_query_answer_model_name.as_deref(),
            ),
            resolved_ui_bootstrap_ai_binding_default(
                "vision",
                self.ui_bootstrap_vision_provider_kind.as_deref(),
                self.ui_bootstrap_vision_model_name.as_deref(),
            ),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

        if provider_secrets.is_empty() && binding_defaults.is_empty() {
            None
        } else {
            Some(UiBootstrapAiSetup { provider_secrets, binding_defaults })
        }
    }

    #[must_use]
    pub fn has_explicit_ui_bootstrap_admin(&self) -> bool {
        self.resolved_ui_bootstrap_admin().is_some()
    }

    #[must_use]
    pub fn runs_http_api(&self) -> bool {
        self.service_role_kind().ok() == Some(ServiceRole::Api)
    }

    #[must_use]
    pub fn runs_probe_http_api(&self) -> bool {
        self.service_role_kind().ok() == Some(ServiceRole::Worker)
    }

    #[must_use]
    pub fn runs_ingestion_workers(&self) -> bool {
        self.service_role_kind().ok() == Some(ServiceRole::Worker)
    }

    #[must_use]
    pub fn runs_startup_authority(&self) -> bool {
        self.service_role_kind().ok() == Some(ServiceRole::Startup)
    }

    pub fn service_role_kind(&self) -> Result<ServiceRole, String> {
        self.service_role.parse()
    }

    pub fn startup_authority_mode_kind(&self) -> Result<StartupAuthorityMode, String> {
        self.startup_authority_mode.parse()
    }

    pub fn content_storage_provider_kind(&self) -> Result<ContentStorageProvider, String> {
        self.content_storage_provider.parse()
    }

    pub fn content_storage_topology_kind(&self) -> Result<DeploymentTopology, String> {
        self.content_storage_topology.parse()
    }

    pub fn dependency_mode(&self, kind: DependencyKind) -> Result<DependencyMode, String> {
        match kind {
            DependencyKind::Postgres => self.dependency_postgres_mode.parse(),
            DependencyKind::Redis => self.dependency_redis_mode.parse(),
            DependencyKind::ArangoDb => self.dependency_arangodb_mode.parse(),
            DependencyKind::ObjectStorage => self.dependency_object_storage_mode.parse(),
        }
    }
}

fn settings_config_builder()
-> Result<config::ConfigBuilder<config::builder::DefaultState>, config::ConfigError> {
    config::Config::builder()
        .set_default("bind_addr", "0.0.0.0:8080")?
        .set_default("service_role", "api")?
        .set_default("service_name", "ironrag-backend")?
        .set_default("environment", "local")?
        .set_default("database_url", "postgres://postgres:postgres@127.0.0.1:5432/ironrag")?
        .set_default("database_max_connections", 64)?
        .set_default("redis_url", "redis://127.0.0.1:6379")?
        .set_default("arangodb_url", "http://127.0.0.1:8529")?
        .set_default("arangodb_database", "ironrag")?
        .set_default("arangodb_username", "root")?
        .set_default("arangodb_password", "ironrag-dev")?
        // 15s was too tight on reference libraries: the two Arango
        // cursors called per turn (`aggregate_library_generation_signals`
        // and `list_documents_by_library`) each push past 15s under
        // ingest-concurrent load, surfacing as `error sending request
        // for url ... /_api/cursor`. 30s covers the observed tail
        // without masking genuine failures.
        .set_default("arangodb_request_timeout_seconds", 30)?
        .set_default("arangodb_bootstrap_collections", true)?
        .set_default("arangodb_bootstrap_views", true)?
        .set_default("arangodb_bootstrap_graph", true)?
        .set_default("arangodb_bootstrap_vector_indexes", true)?
        .set_default("arangodb_vector_dimensions", 3072)?
        .set_default("arangodb_vector_index_n_lists", 100)?
        .set_default("arangodb_vector_index_default_n_probe", 8)?
        .set_default("arangodb_vector_index_training_iterations", 25)?
        .set_default("log_filter", "info")?
        .set_default("destructive_fresh_bootstrap_required", false)?
        .set_default("frontend_origin", "http://127.0.0.1:19000,http://localhost:19000")?
        .set_default("ui_session_secret", "local-ui-session-secret")?
        .set_default("ui_default_locale", "ru")?
        .set_default("ui_session_ttl_hours", 720)?
        .set_default("upload_max_size_mb", 50)?
        .set_default("recognition_default_raster_image_engine", "docling")?
        .set_default("startup_authority_mode", "not_required")?
        .set_default("dependency_postgres_mode", "external")?
        .set_default("dependency_redis_mode", "external")?
        .set_default("dependency_arangodb_mode", "external")?
        .set_default("dependency_object_storage_mode", "disabled")?
        .set_default("content_storage_provider", "filesystem")?
        .set_default("content_storage_topology", "single_node")?
        .set_default("content_storage_key_prefix", "")?
        .set_default("content_storage_root", "/var/lib/ironrag/content-storage")?
        .set_default("content_storage_s3_region", "us-east-1")?
        .set_default("content_storage_s3_force_path_style", true)?
        .set_default("ingestion_max_parallel_jobs_global", 64)?
        .set_default("ingestion_max_parallel_jobs_per_workspace", 16)?
        .set_default("ingestion_max_parallel_jobs_per_library", 4)?
        .set_default("ingestion_memory_soft_limit_mib", 0)?
        .set_default("ingestion_worker_lease_seconds", 300)?
        .set_default("ingestion_worker_heartbeat_interval_seconds", 15)?
        .set_default("ingestion_embedding_parallelism", 2)?
        .set_default("ingestion_graph_extract_parallelism_per_doc", 2)?
        .set_default("web_ingest_http_timeout_seconds", 20)?
        .set_default("web_ingest_max_redirects", 10)?
        .set_default("web_ingest_user_agent", "IronRAG-WebIngest/0.1")?
        .set_default("web_ingest_crawl_concurrency", 4)?
        .set_default("llm_http_timeout_seconds", 120)?
        .set_default("llm_transport_retry_attempts", 5)?
        .set_default("llm_transport_retry_base_delay_ms", 500)?
        .set_default("runtime_agent_max_turns", 4)?
        .set_default("runtime_agent_max_parallel_actions", 4)?
        .set_default(
            "runtime_trace_payload_budget_bytes",
            DEFAULT_RUNTIME_DIAGNOSTIC_PAYLOAD_BUDGET_BYTES as i64,
        )?
        .set_default(
            "runtime_policy_reason_budget_chars",
            DEFAULT_RUNTIME_POLICY_REASON_BUDGET_CHARS as i64,
        )?
        .set_default("query_intent_cache_ttl_hours", 24)?
        .set_default("query_intent_cache_max_entries_per_library", 500)?
        .set_default("query_answer_source_links_enabled", false)?
        .set_default("release_check_repository", "mlimarenko/IronRAG")?
        .set_default("release_check_interval_hours", 12)?
        .set_default("query_rerank_enabled", true)?
        .set_default("query_rerank_candidate_limit", 24)?
        .set_default("query_balanced_context_enabled", true)?
        .set_default("runtime_graph_extract_recovery_enabled", true)?
        .set_default("runtime_graph_extract_recovery_max_attempts", 4)?
        .set_default("runtime_graph_extract_stage_timeout_seconds", 600)?
        .set_default("runtime_graph_extract_resume_downgrade_level_one_after_replays", 3)?
        .set_default("runtime_graph_extract_resume_downgrade_level_two_after_replays", 5)?
        .set_default("runtime_graph_summary_refresh_batch_size", 64)?
        .set_default("runtime_graph_targeted_reconciliation_enabled", true)?
        .set_default("runtime_graph_targeted_reconciliation_max_targets", 128)?
        // Activity freshness window must be wider than the worker's heartbeat
        // interval (`CANONICAL_HEARTBEAT_INTERVAL = 15s`) by a comfortable
        // margin, otherwise the UI flips to "stalled" every time a heartbeat
        // is briefly delayed by DB lock contention from many parallel
        // attempts hitting `touch_attempt_heartbeat`. 90s = 6× heartbeat,
        // matched to the dispatcher's `active_leases` freshness window so
        // all three thresholds (worker heartbeat, dispatcher count, UI
        // stalled flag) agree on the same definition of "this lease is
        // alive".
        .set_default("runtime_document_activity_freshness_seconds", 90)?
        .set_default("runtime_document_stalled_after_seconds", 240)?
        .set_default("runtime_graph_filter_empty_relations", true)?
        .set_default("runtime_graph_filter_degenerate_self_loops", true)?
        .set_default("runtime_graph_convergence_warning_backlog_threshold", 1)?
        .set_default("mcp_memory_default_read_window_chars", 48_000)?
        .set_default("mcp_memory_max_read_window_chars", 192_000)?
        .set_default("mcp_memory_default_search_limit", 10)?
        .set_default("mcp_memory_max_search_limit", 25)?
        .set_default("mcp_memory_idempotency_retention_hours", 72)?
        .set_default("mcp_memory_audit_enabled", true)?
        .set_default("chunking_max_chars", 2800)?
        .set_default("chunking_overlap_chars", 280)
}

fn validate_service_role(settings: &Settings) -> Result<(), String> {
    settings.service_role.parse::<ServiceRole>().map(|_| ())
}

fn validate_startup_authority_mode(settings: &Settings) -> Result<(), String> {
    settings.startup_authority_mode.parse::<StartupAuthorityMode>().map(|_| ())
}

fn validate_dependency_modes(settings: &Settings) -> Result<(), String> {
    for kind in [
        DependencyKind::Postgres,
        DependencyKind::Redis,
        DependencyKind::ArangoDb,
        DependencyKind::ObjectStorage,
    ] {
        let mode = settings.dependency_mode(kind)?;
        if matches!(
            kind,
            DependencyKind::Postgres | DependencyKind::Redis | DependencyKind::ArangoDb
        ) && matches!(mode, DependencyMode::Disabled)
        {
            return Err(format!("{} must not use disabled mode", kind.as_str()));
        }
    }
    Ok(())
}

fn validate_content_storage_settings(settings: &Settings) -> Result<(), String> {
    let provider = settings.content_storage_provider_kind()?;
    let topology = settings.content_storage_topology_kind()?;
    if settings.content_storage_key_prefix.trim().contains("..") {
        return Err("content_storage_key_prefix must not contain '..'".into());
    }

    match provider {
        ContentStorageProvider::Filesystem => {
            if settings.content_storage_root.trim().is_empty() {
                return Err("content_storage_root must not be empty".into());
            }
            if !matches!(topology, DeploymentTopology::SingleNode) {
                return Err(
                    "filesystem storage is supported only with content_storage_topology=single_node"
                        .into(),
                );
            }
            if !matches!(
                settings.dependency_mode(DependencyKind::ObjectStorage)?,
                DependencyMode::Disabled
            ) {
                return Err(
                    "dependency_object_storage_mode must be disabled when content_storage_provider=filesystem"
                        .into(),
                );
            }
        }
        ContentStorageProvider::S3 => {
            if matches!(
                settings.dependency_mode(DependencyKind::ObjectStorage)?,
                DependencyMode::Disabled
            ) {
                return Err(
                    "dependency_object_storage_mode must be bundled or external when content_storage_provider=s3"
                        .into(),
                );
            }
            for (field, value) in [
                ("content_storage_s3_bucket", settings.content_storage_s3_bucket.as_deref()),
                ("content_storage_s3_endpoint", settings.content_storage_s3_endpoint.as_deref()),
                (
                    "content_storage_s3_access_key_id",
                    settings.content_storage_s3_access_key_id.as_deref(),
                ),
                (
                    "content_storage_s3_secret_access_key",
                    settings.content_storage_s3_secret_access_key.as_deref(),
                ),
            ] {
                if value.map(str::trim).filter(|item| !item.is_empty()).is_none() {
                    return Err(format!(
                        "{field} must not be empty when content_storage_provider=s3"
                    ));
                }
            }
        }
    }

    Ok(())
}

fn validate_service_name(settings: &Settings) -> Result<(), String> {
    let value = settings.service_name.as_str();
    if value.is_empty() {
        return Err("service_name must not be empty".into());
    }
    if value
        .bytes()
        .any(|byte| !matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'_' | b'-'))
    {
        return Err("service_name must contain only ASCII letters, digits, '.', '_' or '-'".into());
    }
    Ok(())
}

fn resolved_bootstrap_provider_api_key(env_name: &str) -> Option<String> {
    std::env::var(env_name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn resolved_ui_bootstrap_ai_binding_default(
    binding_purpose: &str,
    provider_kind: Option<&str>,
    model_name: Option<&str>,
) -> Option<UiBootstrapAiBindingDefault> {
    let provider_kind =
        provider_kind.map(str::trim).filter(|value| !value.is_empty()).map(str::to_ascii_lowercase);
    let model_name = model_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(std::string::ToString::to_string);
    if provider_kind.is_none() && model_name.is_none() {
        return None;
    }
    Some(UiBootstrapAiBindingDefault {
        binding_purpose: binding_purpose.to_string(),
        provider_kind,
        model_name,
    })
}

fn validate_arangodb_settings(settings: &Settings) -> Result<(), String> {
    if settings.arangodb_url.trim().is_empty() {
        return Err("arangodb_url must not be empty".into());
    }
    if settings.arangodb_database.trim().is_empty() {
        return Err("arangodb_database must not be empty".into());
    }
    if settings.arangodb_username.trim().is_empty() {
        return Err("arangodb_username must not be empty".into());
    }
    if settings.arangodb_request_timeout_seconds == 0 {
        return Err("arangodb_request_timeout_seconds must be greater than zero".into());
    }
    if settings.arangodb_vector_dimensions == 0 {
        return Err("arangodb_vector_dimensions must be greater than zero".into());
    }
    if settings.arangodb_vector_index_n_lists == 0 {
        return Err("arangodb_vector_index_n_lists must be greater than zero".into());
    }
    if settings.arangodb_vector_index_default_n_probe == 0 {
        return Err("arangodb_vector_index_default_n_probe must be greater than zero".into());
    }
    if settings.arangodb_vector_index_training_iterations == 0 {
        return Err("arangodb_vector_index_training_iterations must be greater than zero".into());
    }
    Ok(())
}

fn validate_ingestion_settings(settings: &Settings) -> Result<(), String> {
    if settings.ingestion_max_parallel_jobs_global == 0 {
        return Err("ingestion_max_parallel_jobs_global must be greater than zero".into());
    }
    if settings.ingestion_max_parallel_jobs_per_workspace == 0 {
        return Err("ingestion_max_parallel_jobs_per_workspace must be greater than zero".into());
    }
    if settings.ingestion_max_parallel_jobs_per_library == 0 {
        return Err("ingestion_max_parallel_jobs_per_library must be greater than zero".into());
    }
    if settings.ingestion_max_parallel_jobs_per_workspace
        > settings.ingestion_max_parallel_jobs_global
    {
        return Err(
            "ingestion_max_parallel_jobs_per_workspace must be less than or equal to ingestion_max_parallel_jobs_global"
                .into(),
        );
    }
    if settings.ingestion_max_parallel_jobs_per_library
        > settings.ingestion_max_parallel_jobs_per_workspace
    {
        return Err(
            "ingestion_max_parallel_jobs_per_library must be less than or equal to ingestion_max_parallel_jobs_per_workspace"
                .into(),
        );
    }
    Ok(())
}

fn validate_recognition_settings(settings: &Settings) -> Result<(), String> {
    let engine = settings
        .recognition_default_raster_image_engine
        .parse::<RecognitionEngine>()
        .map_err(|error| format!("recognition_default_raster_image_engine: {error}"))?;
    let policy = LibraryRecognitionPolicy { raster_image_engine: engine };
    policy.validate().map_err(|error| format!("recognition_default_raster_image_engine: {error}"))
}

fn validate_runtime_agent_settings(settings: &Settings) -> Result<(), String> {
    if settings.runtime_agent_max_turns == 0 {
        return Err("runtime_agent_max_turns must be greater than zero".into());
    }
    if settings.runtime_agent_max_parallel_actions == 0 {
        return Err("runtime_agent_max_parallel_actions must be greater than zero".into());
    }
    if settings.runtime_trace_payload_budget_bytes == 0 {
        return Err("runtime_trace_payload_budget_bytes must be greater than zero".into());
    }
    if settings.runtime_policy_reason_budget_chars == 0 {
        return Err("runtime_policy_reason_budget_chars must be greater than zero".into());
    }
    for task_kind in parse_runtime_policy_csv(settings.runtime_policy_reject_task_kinds.as_ref()) {
        task_kind
            .parse::<crate::domains::agent_runtime::RuntimeTaskKind>()
            .map_err(|error| format!("runtime_policy_reject_task_kinds contains {error}"))?;
    }
    for target_kind in
        parse_runtime_policy_csv(settings.runtime_policy_reject_target_kinds.as_ref())
    {
        target_kind
            .parse::<crate::domains::agent_runtime::RuntimeDecisionTargetKind>()
            .map_err(|error| format!("runtime_policy_reject_target_kinds contains {error}"))?;
    }
    Ok(())
}

fn validate_release_monitor_settings(settings: &Settings) -> Result<(), String> {
    let repository = settings.release_check_repository.trim();
    let mut components = repository.split('/');
    let owner = components.next().unwrap_or_default();
    let repo = components.next().unwrap_or_default();
    let has_exactly_two_components = components.next().is_none();
    let is_valid_component = |value: &str| {
        !value.is_empty()
            && value.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
            })
    };

    if !(has_exactly_two_components && is_valid_component(owner) && is_valid_component(repo)) {
        return Err(
            "release_check_repository must be a GitHub repository slug like owner/repo".into()
        );
    }
    if settings.release_check_interval_hours == 0 {
        return Err("release_check_interval_hours must be greater than zero".into());
    }

    Ok(())
}

fn parse_runtime_policy_csv(value: Option<&String>) -> Vec<&str> {
    value
        .map(std::string::String::as_str)
        .map(|raw| {
            raw.split(',').map(str::trim).filter(|item| !item.is_empty()).collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn validate_mcp_memory_settings(settings: &Settings) -> Result<(), String> {
    if settings.mcp_memory_default_read_window_chars == 0 {
        return Err("mcp_memory_default_read_window_chars must be greater than zero".into());
    }
    if settings.mcp_memory_max_read_window_chars == 0 {
        return Err("mcp_memory_max_read_window_chars must be greater than zero".into());
    }
    if settings.mcp_memory_default_read_window_chars > settings.mcp_memory_max_read_window_chars {
        return Err(
            "mcp_memory_default_read_window_chars must be less than or equal to mcp_memory_max_read_window_chars"
                .into(),
        );
    }
    if settings.mcp_memory_default_search_limit == 0 {
        return Err("mcp_memory_default_search_limit must be greater than zero".into());
    }
    if settings.mcp_memory_max_search_limit == 0 {
        return Err("mcp_memory_max_search_limit must be greater than zero".into());
    }
    if settings.mcp_memory_default_search_limit > settings.mcp_memory_max_search_limit {
        return Err(
            "mcp_memory_default_search_limit must be less than or equal to mcp_memory_max_search_limit"
                .into(),
        );
    }
    if settings.mcp_memory_idempotency_retention_hours == 0 {
        return Err("mcp_memory_idempotency_retention_hours must be greater than zero".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
