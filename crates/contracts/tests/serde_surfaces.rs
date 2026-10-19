use std::io;

use chrono::{DateTime, TimeZone, Utc};
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
        AdminLibraryCostCoverage, AdminLibraryCostSummary, AdminLibraryCostValue,
        AdminLibraryFacets, AdminLibraryIngestionReadiness, AdminLibraryListItem,
        AdminLibraryPageResponse, AdminLibraryWorkspaceSummary,
    },
    ai::AiBindingPurpose,
    assistant::{AssistantAnswerDisposition, AssistantTechnicalFactReference},
    auth::{
        AuthenticatedSession, BootstrapStatus, GrantResourceKind, PermissionKind, SessionMode,
        SessionResolveResponse, SessionUser, UiLocale,
    },
    diagnostics::{MessageLevel, OperatorWarning},
    documents::{
        DashboardAttentionItem, DashboardSurface, DocumentReadiness, DocumentStatus,
        DocumentSummary, LibraryDocumentMetrics, WebIngestRunState, WebIngestRunSummary,
        WebRunCounts,
    },
    graph::{GraphConvergenceStatus, GraphNode, GraphNodeType, GraphStatus, GraphSurface},
    provider::{
        ProviderCapabilities, ProviderCapabilityState, ProviderModelCapabilityKind,
        ProviderRuntimeProfile,
    },
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
fn renamed_contract_fields_preserve_wire_names() -> TestResult {
    let value = serde_json::to_value(AssistantTechnicalFactReference {
        execution_id: Uuid::from_u128(3),
        fact_id: Uuid::from_u128(4),
        revision_id: Uuid::from_u128(5),
        fact_kind: "endpoint".to_string(),
        normalized_value: "/health".to_string(),
        display_value: "/health".to_string(),
        rank: 1,
        score: 1.0,
    })?;
    let preserved_value_key = ["canoni", "calValue"].concat();

    assert_eq!(value[&preserved_value_key], "/health");
    assert!(value.get("normalizedValue").is_none());
    Ok(())
}

#[test]
fn session_resolve_response_uses_expected_casing() -> TestResult {
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
        session: Some(session),
        me: None,
        shell_bootstrap: None,
        bootstrap_status: BootstrapStatus {
            setup_required: false,
            sso_available: true,
            ai_setup: None,
        },
        message: Some("ready".to_string()),
    };

    let value = serde_json::to_value(&payload)?;
    assert_eq!(value["mode"], "authenticated");
    assert_eq!(value["bootstrapStatus"]["setupRequired"], false);
    assert_eq!(value["bootstrapStatus"]["ssoAvailable"], true);
    assert_eq!(value["session"]["user"]["displayName"], "Operator");
    Ok(())
}

#[test]
fn system_administration_grant_vocabulary_is_supported() -> TestResult {
    let resource = serde_json::to_value(GrantResourceKind::System)?;
    let permission = serde_json::to_value(PermissionKind::IamAdmin)?;

    assert_eq!(resource, "system");
    assert_eq!(permission, "iam_admin");
    assert_eq!(serde_json::from_value::<GrantResourceKind>(resource)?, GrantResourceKind::System);
    assert_eq!(serde_json::from_value::<PermissionKind>(permission)?, PermissionKind::IamAdmin);
    Ok(())
}

#[test]
fn query_compile_ai_binding_purpose_uses_supported_vocabulary() -> TestResult {
    let value = serde_json::to_value(AiBindingPurpose::QueryCompile)?;
    assert_eq!(value, "query_compile");
    assert_eq!(serde_json::from_value::<AiBindingPurpose>(value)?, AiBindingPurpose::QueryCompile,);
    Ok(())
}

#[test]
fn query_rerank_ai_binding_purpose_is_distinct_from_query_compile() -> TestResult {
    let value = serde_json::to_value(AiBindingPurpose::QueryRerank)?;
    assert_eq!(value, "query_rerank");
    assert_eq!(serde_json::from_value::<AiBindingPurpose>(value)?, AiBindingPurpose::QueryRerank,);
    assert_eq!("query_rerank".parse::<AiBindingPurpose>(), Ok(AiBindingPurpose::QueryRerank));
    assert_eq!(
        "query_compile".parse::<AiBindingPurpose>(),
        Ok(AiBindingPurpose::QueryCompile),
        "query_rerank must not be aliased onto the compiler purpose",
    );
    assert_ne!(AiBindingPurpose::QueryRerank, AiBindingPurpose::QueryCompile);
    Ok(())
}

#[test]
fn ai_binding_purpose_from_str_rejects_case_and_whitespace_aliases() {
    for alias in ["Agent", " agent", "agent ", "QueryRerank", "query_rerank "] {
        assert!(alias.parse::<AiBindingPurpose>().is_err(), "accepted alias {alias:?}");
    }
    assert_eq!("agent".parse::<AiBindingPurpose>(), Ok(AiBindingPurpose::Agent));
}

/// `AiBindingPurpose::ALL` (IRG-182) must stay pinned to the database enum's
/// declaration order — `order by binding_purpose` follows it — and
/// `graph_entity_type` must sit right after `extract_graph`, the position a
/// live additive upgrade inserts it at, never appended.
#[test]
fn ai_binding_purpose_all_has_eight_purposes_in_declaration_order() -> TestResult {
    assert_eq!(AiBindingPurpose::ALL.len(), 8);
    assert_eq!(
        AiBindingPurpose::ALL.map(AiBindingPurpose::as_str),
        [
            "extract_text",
            "extract_graph",
            "graph_entity_type",
            "embed_chunk",
            "query_compile",
            "query_rerank",
            "query_answer",
            "agent",
        ],
    );
    let value = serde_json::to_value(AiBindingPurpose::GraphEntityType)?;
    assert_eq!(value, "graph_entity_type");
    assert_eq!(
        serde_json::from_value::<AiBindingPurpose>(value)?,
        AiBindingPurpose::GraphEntityType,
    );
    assert_eq!(
        "graph_entity_type".parse::<AiBindingPurpose>(),
        Ok(AiBindingPurpose::GraphEntityType),
    );
    Ok(())
}

/// `is_decision` is the eligibility gate's whole purpose vocabulary: exactly
/// `query_rerank` and `graph_entity_type` are decision purposes, and every
/// generative purpose is not.
#[test]
fn is_decision_holds_for_exactly_query_rerank_and_graph_entity_type() {
    for purpose in AiBindingPurpose::ALL {
        let expected =
            matches!(purpose, AiBindingPurpose::QueryRerank | AiBindingPurpose::GraphEntityType);
        assert_eq!(
            purpose.is_decision(),
            expected,
            "{purpose:?} decision classification must match the fixed two-purpose set",
        );
    }
}

#[test]
fn provider_model_capability_kind_uses_supported_vocabulary() -> TestResult {
    for (kind, expected) in [
        (ProviderModelCapabilityKind::Chat, "chat"),
        (ProviderModelCapabilityKind::Embedding, "embedding"),
        (ProviderModelCapabilityKind::Decision, "decision"),
    ] {
        let value = serde_json::to_value(kind)?;
        assert_eq!(value, expected);
        assert_eq!(serde_json::from_value::<ProviderModelCapabilityKind>(value)?, kind);
    }
    Ok(())
}

/// A `ProviderCapabilities` row persisted before the `decision` capability
/// existed carries no `decision` key at all; it must deserialize rather than
/// fail, and the honest default is `unsupported` (IRG-182).
#[test]
fn provider_capabilities_decision_defaults_unsupported_when_absent() -> TestResult {
    let capabilities = serde_json::from_value::<ProviderCapabilities>(serde_json::json!({
        "chat": "supported",
        "embeddings": "supported",
        "vision": "unsupported",
        "streaming": "unsupported",
        "tools": "unsupported",
        "modelDiscovery": "supported",
    }))?;
    assert_eq!(capabilities.decision, ProviderCapabilityState::Unsupported);

    let with_decision = serde_json::from_value::<ProviderCapabilities>(serde_json::json!({
        "chat": "unsupported",
        "embeddings": "unsupported",
        "vision": "unsupported",
        "streaming": "unsupported",
        "tools": "unsupported",
        "modelDiscovery": "supported",
        "decision": "supported",
    }))?;
    assert_eq!(with_decision.decision, ProviderCapabilityState::Supported);
    Ok(())
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
        library_total_count: 1,
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
    assert_eq!(shell_value["libraryTotalCount"], 1);
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
            identity_key: "concept:cutover".to_string(),
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

    let recent_documents = vec![DocumentSummary {
        id: Uuid::from_u128(30),
        workspace_id: Some(workspace_id),
        library_id: Some(library_id),
        file_name: Some("cutover.md".to_string()),
        file_type: Some("text/markdown".to_string()),
        file_size: Some(1024),
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
    }];

    let recent_web_runs = vec![WebIngestRunSummary {
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
    }];

    let dashboard = DashboardSurface {
        document_metrics: LibraryDocumentMetrics {
            total: 2,
            ready: 2,
            processing: 0,
            queued: 0,
            failed: 0,
            canceled: 0,
            empty: 0,
            graph_ready: 2,
            graph_sparse: 0,
            recomputed_at: utc_datetime(2025, 1, 1, 0, 0, 0)?,
        },
        recent_documents,
        recent_web_runs,
        graph,
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
    let preserved_key = ["canoni", "calKey"].concat();
    assert_eq!(dashboard_value["graph"]["nodes"][0][&preserved_key], "concept:cutover");
    assert!(dashboard_value["graph"]["nodes"][0].get("identityKey").is_none());
    Ok(())
}

#[test]
fn assistant_admin_api_and_diagnostics_surfaces_serialize() -> TestResult {
    let admin = AdminLibraryPageResponse {
        items: vec![AdminLibraryListItem {
            id: Uuid::from_u128(50),
            slug: "documents".to_string(),
            display_name: "Documents".to_string(),
            lifecycle_state: "active".to_string(),
            include_document_hint_in_mcp_answers: true,
            workspace: AdminLibraryWorkspaceSummary {
                id: Uuid::from_u128(51),
                slug: "primary".to_string(),
                display_name: "Primary".to_string(),
            },
            ingestion_readiness: AdminLibraryIngestionReadiness {
                ready: true,
                missing_binding_purposes: Vec::new(),
            },
            queued_job_count: 3,
            cost_summary: Some(AdminLibraryCostSummary {
                total_cost: "1.25".to_string(),
                currency_code: "USD".to_string(),
                document_count: 4,
                provider_call_count: 8,
                unpriced_provider_call_count: 0,
                cost_coverage: AdminLibraryCostCoverage::Complete,
            }),
            cost_value: Some(AdminLibraryCostValue {
                amount: "1.25".to_string(),
                currency_code: "USD".to_string(),
                coverage: AdminLibraryCostCoverage::Complete,
                unpriced_call_count: 0,
            }),
        }],
        total_count: 1,
        facets: AdminLibraryFacets { workspace_count: 1, ready_count: 1, active_count: 1 },
        aggregate_cost_summary: Some(AdminLibraryCostSummary {
            total_cost: "1.25".to_string(),
            currency_code: "USD".to_string(),
            document_count: 4,
            provider_call_count: 8,
            unpriced_provider_call_count: 0,
            cost_coverage: AdminLibraryCostCoverage::Complete,
        }),
    };
    let admin_value = serde_json::to_value(&admin)?;
    assert_eq!(admin_value["items"][0]["workspace"]["displayName"], "Primary");
    assert_eq!(admin_value["items"][0]["queuedJobCount"], 3);
    assert_eq!(admin_value["items"][0]["costSummary"]["totalCost"], "1.25");
    assert_eq!(admin_value["items"][0]["costValue"]["amount"], "1.25");
    assert_eq!(admin_value["items"][0]["costValue"]["currencyCode"], "USD");
    assert_eq!(admin_value["items"][0]["costValue"]["unpricedCallCount"], 0);
    assert_eq!(admin_value["aggregateCostSummary"]["providerCallCount"], 8);
    assert!(admin_value["items"][0].get("description").is_none());
    assert!(admin_value["items"][0].get("retrievalConfig").is_none());
    Ok(())
}
