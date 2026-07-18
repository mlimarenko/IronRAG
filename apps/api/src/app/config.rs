use std::collections::BTreeSet;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde::Deserialize;
use serde::de::{Error as _, MapAccess, Visitor};
use url::Url;
use zeroize::Zeroize as _;

use crate::domains::{
    ai::AiBindingPurpose,
    deployment::{
        ContentStorageProvider, DependencyKind, DependencyMode, DeploymentTopology, ServiceRole,
        StartupAuthorityMode,
    },
    query::SemanticRerankMode,
    recognition::{LibraryRecognitionPolicy, RecognitionEngine},
};

const DEFAULT_UI_BOOTSTRAP_ADMIN_EMAIL_DOMAIN: &str = "ironrag.local";
const DEFAULT_UI_BOOTSTRAP_ADMIN_NAME: &str = "Admin";
const BOOTSTRAP_PROVIDER_API_KEYS_JSON_B64_ENV: &str = "IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64";
const MAX_BOOTSTRAP_PROVIDER_KIND_BYTES: usize = 128;
const MAX_BOOTSTRAP_PROVIDER_API_KEY_BYTES: usize = 65_536;
const MAX_BOOTSTRAP_PROVIDER_SECRETS: usize = 256;
const MAX_BOOTSTRAP_PROVIDER_MAP_JSON_BYTES: usize = 1_048_576;
const MAX_BOOTSTRAP_PROVIDER_MAP_BASE64_BYTES: usize =
    MAX_BOOTSTRAP_PROVIDER_MAP_JSON_BYTES.div_ceil(3) * 4;
pub const DEFAULT_RUNTIME_DIAGNOSTIC_PAYLOAD_BUDGET_BYTES: usize = 32_768;
pub const DEFAULT_RUNTIME_POLICY_REASON_BUDGET_CHARS: usize = 2_000;
const MIN_DATABASE_CONNECTIONS_PER_RUNTIME_REPLICA: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeHookBehavior {
    ObserveOnly,
}

#[derive(Clone, PartialEq, Eq)]
pub struct UiBootstrapAdmin {
    pub login: String,
    pub email: String,
    pub display_name: String,
    pub password: String,
    pub api_token: Option<String>,
}

impl std::fmt::Debug for UiBootstrapAdmin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UiBootstrapAdmin")
            .field("login", &self.login)
            .field("email", &self.email)
            .field("display_name", &self.display_name)
            .field("password", &"<redacted>")
            .field("api_token", &self.api_token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl Drop for UiBootstrapAdmin {
    fn drop(&mut self) {
        self.password.zeroize();
        if let Some(api_token) = self.api_token.as_mut() {
            api_token.zeroize();
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiBootstrapAiSetup {
    pub provider_secrets: Vec<UiBootstrapAiProviderSecret>,
    pub binding_defaults: Vec<UiBootstrapAiBindingDefault>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct UiBootstrapAiProviderSecret {
    pub provider_kind: String,
    pub api_key: String,
}

impl std::fmt::Debug for UiBootstrapAiProviderSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UiBootstrapAiProviderSecret")
            .field("provider_kind", &self.provider_kind)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

impl Drop for UiBootstrapAiProviderSecret {
    fn drop(&mut self) {
        self.api_key.zeroize();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiBootstrapAiBindingDefault {
    pub binding_purpose: AiBindingPurpose,
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

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CanonicalHttpOrigin(String);

impl CanonicalHttpOrigin {
    fn parse(raw: &str) -> Result<Self, String> {
        if raw.is_empty() || raw != raw.trim() {
            return Err("origin must be a non-empty canonical value".into());
        }
        let parsed = Url::parse(raw).map_err(|_| "origin must be an absolute HTTP URL")?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.path() != "/"
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err("origin must contain only an HTTP scheme, host, and optional port".into());
        }
        let canonical = parsed.origin().ascii_serialization();
        if canonical != raw {
            return Err("origin must use its exact canonical ASCII serialization".into());
        }
        Ok(Self(canonical))
    }
}

/// Exact allowlist for the MCP Streamable HTTP `Origin` security boundary.
///
/// Absence of the header is handled by the transport middleware so native MCP
/// clients remain valid. Any present value must parse to one canonical HTTP
/// origin and match this typed set exactly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct McpHttpOriginPolicy {
    allowed_origins: BTreeSet<CanonicalHttpOrigin>,
}

impl McpHttpOriginPolicy {
    pub(crate) fn try_from_origins<'a>(
        allowed_origins: impl IntoIterator<Item = &'a str>,
        canonical_public_origin: Option<&'a str>,
    ) -> Result<Self, String> {
        let mut parsed_origins = BTreeSet::new();
        for raw in allowed_origins.into_iter().chain(canonical_public_origin) {
            if raw.is_empty() {
                continue;
            }
            parsed_origins.insert(CanonicalHttpOrigin::parse(raw)?);
        }
        Ok(Self { allowed_origins: parsed_origins })
    }

    #[must_use]
    pub(crate) fn allows(&self, raw: &str) -> bool {
        CanonicalHttpOrigin::parse(raw)
            .ok()
            .is_some_and(|origin| self.allowed_origins.contains(&origin))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DestructiveFreshBootstrapSettings {
    pub required: bool,
}

#[derive(Clone, Deserialize, utoipa::ToSchema)]
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
    /// Dedicated encryption key for credentials persisted in `PostgreSQL`.
    /// Must be canonical standard-base64 for exactly 32 random bytes.
    pub credential_master_key: Option<String>,
    /// Identifier embedded and authenticated in newly encrypted v3 envelopes.
    pub credential_master_key_id: Option<String>,
    /// Sorted `key-id=base64-key` map used only to decrypt and rewrap old values.
    pub credential_previous_master_keys: Option<String>,
    /// Explicit second phase of the dual-read-first rollout. While false,
    /// credential-bearing create/update operations fail closed so older pods
    /// can overlap this release without receiving ciphertext as plaintext.
    pub credential_encryption_write_enabled: bool,
    pub ui_session_secret: String,
    pub ui_default_locale: String,
    pub ui_bootstrap_admin_login: Option<String>,
    pub ui_bootstrap_admin_email: Option<String>,
    pub ui_bootstrap_admin_name: Option<String>,
    pub ui_bootstrap_admin_password: Option<String>,
    pub ui_bootstrap_admin_api_token: Option<String>,
    pub ui_bootstrap_extract_text_provider_kind: Option<String>,
    pub ui_bootstrap_extract_text_model_name: Option<String>,
    pub ui_bootstrap_extract_graph_provider_kind: Option<String>,
    pub ui_bootstrap_extract_graph_model_name: Option<String>,
    pub ui_bootstrap_embed_chunk_provider_kind: Option<String>,
    pub ui_bootstrap_embed_chunk_model_name: Option<String>,
    pub ui_bootstrap_query_compile_provider_kind: Option<String>,
    pub ui_bootstrap_query_compile_model_name: Option<String>,
    pub ui_bootstrap_query_answer_provider_kind: Option<String>,
    pub ui_bootstrap_query_answer_model_name: Option<String>,
    pub ui_bootstrap_agent_provider_kind: Option<String>,
    pub ui_bootstrap_agent_model_name: Option<String>,
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
    /// Each batch contains `EMBEDDING_BATCH_SIZE` inputs. Higher values speed up
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
    pub query_semantic_rerank_mode: SemanticRerankMode,
    pub query_semantic_rerank_timeout_ms: u64,
    pub query_semantic_rerank_candidate_limit: usize,
    pub query_semantic_rerank_candidate_text_chars: usize,
    pub query_semantic_rerank_total_text_chars: usize,
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
    /// query lanes. `0` is an explicit unlimited mode; the production-safe
    /// default is bounded.
    pub provider_concurrency_max_outbound: usize,
    /// Permits reserved exclusively for the query lane out of
    /// `provider_concurrency_max_outbound`. Guarantees latency-sensitive
    /// query turns at least this many concurrent permits even under a fully
    /// saturating ingest load. Must be smaller than the max, or zero when the
    /// max is zero.
    pub provider_concurrency_query_reserved: usize,
    /// Maximum time an outbound call waits for a provider permit.
    pub provider_concurrency_acquire_timeout_ms: u64,
    /// Maximum endpoint identities retained by one gateway-local registry.
    pub provider_concurrency_registry_max_entries: usize,
    /// Idle registry entry retention before eviction.
    pub provider_concurrency_registry_idle_ttl_seconds: u64,
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
        validate_provider_concurrency_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_recognition_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_runtime_agent_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_query_rerank_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_release_monitor_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_graph_gc_settings(&settings).map_err(config::ConfigError::Message)?;
        validate_mcp_memory_settings(&settings).map_err(config::ConfigError::Message)?;
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
        let canonical_public_origin = self
            .openapi_public_origin
            .as_deref()
            .map(str::trim)
            .filter(|origin| !origin.is_empty());
        McpHttpOriginPolicy::try_from_origins(frontend_origins, canonical_public_origin).map_err(
            |reason| format!(
                "frontend_origin and openapi_public_origin must contain canonical HTTP origins: {reason}"
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

fn validate_credential_master_key(settings: &Settings) -> Result<(), String> {
    if settings.credential_encryption_write_enabled
        && settings.credential_master_key.as_deref().is_none_or(str::is_empty)
    {
        return Err(
            "IRONRAG_CREDENTIAL_MASTER_KEY is required when IRONRAG_CREDENTIAL_ENCRYPTION_WRITE_ENABLED=true"
                .to_string(),
        );
    }
    crate::shared::secret_encryption::CredentialCipher::from_keyring_base64(
        settings.credential_master_key_id.as_deref(),
        settings.credential_master_key.as_deref(),
        settings.credential_previous_master_keys.as_deref(),
    )
    .map(|_| ())
    .map_err(|_| {
        "IRONRAG_CREDENTIAL_MASTER_KEY, IRONRAG_CREDENTIAL_MASTER_KEY_ID and IRONRAG_CREDENTIAL_PREVIOUS_MASTER_KEYS must define a canonical bounded credential keyring"
            .to_string()
    })
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
        .set_default("credential_encryption_write_enabled", false)?
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
        .set_default("ingestion_embedding_parallelism", 8)?
        .set_default("ingestion_graph_extract_parallelism_per_doc", 16)?
        .set_default("web_ingest_http_timeout_seconds", 20)?
        .set_default("web_ingest_max_redirects", 10)?
        .set_default("web_ingest_user_agent", "IronRAG-WebIngest/0.1")?
        .set_default("web_ingest_crawl_concurrency", 4)?
        .set_default("llm_http_timeout_seconds", 120)?
        .set_default("provider_concurrency_max_outbound", 16)?
        .set_default("provider_concurrency_query_reserved", 4)?
        .set_default("provider_concurrency_acquire_timeout_ms", 30_000)?
        .set_default("provider_concurrency_registry_max_entries", 64)?
        .set_default("provider_concurrency_registry_idle_ttl_seconds", 900)?
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
        .set_default("query_semantic_rerank_timeout_ms", 1_500)?
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
                if value.map(str::trim).as_ref().is_none_or(|item| item.is_empty()) {
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

fn read_bootstrap_provider_api_keys_from_env() -> Result<Vec<UiBootstrapAiProviderSecret>, String> {
    let mut removed_names = std::env::vars_os()
        .filter_map(|(name, _)| name.into_string().ok())
        .filter(|name| is_removed_provider_api_key_env_name(name))
        .collect::<Vec<_>>();
    removed_names.sort_unstable();
    if let Some(removed_name) = removed_names.first() {
        return Err(format!(
            "{removed_name} uses the removed provider-specific credential convention; move the exact provider kind and credential into {BOOTSTRAP_PROVIDER_API_KEYS_JSON_B64_ENV}"
        ));
    }

    let Some(raw_encoded_map) = std::env::var_os(BOOTSTRAP_PROVIDER_API_KEYS_JSON_B64_ENV) else {
        return Ok(Vec::new());
    };
    let mut raw_encoded_map = raw_encoded_map.into_string().map_err(|_| {
        format!("{BOOTSTRAP_PROVIDER_API_KEYS_JSON_B64_ENV} must contain valid UTF-8 base64")
    })?;
    let result = parse_bootstrap_provider_api_keys_b64(&raw_encoded_map);
    raw_encoded_map.zeroize();
    result
}

fn is_removed_provider_api_key_env_name(env_name: &str) -> bool {
    let Some(provider_fragment) =
        env_name.strip_prefix("IRONRAG_").and_then(|value| value.strip_suffix("_API_KEY"))
    else {
        return false;
    };
    !provider_fragment.is_empty()
        && provider_fragment
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn parse_bootstrap_provider_api_keys_b64(
    encoded_map: &str,
) -> Result<Vec<UiBootstrapAiProviderSecret>, String> {
    if encoded_map.is_empty() {
        return Ok(Vec::new());
    }
    if encoded_map != encoded_map.trim()
        || encoded_map.len() > MAX_BOOTSTRAP_PROVIDER_MAP_BASE64_BYTES
    {
        return Err(format!(
            "{BOOTSTRAP_PROVIDER_API_KEYS_JSON_B64_ENV} must be canonical standard-base64 JSON within the configured size limit"
        ));
    }
    let mut decoded = BASE64_STANDARD.decode(encoded_map).map_err(|_| {
        format!("{BOOTSTRAP_PROVIDER_API_KEYS_JSON_B64_ENV} must be canonical standard-base64 JSON")
    })?;
    if decoded.len() > MAX_BOOTSTRAP_PROVIDER_MAP_JSON_BYTES {
        decoded.zeroize();
        return Err(format!(
            "{BOOTSTRAP_PROVIDER_API_KEYS_JSON_B64_ENV} decoded JSON exceeds the configured size limit"
        ));
    }
    let mut canonical_encoding = BASE64_STANDARD.encode(&decoded);
    let is_canonical = canonical_encoding == encoded_map;
    canonical_encoding.zeroize();
    if !is_canonical {
        decoded.zeroize();
        return Err(format!(
            "{BOOTSTRAP_PROVIDER_API_KEYS_JSON_B64_ENV} must be canonical standard-base64 JSON"
        ));
    }
    let mut raw_json = match String::from_utf8(decoded) {
        Ok(raw_json) => raw_json,
        Err(error) => {
            drop(zeroize::Zeroizing::new(error.into_bytes()));
            return Err(format!(
                "{BOOTSTRAP_PROVIDER_API_KEYS_JSON_B64_ENV} must decode to valid UTF-8 JSON"
            ));
        }
    };
    let result = parse_bootstrap_provider_api_keys_json(&raw_json);
    raw_json.zeroize();
    result
}

struct BootstrapProviderSecrets(Vec<UiBootstrapAiProviderSecret>);

impl<'de> Deserialize<'de> for BootstrapProviderSecrets {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(BootstrapProviderSecretsVisitor)
    }
}

struct BootstrapProviderSecretsVisitor;

impl<'de> Visitor<'de> for BootstrapProviderSecretsVisitor {
    type Value = BootstrapProviderSecrets;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON object mapping exact provider kinds to API-key strings")
    }

    fn visit_map<A>(self, mut entries: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut secrets = std::collections::BTreeMap::new();
        let mut entry_count = 0_usize;
        while let Some((provider_kind, mut raw_api_key)) = entries.next_entry::<String, String>()? {
            entry_count += 1;
            if entry_count > MAX_BOOTSTRAP_PROVIDER_SECRETS {
                raw_api_key.zeroize();
                return Err(A::Error::custom("provider credential map has too many entries"));
            }
            if let Err(reason) = validate_bootstrap_provider_kind(&provider_kind) {
                raw_api_key.zeroize();
                return Err(A::Error::custom(format!("provider kind {reason}")));
            }
            if raw_api_key.len() > MAX_BOOTSTRAP_PROVIDER_API_KEY_BYTES {
                raw_api_key.zeroize();
                return Err(A::Error::custom("provider credential exceeds the size limit"));
            }
            if raw_api_key.is_empty() {
                continue;
            }
            // JSON and base64 already provide an unambiguous transport. Keep
            // credential bytes exact: whitespace may be significant to an
            // operator-defined provider and must never be normalized here.
            let api_key = std::mem::take(&mut raw_api_key);
            let candidate =
                UiBootstrapAiProviderSecret { provider_kind: provider_kind.clone(), api_key };
            match secrets.entry(provider_kind) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(candidate);
                }
                std::collections::btree_map::Entry::Occupied(entry) => {
                    return Err(A::Error::custom(format!(
                        "duplicate provider kind {}",
                        entry.key()
                    )));
                }
            }
        }
        Ok(BootstrapProviderSecrets(secrets.into_values().collect()))
    }
}

fn parse_bootstrap_provider_api_keys_json(
    raw_json: &str,
) -> Result<Vec<UiBootstrapAiProviderSecret>, String> {
    let mut deserializer = serde_json::Deserializer::from_str(raw_json);
    let BootstrapProviderSecrets(secrets) =
        BootstrapProviderSecrets::deserialize(&mut deserializer).map_err(|_| {
            // serde diagnostics can include the unexpected scalar value. Do
            // not propagate them because this document consists of secrets.
            format!("{BOOTSTRAP_PROVIDER_API_KEYS_JSON_B64_ENV} decoded JSON is invalid")
        })?;
    deserializer.end().map_err(|_| {
        format!("{BOOTSTRAP_PROVIDER_API_KEYS_JSON_B64_ENV} decoded JSON is invalid")
    })?;
    Ok(secrets)
}

fn validate_bootstrap_provider_kind(provider_kind: &str) -> Result<(), &'static str> {
    if provider_kind.is_empty() || provider_kind.len() > MAX_BOOTSTRAP_PROVIDER_KIND_BYTES {
        return Err("must contain between 1 and 128 UTF-8 bytes");
    }
    if provider_kind.chars().any(char::is_whitespace) {
        return Err("must not contain whitespace");
    }
    if provider_kind.chars().any(char::is_control) {
        return Err("must not contain control characters");
    }
    Ok(())
}

fn resolved_ui_bootstrap_ai_binding_default(
    binding_purpose: AiBindingPurpose,
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
    Some(UiBootstrapAiBindingDefault { binding_purpose, provider_kind, model_name })
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

fn validate_provider_concurrency_settings(settings: &Settings) -> Result<(), String> {
    match (settings.provider_concurrency_max_outbound, settings.provider_concurrency_query_reserved)
    {
        (0, 0) => {}
        (0, _) => {
            return Err(
                "provider_concurrency_query_reserved must be zero when provider_concurrency_max_outbound is zero"
                    .into(),
            );
        }
        (max, reserved) if reserved >= max => {
            return Err(
                "provider_concurrency_query_reserved must be smaller than provider_concurrency_max_outbound"
                    .into(),
            );
        }
        _ => {}
    }
    if settings.provider_concurrency_acquire_timeout_ms == 0 {
        return Err("provider_concurrency_acquire_timeout_ms must be greater than zero".into());
    }
    if settings.provider_concurrency_registry_max_entries == 0 {
        return Err("provider_concurrency_registry_max_entries must be greater than zero".into());
    }
    if settings.provider_concurrency_registry_idle_ttl_seconds == 0 {
        return Err(
            "provider_concurrency_registry_idle_ttl_seconds must be greater than zero".into()
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

fn validate_query_rerank_settings(settings: &Settings) -> Result<(), String> {
    if !settings.query_rerank_enabled
        && settings.query_semantic_rerank_mode != SemanticRerankMode::Off
    {
        return Err(
            "query_semantic_rerank_mode requires query_rerank_enabled=true for shadow or active"
                .into(),
        );
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
#[allow(
    clippy::expect_used,
    reason = "unit-test module uses panic-style diagnostics to identify broken fixture invariants"
)]
mod tests;
