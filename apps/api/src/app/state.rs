#![allow(clippy::missing_const_for_fn, clippy::struct_excessive_bools, clippy::too_many_lines)]

use std::sync::Arc;

use crate::{
    agent_runtime::{
        AgentRuntime,
        default_policy::{DefaultRuntimePolicy, DefaultRuntimePolicyRules},
        tasks::register_task_catalog,
    },
    app::config::{RuntimeHookBehavior, Settings, UiBootstrapAdmin, UiBootstrapAiSetup},
    domains::agent_runtime::{RuntimeDecisionTargetKind, RuntimeTaskKind},
    infra::{
        arangodb::{
            client::ArangoClient, context_store::ArangoContextStore,
            document_store::ArangoDocumentStore, graph_store::ArangoGraphStore,
            search_store::ArangoSearchStore,
        },
        persistence::Persistence,
    },
    integrations::llm::{LlmGateway, UnifiedGateway},
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
    },
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
    pub ttl_hours: u64,
}

#[derive(Clone)]
pub struct GraphRuntimeSettings {
    pub backend_name: String,
}

#[derive(Clone)]
pub struct ArangoRuntimeSettings {
    pub url: String,
    pub database: String,
    pub bootstrap_collections: bool,
    pub bootstrap_views: bool,
    pub bootstrap_graph: bool,
}

#[derive(Clone)]
pub struct RetrievalIntelligenceSettings {
    pub query_intent_cache_ttl_hours: u64,
    pub query_intent_cache_max_entries_per_library: usize,
    pub rerank_enabled: bool,
    pub rerank_candidate_limit: usize,
    pub balanced_context_enabled: bool,
    pub extraction_recovery_enabled: bool,
    pub extraction_recovery_max_attempts: usize,
    pub summary_refresh_batch_size: usize,
    pub targeted_reconciliation_enabled: bool,
    pub targeted_reconciliation_max_targets: usize,
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
    pub fn max_upload_batch_bytes(&self) -> u64 {
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
pub struct CanonicalServices {
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
}

#[derive(Clone, Default)]
pub struct ResolveSettleBlockersServices {
    pub graph_projection_guard: GraphWriteGuardService,
    pub provider_failure_classification: ProviderFailureClassificationService,
}

#[derive(Clone)]
pub struct AppState {
    pub settings: Settings,
    pub persistence: Persistence,
    pub agent_runtime: AgentRuntime,
    pub llm_gateway: Arc<dyn LlmGateway>,
    pub content_storage: ContentStorageService,
    pub deployment_diagnostics: DeploymentDiagnosticsService,
    pub worker_runtime: WorkerRuntimeState,
    pub arango_client: Arc<ArangoClient>,
    pub ui_runtime: UiRuntimeSettings,
    pub ui_bootstrap_admin: Option<UiBootstrapAdmin>,
    pub ui_bootstrap_ai_setup: Option<UiBootstrapAiSetup>,
    pub ui_session_cookie: UiSessionCookieConfig,
    pub arango_runtime: ArangoRuntimeSettings,
    pub graph_runtime: GraphRuntimeSettings,
    pub arango_document_store: ArangoDocumentStore,
    pub arango_graph_store: ArangoGraphStore,
    pub arango_search_store: ArangoSearchStore,
    pub arango_context_store: ArangoContextStore,
    pub retrieval_intelligence: RetrievalIntelligenceSettings,
    pub agent_runtime_settings: AgentRuntimeSettings,
    pub retrieval_intelligence_services: RetrievalIntelligenceServices,
    pub bulk_ingest_hardening: BulkIngestHardeningSettings,
    pub web_ingest_runtime: WebIngestRuntimeSettings,
    pub bulk_ingest_hardening_services: BulkIngestHardeningServices,
    pub mcp_memory: McpMemorySettings,
    pub canonical_services: CanonicalServices,
    pub release_monitor: ReleaseMonitorService,
    pub pipeline_hardening: PipelineHardeningSettings,
    pub resolve_settle_blockers: ResolveSettleBlockersSettings,
    pub resolve_settle_blockers_services: ResolveSettleBlockersServices,
    /// In-memory capture of raw LLM request/response payloads per
    /// assistant execution, used by the chat debug panel. Bounded
    /// FIFO, never persisted.
    pub llm_context_debug: crate::services::query::llm_context_debug::LlmContextDebugStore,
}

impl AppState {
    pub fn from_dependencies(
        settings: Settings,
        persistence: Persistence,
        arango_client: Arc<ArangoClient>,
    ) -> anyhow::Result<Self> {
        let bootstrap_settings = settings.bootstrap_settings();
        let public_origin_settings = settings.public_origin_settings();
        let content_storage = ContentStorageService::from_settings(&settings)?;
        let deployment_diagnostics = DeploymentDiagnosticsService::new();
        let worker_runtime = WorkerRuntimeState::default();
        let ui_bootstrap_admin = bootstrap_settings.ui_bootstrap_admin;
        let ui_bootstrap_ai_setup = settings.resolved_ui_bootstrap_ai_setup();
        let ui_runtime = UiRuntimeSettings {
            frontend_origin: public_origin_settings.raw_frontend_origin,
            default_locale: settings.ui_default_locale.clone(),
            upload_max_size_mb: settings.upload_max_size_mb,
        };
        let ui_session_cookie = UiSessionCookieConfig {
            name: UI_SESSION_COOKIE_NAME,
            ttl_hours: settings.ui_session_ttl_hours,
        };
        let graph_runtime = GraphRuntimeSettings { backend_name: "arangodb".to_string() };
        let arango_runtime = ArangoRuntimeSettings {
            url: settings.arangodb_url.clone(),
            database: settings.arangodb_database.clone(),
            bootstrap_collections: settings.arangodb_bootstrap_collections,
            bootstrap_views: settings.arangodb_bootstrap_views,
            bootstrap_graph: settings.arangodb_bootstrap_graph,
        };
        let arango_document_store = ArangoDocumentStore::new(Arc::clone(&arango_client));
        let arango_graph_store = ArangoGraphStore::new(Arc::clone(&arango_client));
        let arango_search_store = ArangoSearchStore::new(Arc::clone(&arango_client));
        let arango_context_store = ArangoContextStore::new(Arc::clone(&arango_client));
        let retrieval_intelligence = RetrievalIntelligenceSettings {
            query_intent_cache_ttl_hours: settings.query_intent_cache_ttl_hours,
            query_intent_cache_max_entries_per_library: settings
                .query_intent_cache_max_entries_per_library,
            rerank_enabled: settings.query_rerank_enabled,
            rerank_candidate_limit: settings.query_rerank_candidate_limit,
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
        let canonical_services = CanonicalServices {
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
        Ok(Self {
            agent_runtime,
            llm_gateway: Arc::new(UnifiedGateway::from_settings(&settings)),
            content_storage,
            deployment_diagnostics,
            worker_runtime,
            arango_client,
            settings,
            persistence,
            ui_runtime,
            ui_bootstrap_admin,
            ui_bootstrap_ai_setup,
            ui_session_cookie,
            arango_runtime,
            graph_runtime,
            arango_document_store,
            arango_graph_store,
            arango_search_store,
            arango_context_store,
            retrieval_intelligence,
            agent_runtime_settings,
            retrieval_intelligence_services,
            bulk_ingest_hardening,
            web_ingest_runtime,
            bulk_ingest_hardening_services,
            mcp_memory,
            canonical_services,
            release_monitor,
            pipeline_hardening,
            resolve_settle_blockers,
            resolve_settle_blockers_services,
            llm_context_debug:
                crate::services::query::llm_context_debug::LlmContextDebugStore::default(),
        })
    }

    /// Creates shared application state and initializes persistence/gateway dependencies.
    ///
    /// # Errors
    /// Returns any initialization error from persistence setup.
    pub async fn new(settings: Settings) -> anyhow::Result<Self> {
        let persistence = Persistence::connect(&settings).await?;
        let arango_client = Arc::new(ArangoClient::from_settings(&settings)?);
        Self::from_dependencies(settings, persistence, arango_client)
    }
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
