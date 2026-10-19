use std::collections::BTreeSet;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde::Deserialize;
use serde::de::{Error as _, MapAccess, Visitor};
use url::Url;
use zeroize::Zeroize as _;

use crate::{
    domains::{
        ai::AiBindingPurpose,
        deployment::{
            ContentStorageProvider, DependencyKind, DependencyMode, DeploymentTopology,
            ServiceRole, StartupAuthorityMode,
        },
        query::SemanticRerankMode,
        recognition::{LibraryRecognitionPolicy, RecognitionEngine},
    },
    shared::auth_tokens::is_well_formed_api_token,
};

mod bootstrap;
mod loading;
mod validation;

pub(crate) use validation::resolve_ingestion_graph_extract_parallelism;

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
const DEFAULT_PROVIDER_CONCURRENCY_MAX_OUTBOUND: i64 = 16;
const DEFAULT_PROVIDER_CONCURRENCY_QUERY_RESERVED: i64 = 4;
const DEFAULT_PROVIDER_CONCURRENCY_ACQUIRE_TIMEOUT_MS: i64 = 30_000;
const DEFAULT_PROVIDER_CONCURRENCY_REGISTRY_MAX_ENTRIES: i64 = 64;
const DEFAULT_PROVIDER_CONCURRENCY_REGISTRY_IDLE_TTL_SECONDS: i64 = 900;
const DEFAULT_PROVIDER_RATE_LIMIT_REQUESTS_PER_SECOND: i64 = 8;
const DEFAULT_PROVIDER_RATE_LIMIT_BURST: i64 = 16;
const DEFAULT_PROVIDER_RETRY_MAX_ATTEMPTS: i64 = 3;
const DEFAULT_PROVIDER_RETRY_INITIAL_BACKOFF_MS: i64 = 200;
const DEFAULT_PROVIDER_RETRY_MAX_BACKOFF_MS: i64 = 5_000;
const MAX_PROVIDER_RETRY_ATTEMPTS: u32 = 10;
const MAX_PROVIDER_RETRY_BACKOFF_MS: u64 = 300_000;
const MAX_PROVIDER_RATE_LIMIT_REQUESTS_PER_SECOND: u32 = 100_000;
const MAX_PROVIDER_RATE_LIMIT_BURST: u32 = 100_000;
pub(crate) const INGEST_GRAPH_EXTRACT_WAITING_FACTOR: usize = 2;
// Public-login rate limiting (IRG-80). Per-login is deliberately tighter than
// per-source: a shared office/NAT egress IP legitimately carries several
// different users' attempts, but no single account should need more than a
// handful of login attempts per minute.
const DEFAULT_LOGIN_RATE_LIMIT_PER_LOGIN_MAX_ATTEMPTS: i64 = 5;
const DEFAULT_LOGIN_RATE_LIMIT_PER_LOGIN_WINDOW_SECONDS: i64 = 60;
const DEFAULT_LOGIN_RATE_LIMIT_PER_SOURCE_MAX_ATTEMPTS: i64 = 20;
const DEFAULT_LOGIN_RATE_LIMIT_PER_SOURCE_WINDOW_SECONDS: i64 = 60;
const DEFAULT_LOGIN_RATE_LIMIT_MAX_CONCURRENT_VERIFICATIONS: i64 = 8;
const DEFAULT_LOGIN_RATE_LIMIT_ADMISSION_QUEUE: i64 = 32;
const DEFAULT_LOGIN_RATE_LIMIT_ADMISSION_TIMEOUT_MS: i64 = 500;
const DEFAULT_LOGIN_RATE_LIMIT_REGISTRY_MAX_ENTRIES: i64 = 10_000;
const DEFAULT_LOGIN_RATE_LIMIT_REGISTRY_IDLE_TTL_SECONDS: i64 = 600;
const MAX_LOGIN_RATE_LIMIT_MAX_ATTEMPTS: u32 = 100_000;
// One owned proxy appends to X-Forwarded-For in the canonical deployment:
// the bundled web nginx in front of the API. An ingress gateway ahead of it
// is a second hop and must be configured explicitly per deployment.
const DEFAULT_LOGIN_RATE_LIMIT_TRUSTED_PROXY_HOPS: i64 = 1;
const MAX_LOGIN_RATE_LIMIT_TRUSTED_PROXY_HOPS: u32 = 8;
pub(crate) const OIDC_CALLBACK_PATH: &str = "/v1/iam/oidc/callback";
const MAX_OIDC_ISSUER_URL_BYTES: usize = 2_048;
const MAX_OIDC_CLIENT_ID_BYTES: usize = 512;
const MAX_OIDC_REQUIRED_GROUP_BYTES: usize = 512;
const MAX_OIDC_BOOTSTRAP_SUBJECT_BYTES: usize = 1_024;

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
struct NormalizedHttpOrigin(String);

impl NormalizedHttpOrigin {
    fn parse(raw: &str) -> Result<Self, String> {
        if raw.is_empty() || raw != raw.trim() {
            return Err("origin must be a non-empty normalized value".into());
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
        let normalized_origin = parsed.origin().ascii_serialization();
        if normalized_origin != raw {
            return Err("origin must use its exact ASCII serialization".into());
        }
        Ok(Self(normalized_origin))
    }
}

/// Exact allowlist for the MCP Streamable HTTP `Origin` security boundary.
///
/// Absence of the header is handled by the transport middleware so native MCP
/// clients remain valid. Any present value must parse to one primary HTTP
/// origin and match this typed set exactly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct McpHttpOriginPolicy {
    allowed_origins: BTreeSet<NormalizedHttpOrigin>,
}

impl McpHttpOriginPolicy {
    pub(crate) fn try_from_origins<'a>(
        allowed_origins: impl IntoIterator<Item = &'a str>,
        api_public_origin: Option<&'a str>,
    ) -> Result<Self, String> {
        let mut parsed_origins = BTreeSet::new();
        for raw in allowed_origins.into_iter().chain(api_public_origin) {
            if raw.is_empty() {
                continue;
            }
            parsed_origins.insert(NormalizedHttpOrigin::parse(raw)?);
        }
        Ok(Self { allowed_origins: parsed_origins })
    }

    #[must_use]
    pub(crate) fn allows(&self, raw: &str) -> bool {
        NormalizedHttpOrigin::parse(raw)
            .ok()
            .is_some_and(|origin| self.allowed_origins.contains(&origin))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DestructiveFreshBootstrapSettings {
    pub required: bool,
}

/// Validated OpenID Connect runtime settings.
///
/// The client secret remains here, rather than in [`Settings`], after the
/// application state is built. Its custom `Debug` implementation ensures a
/// configuration dump cannot accidentally disclose it.
#[derive(PartialEq, Eq)]
pub struct OidcSettings {
    pub issuer_url: String,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_url: String,
    pub required_group: String,
    pub bootstrap_admin_subject: Option<String>,
}

impl std::fmt::Debug for OidcSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OidcSettings")
            .field("issuer_url", &self.issuer_url)
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("redirect_url", &self.redirect_url)
            .field("required_group", &self.required_group)
            .field(
                "bootstrap_admin_subject",
                &self.bootstrap_admin_subject.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

impl Drop for OidcSettings {
    fn drop(&mut self) {
        self.client_secret.zeroize();
    }
}

#[derive(Clone, Deserialize, utoipa::ToSchema)]
pub struct Settings {
    pub bind_addr: String,
    pub service_role: String,
    pub database_url: String,
    pub database_tls_leaf_certificate: Option<String>,
    pub database_max_connections: u32,
    pub api_replicas: usize,
    pub worker_replicas: usize,
    pub knowledge_plane_backend: String,
    pub redis_url: String,
    pub redis_tls_leaf_certificate: Option<String>,
    pub service_name: String,
    pub environment: String,
    pub log_filter: String,
    pub destructive_fresh_bootstrap_required: bool,
    pub frontend_origin: String,
    /// When set, OpenAPI/Swagger uses this value as the only `servers` URL (API origin without a
    /// duplicate `/v1`; paths in the contract already start with `/v1/`). Env: `IRONRAG_OPENAPI_PUBLIC_ORIGIN`.
    pub openapi_public_origin: Option<String>,
    /// Dedicated encryption key for credentials persisted in `PostgreSQL`.
    /// Must be strict standard-base64 for exactly 32 random bytes.
    pub credential_master_key: Option<String>,
    /// Identifier embedded and authenticated in newly encrypted v3 envelopes.
    pub credential_master_key_id: Option<String>,
    /// Sorted `key-id=base64-key` map used only to decrypt and rewrap old values.
    pub credential_previous_master_keys: Option<String>,
    pub ui_session_secret: String,
    pub ui_default_locale: String,
    pub ui_bootstrap_admin_login: Option<String>,
    pub ui_bootstrap_admin_email: Option<String>,
    pub ui_bootstrap_admin_name: Option<String>,
    pub ui_bootstrap_admin_password: Option<String>,
    pub ui_bootstrap_admin_api_token: Option<String>,
    /// OpenID Connect issuer. OIDC is disabled only when all OIDC core fields
    /// are absent; partial configuration is rejected at startup.
    pub oidc_issuer_url: Option<String>,
    pub oidc_client_id: Option<String>,
    pub oidc_client_secret: Option<String>,
    pub oidc_redirect_url: Option<String>,
    pub oidc_required_group: Option<String>,
    /// Stable OIDC `sub` which receives `admin` only at first provisioning.
    pub oidc_bootstrap_admin_subject: Option<String>,
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
    /// Sliding inactivity window: a session dies this many hours after its
    /// last authenticated request.
    pub ui_session_idle_ttl_hours: u64,
    /// Absolute cap: a session dies this many hours after sign-in no matter
    /// how much it was used.
    pub ui_session_absolute_ttl_hours: u64,
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
    /// A (class, scope) row is still scheduled at least this often even when
    /// the library's source generation has not advanced (safety full pass).
    pub maintenance_full_pass_interval_seconds: u64,
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
    /// Token-bucket refill rate for each provider endpoint. `0` disables the
    /// rate budget and requires a zero burst.
    pub provider_rate_limit_requests_per_second: u32,
    /// Maximum request tokens each provider endpoint can accumulate.
    pub provider_rate_limit_burst: u32,
    /// Total attempts allowed for side-effect-free provider operations.
    pub provider_retry_max_attempts: u32,
    /// Initial exponential retry delay before jitter.
    pub provider_retry_initial_backoff_ms: u64,
    /// Upper bound applied to exponential delays when the provider omits
    /// `Retry-After`.
    pub provider_retry_max_backoff_ms: u64,
    /// Concurrent outermost retrieval turns admitted into Postgres/LLM work.
    pub query_turn_max_concurrent: usize,
    /// Extra waiters allowed before a burst is rejected with retryable 503.
    pub query_turn_admission_queue: usize,
    /// How long a queued turn waits for a retrieval permit.
    pub query_turn_admission_timeout_ms: u64,
    /// Login attempts admitted per normalized login before a fully recovered
    /// bucket starts rejecting further attempts with 429. Guards
    /// `POST /v1/iam/sessions` against hammering one account.
    pub login_rate_limit_per_login_max_attempts: u32,
    /// Seconds for a fully drained per-login bucket to refill to capacity.
    pub login_rate_limit_per_login_window_seconds: u64,
    /// Login attempts admitted per source (client IP or trusted proxy hop)
    /// before a fully recovered bucket starts rejecting further attempts.
    /// Set higher than the per-login limit: a shared NAT/office egress IP can
    /// legitimately carry several different users' login attempts.
    pub login_rate_limit_per_source_max_attempts: u32,
    /// Seconds for a fully drained per-source bucket to refill to capacity.
    pub login_rate_limit_per_source_window_seconds: u64,
    /// Concurrent Argon2id password verifications admitted across all login
    /// attempts, regardless of which login or source they belong to. Bounds
    /// worst-case CPU spend on the replica even when both leaky buckets are
    /// individually satisfied.
    pub login_rate_limit_max_concurrent_verifications: usize,
    /// Extra waiters allowed to queue for a verification permit before a
    /// burst is rejected with 429 instead of piling up pending tasks.
    pub login_rate_limit_admission_queue: usize,
    /// How long a queued verification waits for a permit before 429.
    pub login_rate_limit_admission_timeout_ms: u64,
    /// Maximum distinct keys retained by each of the two bucket registries
    /// (login-keyed and source-keyed).
    pub login_rate_limit_registry_max_entries: usize,
    /// Idle bucket retention before eviction from either registry.
    pub login_rate_limit_registry_idle_ttl_seconds: u64,
    /// Number of proxies this deployment owns that *append* their peer to
    /// `X-Forwarded-For` before a login request reaches this process. The
    /// source key is the entry that many positions from the right; anything
    /// further left is client-supplied. `0` ignores the header and keys on
    /// the TCP peer. Over-estimating is the unsafe direction: it lets a
    /// caller pick its own source bucket with a forged leading entry.
    pub login_rate_limit_trusted_proxy_hops: u32,
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "unit-test module uses panic-style diagnostics to identify broken fixture invariants"
)]
mod tests;
