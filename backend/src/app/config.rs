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

#[derive(Clone, Debug, Deserialize)]
pub struct Settings {
    pub bind_addr: String,
    pub database_url: String,
    pub database_max_connections: u32,
    pub redis_url: String,
    pub neo4j_uri: String,
    pub neo4j_username: String,
    pub neo4j_password: String,
    pub neo4j_database: String,
    pub neo4j_max_connections: usize,
    pub service_name: String,
    pub environment: String,
    pub log_filter: String,
    pub openai_api_key: Option<String>,
    pub deepseek_api_key: Option<String>,
    pub qwen_api_key: Option<String>,
    pub qwen_api_base_url: String,
    pub bootstrap_token: Option<String>,
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
    pub runtime_default_indexing_provider: String,
    pub runtime_default_indexing_model: String,
    pub runtime_default_embedding_provider: String,
    pub runtime_default_embedding_model: String,
    pub runtime_default_answer_provider: String,
    pub runtime_default_answer_model: String,
    pub runtime_default_vision_provider: String,
    pub runtime_default_vision_model: String,
    pub runtime_live_validation_enabled: bool,
    pub runtime_query_intent_cache_ttl_hours: u64,
    pub runtime_query_intent_cache_max_entries_per_library: usize,
    pub runtime_query_rerank_enabled: bool,
    pub runtime_query_rerank_candidate_limit: usize,
    pub runtime_query_balanced_context_enabled: bool,
    pub runtime_graph_extract_recovery_enabled: bool,
    pub runtime_graph_extract_recovery_max_attempts: usize,
    pub runtime_graph_summary_refresh_batch_size: usize,
    pub runtime_graph_targeted_reconciliation_enabled: bool,
    pub runtime_graph_targeted_reconciliation_max_targets: usize,
    pub runtime_document_activity_freshness_seconds: u64,
    pub runtime_document_stalled_after_seconds: u64,
    pub runtime_graph_filter_empty_relations: bool,
    pub runtime_graph_filter_degenerate_self_loops: bool,
    pub runtime_graph_convergence_warning_backlog_threshold: usize,
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
            .set_default("service_name", "rustrag-backend")?
            .set_default("environment", "local")?
            .set_default("database_url", "postgres://postgres:postgres@127.0.0.1:5432/rustrag")?
            .set_default("database_max_connections", 20)?
            .set_default("redis_url", "redis://127.0.0.1:6379")?
            .set_default("neo4j_uri", "127.0.0.1:7687")?
            .set_default("neo4j_username", "neo4j")?
            .set_default("neo4j_password", "rustrag-dev")?
            .set_default("neo4j_database", "neo4j")?
            .set_default("neo4j_max_connections", 16)?
            .set_default("log_filter", "info")?
            .set_default(
                "qwen_api_base_url",
                "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
            )?
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
            .set_default("runtime_default_indexing_provider", "openai")?
            .set_default("runtime_default_indexing_model", "gpt-5-mini")?
            .set_default("runtime_default_embedding_provider", "openai")?
            .set_default("runtime_default_embedding_model", "text-embedding-3-large")?
            .set_default("runtime_default_answer_provider", "openai")?
            .set_default("runtime_default_answer_model", "gpt-5.4")?
            .set_default("runtime_default_vision_provider", "openai")?
            .set_default("runtime_default_vision_model", "gpt-5-mini")?
            .set_default("runtime_live_validation_enabled", false)?
            .set_default("runtime_query_intent_cache_ttl_hours", 24)?
            .set_default("runtime_query_intent_cache_max_entries_per_library", 500)?
            .set_default("runtime_query_rerank_enabled", true)?
            .set_default("runtime_query_rerank_candidate_limit", 24)?
            .set_default("runtime_query_balanced_context_enabled", true)?
            .set_default("runtime_graph_extract_recovery_enabled", true)?
            .set_default("runtime_graph_extract_recovery_max_attempts", 2)?
            .set_default("runtime_graph_summary_refresh_batch_size", 64)?
            .set_default("runtime_graph_targeted_reconciliation_enabled", true)?
            .set_default("runtime_graph_targeted_reconciliation_max_targets", 128)?
            .set_default("runtime_document_activity_freshness_seconds", 45)?
            .set_default("runtime_document_stalled_after_seconds", 180)?
            .set_default("runtime_graph_filter_empty_relations", true)?
            .set_default("runtime_graph_filter_degenerate_self_loops", true)?
            .set_default("runtime_graph_convergence_warning_backlog_threshold", 1)?
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
                std::env::var("RJUSTRAG_BOOTSTRAP_TOKEN")
                    .ok()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
            })
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_settings() -> Settings {
        Settings {
            bind_addr: "0.0.0.0:8080".into(),
            database_url: "postgres://postgres:postgres@127.0.0.1:5432/rustrag".into(),
            database_max_connections: 20,
            redis_url: "redis://127.0.0.1:6379".into(),
            neo4j_uri: "127.0.0.1:7687".into(),
            neo4j_username: "neo4j".into(),
            neo4j_password: "rustrag-dev".into(),
            neo4j_database: "neo4j".into(),
            neo4j_max_connections: 16,
            service_name: "rustrag-backend".into(),
            environment: "local".into(),
            log_filter: "info".into(),
            openai_api_key: None,
            deepseek_api_key: None,
            qwen_api_key: None,
            qwen_api_base_url: "https://dashscope-intl.aliyuncs.com/compatible-mode/v1".into(),
            bootstrap_token: None,
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
            runtime_default_indexing_provider: "openai".into(),
            runtime_default_indexing_model: "gpt-5-mini".into(),
            runtime_default_embedding_provider: "openai".into(),
            runtime_default_embedding_model: "text-embedding-3-large".into(),
            runtime_default_answer_provider: "openai".into(),
            runtime_default_answer_model: "gpt-5.4".into(),
            runtime_default_vision_provider: "openai".into(),
            runtime_default_vision_model: "gpt-5-mini".into(),
            runtime_live_validation_enabled: false,
            runtime_query_intent_cache_ttl_hours: 24,
            runtime_query_intent_cache_max_entries_per_library: 500,
            runtime_query_rerank_enabled: true,
            runtime_query_rerank_candidate_limit: 24,
            runtime_query_balanced_context_enabled: true,
            runtime_graph_extract_recovery_enabled: true,
            runtime_graph_extract_recovery_max_attempts: 2,
            runtime_graph_summary_refresh_batch_size: 64,
            runtime_graph_targeted_reconciliation_enabled: true,
            runtime_graph_targeted_reconciliation_max_targets: 128,
            runtime_document_activity_freshness_seconds: 45,
            runtime_document_stalled_after_seconds: 180,
            runtime_graph_filter_empty_relations: true,
            runtime_graph_filter_degenerate_self_loops: true,
            runtime_graph_convergence_warning_backlog_threshold: 1,
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
        assert_eq!(settings.service_name, "rustrag-backend");
        assert_eq!(settings.environment, "local");
        assert_eq!(settings.database_max_connections, 20);
        assert_eq!(settings.redis_url, "redis://127.0.0.1:6379");
        assert_eq!(settings.neo4j_uri, "127.0.0.1:7687");
        assert_eq!(settings.neo4j_database, "neo4j");
        assert_eq!(settings.log_filter, "info");
        assert_eq!(settings.ingestion_worker_concurrency, 4);
        assert_eq!(settings.runtime_query_intent_cache_ttl_hours, 24);
        assert!(settings.runtime_query_rerank_enabled);
        assert!(settings.runtime_graph_extract_recovery_enabled);
        assert_eq!(settings.runtime_document_activity_freshness_seconds, 45);
        assert_eq!(settings.runtime_document_stalled_after_seconds, 180);
        assert!(settings.runtime_graph_filter_empty_relations);
        assert!(settings.runtime_graph_filter_degenerate_self_loops);
        assert_eq!(settings.runtime_graph_convergence_warning_backlog_threshold, 1);
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
}
