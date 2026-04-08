#![allow(clippy::cast_possible_wrap, clippy::missing_const_for_fn, clippy::struct_excessive_bools)]

use serde::Deserialize;

const DEFAULT_UI_BOOTSTRAP_ADMIN_EMAIL_DOMAIN: &str = "rustrag.local";
const DEFAULT_UI_BOOTSTRAP_ADMIN_NAME: &str = "Admin";
const BOOTSTRAP_PROVIDER_ENV_OPENAI: &str = "RUSTRAG_OPENAI_API_KEY";
const BOOTSTRAP_PROVIDER_ENV_DEEPSEEK: &str = "RUSTRAG_DEEPSEEK_API_KEY";
const BOOTSTRAP_PROVIDER_ENV_QWEN: &str = "RUSTRAG_QWEN_API_KEY";
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
    pub bootstrap_token: Option<String>,
    pub bootstrap_claim_enabled: bool,
    pub legacy_ui_bootstrap_enabled: bool,
    pub legacy_bootstrap_token_endpoint_enabled: bool,
    pub destructive_fresh_bootstrap_required: bool,
    pub destructive_allow_legacy_startup_side_effects: bool,
    pub frontend_origin: String,
    /// When set, OpenAPI/Swagger uses this value as the only `servers` URL (API origin without a
    /// duplicate `/v1`; paths in the contract already start with `/v1/`). Env: `RUSTRAG_OPENAPI_PUBLIC_ORIGIN`.
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
    pub ui_bootstrap_query_answer_provider_kind: Option<String>,
    pub ui_bootstrap_query_answer_model_name: Option<String>,
    pub ui_bootstrap_vision_provider_kind: Option<String>,
    pub ui_bootstrap_vision_model_name: Option<String>,
    pub ui_session_ttl_hours: u64,
    pub upload_max_size_mb: u64,
    pub content_storage_root: String,
    pub ingestion_worker_concurrency: usize,
    pub ingestion_worker_lease_seconds: u64,
    pub ingestion_worker_heartbeat_interval_seconds: u64,
    pub web_ingest_http_timeout_seconds: u64,
    pub web_ingest_max_redirects: usize,
    pub web_ingest_user_agent: String,
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
    pub chunking_max_chars: usize,
    pub chunking_overlap_chars: usize,
}

impl Settings {
    /// Loads application settings from canonical `RUSTRAG_*` environment variables with defaults.
    ///
    /// # Errors
    /// Returns a [`config::ConfigError`] if configuration defaults cannot be built
    /// or environment values fail deserialization.
    pub fn from_env() -> Result<Self, config::ConfigError> {
        let cfg = settings_config_builder()?
            .add_source(config::Environment::with_prefix("RUSTRAG").separator("__"))
            .add_source(
                config::Environment::with_prefix("RUSTRAG").prefix_separator("_").separator("__"),
            )
            .build()?;

        let mut settings: Self = cfg.try_deserialize()?;
        settings.service_role = settings.service_role.trim().to_ascii_lowercase();
        settings.service_name = settings.service_name.trim().to_string();
        validate_service_role(&settings).map_err(config::ConfigError::Message)?;
        validate_service_name(&settings).map_err(config::ConfigError::Message)?;
        validate_arangodb_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_runtime_agent_settings(&settings).map_err(config::ConfigError::Message)?;
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
            legacy_ui_bootstrap_admin: self.resolved_ui_bootstrap_admin(),
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
        matches!(self.service_role.as_str(), "all" | "api")
    }

    #[must_use]
    pub fn runs_ingestion_workers(&self) -> bool {
        matches!(self.service_role.as_str(), "all" | "worker")
    }
}

fn settings_config_builder()
-> Result<config::ConfigBuilder<config::builder::DefaultState>, config::ConfigError> {
    config::Config::builder()
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
        .set_default("bootstrap_claim_enabled", true)?
        .set_default("legacy_ui_bootstrap_enabled", true)?
        .set_default("legacy_bootstrap_token_endpoint_enabled", true)?
        .set_default("destructive_fresh_bootstrap_required", false)?
        .set_default("destructive_allow_legacy_startup_side_effects", true)?
        .set_default("frontend_origin", "http://127.0.0.1:19000,http://localhost:19000")?
        .set_default("ui_session_secret", "local-ui-session-secret")?
        .set_default("ui_default_locale", "ru")?
        .set_default("ui_session_ttl_hours", 720)?
        .set_default("upload_max_size_mb", 50)?
        .set_default("content_storage_root", "/var/lib/rustrag/content-storage")?
        .set_default("ingestion_worker_concurrency", 4)?
        .set_default("ingestion_worker_lease_seconds", 300)?
        .set_default("ingestion_worker_heartbeat_interval_seconds", 15)?
        .set_default("web_ingest_http_timeout_seconds", 20)?
        .set_default("web_ingest_max_redirects", 10)?
        .set_default("web_ingest_user_agent", "RustRAG-WebIngest/0.1")?
        .set_default("llm_http_timeout_seconds", 120)?
        .set_default("llm_transport_retry_attempts", 3)?
        .set_default("llm_transport_retry_base_delay_ms", 250)?
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
        .set_default("chunking_max_chars", 2800)?
        .set_default("chunking_overlap_chars", 280)
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
mod tests {
    use super::*;
    use config::Map;

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
            bootstrap_token: None,
            bootstrap_claim_enabled: true,
            legacy_ui_bootstrap_enabled: true,
            legacy_bootstrap_token_endpoint_enabled: true,
            destructive_fresh_bootstrap_required: false,
            destructive_allow_legacy_startup_side_effects: true,
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
            content_storage_root: "/var/lib/rustrag/content-storage".into(),
            web_ingest_http_timeout_seconds: 20,
            web_ingest_max_redirects: 10,
            web_ingest_user_agent: "RustRAG-WebIngest/0.1".into(),
            ingestion_worker_concurrency: 4,
            ingestion_worker_lease_seconds: 300,
            ingestion_worker_heartbeat_interval_seconds: 15,
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
                config::Environment::with_prefix("RUSTRAG")
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
        validate_mcp_memory_settings(&settings).expect("mcp settings should validate");
        settings
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
        assert_eq!(settings.runtime_agent_max_turns, 4);
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
        assert_eq!(settings.content_storage_root, "/var/lib/rustrag/content-storage");
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
    fn canonical_prefixed_flat_variables_override_defaults() {
        let settings = settings_from_env_entries(&[
            ("RUSTRAG_DATABASE_URL", "postgres://postgres:postgres@postgres:5432/rustrag"),
            ("RUSTRAG_SERVICE_ROLE", "API"),
            ("RUSTRAG_LOG_FILTER", "debug"),
        ]);

        assert_eq!(settings.database_url, "postgres://postgres:postgres@postgres:5432/rustrag");
        assert_eq!(settings.service_role, "api");
        assert_eq!(settings.log_filter, "debug");
    }

    #[test]
    fn resolved_bootstrap_token_uses_configured_value() {
        let mut settings = sample_settings();
        settings.bootstrap_token = Some(" bootstrap-secret ".into());

        assert_eq!(settings.resolved_bootstrap_token().as_deref(), Some("bootstrap-secret"));
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
                email: "owner@rustrag.local".into(),
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
    fn bootstrap_settings_expose_legacy_bootstrap_boundary() {
        let settings = sample_settings();
        let bootstrap = settings.bootstrap_settings();

        assert!(bootstrap.bootstrap_claim_enabled);
        assert!(bootstrap.legacy_ui_bootstrap_enabled);
        assert!(bootstrap.legacy_bootstrap_token_endpoint_enabled);
        assert_eq!(bootstrap.legacy_ui_bootstrap_admin, None);
    }

    #[test]
    fn bootstrap_settings_keep_explicit_admin_even_when_legacy_ui_bootstrap_is_disabled() {
        let mut settings = sample_settings();
        settings.legacy_ui_bootstrap_enabled = false;
        settings.ui_bootstrap_admin_login = Some(" root ".into());
        settings.ui_bootstrap_admin_password = Some(" secret ".into());

        let bootstrap = settings.bootstrap_settings();

        assert!(!bootstrap.legacy_ui_bootstrap_enabled);
        assert_eq!(
            bootstrap.legacy_ui_bootstrap_admin,
            Some(UiBootstrapAdmin {
                login: "root".into(),
                email: "root@rustrag.local".into(),
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
