use std::sync::Arc;

use crate::{
    app::config::{Settings, UiBootstrapAdmin},
    domains::provider_profiles::{EffectiveProviderProfile, RuntimeProviderProfileDefaults},
    infra::{graph_store::GraphStore, neo4j_store::Neo4jStore, persistence::Persistence},
    integrations::llm::{LlmGateway, UnifiedGateway},
    services::{
        document_accounting::DocumentAccountingService,
        document_reconciliation::DocumentReconciliationService,
        extraction_recovery::ExtractionRecoveryService,
        graph_quality_guard::GraphQualityGuardService,
        graph_reconciliation_scope::GraphReconciliationScopeService,
        graph_summary::GraphSummaryService, ingest_activity::IngestActivityService,
        pricing_catalog::PricingCatalogService, query_intelligence::QueryIntelligenceService,
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
    pub runtime_provider_defaults: RuntimeProviderProfileDefaults,
}

impl AppState {
    /// Creates shared application state and initializes persistence/gateway dependencies.
    ///
    /// # Errors
    /// Returns any initialization error from persistence setup.
    pub async fn new(settings: Settings) -> anyhow::Result<Self> {
        let persistence = Persistence::connect(&settings).await?;
        let graph_store = Arc::new(Neo4jStore::connect(&settings)?);
        graph_store.ping().await?;
        let ui_bootstrap_admin = settings.resolved_ui_bootstrap_admin();
        let ui_runtime = UiRuntimeSettings {
            frontend_origin: settings.frontend_origin.clone(),
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
            seed_from_env: settings.runtime_pricing_seed_from_env,
            default_currency: settings.runtime_pricing_default_currency.clone(),
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
        let runtime_provider_defaults = RuntimeProviderProfileDefaults::from_settings(&settings);
        Ok(Self {
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
            runtime_provider_defaults,
        })
    }

    #[must_use]
    pub fn effective_provider_profile(&self) -> EffectiveProviderProfile {
        self.runtime_provider_defaults.effective_profile()
    }
}
