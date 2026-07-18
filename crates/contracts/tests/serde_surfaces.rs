use std::io;

use chrono::{DateTime, TimeZone, Utc};
use rust_decimal as _;
use serde as _;
use utoipa as _;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn utc_datetime(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> Result<DateTime<Utc>, io::Error> {
    Utc.with_ymd_and_hms(year, month, day, hour, minute, second)
        .single()
        .ok_or_else(|| io::Error::other("test fixture contains an invalid UTC date-time"))
}

use ironrag_contracts::{
    admin::{
        AdminCapabilityState, AdminSection, AdminSectionSummary, AdminSurface, AdminViewerSummary,
        CapabilityGate,
    },
    ai::AiBindingPurpose,
    apiref::{ApiReferenceFormat, ApiReferenceStatus, ApiReferenceSurface},
    assistant::{
        AssistantAnswerDisposition, AssistantComposerState, AssistantEvidenceGroup,
        AssistantEvidenceItem, AssistantSessionListItem, AssistantStageItem,
        AssistantWorkspaceSurface,
    },
    auth::{
        AuthenticatedSession, BootstrapStatus, LoginResponse, SessionMode, SessionResolveResponse,
        SessionUser, UiLocale,
    },
    diagnostics::{
        DegradedState, DiagnosticCounter, MessageLevel, OperatorWarning, SurfaceDiagnostics,
        SurfaceHealth,
    },
    documents::{
        DashboardAttentionItem, DashboardSurface, DocumentFilterState, DocumentReadiness,
        DocumentStatus, DocumentSummary, DocumentsOverview, DocumentsSurface,
        LibraryDocumentMetrics, WebIngestRunState, WebIngestRunSummary, WebRunCounts,
    },
    graph::{
        GraphConvergenceStatus, GraphFilterState, GraphNode, GraphNodeType, GraphStatus,
        GraphSurface, GraphWorkbenchSurface,
    },
    provider::ProviderRuntimeProfile,
    shell::{
        LibrarySummary, ShellBootstrap, ShellCapability, ShellRole, ShellViewer, WorkspaceSummary,
    },
};

#[test]
fn assistant_answer_disposition_uses_additive_stable_vocabulary() -> TestResult {
    for (disposition, expected) in [
        (AssistantAnswerDisposition::NonTerminal, "non_terminal"),
        (AssistantAnswerDisposition::FactualReady, "factual_ready"),
        (AssistantAnswerDisposition::SafeFallback, "safe_fallback"),
        (AssistantAnswerDisposition::Clarification, "clarification"),
    ] {
        let value = serde_json::to_value(disposition)?;
        assert_eq!(value, expected);
        assert_eq!(serde_json::from_value::<AssistantAnswerDisposition>(value)?, disposition);
    }
    Ok(())
}

#[test]
fn session_resolve_response_uses_canonical_casing() -> TestResult {
    let session = AuthenticatedSession {
        session_id: Uuid::from_u128(1),
        expires_at: utc_datetime(2026, 4, 4, 10, 0, 0)?,
        user: SessionUser {
            principal_id: Uuid::from_u128(2),
            login: "operator".to_string(),
            email: "operator@example.test".to_string(),
            display_name: "Operator".to_string(),
        },
    };
    let payload = SessionResolveResponse {
        mode: SessionMode::Authenticated,
        locale: UiLocale::En,
        session: Some(session.clone()),
        me: None,
        shell_bootstrap: None,
        bootstrap_status: BootstrapStatus { setup_required: false, ai_setup: None },
        message: Some("ready".to_string()),
    };

    let value = serde_json::to_value(&payload)?;
    assert_eq!(value["mode"], "authenticated");
    assert_eq!(value["bootstrapStatus"]["setupRequired"], false);
    assert_eq!(value["session"]["user"]["displayName"], "Operator");

    let login_response = LoginResponse { session, locale: UiLocale::Ru };
    let login_value = serde_json::to_value(&login_response)?;
    assert_eq!(login_value["locale"], "ru");
    Ok(())
}

#[test]
fn query_compile_ai_binding_purpose_uses_canonical_vocabulary() -> TestResult {
    let value = serde_json::to_value(AiBindingPurpose::QueryCompile)?;
    assert_eq!(value, "query_compile");
    assert_eq!(serde_json::from_value::<AiBindingPurpose>(value)?, AiBindingPurpose::QueryCompile,);
    Ok(())
}

#[test]
fn ai_binding_purpose_from_str_rejects_case_and_whitespace_aliases() {
    for alias in ["Agent", " agent", "agent "] {
        assert!(alias.parse::<AiBindingPurpose>().is_err(), "accepted alias {alias:?}");
    }
    assert_eq!("agent".parse::<AiBindingPurpose>(), Ok(AiBindingPurpose::Agent));
}

#[test]
fn provider_runtime_profile_requires_structured_output() {
    let result = serde_json::from_value::<ProviderRuntimeProfile>(serde_json::json!({
        "kind": "openai_compatible",
        "authScheme": "bearer",
        "tokenLimitParameter": "max_tokens",
        "chatPath": "/chat/completions",
        "embeddingsPath": "/embeddings",
        "modelsPath": "/models"
    }));

    assert!(result.is_err(), "provider runtime profile must fail loud without structuredOutput");
    let error_message = result.err().map(|error| error.to_string()).unwrap_or_default();
    assert!(error_message.contains("structuredOutput"));
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one roundtrip scenario intentionally assembles the complete shell contract surface"
)]
fn shell_and_feature_surfaces_roundtrip() -> TestResult {
    let workspace_id = Uuid::from_u128(10);
    let library_id = Uuid::from_u128(11);
    let viewer_id = Uuid::from_u128(12);

    let shell = ShellBootstrap {
        viewer: ShellViewer {
            principal_id: viewer_id,
            login: "operator".to_string(),
            display_name: "Operator".to_string(),
            access_label: "Admin".to_string(),
            role: ShellRole::Admin,
            is_admin: true,
        },
        locale: UiLocale::En,
        workspaces: vec![WorkspaceSummary {
            id: workspace_id,
            slug: "primary".to_string(),
            name: "Primary".to_string(),
            lifecycle_state: "ready".to_string(),
        }],
        active_workspace_id: Some(workspace_id),
        libraries: vec![LibrarySummary {
            id: library_id,
            workspace_id,
            slug: "docs".to_string(),
            name: "Docs".to_string(),
            description: None,
            lifecycle_state: "ready".to_string(),
            ingestion_ready: true,
            missing_binding_purposes: vec![
                AiBindingPurpose::EmbedChunk,
                AiBindingPurpose::QueryAnswer,
                AiBindingPurpose::ExtractText,
            ],
            query_ready: Some(true),
        }],
        active_library_id: Some(library_id),
        workspace_memberships: Vec::new(),
        effective_grants: Vec::new(),
        capabilities: vec![ShellCapability { key: "admin_access".to_string(), enabled: true }],
        warnings: vec![OperatorWarning {
            code: "foundation".to_string(),
            level: MessageLevel::Info,
            title: "Foundation".to_string(),
            detail: "Rust shell is active.".to_string(),
        }],
    };
    let shell_value = serde_json::to_value(&shell)?;
    assert!(shell_value.get("workspaceMemberships").is_some());
    assert!(shell_value.get("effectiveGrants").is_some());
    assert_eq!(
        shell_value["libraries"][0]["missingBindingPurposes"],
        serde_json::json!(["embed_chunk", "query_answer", "extract_text"]),
    );

    let graph = GraphSurface {
        library_id,
        status: GraphStatus::Partial,
        convergence_status: Some(GraphConvergenceStatus::Degraded),
        warning: Some("Sparse".to_string()),
        node_count: 2,
        relation_count: 1,
        edge_count: 1,
        graph_ready_document_count: 1,
        graph_sparse_document_count: 1,
        typed_fact_document_count: 1,
        updated_at: None,
        nodes: vec![GraphNode {
            id: Uuid::from_u128(20),
            canonical_key: "concept:cutover".to_string(),
            label: "Cutover".to_string(),
            node_type: GraphNodeType::Concept,
            secondary_label: None,
            support_count: 2,
            summary: None,
            filtered_artifact: false,
        }],
        edges: Vec::new(),
        readiness_summary: None,
    };

    let documents = DocumentsSurface {
        overview: DocumentsOverview {
            total_documents: 2,
            ready_documents: 1,
            processing_documents: 1,
            failed_documents: 0,
            graph_sparse_documents: 1,
        },
        filters: DocumentFilterState {
            search_query: Some("cutover".to_string()),
            statuses: vec![DocumentStatus::Ready],
            readiness: vec![DocumentReadiness::GraphReady],
            source_formats: vec!["markdown".to_string()],
        },
        documents: vec![DocumentSummary {
            id: Uuid::from_u128(30),
            workspace_id: Some(workspace_id),
            library_id: Some(library_id),
            file_name: "cutover.md".to_string(),
            file_type: "text/markdown".to_string(),
            file_size: 1024,
            uploaded_at: utc_datetime(2026, 4, 4, 10, 0, 0)?,
            status: DocumentStatus::Ready,
            readiness: DocumentReadiness::GraphReady,
            stage_label: Some("ready".to_string()),
            progress_percent: Some(100),
            cost_usd: Some(1.2),
            failure_message: None,
            can_retry: false,
            prepared_segment_count: Some(3),
            technical_fact_count: Some(2),
            source_format: Some("markdown".to_string()),
        }],
        selected_document_id: None,
        selected_document: None,
        web_runs: vec![WebIngestRunSummary {
            run_id: Uuid::from_u128(31),
            library_id,
            mode: "crawl".to_string(),
            boundary_policy: "same_host".to_string(),
            max_depth: 1,
            max_pages: 8,
            crawl_filter: ironrag_contracts::documents::WebIngestUrlFilter {
                allow_patterns: Vec::new(),
                block_patterns: Vec::new(),
            },
            materialization_filter: ironrag_contracts::documents::WebIngestUrlFilter {
                allow_patterns: Vec::new(),
                block_patterns: Vec::new(),
            },
            run_state: WebIngestRunState::Completed,
            seed_url: "https://example.test".to_string(),
            counts: WebRunCounts {
                discovered: 2,
                eligible: 2,
                processed: 2,
                queued: 0,
                processing: 0,
                duplicates: 0,
                excluded: 0,
                blocked: 0,
                failed: 0,
                canceled: 0,
            },
            last_activity_at: None,
        }],
        warnings: Vec::new(),
    };

    let dashboard = DashboardSurface {
        document_metrics: LibraryDocumentMetrics {
            total: 2,
            ready: 2,
            processing: 0,
            queued: 0,
            failed: 0,
            canceled: 0,
            graph_ready: 2,
            graph_sparse: 0,
            recomputed_at: utc_datetime(2025, 1, 1, 0, 0, 0)?,
        },
        recent_documents: documents.documents.clone(),
        recent_web_runs: documents.web_runs,
        graph: graph.clone(),
        attention: vec![DashboardAttentionItem {
            code: "sparse".to_string(),
            title: "Sparse graph".to_string(),
            detail: "Graph extraction is partial.".to_string(),
            route_path: "/graph".to_string(),
            level: MessageLevel::Warning,
        }],
        warnings: Vec::new(),
    };

    let dashboard_value = serde_json::to_value(&dashboard)?;
    assert_eq!(dashboard_value["documentMetrics"]["total"], 2);
    assert!(dashboard_value.get("overview").is_none());
    assert!(dashboard_value.get("metrics").is_none());
    assert_eq!(dashboard_value["graph"]["status"], "partial");

    let workbench = GraphWorkbenchSurface {
        graph,
        filters: GraphFilterState {
            search_query: None,
            focus_document_id: None,
            include_filtered_artifacts: false,
        },
        selected_node_id: None,
        selected_node: None,
        diagnostics: Vec::new(),
    };
    let workbench_value = serde_json::to_value(&workbench)?;
    assert_eq!(workbench_value["graph"]["convergenceStatus"], "degraded");
    Ok(())
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one serialization scenario intentionally assembles the complete cross-surface payload"
)]
fn assistant_admin_api_and_diagnostics_surfaces_serialize() -> TestResult {
    let assistant = AssistantWorkspaceSurface {
        sessions: vec![AssistantSessionListItem {
            id: Uuid::from_u128(40),
            workspace_id: Uuid::from_u128(41),
            library_id: Uuid::from_u128(42),
            title: "Release blockers".to_string(),
            updated_at: utc_datetime(2026, 4, 4, 10, 0, 0)?,
            created_at: utc_datetime(2026, 4, 4, 9, 0, 0)?,
            conversation_state: "ready".to_string(),
            turn_count: 2,
        }],
        active_session_id: Some(Uuid::from_u128(40)),
        messages: Vec::new(),
        stages: vec![AssistantStageItem {
            stage_kind: "retrieve".to_string(),
            stage_label: "Retrieve".to_string(),
            active: true,
            completed: false,
            failed: false,
        }],
        composer: AssistantComposerState {
            can_submit: true,
            draft: None,
            placeholder: Some("Ask about release state".to_string()),
        },
        evidence_groups: vec![AssistantEvidenceGroup {
            key: "sources".to_string(),
            label: "Sources".to_string(),
            items: vec![AssistantEvidenceItem {
                id: "doc-1".to_string(),
                label: "release-0.1.0.md".to_string(),
                detail: "Primary release checklist".to_string(),
                score: Some(0.91),
                document_id: Some(Uuid::from_u128(43)),
            }],
        }],
        warnings: Vec::new(),
    };
    let assistant_value = serde_json::to_value(&assistant)?;
    assert_eq!(assistant_value["stages"][0]["stageKind"], "retrieve");

    let admin = AdminSurface {
        viewer: AdminViewerSummary {
            principal_id: Uuid::from_u128(50),
            display_name: "Operator".to_string(),
            access_label: "Admin".to_string(),
            is_admin: true,
        },
        capabilities: AdminCapabilityState {
            admin_enabled: true,
            can_manage_tokens: true,
            can_read_audit: true,
            can_read_operations: true,
            can_manage_ai: true,
        },
        sections: vec![AdminSectionSummary {
            section: AdminSection::Access,
            title: "Access".to_string(),
            summary: "Manage identities and grants.".to_string(),
            item_count: Some(4),
            gate: CapabilityGate { section: AdminSection::Access, allowed: true, reason: None },
        }],
        warnings: Vec::new(),
    };
    let admin_value = serde_json::to_value(&admin)?;
    assert_eq!(admin_value["sections"][0]["section"], "access");

    let apiref = ApiReferenceSurface {
        status: ApiReferenceStatus::Ready,
        document_path: "/v1/openapi/ironrag.openapi.yaml".to_string(),
        server_origin: Some("/v1".to_string()),
        document_format: ApiReferenceFormat::OpenApiYaml,
        body: Some("openapi: 3.1.0".to_string()),
        message: None,
        warnings: vec![OperatorWarning {
            code: "preview".to_string(),
            level: MessageLevel::Info,
            title: "Preview".to_string(),
            detail: "Served by the Rust backend.".to_string(),
        }],
    };
    let apiref_value = serde_json::to_value(&apiref)?;
    assert_eq!(apiref_value["documentFormat"], "open_api_yaml");

    let diagnostics = SurfaceDiagnostics {
        health: SurfaceHealth::Degraded,
        counters: vec![DiagnosticCounter {
            key: "warnings".to_string(),
            label: "Warnings".to_string(),
            value: 1,
            level: MessageLevel::Warning,
        }],
        warnings: vec![OperatorWarning {
            code: "degraded".to_string(),
            level: MessageLevel::Warning,
            title: "Degraded".to_string(),
            detail: "Graph extraction is partial.".to_string(),
        }],
        degraded: vec![DegradedState {
            code: "graph_partial".to_string(),
            summary: "Graph convergence is partial".to_string(),
            detail: None,
        }],
        updated_at: None,
    };
    let diagnostics_value = serde_json::to_value(&diagnostics)?;
    assert_eq!(diagnostics_value["health"], "degraded");
    Ok(())
}
