use super::*;
use url::Url;

use super::bootstrap::{
    read_bootstrap_provider_api_keys_from_env, resolved_ui_bootstrap_ai_binding_default,
};
use super::validation::{
    validate_content_storage_settings, validate_credential_master_key, validate_database_settings,
    validate_dependency_modes, validate_graph_gc_settings, validate_ingestion_settings,
    validate_knowledge_plane_backend, validate_login_rate_limit_settings,
    validate_mcp_memory_settings, validate_oidc_settings, validate_provider_resilience_settings,
    validate_query_rerank_settings, validate_query_turn_admission_settings,
    validate_recognition_settings, validate_release_monitor_settings,
    validate_runtime_agent_settings, validate_service_name, validate_service_role,
    validate_startup_authority_mode, validate_ui_bootstrap_admin_settings,
};

impl Settings {
    /// Loads application settings from supported `IRONRAG_*` environment variables with defaults.
    ///
    /// # Errors
    /// Returns a [`config::ConfigError`] if configuration defaults cannot be built
    /// or environment values fail deserialization.
    pub fn from_env() -> Result<Self, config::ConfigError> {
        let cfg =
            settings_config_builder()?.add_source(ironrag_environment_source(None)).build()?;

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
        // Validate the dynamic secret namespace at the startup boundary. The
        // returned values are immediately dropped (and zeroized); operations
        // that need them read the environment again for the shortest possible
        // plaintext lifetime.
        read_bootstrap_provider_api_keys_from_env().map_err(config::ConfigError::Message)?;
        if settings.credential_master_key.as_deref() == Some("") {
            settings.credential_master_key = None;
        }
        if settings.credential_master_key_id.as_deref() == Some("") {
            settings.credential_master_key_id = None;
        }
        if settings.credential_previous_master_keys.as_deref() == Some("") {
            settings.credential_previous_master_keys = None;
        }
        validate_service_role(&settings).map_err(config::ConfigError::Message)?;
        validate_startup_authority_mode(&settings).map_err(config::ConfigError::Message)?;
        validate_dependency_modes(&settings).map_err(config::ConfigError::Message)?;
        validate_database_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_knowledge_plane_backend(&settings).map_err(config::ConfigError::Message)?;
        validate_content_storage_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_service_name(&settings).map_err(config::ConfigError::Message)?;
        settings.mcp_http_origin_policy().map_err(config::ConfigError::Message)?;
        validate_ingestion_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_provider_resilience_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_query_turn_admission_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_login_rate_limit_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_recognition_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_runtime_agent_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_query_rerank_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_release_monitor_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_graph_gc_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_mcp_memory_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_ui_bootstrap_admin_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_oidc_settings(&settings).map_err(config::ConfigError::Message)?;
        if let Err(message) = validate_credential_master_key(&settings) {
            settings.discard_credential_master_key();
            return Err(config::ConfigError::Message(message));
        }

        Ok(settings)
    }

    /// Removes and zeroizes raw credential key configuration when a process
    /// only needs non-secret settings (for example, database-only admin
    /// commands).
    pub fn discard_credential_master_key(&mut self) {
        if let Some(mut encoded_master_key) = self.credential_master_key.take() {
            encoded_master_key.zeroize();
        }
        if let Some(mut encoded_previous_keys) = self.credential_previous_master_keys.take() {
            encoded_previous_keys.zeroize();
        }
        self.credential_master_key_id = None;
    }

    /// Removes plaintext bootstrap administrator credentials after the
    /// startup-only owner has been prepared.
    ///
    /// Login, email and display name are not credentials and remain available
    /// for diagnostics. Password and API-token buffers are zeroized before the
    /// retained settings value can be cloned into application state.
    pub fn discard_ui_bootstrap_admin_secrets(&mut self) {
        if let Some(mut password) = self.ui_bootstrap_admin_password.take() {
            password.zeroize();
        }
        if let Some(mut api_token) = self.ui_bootstrap_admin_api_token.take() {
            api_token.zeroize();
        }
    }

    /// Resolves OIDC settings only after every core field has been supplied
    /// and validated. A deployment has no partial/degraded OIDC mode: a
    /// partial configuration would otherwise advertise a sign-in path that
    /// cannot prove identity safely.
    pub fn resolved_oidc_settings(&self) -> Result<Option<OidcSettings>, String> {
        // Compose and ConfigMap systems commonly materialize an unset optional
        // variable as an empty string. Treat every blank OIDC field as absent
        // before testing the all-or-none invariant, otherwise a stock
        // non-SSO deployment would fail as a misleading partial configuration.
        // Nonblank secrets remain byte-for-byte opaque below.
        let core_values = [
            nonblank_oidc_value(self.oidc_issuer_url.as_deref()),
            nonblank_oidc_value(self.oidc_client_id.as_deref()),
            nonblank_oidc_value(self.oidc_client_secret.as_deref()),
            nonblank_oidc_value(self.oidc_redirect_url.as_deref()),
            nonblank_oidc_value(self.oidc_required_group.as_deref()),
        ];
        let any_core_value_is_present = core_values.iter().any(|value| value.is_some());
        if !any_core_value_is_present {
            if nonblank_oidc_value(self.oidc_bootstrap_admin_subject.as_deref()).is_some() {
                return Err(
                    "IRONRAG_OIDC_BOOTSTRAP_ADMIN_SUBJECT requires complete OIDC configuration"
                        .to_string(),
                );
            }
            return Ok(None);
        }

        let issuer_url = required_oidc_value(
            core_values[0],
            "IRONRAG_OIDC_ISSUER_URL",
            MAX_OIDC_ISSUER_URL_BYTES,
        )?;
        let client_id = required_oidc_value(
            core_values[1],
            "IRONRAG_OIDC_CLIENT_ID",
            MAX_OIDC_CLIENT_ID_BYTES,
        )?;
        // A client secret is opaque material: do not trim, normalize, or put
        // it in validation errors. This also preserves a legitimate leading
        // or trailing byte if an IdP issued one.
        let client_secret =
            core_values[2].map(std::string::ToString::to_string).ok_or_else(|| {
                "IRONRAG_OIDC_ISSUER_URL, IRONRAG_OIDC_CLIENT_ID, IRONRAG_OIDC_CLIENT_SECRET, \
IRONRAG_OIDC_REDIRECT_URL and IRONRAG_OIDC_REQUIRED_GROUP must be configured together"
                    .to_string()
            })?;
        let redirect_url = required_oidc_value(
            core_values[3],
            "IRONRAG_OIDC_REDIRECT_URL",
            MAX_OIDC_ISSUER_URL_BYTES,
        )?;
        let required_group = required_oidc_value(
            core_values[4],
            "IRONRAG_OIDC_REQUIRED_GROUP",
            MAX_OIDC_REQUIRED_GROUP_BYTES,
        )?;
        let bootstrap_admin_subject = self
            .oidc_bootstrap_admin_subject
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .map(|value| {
                let trimmed = value.trim();
                if trimmed.is_empty()
                    || trimmed.len() > MAX_OIDC_BOOTSTRAP_SUBJECT_BYTES
                    || trimmed != value
                {
                    Err("IRONRAG_OIDC_BOOTSTRAP_ADMIN_SUBJECT must be a non-empty normalized value"
                        .to_string())
                } else {
                    Ok(trimmed.to_string())
                }
            })
            .transpose()?;

        validate_oidc_issuer_url(&issuer_url)?;
        validate_oidc_redirect_url(self, &redirect_url)?;

        Ok(Some(OidcSettings {
            issuer_url,
            client_id,
            client_secret,
            redirect_url,
            required_group,
            bootstrap_admin_subject,
        }))
    }

    /// Removes the raw OIDC client secret after it has been moved into the
    /// narrowly scoped runtime configuration.
    pub fn discard_oidc_client_secret(&mut self) {
        if let Some(mut client_secret) = self.oidc_client_secret.take() {
            client_secret.zeroize();
        }
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

    pub(crate) fn mcp_http_origin_policy(&self) -> Result<McpHttpOriginPolicy, String> {
        let frontend_origins =
            self.frontend_origin.split(',').map(str::trim).filter(|origin| !origin.is_empty());
        let api_public_origin = self
            .openapi_public_origin
            .as_deref()
            .map(str::trim)
            .filter(|origin| !origin.is_empty());
        McpHttpOriginPolicy::try_from_origins(frontend_origins, api_public_origin).map_err(
            |reason| format!(
                "frontend_origin and openapi_public_origin must contain primary HTTP origins: {reason}"
            ),
        )
    }

    #[must_use]
    pub fn default_recognition_policy(&self) -> LibraryRecognitionPolicy {
        // validated at startup; parse failure here is a programming error.
        #[allow(
            clippy::expect_used,
            reason = "startup validation guarantees the configured recognition engine parses"
        )]
        let raster_image_engine = self
            .recognition_default_raster_image_engine
            .parse::<RecognitionEngine>()
            .expect("recognition_default_raster_image_engine must be validated before use");
        LibraryRecognitionPolicy { raster_image_engine }
    }

    #[must_use]
    pub const fn destructive_fresh_bootstrap_settings(&self) -> DestructiveFreshBootstrapSettings {
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
            .map(str::to_lowercase)?;
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
        // Settings::from_env validates this namespace before AppState exists.
        // If a test or embedding mutates the process environment afterwards,
        // fail closed by exposing no dynamic credentials.
        let provider_secrets = read_bootstrap_provider_api_keys_from_env().unwrap_or_default();

        let binding_defaults = [
            resolved_ui_bootstrap_ai_binding_default(
                AiBindingPurpose::ExtractText,
                self.ui_bootstrap_extract_text_provider_kind.as_deref(),
                self.ui_bootstrap_extract_text_model_name.as_deref(),
            ),
            resolved_ui_bootstrap_ai_binding_default(
                AiBindingPurpose::ExtractGraph,
                self.ui_bootstrap_extract_graph_provider_kind.as_deref(),
                self.ui_bootstrap_extract_graph_model_name.as_deref(),
            ),
            resolved_ui_bootstrap_ai_binding_default(
                AiBindingPurpose::EmbedChunk,
                self.ui_bootstrap_embed_chunk_provider_kind.as_deref(),
                self.ui_bootstrap_embed_chunk_model_name.as_deref(),
            ),
            resolved_ui_bootstrap_ai_binding_default(
                AiBindingPurpose::QueryCompile,
                self.ui_bootstrap_query_compile_provider_kind.as_deref(),
                self.ui_bootstrap_query_compile_model_name.as_deref(),
            ),
            resolved_ui_bootstrap_ai_binding_default(
                AiBindingPurpose::QueryAnswer,
                self.ui_bootstrap_query_answer_provider_kind.as_deref(),
                self.ui_bootstrap_query_answer_model_name.as_deref(),
            ),
            resolved_ui_bootstrap_ai_binding_default(
                AiBindingPurpose::Agent,
                self.ui_bootstrap_agent_provider_kind.as_deref(),
                self.ui_bootstrap_agent_model_name.as_deref(),
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

fn required_oidc_value(
    value: Option<&str>,
    env_name: &str,
    maximum_bytes: usize,
) -> Result<String, String> {
    let value = value.map(str::trim).filter(|value| !value.is_empty()).ok_or_else(|| {
        "IRONRAG_OIDC_ISSUER_URL, IRONRAG_OIDC_CLIENT_ID, IRONRAG_OIDC_CLIENT_SECRET, \
IRONRAG_OIDC_REDIRECT_URL and IRONRAG_OIDC_REQUIRED_GROUP must be configured together"
            .to_string()
    })?;
    if value.len() > maximum_bytes {
        return Err(format!("{env_name} exceeds its maximum supported length"));
    }
    Ok(value.to_string())
}

fn nonblank_oidc_value(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

fn validate_oidc_issuer_url(value: &str) -> Result<(), String> {
    let parsed = Url::parse(value)
        .map_err(|_| "IRONRAG_OIDC_ISSUER_URL must be an absolute HTTPS URL".to_string())?;
    if parsed.scheme() != "https"
        || parsed.host().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err("IRONRAG_OIDC_ISSUER_URL must be an absolute HTTPS URL without credentials, query, or fragment".to_string());
    }
    Ok(())
}

fn validate_oidc_redirect_url(settings: &Settings, value: &str) -> Result<(), String> {
    let parsed = Url::parse(value).map_err(|_| {
        "IRONRAG_OIDC_REDIRECT_URL must be an absolute HTTP(S) callback URL".to_string()
    })?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.path() != OIDC_CALLBACK_PATH
    {
        return Err(format!(
            "IRONRAG_OIDC_REDIRECT_URL must be an origin callback at {OIDC_CALLBACK_PATH}"
        ));
    }

    let redirect_origin = parsed.origin().ascii_serialization();
    let public_origins = settings.public_origin_settings();
    let is_known_origin = public_origins.allowed_origins.iter().any(|origin| {
        NormalizedHttpOrigin::parse(origin)
            .ok()
            .is_some_and(|normalized| normalized.0 == redirect_origin)
    });
    if !is_known_origin {
        return Err(
            "IRONRAG_OIDC_REDIRECT_URL origin must exactly match one configured frontend origin"
                .to_string(),
        );
    }
    if public_origins.session_cookie_secure && parsed.scheme() != "https" {
        return Err(
            "IRONRAG_OIDC_REDIRECT_URL must use HTTPS when browser sessions use secure cookies"
                .to_string(),
        );
    }
    Ok(())
}

pub(super) fn ironrag_environment_source(
    source: Option<config::Map<String, String>>,
) -> config::Environment {
    config::Environment::with_prefix("IRONRAG").prefix_separator("_").separator("__").source(source)
}

pub(super) fn settings_config_builder()
-> Result<config::ConfigBuilder<config::builder::DefaultState>, config::ConfigError> {
    config::Config::builder()
        .set_default("bind_addr", "0.0.0.0:8080")?
        .set_default("service_role", "api")?
        .set_default("service_name", "ironrag-backend")?
        .set_default("environment", "local")?
        .set_default("database_url", "postgres://postgres:postgres@127.0.0.1:5432/ironrag")?
        .set_default("database_tls_leaf_certificate", None::<String>)?
        .set_default("database_max_connections", 20)?
        .set_default("api_replicas", 1)?
        .set_default("worker_replicas", 1)?
        .set_default("knowledge_plane_backend", "postgres")?
        .set_default("redis_url", "redis://127.0.0.1:6379")?
        .set_default("redis_tls_leaf_certificate", None::<String>)?
        .set_default("log_filter", "info")?
        .set_default("destructive_fresh_bootstrap_required", false)?
        .set_default("frontend_origin", "http://127.0.0.1:19000,http://localhost:19000")?
        .set_default("ui_session_secret", "local-ui-session-secret")?
        .set_default("ui_default_locale", "ru")?
        .set_default("oidc_issuer_url", None::<String>)?
        .set_default("oidc_client_id", None::<String>)?
        .set_default("oidc_client_secret", None::<String>)?
        .set_default("oidc_redirect_url", None::<String>)?
        .set_default("oidc_required_group", None::<String>)?
        .set_default("oidc_bootstrap_admin_subject", None::<String>)?
        .set_default("ui_session_idle_ttl_hours", 72)?
        .set_default("ui_session_absolute_ttl_hours", 720)?
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
        .set_default("ingestion_max_parallel_jobs_global", 64)?
        .set_default("ingestion_max_parallel_jobs_per_workspace", 16)?
        .set_default("ingestion_max_parallel_jobs_per_library", 4)?
        .set_default("ingestion_memory_soft_limit_mib", 0)?
        .set_default("ingestion_worker_lease_seconds", 300)?
        .set_default("ingestion_worker_heartbeat_interval_seconds", 15)?
        .set_default("maintenance_enabled", true)?
        .set_default("maintenance_tick_interval_seconds", 30)?
        .set_default("maintenance_class_interval_seconds", 3600)?
        .set_default("maintenance_stale_lease_seconds", 300)?
        .set_default("maintenance_full_pass_interval_seconds", 86400)?
        .set_default("ingestion_embedding_parallelism", 8)?
        .set_default("ingestion_graph_extract_parallelism_per_doc", 16)?
        .set_default("web_ingest_http_timeout_seconds", 20)?
        .set_default("web_ingest_max_redirects", 10)?
        .set_default("web_ingest_user_agent", "IronRAG-WebIngest/0.1")?
        .set_default("web_ingest_crawl_concurrency", 4)?
        .set_default("llm_http_timeout_seconds", 120)?
        .set_default(
            "provider_concurrency_max_outbound",
            DEFAULT_PROVIDER_CONCURRENCY_MAX_OUTBOUND,
        )?
        .set_default(
            "provider_concurrency_query_reserved",
            DEFAULT_PROVIDER_CONCURRENCY_QUERY_RESERVED,
        )?
        .set_default(
            "provider_concurrency_acquire_timeout_ms",
            DEFAULT_PROVIDER_CONCURRENCY_ACQUIRE_TIMEOUT_MS,
        )?
        .set_default(
            "provider_concurrency_registry_max_entries",
            DEFAULT_PROVIDER_CONCURRENCY_REGISTRY_MAX_ENTRIES,
        )?
        .set_default(
            "provider_concurrency_registry_idle_ttl_seconds",
            DEFAULT_PROVIDER_CONCURRENCY_REGISTRY_IDLE_TTL_SECONDS,
        )?
        .set_default(
            "provider_rate_limit_requests_per_second",
            DEFAULT_PROVIDER_RATE_LIMIT_REQUESTS_PER_SECOND,
        )?
        .set_default("provider_rate_limit_burst", DEFAULT_PROVIDER_RATE_LIMIT_BURST)?
        .set_default("provider_retry_max_attempts", DEFAULT_PROVIDER_RETRY_MAX_ATTEMPTS)?
        .set_default(
            "provider_retry_initial_backoff_ms",
            DEFAULT_PROVIDER_RETRY_INITIAL_BACKOFF_MS,
        )?
        .set_default("provider_retry_max_backoff_ms", DEFAULT_PROVIDER_RETRY_MAX_BACKOFF_MS)?
        .set_default("query_turn_max_concurrent", 32)?
        .set_default("query_turn_admission_queue", 80)?
        .set_default("query_turn_admission_timeout_ms", 15_000)?
        .set_default(
            "login_rate_limit_per_login_max_attempts",
            DEFAULT_LOGIN_RATE_LIMIT_PER_LOGIN_MAX_ATTEMPTS,
        )?
        .set_default(
            "login_rate_limit_per_login_window_seconds",
            DEFAULT_LOGIN_RATE_LIMIT_PER_LOGIN_WINDOW_SECONDS,
        )?
        .set_default(
            "login_rate_limit_per_source_max_attempts",
            DEFAULT_LOGIN_RATE_LIMIT_PER_SOURCE_MAX_ATTEMPTS,
        )?
        .set_default(
            "login_rate_limit_per_source_window_seconds",
            DEFAULT_LOGIN_RATE_LIMIT_PER_SOURCE_WINDOW_SECONDS,
        )?
        .set_default(
            "login_rate_limit_max_concurrent_verifications",
            DEFAULT_LOGIN_RATE_LIMIT_MAX_CONCURRENT_VERIFICATIONS,
        )?
        .set_default("login_rate_limit_admission_queue", DEFAULT_LOGIN_RATE_LIMIT_ADMISSION_QUEUE)?
        .set_default(
            "login_rate_limit_admission_timeout_ms",
            DEFAULT_LOGIN_RATE_LIMIT_ADMISSION_TIMEOUT_MS,
        )?
        .set_default(
            "login_rate_limit_registry_max_entries",
            DEFAULT_LOGIN_RATE_LIMIT_REGISTRY_MAX_ENTRIES,
        )?
        .set_default(
            "login_rate_limit_registry_idle_ttl_seconds",
            DEFAULT_LOGIN_RATE_LIMIT_REGISTRY_IDLE_TTL_SECONDS,
        )?
        .set_default(
            "login_rate_limit_trusted_proxy_hops",
            DEFAULT_LOGIN_RATE_LIMIT_TRUSTED_PROXY_HOPS,
        )?
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
        .set_default("query_semantic_rerank_mode", "off")?
        .set_default(
            "query_semantic_rerank_timeout_ms",
            crate::app::state::SEMANTIC_RERANK_TURN_WAIT_MS as i64,
        )?
        .set_default("query_semantic_rerank_candidate_limit", 16)?
        .set_default("query_semantic_rerank_candidate_text_chars", 1_200)?
        .set_default("query_semantic_rerank_total_text_chars", 18_000)?
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
        // interval (`INGEST_HEARTBEAT_INTERVAL = 15s`) by a comfortable
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
