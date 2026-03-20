use std::sync::Arc;

use crate::{
    app::config::{Settings, UiBootstrapAdmin},
    domains::provider_profiles::{EffectiveProviderProfile, RuntimeProviderProfileDefaults},
    infra::{graph_store::GraphStore, neo4j_store::Neo4jStore, persistence::Persistence},
    integrations::llm::{LlmGateway, UnifiedGateway},
    services::{
        ai_catalog_service::AiCatalogService, audit_service::AuditService,
        billing_service::BillingService, catalog_service::CatalogService,
        collection_settlement::CollectionSettlementService, content_service::ContentService,
        document_accounting::DocumentAccountingService,
        document_reconciliation::DocumentReconciliationService,
        documents_workspace::DocumentsWorkspaceService, extract_service::ExtractService,
        extraction_recovery::ExtractionRecoveryService,
        graph_diagnostics_snapshot::GraphDiagnosticsSnapshotService,
        graph_projection_guard::GraphProjectionGuardService,
        graph_quality_guard::GraphQualityGuardService,
        graph_reconciliation_scope::GraphReconciliationScopeService, graph_service::GraphService,
        graph_summary::GraphSummaryService, iam_service::IamService,
        ingest_activity::IngestActivityService, ingest_service::IngestService,
        mcp_memory::McpMemoryService, operator_warning::OperatorWarningService,
        ops_service::OpsService, pricing_catalog::PricingCatalogService,
        provider_failure_classification::ProviderFailureClassificationService,
        query_intelligence::QueryIntelligenceService, query_service::QueryService,
        queue_isolation::QueueIsolationService, search_service::SearchService,
        terminal_settlement::TerminalSettlementService,
    },
};

pub const UI_SESSION_COOKIE_NAME: &str = "rustrag_ui_session";

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
    pub neo4j_uri: String,
    pub neo4j_database: String,
    pub neo4j_max_connections: usize,
    pub live_validation_enabled: bool,
}

#[derive(Clone)]
pub struct PricingCatalogBootstrapSettings {
    pub seed_from_env: bool,
    pub default_currency: String,
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
    pub fn max_upload_file_bytes(&self) -> u64 {
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
    pub queue_isolation_enabled: bool,
    pub minimum_slice_capacity: usize,
    pub total_worker_slots: usize,
    pub token_touch_min_interval_seconds: u64,
    pub heartbeat_write_min_interval_seconds: u64,
    pub graph_progress_checkpoint_interval_seconds: u64,
}

#[derive(Clone)]
pub struct ResolveSettleBlockersSettings {
    pub projection_retry_limit: usize,
    pub diagnostics_snapshot_stale_after_seconds: u64,
    pub provider_request_size_soft_limit_bytes: usize,
    pub provider_timeout_retry_limit: usize,
    pub extraction_resume_downgrade_level_one_after_replays: usize,
    pub extraction_resume_downgrade_level_two_after_replays: usize,
}

#[derive(Clone, Default)]
pub struct LifecycleServices {
    pub document_accounting: DocumentAccountingService,
    pub document_reconciliation: DocumentReconciliationService,
    pub pricing_catalog: PricingCatalogService,
}

#[derive(Clone, Default)]
pub struct RetrievalIntelligenceServices {
    pub query_intelligence: QueryIntelligenceService,
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
pub struct McpMemoryServices {
    pub memory: McpMemoryService,
}

#[derive(Clone, Default)]
pub struct CanonicalServices {
    pub catalog: CatalogService,
    pub iam: IamService,
    pub ai_catalog: AiCatalogService,
    pub content: ContentService,
    pub ingest: IngestService,
    pub extract: ExtractService,
    pub graph: GraphService,
    pub search: SearchService,
    pub query: QueryService,
    pub billing: BillingService,
    pub ops: OpsService,
    pub audit: AuditService,
}

#[derive(Clone, Default)]
pub struct PipelineHardeningServices {
    pub queue_isolation: QueueIsolationService,
    pub collection_settlement: CollectionSettlementService,
    pub operator_warning: OperatorWarningService,
}

#[derive(Clone, Default)]
pub struct ResolveSettleBlockersServices {
    pub terminal_settlement: TerminalSettlementService,
    pub graph_projection_guard: GraphProjectionGuardService,
    pub graph_diagnostics_snapshot: GraphDiagnosticsSnapshotService,
    pub provider_failure_classification: ProviderFailureClassificationService,
    pub documents_workspace: DocumentsWorkspaceService,
}

#[derive(Clone)]
pub struct AppState {
    pub settings: Settings,
    pub persistence: Persistence,
    pub llm_gateway: Arc<dyn LlmGateway>,
    pub graph_store: Arc<dyn GraphStore>,
    pub ui_runtime: UiRuntimeSettings,
    pub ui_bootstrap_admin: Option<UiBootstrapAdmin>,
    pub ui_session_cookie: UiSessionCookieConfig,
    pub graph_runtime: GraphRuntimeSettings,
    pub pricing_catalog_bootstrap: PricingCatalogBootstrapSettings,
    pub lifecycle_services: LifecycleServices,
    pub retrieval_intelligence: RetrievalIntelligenceSettings,
    pub retrieval_intelligence_services: RetrievalIntelligenceServices,
    pub bulk_ingest_hardening: BulkIngestHardeningSettings,
    pub bulk_ingest_hardening_services: BulkIngestHardeningServices,
    pub mcp_memory: McpMemorySettings,
    pub mcp_memory_services: McpMemoryServices,
    pub canonical_services: CanonicalServices,
    pub pipeline_hardening: PipelineHardeningSettings,
    pub pipeline_hardening_services: PipelineHardeningServices,
    pub resolve_settle_blockers: ResolveSettleBlockersSettings,
    pub resolve_settle_blockers_services: ResolveSettleBlockersServices,
    pub runtime_provider_defaults: RuntimeProviderProfileDefaults,
}

impl AppState {
    #[must_use]
    pub fn from_dependencies(
        settings: Settings,
        persistence: Persistence,
        graph_store: Arc<dyn GraphStore>,
    ) -> Self {
        let bootstrap_settings = settings.bootstrap_settings();
        let ai_catalog_validation = settings.ai_catalog_validation_settings();
        let public_origin_settings = settings.public_origin_settings();
        let ui_bootstrap_admin = bootstrap_settings.legacy_ui_bootstrap_admin;
        let ui_runtime = UiRuntimeSettings {
            frontend_origin: public_origin_settings.raw_frontend_origin,
            default_locale: settings.ui_default_locale.clone(),
            upload_max_size_mb: settings.upload_max_size_mb,
        };
        let ui_session_cookie = UiSessionCookieConfig {
            name: UI_SESSION_COOKIE_NAME,
            ttl_hours: settings.ui_session_ttl_hours,
        };
        let graph_runtime = GraphRuntimeSettings {
            neo4j_uri: settings.neo4j_uri.clone(),
            neo4j_database: settings.neo4j_database.clone(),
            neo4j_max_connections: settings.neo4j_max_connections,
            live_validation_enabled: settings.runtime_live_validation_enabled,
        };
        let pricing_catalog_bootstrap = PricingCatalogBootstrapSettings {
            seed_from_env: ai_catalog_validation.seed_from_env,
            default_currency: ai_catalog_validation.default_currency,
        };
        let lifecycle_services = LifecycleServices::default();
        let retrieval_intelligence = RetrievalIntelligenceSettings {
            query_intent_cache_ttl_hours: settings.runtime_query_intent_cache_ttl_hours,
            query_intent_cache_max_entries_per_library: settings
                .runtime_query_intent_cache_max_entries_per_library,
            rerank_enabled: settings.runtime_query_rerank_enabled,
            rerank_candidate_limit: settings.runtime_query_rerank_candidate_limit,
            balanced_context_enabled: settings.runtime_query_balanced_context_enabled,
            extraction_recovery_enabled: settings.runtime_graph_extract_recovery_enabled,
            extraction_recovery_max_attempts: settings.runtime_graph_extract_recovery_max_attempts,
            summary_refresh_batch_size: settings.runtime_graph_summary_refresh_batch_size,
            targeted_reconciliation_enabled: settings.runtime_graph_targeted_reconciliation_enabled,
            targeted_reconciliation_max_targets: settings
                .runtime_graph_targeted_reconciliation_max_targets,
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
        let mcp_memory = McpMemorySettings {
            default_read_window_chars: settings.mcp_memory_default_read_window_chars,
            max_read_window_chars: settings.mcp_memory_max_read_window_chars,
            default_search_limit: settings.mcp_memory_default_search_limit,
            max_search_limit: settings.mcp_memory_max_search_limit,
            idempotency_retention_hours: settings.mcp_memory_idempotency_retention_hours,
            audit_enabled: settings.mcp_memory_audit_enabled,
            upload_max_size_mb: settings.upload_max_size_mb,
        };
        let mcp_memory_services =
            McpMemoryServices { memory: McpMemoryService::new(mcp_memory.clone()) };
        let canonical_services = CanonicalServices {
            catalog: CatalogService::new(),
            iam: IamService::new(),
            ai_catalog: AiCatalogService::new(),
            content: ContentService::new(),
            ingest: IngestService::new(),
            extract: ExtractService::new(),
            graph: GraphService::new(),
            search: SearchService::new(),
            query: QueryService::new(),
            billing: BillingService::new(),
            ops: OpsService::new(),
            audit: AuditService::new(),
        };
        let pipeline_hardening = PipelineHardeningSettings {
            queue_isolation_enabled: true,
            minimum_slice_capacity: 1,
            total_worker_slots: settings.ingestion_worker_concurrency.max(1),
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
        let pipeline_hardening_services = PipelineHardeningServices {
            queue_isolation: QueueIsolationService::new(
                pipeline_hardening.total_worker_slots,
                pipeline_hardening.minimum_slice_capacity,
            ),
            collection_settlement: CollectionSettlementService::new(),
            operator_warning: OperatorWarningService::new(),
        };
        let resolve_settle_blockers = ResolveSettleBlockersSettings {
            projection_retry_limit: 3,
            diagnostics_snapshot_stale_after_seconds: settings
                .ingestion_worker_heartbeat_interval_seconds
                .saturating_mul(3)
                .max(3),
            provider_request_size_soft_limit_bytes: 256 * 1024,
            provider_timeout_retry_limit: 1,
            extraction_resume_downgrade_level_one_after_replays: settings
                .runtime_graph_extract_resume_downgrade_level_one_after_replays,
            extraction_resume_downgrade_level_two_after_replays: settings
                .runtime_graph_extract_resume_downgrade_level_two_after_replays,
        };
        let resolve_settle_blockers_services = ResolveSettleBlockersServices {
            terminal_settlement: TerminalSettlementService::new(),
            graph_projection_guard: GraphProjectionGuardService::new(
                resolve_settle_blockers.projection_retry_limit,
            ),
            graph_diagnostics_snapshot: GraphDiagnosticsSnapshotService::new(
                resolve_settle_blockers.diagnostics_snapshot_stale_after_seconds,
            ),
            provider_failure_classification: ProviderFailureClassificationService::new(
                resolve_settle_blockers.provider_request_size_soft_limit_bytes,
            ),
            documents_workspace: DocumentsWorkspaceService::new(),
        };
        let runtime_provider_defaults = RuntimeProviderProfileDefaults::from_settings(&settings);

        Self {
            llm_gateway: Arc::new(UnifiedGateway::from_settings(&settings)),
            graph_store,
            settings,
            persistence,
            ui_runtime,
            ui_bootstrap_admin,
            ui_session_cookie,
            graph_runtime,
            pricing_catalog_bootstrap,
            lifecycle_services,
            retrieval_intelligence,
            retrieval_intelligence_services,
            bulk_ingest_hardening,
            bulk_ingest_hardening_services,
            mcp_memory,
            mcp_memory_services,
            canonical_services,
            pipeline_hardening,
            pipeline_hardening_services,
            resolve_settle_blockers,
            resolve_settle_blockers_services,
            runtime_provider_defaults,
        }
    }

    /// Creates shared application state and initializes persistence/gateway dependencies.
    ///
    /// # Errors
    /// Returns any initialization error from persistence setup.
    pub async fn new(settings: Settings) -> anyhow::Result<Self> {
        let persistence = Persistence::connect(&settings).await?;
        if settings.destructive_fresh_bootstrap_settings().allow_legacy_startup_side_effects
            && crate::infra::persistence::legacy_runtime_repair_tables_present(
                &persistence.postgres,
            )
            .await?
        {
            crate::infra::repositories::repair_queued_runtime_ingestion_job_attempt_counts(
                &persistence.postgres,
            )
            .await
            .map_err(|error| {
                anyhow::anyhow!("failed to repair queued runtime ingestion attempts: {error}")
            })?;
        }
        let graph_store = Arc::new(Neo4jStore::connect(&settings)?);
        graph_store.ping().await?;
        Ok(Self::from_dependencies(settings, persistence, graph_store))
    }

    #[must_use]
    pub fn effective_provider_profile(&self) -> EffectiveProviderProfile {
        self.runtime_provider_defaults.effective_profile()
    }
}
