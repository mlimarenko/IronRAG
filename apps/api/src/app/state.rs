use std::sync::Arc;
use zeroize::Zeroize as _;

use crate::{
    agent_runtime::{
        AgentRuntime,
        default_policy::{DefaultRuntimePolicy, DefaultRuntimePolicyRules},
        tasks::register_task_catalog,
    },
    app::config::{
        INGEST_GRAPH_EXTRACT_WAITING_FACTOR, McpHttpOriginPolicy, OidcSettings,
        RuntimeHookBehavior, Settings, UiBootstrapAdmin, UiBootstrapAiSetup,
        resolve_ingestion_graph_extract_parallelism,
    },
    domains::agent_runtime::{RuntimeDecisionTargetKind, RuntimeTaskKind},
    infra::{
        knowledge_plane::{ContextStoreRef, DocumentStoreRef, GraphStoreRef, SearchStoreRef},
        persistence::{self, Persistence},
        postgres::{
            pg_context_store::PgContextStore, pg_document_store::PgDocumentStore,
            pg_graph_store::PgGraphStore, pg_search_store::PgSearchStore,
        },
    },
    integrations::llm::{LlmGateway, UnifiedGateway},
    integrations::provider_budget::ProviderBudgetRegistry,
    services::{
        ai_catalog_service::AiCatalogService,
        catalog_service::CatalogService,
        content::service::ContentService,
        content::storage::ContentStorageService,
        graph::projection_guard::GraphWriteGuardService,
        graph::quality_guard::GraphQualityGuardService,
        graph::reconciliation_scope::GraphReconciliationScopeService,
        graph::service::GraphService,
        graph::summary::GraphSummaryService,
        iam::audit::AuditService,
        iam::service::IamService,
        ingest::activity::IngestActivityService,
        ingest::extract::ExtractService,
        ingest::extraction_recovery::ExtractionRecoveryService,
        ingest::service::IngestService,
        ingest::structured_preparation::StructuredPreparationService,
        ingest::technical_facts::TechnicalFactService,
        ingest::web::WebIngestService,
        knowledge::service::KnowledgeService,
        ops::billing::BillingService,
        ops::deployment_diagnostics::{DeploymentDiagnosticsService, WorkerRuntimeState},
        ops::provider_failure::ProviderFailureClassificationService,
        ops::release_monitor::ReleaseMonitorService,
        ops::service::OpsService,
        query::search::SearchService,
        query::service::QueryService,
        webhook::WebhookService,
    },
    shared::secret_encryption::CredentialCipher,
};

pub const UI_SESSION_COOKIE_NAME: &str = "ironrag_ui_session";

#[derive(Clone)]
pub struct UiRuntimeSettings {
    pub frontend_origin: String,
    pub default_locale: String,
    pub upload_max_size_mb: u64,
}

#[derive(Clone)]
pub struct UiSessionCookieConfig {
    pub name: &'static str,
    pub secure: bool,
    /// The one place the browser-session idle window and absolute cap are
    /// carried; every issue/validate site reads the policy from here rather
    /// than re-deriving a deadline of its own.
    pub lifetime: crate::domains::iam::SessionLifetimePolicy,
}

#[derive(Clone)]
pub struct GraphRuntimeSettings {
    pub backend_name: String,
}

#[derive(Clone)]
pub struct RetrievalIntelligenceSettings {
    pub query_intent_cache_ttl_hours: u64,
    pub query_intent_cache_max_entries_per_library: usize,
    pub rerank_enabled: bool,
    pub rerank_candidate_limit: usize,
    pub semantic_rerank: SemanticRerankRuntimeSettings,
    pub balanced_context_enabled: bool,
    pub extraction_recovery_enabled: bool,
    pub extraction_recovery_max_attempts: usize,
    pub summary_refresh_batch_size: usize,
    pub targeted_reconciliation_enabled: bool,
    pub targeted_reconciliation_max_targets: usize,
}

#[derive(Clone, Copy)]
pub struct SemanticRerankRuntimeSettings {
    pub mode: crate::domains::query::SemanticRerankMode,
    pub timeout_ms: u64,
    pub candidate_limit: usize,
    pub candidate_text_chars: usize,
    pub total_text_chars: usize,
}

pub(crate) const SEMANTIC_RERANK_HARD_MAX_TIMEOUT_MS: u64 = 5_000;
/// Turn-facing wait budget for the optional semantic rerank. The provider
/// decision budget above may be larger because `shadow` runs off the answer
/// path, but an `active` provider call is never started when it cannot fit
/// this budget, and the answer turn never waits longer than it. The
/// deterministic lexical order is the fail-safe on either side of the gate.
pub(crate) const SEMANTIC_RERANK_TURN_WAIT_MS: u64 = 1_000;
pub(crate) const SEMANTIC_RERANK_HARD_MAX_CANDIDATES: usize = 32;
pub(crate) const SEMANTIC_RERANK_HARD_MAX_CANDIDATE_TEXT_CHARS: usize = 2_400;
pub(crate) const SEMANTIC_RERANK_HARD_MAX_TOTAL_TEXT_CHARS: usize = 32_000;

impl SemanticRerankRuntimeSettings {
    #[must_use]
    pub(crate) fn bounded(self) -> Self {
        Self {
            mode: self.mode,
            timeout_ms: self.timeout_ms.clamp(1, SEMANTIC_RERANK_HARD_MAX_TIMEOUT_MS),
            candidate_limit: self.candidate_limit.clamp(1, SEMANTIC_RERANK_HARD_MAX_CANDIDATES),
            candidate_text_chars: self
                .candidate_text_chars
                .clamp(1, SEMANTIC_RERANK_HARD_MAX_CANDIDATE_TEXT_CHARS),
            total_text_chars: self
                .total_text_chars
                .clamp(1, SEMANTIC_RERANK_HARD_MAX_TOTAL_TEXT_CHARS),
        }
    }
}

#[derive(Clone)]
pub struct BulkIngestHardeningSettings {
    pub document_activity_freshness_seconds: u64,
    pub document_stalled_after_seconds: u64,
    pub graph_filter_empty_relations: bool,
    pub graph_filter_degenerate_self_loops: bool,
    pub graph_convergence_warning_backlog_threshold: usize,
}

#[derive(Clone)]
pub struct RuntimeDiagnosticsSettings {
    pub maximum_payload_bytes: usize,
    pub policy_reason_budget_chars: usize,
}

#[derive(Clone)]
pub struct AgentRuntimeSettings {
    pub max_turns: u8,
    pub max_parallel_actions: u8,
    pub diagnostics: RuntimeDiagnosticsSettings,
    pub hook_behavior: RuntimeHookBehavior,
}

#[derive(Clone)]
pub struct WebIngestRuntimeSettings {
    pub request_timeout_seconds: u64,
    pub max_redirects: usize,
    pub user_agent: String,
}

#[derive(Clone, Debug)]
pub struct McpMemorySettings {
    pub default_read_window_chars: usize,
    pub max_read_window_chars: usize,
    pub default_search_limit: usize,
    pub max_search_limit: usize,
    pub idempotency_retention_hours: u64,
    pub audit_enabled: bool,
    pub upload_max_size_mb: u64,
}

impl McpMemorySettings {
    const MIB: u64 = 1024 * 1024;
    const BODY_ENVELOPE_HEADROOM_BYTES: u64 = 1024 * 1024;

    #[must_use]
    pub const fn max_upload_file_bytes(&self) -> u64 {
        self.upload_max_size_mb.saturating_mul(Self::MIB)
    }

    #[must_use]
    pub const fn max_upload_batch_bytes(&self) -> u64 {
        self.max_upload_file_bytes()
    }

    #[must_use]
    pub fn max_request_body_bytes(&self) -> usize {
        let raw_batch_limit = self.max_upload_batch_bytes();
        let encoded_limit = raw_batch_limit.saturating_add(2).saturating_div(3).saturating_mul(4);
        usize::try_from(encoded_limit.saturating_add(Self::BODY_ENVELOPE_HEADROOM_BYTES))
            .unwrap_or(usize::MAX)
    }
}

#[derive(Clone)]
pub struct PipelineHardeningSettings {
    pub minimum_slice_capacity: usize,
    pub total_worker_slots: usize,
    pub token_touch_min_interval_seconds: u64,
    pub heartbeat_write_min_interval_seconds: u64,
    pub graph_progress_checkpoint_interval_seconds: u64,
}

#[derive(Clone)]
pub struct ResolveSettleBlockersSettings {
    pub projection_retry_limit: usize,
    pub provider_request_size_soft_limit_bytes: usize,
    pub provider_timeout_retry_limit: usize,
    pub extraction_resume_downgrade_level_one_after_replays: usize,
    pub extraction_resume_downgrade_level_two_after_replays: usize,
}

#[derive(Clone, Default)]
pub struct RetrievalIntelligenceServices {
    pub extraction_recovery: ExtractionRecoveryService,
    pub graph_summary: GraphSummaryService,
    pub graph_reconciliation_scope: GraphReconciliationScopeService,
}

#[derive(Clone, Default)]
pub struct BulkIngestHardeningServices {
    pub ingest_activity: IngestActivityService,
    pub graph_quality_guard: GraphQualityGuardService,
}

#[derive(Clone, Default)]
pub struct ApplicationServices {
    pub catalog: CatalogService,
    pub iam: IamService,
    pub ai_catalog: AiCatalogService,
    pub knowledge: KnowledgeService,
    pub content: ContentService,
    pub ingest: IngestService,
    pub extract: ExtractService,
    pub structured_preparation: StructuredPreparationService,
    pub technical_facts: TechnicalFactService,
    pub web_ingest: WebIngestService,
    pub graph: GraphService,
    pub search: SearchService,
    pub query: QueryService,
    pub billing: BillingService,
    pub ops: OpsService,
    pub audit: AuditService,
    pub webhook: WebhookService,
}

#[derive(Clone, Default)]
pub struct ResolveSettleBlockersServices {
    pub graph_projection_guard: GraphWriteGuardService,
    pub provider_failure_classification: ProviderFailureClassificationService,
}

#[derive(Clone)]
pub struct AppState {
    pub settings: Settings,
    pub credential_cipher: CredentialCipher,
    pub persistence: Persistence,
    pub agent_runtime: AgentRuntime,
    pub llm_gateway: Arc<dyn LlmGateway>,
    /// The provider concurrency/rate registry shared with [`Self::llm_gateway`].
    ///
    /// IRG-187's TypeSafe SystemOne decision call is not a chat call and cannot
    /// travel through the gateway, but it targets the same physical provider
    /// endpoint; it takes its outbound permit from this same registry so the
    /// endpoint is protected by one limiter instead of two independent ones.
    pub provider_budget: Arc<ProviderBudgetRegistry>,
    pub content_storage: ContentStorageService,
    pub deployment_diagnostics: DeploymentDiagnosticsService,
    pub worker_runtime: WorkerRuntimeState,
    pub ui_runtime: UiRuntimeSettings,
    pub(crate) mcp_http_origin_policy: McpHttpOriginPolicy,
    /// Whether environment-managed bootstrap admin credentials were present at
    /// startup. This non-secret flag remains available to API authorization
    /// after the plaintext settings fields have been scrubbed.
    pub bootstrap_admin_configured: bool,
    pub ui_bootstrap_admin: Option<UiBootstrapAdmin>,
    /// Complete validated OIDC configuration. Cloned request state shares this
    /// immutable allocation, so the client secret has one owner and is
    /// zeroized only when the final application-state clone drops it.
    pub(crate) oidc: Option<Arc<OidcSettings>>,
    /// Admission guard for browser OIDC login starts. Cloned request state
    /// shares both the local in-flight budget and the issuer-scoped Redis
    /// bucket client rather than accidentally resetting either per request.
    pub(crate) oidc_login_admission: crate::services::iam::oidc::OidcLoginAdmission,
    /// Explicit embedding/test override. Production constructors leave this
    /// empty and bootstrap provider credentials are resolved only inside the
    /// operation that consumes them.
    pub ui_bootstrap_ai_setup: Option<UiBootstrapAiSetup>,
    pub ui_session_cookie: UiSessionCookieConfig,
    pub graph_runtime: GraphRuntimeSettings,
    pub document_store: DocumentStoreRef,
    pub graph_store: GraphStoreRef,
    pub search_store: SearchStoreRef,
    pub context_store: ContextStoreRef,
    pub retrieval_intelligence: RetrievalIntelligenceSettings,
    pub agent_runtime_settings: AgentRuntimeSettings,
    pub retrieval_intelligence_services: RetrievalIntelligenceServices,
    pub bulk_ingest_hardening: BulkIngestHardeningSettings,
    pub web_ingest_runtime: WebIngestRuntimeSettings,
    pub bulk_ingest_hardening_services: BulkIngestHardeningServices,
    pub mcp_memory: McpMemorySettings,
    pub application_services: ApplicationServices,
    pub release_monitor: ReleaseMonitorService,
    pub pipeline_hardening: PipelineHardeningSettings,
    pub resolve_settle_blockers: ResolveSettleBlockersSettings,
    pub resolve_settle_blockers_services: ResolveSettleBlockersServices,
    /// Bounded outermost retrieval-turn admission. Nested grounded-answer
    /// calls reuse the parent permit so the agent loop cannot deadlock.
    pub(crate) query_turn_admission: crate::services::query::turn_admission::QueryTurnAdmission,
    /// Rate limiting for the public login endpoint: per-login and per-source
    /// leaky buckets plus a shared cap on concurrent Argon2id verifications,
    /// checked before any database lookup or password hash (IRG-80).
    pub(crate) login_rate_limiter: crate::services::iam::login_rate_limit::LoginRateLimiter,
    /// Per-library cache of admitted runtime graph projections keyed
    /// by `(library_id, projection_version)`. The projection is
    /// loaded lazily on first use and reused for every subsequent
    /// query against the same version — `grounded_answer` pulled
    /// 100k+ edges from Postgres on every turn before this, which
    /// dominated MCP tool-call latency on large libraries. A version
    /// bump from `services::graph::projection::next_projection_version`
    /// is atomic, so cache hits can never serve a stale graph; on
    /// miss we also evict any older versions for the same library.
    pub runtime_graph_projection_cache:
        crate::services::knowledge::runtime_read::RuntimeGraphProjectionCache,
}

impl AppState {
    pub fn from_dependencies(
        mut settings: Settings,
        persistence: Persistence,
    ) -> anyhow::Result<Self> {
        let credential_cipher = take_credential_cipher(&mut settings)?;
        let bootstrap_secrets = take_bootstrap_secrets(&mut settings);
        let oidc_settings = take_oidc_settings(&mut settings)?;
        Self::from_prepared_dependencies(
            settings,
            persistence,
            credential_cipher,
            bootstrap_secrets,
            oidc_settings,
        )
    }

    /// Points browser SSO at an identity provider the settings loader would
    /// refuse: the in-process mock answers on a plain-HTTP loopback origin,
    /// which `IRONRAG_OIDC_ISSUER_URL` validation rejects by design.
    #[cfg(feature = "test-support")]
    pub fn with_oidc_settings_for_tests(mut self, oidc_settings: OidcSettings) -> Self {
        self.oidc = Some(Arc::new(oidc_settings));
        self
    }

    fn from_prepared_dependencies(
        mut settings: Settings,
        persistence: Persistence,
        credential_cipher: CredentialCipher,
        bootstrap_secrets: BootstrapSecrets,
        oidc_settings: Option<Arc<OidcSettings>>,
    ) -> anyhow::Result<Self> {
        let oidc_login_admission =
            crate::services::iam::oidc::OidcLoginAdmission::new(persistence.redis.clone());
        let graph_extract_parallel_jobs = settings
            .ingestion_max_parallel_jobs_global
            .min(persistence::worker_process_db_job_limit(&settings))
            .max(1);
        let graph_extract_parallelism_per_doc = resolve_ingestion_graph_extract_parallelism(
            settings.ingestion_graph_extract_parallelism_per_doc,
            graph_extract_parallel_jobs,
            settings.provider_concurrency_max_outbound,
            settings.provider_concurrency_query_reserved,
        )
        .map_err(anyhow::Error::msg)?;
        let provider_ingest_lane_capacity = settings
            .provider_concurrency_max_outbound
            .checked_sub(settings.provider_concurrency_query_reserved)
            .filter(|_| settings.provider_concurrency_max_outbound > 0);
        tracing::info!(
            configured_parallelism_per_doc = settings.ingestion_graph_extract_parallelism_per_doc,
            effective_parallelism_per_doc = graph_extract_parallelism_per_doc,
            parallel_jobs = graph_extract_parallel_jobs,
            ?provider_ingest_lane_capacity,
            waiting_factor = INGEST_GRAPH_EXTRACT_WAITING_FACTOR,
            "ingest graph extraction parallelism resolved",
        );
        settings.ingestion_graph_extract_parallelism_per_doc = graph_extract_parallelism_per_doc;
        let public_origin_settings = settings.public_origin_settings();
        let mcp_http_origin_policy =
            settings.mcp_http_origin_policy().map_err(anyhow::Error::msg)?;
        let content_storage = ContentStorageService::from_settings(&settings)?;
        let deployment_diagnostics = DeploymentDiagnosticsService::new();
        let worker_runtime = WorkerRuntimeState::default();
        let ui_runtime = UiRuntimeSettings {
            frontend_origin: public_origin_settings.raw_frontend_origin,
            default_locale: settings.ui_default_locale.clone(),
            upload_max_size_mb: settings.upload_max_size_mb,
        };
        let ui_session_cookie = UiSessionCookieConfig {
            name: UI_SESSION_COOKIE_NAME,
            secure: public_origin_settings.session_cookie_secure,
            lifetime: crate::domains::iam::SessionLifetimePolicy {
                idle_ttl_hours: settings.ui_session_idle_ttl_hours,
                absolute_ttl_hours: settings.ui_session_absolute_ttl_hours,
            },
        };
        let graph_runtime = GraphRuntimeSettings { backend_name: "postgres".to_string() };
        let document_store: DocumentStoreRef =
            Arc::new(PgDocumentStore { pool: persistence.postgres.clone() });
        let graph_store: GraphStoreRef =
            Arc::new(PgGraphStore { pool: persistence.postgres.clone() });
        let search_store: SearchStoreRef =
            Arc::new(PgSearchStore { pool: persistence.postgres.clone() });
        let context_store: ContextStoreRef =
            Arc::new(PgContextStore { pool: persistence.postgres.clone() });
        let retrieval_intelligence = RetrievalIntelligenceSettings {
            query_intent_cache_ttl_hours: settings.query_intent_cache_ttl_hours,
            query_intent_cache_max_entries_per_library: settings
                .query_intent_cache_max_entries_per_library,
            rerank_enabled: settings.query_rerank_enabled,
            rerank_candidate_limit: settings.query_rerank_candidate_limit,
            semantic_rerank: SemanticRerankRuntimeSettings {
                mode: settings.query_semantic_rerank_mode,
                timeout_ms: settings.query_semantic_rerank_timeout_ms,
                candidate_limit: settings.query_semantic_rerank_candidate_limit,
                candidate_text_chars: settings.query_semantic_rerank_candidate_text_chars,
                total_text_chars: settings.query_semantic_rerank_total_text_chars,
            }
            .bounded(),
            balanced_context_enabled: settings.query_balanced_context_enabled,
            extraction_recovery_enabled: settings.runtime_graph_extract_recovery_enabled,
            extraction_recovery_max_attempts: settings.runtime_graph_extract_recovery_max_attempts,
            summary_refresh_batch_size: settings.runtime_graph_summary_refresh_batch_size,
            targeted_reconciliation_enabled: settings.runtime_graph_targeted_reconciliation_enabled,
            targeted_reconciliation_max_targets: settings
                .runtime_graph_targeted_reconciliation_max_targets,
        };
        let agent_runtime_settings = AgentRuntimeSettings {
            max_turns: settings.runtime_agent_max_turns,
            max_parallel_actions: settings.runtime_agent_max_parallel_actions,
            diagnostics: RuntimeDiagnosticsSettings {
                maximum_payload_bytes: settings.runtime_maximum_diagnostic_payload_bytes(),
                policy_reason_budget_chars: settings.runtime_policy_reason_budget_chars,
            },
            hook_behavior: settings.runtime_hook_behavior(),
        };
        let retrieval_intelligence_services = RetrievalIntelligenceServices::default();
        let bulk_ingest_hardening = BulkIngestHardeningSettings {
            document_activity_freshness_seconds: settings
                .runtime_document_activity_freshness_seconds,
            document_stalled_after_seconds: settings.runtime_document_stalled_after_seconds,
            graph_filter_empty_relations: settings.runtime_graph_filter_empty_relations,
            graph_filter_degenerate_self_loops: settings.runtime_graph_filter_degenerate_self_loops,
            graph_convergence_warning_backlog_threshold: settings
                .runtime_graph_convergence_warning_backlog_threshold,
        };
        let bulk_ingest_hardening_services = BulkIngestHardeningServices {
            ingest_activity: IngestActivityService::new(
                bulk_ingest_hardening.document_activity_freshness_seconds,
                bulk_ingest_hardening.document_stalled_after_seconds,
            ),
            graph_quality_guard: GraphQualityGuardService::new(
                bulk_ingest_hardening.graph_filter_empty_relations,
                bulk_ingest_hardening.graph_filter_degenerate_self_loops,
            ),
        };
        let web_ingest_runtime = WebIngestRuntimeSettings {
            request_timeout_seconds: settings.web_ingest_http_timeout_seconds,
            max_redirects: settings.web_ingest_max_redirects,
            user_agent: settings.web_ingest_user_agent.clone(),
        };
        let mcp_memory = McpMemorySettings {
            default_read_window_chars: settings.mcp_memory_default_read_window_chars,
            max_read_window_chars: settings.mcp_memory_max_read_window_chars,
            default_search_limit: settings.mcp_memory_default_search_limit,
            max_search_limit: settings.mcp_memory_max_search_limit,
            idempotency_retention_hours: settings.mcp_memory_idempotency_retention_hours,
            audit_enabled: settings.mcp_memory_audit_enabled,
            upload_max_size_mb: settings.upload_max_size_mb,
        };
        let release_monitor = ReleaseMonitorService::new(
            settings.release_check_repository.clone(),
            settings.release_check_interval_hours,
        );
        let application_services = ApplicationServices {
            catalog: CatalogService::new(),
            iam: IamService::new(),
            ai_catalog: AiCatalogService::new(),
            knowledge: KnowledgeService::new(),
            content: ContentService::new(),
            ingest: IngestService::new(),
            extract: ExtractService::new(),
            structured_preparation: StructuredPreparationService::with_chunking(
                settings.chunking_max_chars,
                settings.chunking_overlap_chars,
            ),
            technical_facts: TechnicalFactService::new(),
            web_ingest: WebIngestService::new(
                crate::services::ingest::web::WebIngestRuntimeSettings {
                    request_timeout_seconds: web_ingest_runtime.request_timeout_seconds,
                    max_redirects: web_ingest_runtime.max_redirects,
                    user_agent: web_ingest_runtime.user_agent.clone(),
                },
            ),
            graph: GraphService::new(),
            search: SearchService::new(),
            query: QueryService::new(),
            billing: BillingService::new(),
            ops: OpsService::new(),
            audit: AuditService::new(),
            webhook: WebhookService::new(),
        };
        let pipeline_hardening = PipelineHardeningSettings {
            minimum_slice_capacity: 1,
            total_worker_slots: settings.ingestion_max_parallel_jobs_global.max(1),
            token_touch_min_interval_seconds: settings
                .ingestion_worker_heartbeat_interval_seconds
                .max(1),
            heartbeat_write_min_interval_seconds: settings
                .ingestion_worker_heartbeat_interval_seconds
                .max(1),
            graph_progress_checkpoint_interval_seconds: settings
                .ingestion_worker_heartbeat_interval_seconds
                .max(1),
        };
        let resolve_settle_blockers = ResolveSettleBlockersSettings {
            projection_retry_limit: 3,
            provider_request_size_soft_limit_bytes: 256 * 1024,
            provider_timeout_retry_limit: 1,
            extraction_resume_downgrade_level_one_after_replays: settings
                .runtime_graph_extract_resume_downgrade_level_one_after_replays,
            extraction_resume_downgrade_level_two_after_replays: settings
                .runtime_graph_extract_resume_downgrade_level_two_after_replays,
        };
        let resolve_settle_blockers_services = ResolveSettleBlockersServices {
            graph_projection_guard: GraphWriteGuardService::new(
                resolve_settle_blockers.projection_retry_limit,
            ),
            provider_failure_classification: ProviderFailureClassificationService::new(
                resolve_settle_blockers.provider_request_size_soft_limit_bytes,
            ),
        };
        let agent_runtime = AgentRuntime::with_defaults();
        let agent_runtime = AgentRuntime::new(
            register_task_catalog(agent_runtime.registry()),
            Arc::new(DefaultRuntimePolicy::new(
                agent_runtime_settings.diagnostics.policy_reason_budget_chars,
                DefaultRuntimePolicyRules::new(
                    parse_runtime_task_kind_list(
                        settings.runtime_policy_reject_task_kinds.as_ref(),
                    ),
                    parse_runtime_decision_target_kind_list(
                        settings.runtime_policy_reject_target_kinds.as_ref(),
                    ),
                ),
            )),
            agent_runtime.hooks(),
        );
        let provider_gateway = UnifiedGateway::from_settings(&settings)?;
        let provider_budget = provider_gateway.provider_budget();
        let llm_gateway: Arc<dyn LlmGateway> = Arc::new(provider_gateway);
        let query_turn_admission = crate::services::query::turn_admission::QueryTurnAdmission::new(
            crate::services::query::turn_admission::QueryTurnAdmissionConfig::from_settings(
                settings.query_turn_max_concurrent,
                settings.query_turn_admission_queue,
                settings.query_turn_admission_timeout_ms,
            )
            .map_err(anyhow::Error::msg)?,
        );
        let login_rate_limiter = crate::services::iam::login_rate_limit::LoginRateLimiter::new(
            crate::services::iam::login_rate_limit::LoginRateLimiterConfig::from_settings(
                &settings,
            )
            .map_err(anyhow::Error::msg)?,
        )
        .map_err(anyhow::Error::msg)?;
        Ok(Self {
            agent_runtime,
            llm_gateway,
            provider_budget,
            content_storage,
            deployment_diagnostics,
            worker_runtime,
            settings,
            credential_cipher,
            persistence,
            ui_runtime,
            mcp_http_origin_policy,
            bootstrap_admin_configured: bootstrap_secrets.admin_configured,
            ui_bootstrap_admin: bootstrap_secrets.startup_admin,
            oidc: oidc_settings,
            oidc_login_admission,
            ui_bootstrap_ai_setup: None,
            ui_session_cookie,
            graph_runtime,
            document_store,
            graph_store,
            search_store,
            context_store,
            retrieval_intelligence,
            agent_runtime_settings,
            retrieval_intelligence_services,
            bulk_ingest_hardening,
            web_ingest_runtime,
            bulk_ingest_hardening_services,
            mcp_memory,
            application_services,
            release_monitor,
            pipeline_hardening,
            resolve_settle_blockers,
            resolve_settle_blockers_services,
            query_turn_admission,
            login_rate_limiter,
            runtime_graph_projection_cache:
                crate::services::knowledge::runtime_read::RuntimeGraphProjectionCache::default(),
        })
    }

    /// Creates shared application state and initializes persistence/gateway dependencies.
    ///
    /// # Errors
    /// Returns any initialization error from persistence setup.
    pub async fn new(mut settings: Settings) -> anyhow::Result<Self> {
        let credential_cipher = take_credential_cipher(&mut settings)?;
        let bootstrap_secrets = take_bootstrap_secrets(&mut settings);
        let oidc_settings = take_oidc_settings(&mut settings)?;
        let persistence = Persistence::connect(&settings).await?;
        Self::from_prepared_dependencies(
            settings,
            persistence,
            credential_cipher,
            bootstrap_secrets,
            oidc_settings,
        )
    }

    /// Creates shared state while the caller retains its `Settings` value.
    ///
    /// The raw credential keyring is parsed, removed and zeroized before cloning
    /// the remaining non-secret settings into application state. This is the
    /// required constructor for startup paths that continue using settings
    /// after state creation.
    ///
    /// # Errors
    /// Returns any credential parsing or persistence initialization error.
    pub async fn new_from_retained_settings(settings: &mut Settings) -> anyhow::Result<Self> {
        let credential_cipher = take_credential_cipher(settings)?;
        let bootstrap_secrets = take_bootstrap_secrets(settings);
        let oidc_settings = take_oidc_settings(settings)?;
        let persistence = Persistence::connect(settings).await?;
        Self::from_prepared_dependencies(
            settings.clone(),
            persistence,
            credential_cipher,
            bootstrap_secrets,
            oidc_settings,
        )
    }
}

struct BootstrapSecrets {
    admin_configured: bool,
    startup_admin: Option<UiBootstrapAdmin>,
}

fn take_bootstrap_secrets(settings: &mut Settings) -> BootstrapSecrets {
    let resolved_admin = settings.resolved_ui_bootstrap_admin();
    let admin_configured = resolved_admin.is_some();
    let startup_admin = (settings.service_role == "startup").then_some(resolved_admin).flatten();
    settings.discard_ui_bootstrap_admin_secrets();
    BootstrapSecrets { admin_configured, startup_admin }
}

fn take_oidc_settings(settings: &mut Settings) -> anyhow::Result<Option<Arc<OidcSettings>>> {
    let oidc_settings =
        settings.resolved_oidc_settings().map_err(anyhow::Error::msg)?.map(Arc::new);
    settings.discard_oidc_client_secret();
    Ok(oidc_settings)
}

fn take_credential_cipher(settings: &mut Settings) -> anyhow::Result<CredentialCipher> {
    let active_key_id = settings.credential_master_key_id.take();
    let mut encoded_master_key = settings.credential_master_key.take();
    let mut encoded_previous_keys = settings.credential_previous_master_keys.take();
    let parsed = CredentialCipher::from_keyring_base64(
        active_key_id.as_deref(),
        encoded_master_key.as_deref(),
        encoded_previous_keys.as_deref(),
    )
    .map_err(|error| anyhow::anyhow!(error));
    if let Some(encoded_master_key) = encoded_master_key.as_mut() {
        encoded_master_key.zeroize();
    }
    if let Some(encoded_previous_keys) = encoded_previous_keys.as_mut() {
        encoded_previous_keys.zeroize();
    }
    parsed
}

fn parse_runtime_task_kind_list(value: Option<&String>) -> Vec<RuntimeTaskKind> {
    parse_runtime_policy_list(value, |item| item.parse::<RuntimeTaskKind>().ok())
}

fn parse_runtime_decision_target_kind_list(
    value: Option<&String>,
) -> Vec<RuntimeDecisionTargetKind> {
    parse_runtime_policy_list(value, |item| item.parse::<RuntimeDecisionTargetKind>().ok())
}

fn parse_runtime_policy_list<T>(
    value: Option<&String>,
    parse: impl Fn(&str) -> Option<T>,
) -> Vec<T> {
    value
        .map(std::string::String::as_str)
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .filter_map(parse)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod credential_cipher_tests {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    use super::*;
    use crate::shared::secret_encryption::SecretPurpose;

    #[test]
    fn retained_startup_settings_and_their_clones_cannot_keep_the_raw_key() {
        let mut settings = Settings::from_env().expect("default settings should load");
        settings.discard_credential_master_key();
        settings.credential_master_key = Some(STANDARD.encode([59_u8; 32]));

        let cipher = take_credential_cipher(&mut settings).expect("valid key should parse");
        let cloned_after_preparation = settings.clone();
        let record_id = uuid::Uuid::now_v7();

        assert!(settings.credential_master_key.is_none());
        assert!(settings.credential_master_key_id.is_none());
        assert!(settings.credential_previous_master_keys.is_none());
        assert!(cloned_after_preparation.credential_master_key.is_none());
        assert!(cloned_after_preparation.credential_previous_master_keys.is_none());
        let encrypted = cipher
            .encrypt(SecretPurpose::AiAccountApiKey, record_id, "synthetic-value")
            .expect("configured cipher should encrypt");
        assert_eq!(
            cipher
                .decrypt(SecretPurpose::AiAccountApiKey, record_id, encrypted.as_str())
                .expect("configured cipher should decrypt")
                .expose_secret(),
            "synthetic-value"
        );
        assert!(encrypted.as_str().starts_with("ironrag:enc:v3:xchacha20poly1305:default:"));
    }

    #[test]
    fn prepared_state_uses_active_key_and_retains_previous_keys_only_in_the_cipher() {
        let mut settings = Settings::from_env().expect("default settings should load");
        settings.discard_credential_master_key();
        settings.credential_master_key = Some(STANDARD.encode([61_u8; 32]));
        settings.credential_master_key_id = Some("current-2026".into());
        settings.credential_previous_master_keys =
            Some(format!("old-2026={}", STANDARD.encode([60_u8; 32])));
        let old_cipher = CredentialCipher::from_keyring_base64(
            Some("old-2026"),
            Some(&STANDARD.encode([60_u8; 32])),
            None,
        )
        .expect("old cipher should parse");
        let record_id = uuid::Uuid::now_v7();
        let old_envelope = old_cipher
            .encrypt(SecretPurpose::AiAccountApiKey, record_id, "old-value")
            .expect("old value should encrypt");

        let cipher = take_credential_cipher(&mut settings).expect("rotated keyring should parse");
        let retained = settings.clone();

        assert!(settings.credential_master_key.is_none());
        assert!(settings.credential_master_key_id.is_none());
        assert!(settings.credential_previous_master_keys.is_none());
        assert!(retained.credential_master_key.is_none());
        assert!(retained.credential_previous_master_keys.is_none());
        assert_eq!(
            cipher
                .decrypt(SecretPurpose::AiAccountApiKey, record_id, old_envelope.as_str())
                .expect("previous key should remain available in cipher")
                .expose_secret(),
            "old-value"
        );
        let current = cipher
            .encrypt(SecretPurpose::AiAccountApiKey, record_id, "new-value")
            .expect("active key should encrypt");
        assert!(current.as_str().starts_with("ironrag:enc:v3:xchacha20poly1305:current-2026:"));
    }

    #[test]
    fn bootstrap_admin_plaintext_is_retained_only_by_startup_role() {
        let mut api_settings = Settings::from_env().expect("default settings should load");
        api_settings.service_role = "api".to_string();
        api_settings.ui_bootstrap_admin_login = Some("owner".to_string());
        api_settings.ui_bootstrap_admin_email = Some("owner@example.test".to_string());
        api_settings.ui_bootstrap_admin_password = Some("api-password-regression".to_string());
        api_settings.ui_bootstrap_admin_api_token =
            Some("inrg_0123456789abcdef0123456789abcdef".to_string());

        let api_secrets = take_bootstrap_secrets(&mut api_settings);
        assert!(api_secrets.admin_configured);
        assert!(api_secrets.startup_admin.is_none());
        assert!(api_settings.ui_bootstrap_admin_password.is_none());
        assert!(api_settings.ui_bootstrap_admin_api_token.is_none());

        let mut startup_settings = Settings::from_env().expect("default settings should load");
        startup_settings.service_role = "startup".to_string();
        startup_settings.ui_bootstrap_admin_login = Some("owner".to_string());
        startup_settings.ui_bootstrap_admin_email = Some("owner@example.test".to_string());
        startup_settings.ui_bootstrap_admin_password =
            Some("startup-password-regression".to_string());

        let startup_secrets = take_bootstrap_secrets(&mut startup_settings);
        assert!(startup_secrets.admin_configured);
        assert!(startup_secrets.startup_admin.is_some());
        assert!(startup_settings.ui_bootstrap_admin_password.is_none());
    }
}

#[cfg(test)]
mod oidc_runtime_settings_tests {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    use super::*;

    #[tokio::test]
    async fn cloned_app_state_shares_the_oidc_runtime_settings_allocation() {
        let mut settings = Settings::from_env().expect("default settings should load");
        settings.discard_credential_master_key();
        settings.credential_master_key = Some(STANDARD.encode([71_u8; 32]));
        settings.credential_master_key_id = None;
        settings.credential_previous_master_keys = None;
        settings.service_role = "api".to_string();
        settings.ui_bootstrap_admin_login = None;
        settings.ui_bootstrap_admin_email = None;
        settings.ui_bootstrap_admin_name = None;
        settings.ui_bootstrap_admin_password = None;
        settings.ui_bootstrap_admin_api_token = None;
        settings.frontend_origin = "https://graph.example.test".to_string();
        settings.oidc_issuer_url = Some("https://auth.example.test/realms/ironrag".to_string());
        settings.oidc_client_id = Some("ironrag-test-client".to_string());
        settings.oidc_client_secret = Some("test-only-oidc-client-secret".to_string());
        settings.oidc_redirect_url =
            Some("https://graph.example.test/v1/iam/oidc/callback".to_string());
        settings.oidc_required_group = Some("ironrag-users".to_string());
        settings.oidc_bootstrap_admin_subject = None;

        let postgres = sqlx::PgPool::connect_lazy("postgres://127.0.0.1/ironrag")
            .expect("test-only lazy Postgres pool should parse");
        let redis =
            redis::Client::open("redis://127.0.0.1/").expect("test-only Redis client should parse");
        let state = AppState::from_dependencies(settings, Persistence::for_tests(postgres, redis))
            .expect("OIDC state should prepare without connecting dependencies");
        let cloned_state = state.clone();

        let original_oidc = state.oidc.as_ref().expect("OIDC should be enabled");
        let cloned_oidc = cloned_state.oidc.as_ref().expect("OIDC should be enabled");
        assert!(Arc::ptr_eq(original_oidc, cloned_oidc));
        assert_eq!(Arc::strong_count(original_oidc), 2);
    }
}
