use serde::Deserialize;

const DEFAULT_UI_BOOTSTRAP_ADMIN_LOGIN: &str = "admin";
const DEFAULT_UI_BOOTSTRAP_ADMIN_EMAIL: &str = "admin@rustrag.local";
const DEFAULT_UI_BOOTSTRAP_ADMIN_NAME: &str = "Admin";
const DEFAULT_UI_BOOTSTRAP_ADMIN_PASSWORD: &str = "rustrag";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiBootstrapAdmin {
    pub login: String,
    pub email: String,
    pub display_name: String,
    pub password: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootstrapSettings {
    pub bootstrap_token: Option<String>,
    pub bootstrap_claim_enabled: bool,
    pub legacy_ui_bootstrap_enabled: bool,
    pub legacy_bootstrap_token_endpoint_enabled: bool,
    pub legacy_ui_bootstrap_admin: Option<UiBootstrapAdmin>,
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
pub struct AiCatalogValidationSettings {
    pub live_validation_enabled: bool,
    pub seed_from_env: bool,
    pub default_currency: String,
    pub default_indexing_provider: String,
    pub default_indexing_model: String,
    pub default_embedding_provider: String,
    pub default_embedding_model: String,
    pub default_answer_provider: String,
    pub default_answer_model: String,
    pub default_vision_provider: String,
    pub default_vision_model: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DestructiveFreshBootstrapSettings {
    pub required: bool,
    pub allow_legacy_startup_side_effects: bool,
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
    pub openai_api_key: Option<String>,
    pub deepseek_api_key: Option<String>,
    pub qwen_api_key: Option<String>,
    pub qwen_api_base_url: String,
    pub bootstrap_token: Option<String>,
    pub bootstrap_claim_enabled: bool,
    pub legacy_ui_bootstrap_enabled: bool,
    pub legacy_bootstrap_token_endpoint_enabled: bool,
    pub destructive_fresh_bootstrap_required: bool,
    pub destructive_allow_legacy_startup_side_effects: bool,
    pub frontend_origin: String,
    pub ui_session_secret: String,
    pub ui_default_locale: String,
    pub ui_bootstrap_admin_login: Option<String>,
    pub ui_bootstrap_admin_email: Option<String>,
    pub ui_bootstrap_admin_name: Option<String>,
    pub ui_bootstrap_admin_password: Option<String>,
    pub ui_session_ttl_hours: u64,
    pub upload_max_size_mb: u64,
    pub ingestion_worker_concurrency: usize,
    pub ingestion_worker_lease_seconds: u64,
    pub ingestion_worker_heartbeat_interval_seconds: u64,
    pub llm_http_timeout_seconds: u64,
    pub llm_transport_retry_attempts: usize,
    pub llm_transport_retry_base_delay_ms: u64,
    pub runtime_default_indexing_provider: String,
    pub runtime_default_indexing_model: String,
    pub runtime_default_embedding_provider: String,
    pub runtime_default_embedding_model: String,
    pub runtime_default_answer_provider: String,
    pub runtime_default_answer_model: String,
    pub runtime_default_vision_provider: String,
    pub runtime_default_vision_model: String,
    pub runtime_live_validation_enabled: bool,
    pub query_intent_cache_ttl_hours: u64,
    pub query_intent_cache_max_entries_per_library: usize,
    pub query_rerank_enabled: bool,
    pub query_rerank_candidate_limit: usize,
    pub query_balanced_context_enabled: bool,
    pub runtime_graph_extract_recovery_enabled: bool,
    pub runtime_graph_extract_recovery_max_attempts: usize,
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
    pub runtime_pricing_seed_from_env: bool,
    pub runtime_pricing_default_currency: String,
    pub openai_input_price_per_1m: f64,
    pub openai_output_price_per_1m: f64,
    pub deepseek_input_price_per_1m: f64,
    pub deepseek_output_price_per_1m: f64,
    pub qwen_input_price_per_1m: f64,
    pub qwen_chat_input_price_per_1m: f64,
    pub qwen_chat_output_price_per_1m: f64,
    pub qwen_vision_input_price_per_1m: f64,
    pub qwen_vision_output_price_per_1m: f64,
}

impl Settings {
    /// Loads application settings from environment variables with defaults.
    ///
    /// # Errors
    /// Returns a [`config::ConfigError`] if configuration defaults cannot be built
    /// or environment values fail deserialization.
    pub fn from_env() -> Result<Self, config::ConfigError> {
        let cfg = config::Config::builder()
            .set_default("bind_addr", "0.0.0.0:8080")?
            .set_default("service_role", "all")?
            .set_default("service_name", "rustrag-backend")?
            .set_default("environment", "local")?
            .set_default("database_url", "postgres://postgres:postgres@127.0.0.1:5432/rustrag")?
            .set_default("database_max_connections", 20)?
            .set_default("redis_url", "redis://127.0.0.1:6379")?
            .set_default("arangodb_url", "http://127.0.0.1:8529")?
            .set_default("arangodb_database", "rustrag")?
            .set_default("arangodb_username", "root")?
            .set_default("arangodb_password", "rustrag-dev")?
            .set_default("arangodb_request_timeout_seconds", 15)?
            .set_default("arangodb_bootstrap_collections", true)?
            .set_default("arangodb_bootstrap_views", true)?
            .set_default("arangodb_bootstrap_graph", true)?
            .set_default("arangodb_bootstrap_vector_indexes", true)?
            .set_default("arangodb_vector_dimensions", 3072)?
            .set_default("arangodb_vector_index_n_lists", 100)?
            .set_default("arangodb_vector_index_default_n_probe", 8)?
            .set_default("arangodb_vector_index_training_iterations", 25)?
            .set_default("log_filter", "info")?
            .set_default(
                "qwen_api_base_url",
                "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
            )?
            .set_default("bootstrap_claim_enabled", true)?
            .set_default("legacy_ui_bootstrap_enabled", true)?
            .set_default("legacy_bootstrap_token_endpoint_enabled", true)?
            .set_default("destructive_fresh_bootstrap_required", false)?
            .set_default("destructive_allow_legacy_startup_side_effects", true)?
            .set_default(
                "frontend_origin",
                "http://127.0.0.1:19000,http://localhost:19000,http://127.0.0.1:9000,http://localhost:9000,http://127.0.0.1:3000,http://localhost:3000",
            )?
            .set_default("ui_session_secret", "local-ui-session-secret")?
            .set_default("ui_default_locale", "ru")?
            .set_default("ui_session_ttl_hours", 720)?
            .set_default("upload_max_size_mb", 50)?
            .set_default("ingestion_worker_concurrency", 4)?
            .set_default("ingestion_worker_lease_seconds", 300)?
            .set_default("ingestion_worker_heartbeat_interval_seconds", 15)?
            .set_default("llm_http_timeout_seconds", 120)?
            .set_default("llm_transport_retry_attempts", 3)?
            .set_default("llm_transport_retry_base_delay_ms", 250)?
            .set_default("runtime_default_indexing_provider", "openai")?
            .set_default("runtime_default_indexing_model", "gpt-5-mini")?
            .set_default("runtime_default_embedding_provider", "openai")?
            .set_default("runtime_default_embedding_model", "text-embedding-3-large")?
            .set_default("runtime_default_answer_provider", "openai")?
            .set_default("runtime_default_answer_model", "gpt-5.4")?
            .set_default("runtime_default_vision_provider", "openai")?
            .set_default("runtime_default_vision_model", "gpt-5-mini")?
            .set_default("runtime_live_validation_enabled", false)?
            .set_default("query_intent_cache_ttl_hours", 24)?
            .set_default("query_intent_cache_max_entries_per_library", 500)?
            .set_default("query_rerank_enabled", true)?
            .set_default("query_rerank_candidate_limit", 24)?
            .set_default("query_balanced_context_enabled", true)?
            .set_default("runtime_graph_extract_recovery_enabled", true)?
            .set_default("runtime_graph_extract_recovery_max_attempts", 2)?
            .set_default("runtime_graph_extract_resume_downgrade_level_one_after_replays", 3)?
            .set_default("runtime_graph_extract_resume_downgrade_level_two_after_replays", 5)?
            .set_default("runtime_graph_summary_refresh_batch_size", 64)?
            .set_default("runtime_graph_targeted_reconciliation_enabled", true)?
            .set_default("runtime_graph_targeted_reconciliation_max_targets", 128)?
            .set_default("runtime_document_activity_freshness_seconds", 45)?
            .set_default("runtime_document_stalled_after_seconds", 180)?
            .set_default("runtime_graph_filter_empty_relations", true)?
            .set_default("runtime_graph_filter_degenerate_self_loops", true)?
            .set_default("runtime_graph_convergence_warning_backlog_threshold", 1)?
            .set_default("mcp_memory_default_read_window_chars", 12_000)?
            .set_default("mcp_memory_max_read_window_chars", 50_000)?
            .set_default("mcp_memory_default_search_limit", 10)?
            .set_default("mcp_memory_max_search_limit", 25)?
            .set_default("mcp_memory_idempotency_retention_hours", 72)?
            .set_default("mcp_memory_audit_enabled", true)?
            .set_default("runtime_pricing_seed_from_env", true)?
            .set_default("runtime_pricing_default_currency", "USD")?
            .set_default("openai_input_price_per_1m", 0.25)?
            .set_default("openai_output_price_per_1m", 2.0)?
            .set_default("deepseek_input_price_per_1m", 0.27)?
            .set_default("deepseek_output_price_per_1m", 1.10)?
            .set_default("qwen_input_price_per_1m", 0.07)?
            .set_default("qwen_chat_input_price_per_1m", 0.0)?
            .set_default("qwen_chat_output_price_per_1m", 0.0)?
            .set_default("qwen_vision_input_price_per_1m", 0.0)?
            .set_default("qwen_vision_output_price_per_1m", 0.0)?
            .add_source(config::Environment::default().separator("__"))
            .add_source(config::Environment::with_prefix("RUSTRAG").separator("__"))
            .build()?;

        let mut settings: Self = cfg.try_deserialize()?;
        settings.service_role = settings.service_role.trim().to_ascii_lowercase();
        settings.service_name = settings.service_name.trim().to_string();
        validate_service_role(&settings).map_err(config::ConfigError::Message)?;
        validate_service_name(&settings).map_err(config::ConfigError::Message)?;
        validate_arangodb_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_mcp_memory_settings(&settings).map_err(config::ConfigError::Message)?;

        if settings.openai_api_key.as_deref().is_none_or(|value| value.trim().is_empty()) {
            settings.openai_api_key = std::env::var("RUSTRAG_OPENAI_API_KEY")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
        }

        if settings.deepseek_api_key.as_deref().is_none_or(|value| value.trim().is_empty()) {
            settings.deepseek_api_key = std::env::var("RUSTRAG_DEEPSEEK_API_KEY")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
        }

        if settings.qwen_api_key.as_deref().is_none_or(|value| value.trim().is_empty()) {
            settings.qwen_api_key = std::env::var("RUSTRAG_QWEN_API_KEY")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
        }

        Ok(settings)
    }

    #[must_use]
    pub fn resolved_bootstrap_token(&self) -> Option<String> {
        self.bootstrap_token
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(std::string::ToString::to_string)
            .or_else(|| {
                std::env::var("RUSTRAG_BOOTSTRAP_TOKEN")
                    .ok()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
            })
    }

    #[must_use]
    pub fn bootstrap_settings(&self) -> BootstrapSettings {
        BootstrapSettings {
            bootstrap_token: self.resolved_bootstrap_token(),
            bootstrap_claim_enabled: self.bootstrap_claim_enabled,
            legacy_ui_bootstrap_enabled: self.legacy_ui_bootstrap_enabled,
            legacy_bootstrap_token_endpoint_enabled: self.legacy_bootstrap_token_endpoint_enabled,
            legacy_ui_bootstrap_admin: self
                .legacy_ui_bootstrap_enabled
                .then(|| self.resolved_ui_bootstrap_admin())
                .flatten(),
        }
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
    pub fn ai_catalog_validation_settings(&self) -> AiCatalogValidationSettings {
        AiCatalogValidationSettings {
            live_validation_enabled: self.runtime_live_validation_enabled,
            seed_from_env: self.runtime_pricing_seed_from_env,
            default_currency: self.runtime_pricing_default_currency.clone(),
            default_indexing_provider: self.runtime_default_indexing_provider.clone(),
            default_indexing_model: self.runtime_default_indexing_model.clone(),
            default_embedding_provider: self.runtime_default_embedding_provider.clone(),
            default_embedding_model: self.runtime_default_embedding_model.clone(),
            default_answer_provider: self.runtime_default_answer_provider.clone(),
            default_answer_model: self.runtime_default_answer_model.clone(),
            default_vision_provider: self.runtime_default_vision_provider.clone(),
            default_vision_model: self.runtime_default_vision_model.clone(),
        }
    }

    #[must_use]
    pub fn destructive_fresh_bootstrap_settings(&self) -> DestructiveFreshBootstrapSettings {
        DestructiveFreshBootstrapSettings {
            required: self.destructive_fresh_bootstrap_required,
            allow_legacy_startup_side_effects: self.destructive_allow_legacy_startup_side_effects,
        }
    }

    #[must_use]
    pub fn resolved_ui_bootstrap_admin(&self) -> Option<UiBootstrapAdmin> {
        let login = self
            .ui_bootstrap_admin_login
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_lowercase)
            .unwrap_or_else(|| DEFAULT_UI_BOOTSTRAP_ADMIN_LOGIN.to_string());
        let email = self
            .ui_bootstrap_admin_email
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_lowercase)
            .unwrap_or_else(|| DEFAULT_UI_BOOTSTRAP_ADMIN_EMAIL.to_string());
        let password = self
            .ui_bootstrap_admin_password
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(std::string::ToString::to_string)
            .unwrap_or_else(|| DEFAULT_UI_BOOTSTRAP_ADMIN_PASSWORD.to_string());
        let display_name = self
            .ui_bootstrap_admin_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(std::string::ToString::to_string)
            .unwrap_or_else(|| DEFAULT_UI_BOOTSTRAP_ADMIN_NAME.to_string());

        Some(UiBootstrapAdmin { login, email, display_name, password })
    }

    #[must_use]
    pub fn has_explicit_ui_bootstrap_admin(&self) -> bool {
        [
            self.ui_bootstrap_admin_login.as_deref(),
            self.ui_bootstrap_admin_email.as_deref(),
            self.ui_bootstrap_admin_name.as_deref(),
            self.ui_bootstrap_admin_password.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(str::trim)
        .any(|value| !value.is_empty())
    }

    #[must_use]
    pub fn runs_http_api(&self) -> bool {
        matches!(self.service_role.as_str(), "all" | "api")
    }

    #[must_use]
    pub fn runs_ingestion_workers(&self) -> bool {
        matches!(self.service_role.as_str(), "all" | "worker")
    }
}

fn validate_service_role(settings: &Settings) -> Result<(), String> {
    match settings.service_role.as_str() {
        "all" | "api" | "worker" => Ok(()),
        value => Err(format!("service_role must be one of all, api, worker; got {value}")),
    }
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
mod tests {
    use super::*;

    fn sample_settings() -> Settings {
        Settings {
            bind_addr: "0.0.0.0:8080".into(),
            service_role: "all".into(),
            database_url: "postgres://postgres:postgres@127.0.0.1:5432/rustrag".into(),
            database_max_connections: 20,
            redis_url: "redis://127.0.0.1:6379".into(),
            arangodb_url: "http://127.0.0.1:8529".into(),
            arangodb_database: "rustrag".into(),
            arangodb_username: "root".into(),
            arangodb_password: "rustrag-dev".into(),
            arangodb_request_timeout_seconds: 15,
            arangodb_bootstrap_collections: true,
            arangodb_bootstrap_views: true,
            arangodb_bootstrap_graph: true,
            arangodb_bootstrap_vector_indexes: true,
            arangodb_vector_dimensions: 3072,
            arangodb_vector_index_n_lists: 100,
            arangodb_vector_index_default_n_probe: 8,
            arangodb_vector_index_training_iterations: 25,
            service_name: "rustrag-backend".into(),
            environment: "local".into(),
            log_filter: "info".into(),
            openai_api_key: None,
            deepseek_api_key: None,
            qwen_api_key: None,
            qwen_api_base_url: "https://dashscope-intl.aliyuncs.com/compatible-mode/v1".into(),
            bootstrap_token: None,
            bootstrap_claim_enabled: true,
            legacy_ui_bootstrap_enabled: true,
            legacy_bootstrap_token_endpoint_enabled: true,
            destructive_fresh_bootstrap_required: false,
            destructive_allow_legacy_startup_side_effects: true,
            frontend_origin: "http://127.0.0.1:19000,http://localhost:19000,http://127.0.0.1:9000,http://localhost:9000,http://127.0.0.1:3000,http://localhost:3000".into(),
            ui_session_secret: "local-ui-session-secret".into(),
            ui_default_locale: "ru".into(),
            ui_bootstrap_admin_login: None,
            ui_bootstrap_admin_email: None,
            ui_bootstrap_admin_name: None,
            ui_bootstrap_admin_password: None,
            ui_session_ttl_hours: 720,
            upload_max_size_mb: 50,
            ingestion_worker_concurrency: 4,
            ingestion_worker_lease_seconds: 300,
            ingestion_worker_heartbeat_interval_seconds: 15,
            llm_http_timeout_seconds: 120,
            llm_transport_retry_attempts: 3,
            llm_transport_retry_base_delay_ms: 250,
            runtime_default_indexing_provider: "openai".into(),
            runtime_default_indexing_model: "gpt-5-mini".into(),
            runtime_default_embedding_provider: "openai".into(),
            runtime_default_embedding_model: "text-embedding-3-large".into(),
            runtime_default_answer_provider: "openai".into(),
            runtime_default_answer_model: "gpt-5.4".into(),
            runtime_default_vision_provider: "openai".into(),
            runtime_default_vision_model: "gpt-5-mini".into(),
            runtime_live_validation_enabled: false,
            query_intent_cache_ttl_hours: 24,
            query_intent_cache_max_entries_per_library: 500,
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
            runtime_pricing_seed_from_env: true,
            runtime_pricing_default_currency: "USD".into(),
            openai_input_price_per_1m: 0.25,
            openai_output_price_per_1m: 2.0,
            deepseek_input_price_per_1m: 0.27,
            deepseek_output_price_per_1m: 1.10,
            qwen_input_price_per_1m: 0.07,
            qwen_chat_input_price_per_1m: 0.0,
            qwen_chat_output_price_per_1m: 0.0,
            qwen_vision_input_price_per_1m: 0.0,
            qwen_vision_output_price_per_1m: 0.0,
        }
    }

    #[test]
    fn from_env_has_sane_local_defaults() {
        let settings = Settings::from_env().expect("settings should load with defaults");

        assert_eq!(settings.bind_addr, "0.0.0.0:8080");
        assert_eq!(settings.service_role, "all");
        assert_eq!(settings.service_name, "rustrag-backend");
        assert_eq!(settings.environment, "local");
        assert_eq!(settings.database_max_connections, 20);
        assert_eq!(settings.redis_url, "redis://127.0.0.1:6379");
        assert_eq!(settings.arangodb_url, "http://127.0.0.1:8529");
        assert_eq!(settings.arangodb_database, "rustrag");
        assert_eq!(settings.log_filter, "info");
        assert_eq!(settings.ingestion_worker_concurrency, 4);
        assert_eq!(settings.query_intent_cache_ttl_hours, 24);
        assert!(settings.query_rerank_enabled);
        assert!(settings.runtime_graph_extract_recovery_enabled);
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

        assert_eq!(settings.database_url, "postgres://postgres:postgres@127.0.0.1:5432/rustrag");
    }

    #[test]
    fn resolved_bootstrap_token_uses_configured_value() {
        let mut settings = sample_settings();
        settings.bootstrap_token = Some(" bootstrap-secret ".into());

        assert_eq!(settings.resolved_bootstrap_token().as_deref(), Some("bootstrap-secret"));
    }

    #[test]
    fn resolved_ui_bootstrap_admin_falls_back_to_builtin_admin() {
        let settings = sample_settings();

        assert_eq!(
            settings.resolved_ui_bootstrap_admin(),
            Some(UiBootstrapAdmin {
                login: "admin".into(),
                email: "admin@rustrag.local".into(),
                display_name: "Admin".into(),
                password: "rustrag".into(),
            })
        );
        assert!(!settings.has_explicit_ui_bootstrap_admin());
    }

    #[test]
    fn resolved_ui_bootstrap_admin_uses_configured_credentials() {
        let mut settings = sample_settings();
        settings.ui_bootstrap_admin_login = Some(" root ".into());
        settings.ui_bootstrap_admin_email = Some(" admin@example.com ".into());
        settings.ui_bootstrap_admin_name = Some(" Platform Owner ".into());
        settings.ui_bootstrap_admin_password = Some(" secret ".into());

        assert_eq!(
            settings.resolved_ui_bootstrap_admin(),
            Some(UiBootstrapAdmin {
                login: "root".into(),
                email: "admin@example.com".into(),
                display_name: "Platform Owner".into(),
                password: "secret".into(),
            })
        );
        assert!(settings.has_explicit_ui_bootstrap_admin());
    }

    #[test]
    fn bootstrap_settings_expose_legacy_bootstrap_boundary() {
        let settings = sample_settings();
        let bootstrap = settings.bootstrap_settings();

        assert!(bootstrap.bootstrap_claim_enabled);
        assert!(bootstrap.legacy_ui_bootstrap_enabled);
        assert!(bootstrap.legacy_bootstrap_token_endpoint_enabled);
        assert_eq!(
            bootstrap.legacy_ui_bootstrap_admin,
            Some(UiBootstrapAdmin {
                login: "admin".into(),
                email: "admin@rustrag.local".into(),
                display_name: "Admin".into(),
                password: "rustrag".into(),
            })
        );
    }

    #[test]
    fn public_origin_settings_split_and_trim_allowed_origins() {
        let mut settings = sample_settings();
        settings.frontend_origin = " https://app.example.com , http://localhost:19000 ".into();

        let origins = settings.public_origin_settings();

        assert_eq!(
            origins.raw_frontend_origin,
            " https://app.example.com , http://localhost:19000 "
        );
        assert_eq!(
            origins.allowed_origins,
            vec!["https://app.example.com".to_string(), "http://localhost:19000".to_string()]
        );
    }

    #[test]
    fn ai_catalog_validation_settings_capture_runtime_defaults() {
        let settings = sample_settings();
        let ai_catalog = settings.ai_catalog_validation_settings();

        assert!(!ai_catalog.live_validation_enabled);
        assert!(ai_catalog.seed_from_env);
        assert_eq!(ai_catalog.default_currency, "USD");
        assert_eq!(ai_catalog.default_embedding_model, "text-embedding-3-large");
        assert_eq!(ai_catalog.default_answer_model, "gpt-5.4");
    }

    #[test]
    fn arango_settings_expose_bootstrap_toggles() {
        let settings = sample_settings();
        let arango = settings.arango_settings();

        assert_eq!(arango.url, "http://127.0.0.1:8529");
        assert_eq!(arango.database, "rustrag");
        assert!(arango.bootstrap_collections);
        assert!(arango.bootstrap_views);
        assert!(arango.bootstrap_graph);
        assert!(arango.bootstrap_vector_indexes);
        assert_eq!(arango.vector_dimensions, 3072);
    }

    #[test]
    fn destructive_fresh_bootstrap_settings_preserve_legacy_boundary_flags() {
        let settings = sample_settings();
        let destructive = settings.destructive_fresh_bootstrap_settings();

        assert!(!destructive.required);
        assert!(destructive.allow_legacy_startup_side_effects);
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
    fn service_role_helpers_match_role() {
        let mut settings = sample_settings();

        settings.service_role = "api".into();
        assert!(settings.runs_http_api());
        assert!(!settings.runs_ingestion_workers());

        settings.service_role = "worker".into();
        assert!(!settings.runs_http_api());
        assert!(settings.runs_ingestion_workers());
    }

    #[test]
    fn rejects_invalid_service_roles() {
        let mut settings = sample_settings();
        settings.service_role = "scheduler".into();

        let error = validate_service_role(&settings).expect_err("invalid role should fail");
        assert!(error.contains("service_role"));
    }

    #[test]
    fn accepts_service_names_with_identity_safe_characters() {
        let mut settings = sample_settings();
        settings.service_name = "rustrag.worker_01-api".into();

        validate_service_name(&settings).expect("valid service name should pass");
    }

    #[test]
    fn rejects_invalid_service_names() {
        let mut settings = sample_settings();
        settings.service_name = "worker:api".into();

        let error = validate_service_name(&settings).expect_err("invalid service name should fail");
        assert!(error.contains("service_name"));
    }
}
