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
const BOOTSTRAP_PROVIDER_SECRET_ENVS: &[(&str, &str)] = &[
    ("openai", "IRONRAG_OPENAI_API_KEY"),
    ("deepseek", "IRONRAG_DEEPSEEK_API_KEY"),
    ("qwen", "IRONRAG_QWEN_API_KEY"),
    ("openrouter", "IRONRAG_OPENROUTER_API_KEY"),
    ("gptunnel", "IRONRAG_GPTUNNEL_API_KEY"),
    ("routerai", "IRONRAG_ROUTERAI_API_KEY"),
    ("minimax", "IRONRAG_MINIMAX_API_KEY"),
];
pub const DEFAULT_RUNTIME_DIAGNOSTIC_PAYLOAD_BUDGET_BYTES: usize = 32_768;
pub const DEFAULT_RUNTIME_POLICY_REASON_BUDGET_CHARS: usize = 2_000;
const MIN_DATABASE_CONNECTIONS_PER_RUNTIME_REPLICA: u32 = 4;

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
    pub session_cookie_secure: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DestructiveFreshBootstrapSettings {
    pub required: bool,
}

#[derive(Clone, Debug, Deserialize, utoipa::ToSchema)]
pub struct Settings {
    pub bind_addr: String,
    pub service_role: String,
    pub database_url: String,
    pub database_max_connections: u32,
    pub api_replicas: usize,
    pub worker_replicas: usize,
    pub knowledge_plane_backend: String,
    pub redis_url: String,
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
    pub ui_bootstrap_query_retrieve_provider_kind: Option<String>,
    pub ui_bootstrap_query_retrieve_model_name: Option<String>,
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
    /// Maintenance scheduler kill switch. When `false` the worker role
    /// boots without the recurring sweeper loop — operators can still
    /// invoke maintenance via the `ironrag-maintenance` CLI. Defaults
    /// to `true` so the worker container picks up garbage automatically
    /// once a deployment is healthy enough to run it.
    pub maintenance_enabled: bool,
    /// Wall-clock interval between scheduler ticks. Each tick reaps
    /// stale leases and picks at most one due (class, scope) pair per
    /// configured class.
    pub maintenance_tick_interval_seconds: u64,
    /// Default cadence between successful runs of the same (class,
    /// scope) pair. The sweeper's row is re-queued with `next_due_at =
    /// now() + class_interval` on completion.
    pub maintenance_class_interval_seconds: u64,
    /// A leased row whose heartbeat is older than this is returned to
    /// `pending` so a healthy scheduler can pick it up again.
    pub maintenance_stale_lease_seconds: u64,
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
    pub runtime_agent_max_turns: u8,
    pub runtime_agent_max_parallel_actions: u8,
    pub runtime_trace_payload_budget_bytes: usize,
    pub runtime_policy_reason_budget_chars: usize,
    pub runtime_policy_reject_task_kinds: Option<String>,
    pub runtime_policy_reject_target_kinds: Option<String>,
    pub query_intent_cache_ttl_hours: u64,
    pub query_intent_cache_max_entries_per_library: usize,
    pub release_check_repository: String,
    pub release_check_interval_hours: u64,
    pub graph_gc_hours: u64,
    pub query_rerank_enabled: bool,
    pub query_rerank_candidate_limit: usize,
    pub query_balanced_context_enabled: bool,
    pub runtime_graph_extract_recovery_enabled: bool,
    pub runtime_graph_extract_recovery_max_attempts: usize,
    /// Idle cap for graph candidate materialization. The stage may run for a
    /// long time on large documents, but it must keep completing per-chunk
    /// graph extraction checkpoints within this window.
    pub runtime_graph_extract_idle_timeout_seconds: u64,
    /// Wall-clock cap for the final revision graph reconcile step. Candidate
    /// materialization is guarded by the idle timeout above instead.
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
    /// API-role runtime graph prewarm is opt-in because large libraries can
    /// allocate hundreds of MiB each before the first user query. Keep disabled
    /// on constrained hosts; lazy per-library loading still works on demand.
    pub runtime_graph_projection_prewarm_enabled: bool,
    /// Maximum number of active libraries to prewarm when prewarm is enabled.
    /// `0` means no library-count cap.
    pub runtime_graph_projection_prewarm_max_libraries: usize,
    pub mcp_memory_default_read_window_chars: usize,
    pub mcp_memory_max_read_window_chars: usize,
    pub mcp_memory_default_search_limit: usize,
    pub mcp_memory_max_search_limit: usize,
    pub mcp_memory_idempotency_retention_hours: u64,
    pub mcp_memory_audit_enabled: bool,
    pub chunking_max_chars: usize,
    pub chunking_overlap_chars: usize,
    /// Maximum concurrent outbound calls to a single provider endpoint
    /// (keyed by `provider_kind` + base URL), shared across the ingest and
    /// query lanes. `0` (the default) means unlimited, which is a full
    /// no-op: no semaphore is created and no call ever waits. Set a positive
    /// cap to protect a self-hosted or rate-limited provider from being
    /// overwhelmed by concurrent ingest fan-out plus query turns.
    pub provider_concurrency_max_outbound: usize,
    /// Permits reserved exclusively for the query lane out of
    /// `provider_concurrency_max_outbound`. Guarantees latency-sensitive
    /// query turns at least this many concurrent permits even under a fully
    /// saturating ingest load. Clamped to the max; ignored when the max is
    /// `0` (unlimited).
    pub provider_concurrency_query_reserved: usize,
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
        settings.knowledge_plane_backend =
            settings.knowledge_plane_backend.trim().to_ascii_lowercase();
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
        validate_database_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_knowledge_plane_backend(&settings).map_err(config::ConfigError::Message)?;
        validate_content_storage_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_service_name(&settings).map_err(config::ConfigError::Message)?;
        validate_ingestion_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_recognition_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_runtime_agent_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_release_monitor_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_graph_gc_settings(&settings).map_err(config::ConfigError::Message)?;
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
        let allowed_origins = self
            .frontend_origin
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>();
        PublicOriginSettings {
            raw_frontend_origin: self.frontend_origin.clone(),
            session_cookie_secure: allowed_origins.iter().any(|origin| {
                origin.get(..8).is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
            }),
            allowed_origins,
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
        let provider_secrets = BOOTSTRAP_PROVIDER_SECRET_ENVS
            .iter()
            .map(|(provider_kind, env_name)| {
                (*provider_kind, resolved_bootstrap_provider_api_key(env_name))
            })
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
                "query_retrieve",
                self.ui_bootstrap_query_retrieve_provider_kind.as_deref(),
                self.ui_bootstrap_query_retrieve_model_name.as_deref(),
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
            // The in-product agent turn orchestrates grounded answers with the
            // same chat model as `query_answer` (MCP-UI parity: the UI agent and
            // external MCP clients share one answer tool surface). Default the
            // `agent` binding to the query_answer provider/model so a clean stack
            // resolves an active agent binding without extra operator config;
            // operators can still override per-workspace/library via the UI.
            resolved_ui_bootstrap_ai_binding_default(
                "agent",
                self.ui_bootstrap_query_answer_provider_kind.as_deref(),
                self.ui_bootstrap_query_answer_model_name.as_deref(),
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

    /// `true` when this process should host the maintenance scheduler:
    /// it runs in the worker role AND the kill switch is set.
    #[must_use]
    pub fn runs_maintenance_scheduler(&self) -> bool {
        self.maintenance_enabled
            && self.service_role_kind().ok().is_some_and(ServiceRole::runs_maintenance_scheduler)
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
        .set_default("database_max_connections", 20)?
        .set_default("api_replicas", 1)?
        .set_default("worker_replicas", 1)?
        .set_default("knowledge_plane_backend", "postgres")?
        .set_default("redis_url", "redis://127.0.0.1:6379")?
        .set_default("log_filter", "info")?
        .set_default("destructive_fresh_bootstrap_required", false)?
        .set_default("frontend_origin", "http://127.0.0.1:19000,http://localhost:19000")?
        .set_default("ui_session_secret", "local-ui-session-secret")?
        .set_default("ui_default_locale", "ru")?
        .set_default("ui_session_ttl_hours", 720)?
        .set_default("upload_max_size_mb", 50)?
        .set_default("recognition_default_raster_image_engine", "vision")?
        .set_default("startup_authority_mode", "not_required")?
        .set_default("dependency_postgres_mode", "external")?
        .set_default("dependency_redis_mode", "external")?
        .set_default("dependency_object_storage_mode", "disabled")?
        .set_default("content_storage_provider", "filesystem")?
        .set_default("content_storage_topology", "single_node")?
        .set_default("content_storage_key_prefix", "")?
        .set_default("content_storage_root", "/var/lib/ironrag/content-storage")?
        .set_default("content_storage_s3_region", "us-east-1")?
        .set_default("content_storage_s3_force_path_style", true)?
        .set_default("ingestion_max_parallel_jobs_global", 16)?
        .set_default("ingestion_max_parallel_jobs_per_workspace", 8)?
        .set_default("ingestion_max_parallel_jobs_per_library", 4)?
        .set_default("ingestion_memory_soft_limit_mib", 0)?
        .set_default("ingestion_worker_lease_seconds", 300)?
        .set_default("ingestion_worker_heartbeat_interval_seconds", 15)?
        .set_default("maintenance_enabled", true)?
        .set_default("maintenance_tick_interval_seconds", 30)?
        .set_default("maintenance_class_interval_seconds", 3600)?
        .set_default("maintenance_stale_lease_seconds", 300)?
        .set_default("ingestion_embedding_parallelism", 8)?
        .set_default("ingestion_graph_extract_parallelism_per_doc", 16)?
        .set_default("web_ingest_http_timeout_seconds", 20)?
        .set_default("web_ingest_max_redirects", 10)?
        .set_default("web_ingest_user_agent", "IronRAG-WebIngest/0.1")?
        .set_default("web_ingest_crawl_concurrency", 4)?
        .set_default("llm_http_timeout_seconds", 120)?
        .set_default("provider_concurrency_max_outbound", 0)?
        .set_default("provider_concurrency_query_reserved", 0)?
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
        .set_default("release_check_repository", "mlimarenko/IronRAG")?
        .set_default("release_check_interval_hours", 12)?
        .set_default("graph_gc_hours", 24)?
        .set_default("query_rerank_enabled", true)?
        .set_default("query_rerank_candidate_limit", 24)?
        .set_default("query_balanced_context_enabled", true)?
        .set_default("runtime_graph_extract_recovery_enabled", true)?
        .set_default("runtime_graph_extract_recovery_max_attempts", 4)?
        .set_default("runtime_graph_extract_idle_timeout_seconds", 300)?
        .set_default("runtime_graph_extract_stage_timeout_seconds", 1800)?
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
        .set_default("runtime_graph_projection_prewarm_enabled", false)?
        .set_default("runtime_graph_projection_prewarm_max_libraries", 0)?
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
    for kind in [DependencyKind::Postgres, DependencyKind::Redis, DependencyKind::ObjectStorage] {
        let mode = settings.dependency_mode(kind)?;
        if matches!(kind, DependencyKind::Postgres | DependencyKind::Redis)
            && matches!(mode, DependencyMode::Disabled)
        {
            return Err(format!("{} must not use disabled mode", kind.as_str()));
        }
    }
    Ok(())
}

fn validate_database_settings(settings: &Settings) -> Result<(), String> {
    let runtime_replicas = settings.api_replicas.saturating_add(settings.worker_replicas);
    if settings.runs_http_api() && settings.api_replicas == 0 {
        return Err("api_replicas must be at least 1 when service_role=api".into());
    }
    if settings.runs_ingestion_workers() && settings.worker_replicas == 0 {
        return Err("worker_replicas must be at least 1 when service_role=worker".into());
    }
    if runtime_replicas == 0 {
        return Err("api_replicas and worker_replicas must not both be zero".into());
    }

    let runtime_replicas = u32::try_from(runtime_replicas).unwrap_or(u32::MAX);
    let minimum_budget =
        MIN_DATABASE_CONNECTIONS_PER_RUNTIME_REPLICA.saturating_mul(runtime_replicas);
    if settings.database_max_connections < minimum_budget {
        return Err(format!(
            "database_max_connections must be at least {minimum_budget} for api_replicas={} and worker_replicas={}",
            settings.api_replicas, settings.worker_replicas
        ));
    }
    Ok(())
}

fn validate_knowledge_plane_backend(settings: &Settings) -> Result<(), String> {
    match settings.knowledge_plane_backend.as_str() {
        "postgres" => Ok(()),
        _ => Err("knowledge_plane_backend must be postgres".into()),
    }
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

fn validate_graph_gc_settings(settings: &Settings) -> Result<(), String> {
    if settings.graph_gc_hours == 0 {
        return Err("graph_gc_hours must be greater than zero".into());
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
