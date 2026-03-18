use axum::{
    Json, Router,
    extract::{Path, Query, State},
};
use serde::Deserialize;
use tracing::info;

use crate::{
    app::state::AppState,
    domains::query_modes::RuntimeQueryMode,
    domains::ui_graph::{
        GraphAssistantAnswerModel, GraphAssistantProviderModel, GraphAssistantReferenceModel,
        GraphDiagnosticsModel, GraphEdgeModel, GraphEvidenceModel, GraphLegendItemModel,
        GraphNodeDetailModel, GraphNodeModel, GraphRelatedEdgeModel, GraphSearchHitModel,
        GraphSurfaceModel,
    },
    infra::{repositories, ui_queries},
    interfaces::http::{
        retrieval::{QueryRequest, run_query_with_workspace},
        router_support::ApiError,
        runtime_graph::{
            RuntimeGraphDiagnosticsResponse, RuntimeGraphNodeDetailResponse,
            RuntimeGraphSearchHitResponse, load_runtime_graph_diagnostics,
            load_runtime_graph_node_detail, load_runtime_graph_surface,
        },
        ui_support::{UiSessionContext, load_active_ui_context},
    },
};

#[derive(Debug, Deserialize)]
struct GraphSearchQuery {
    q: String,
    limit: Option<usize>,
    include_filtered: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct GraphSurfaceQuery {
    include_filtered: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct GraphAskRequest {
    question: String,
    session_id: Option<String>,
    node_id: Option<String>,
    mode: Option<RuntimeQueryMode>,
}

fn augment_question_with_focus(question: &str, detail: Option<&GraphNodeDetailModel>) -> String {
    let Some(detail) = detail else {
        return question.to_string();
    };
    format!(
        "Focus node: {} ({})\n{}\n\nQuestion: {}",
        detail.label, detail.node_type, detail.summary, question
    )
}

fn map_runtime_hit(hit: RuntimeGraphSearchHitResponse) -> GraphSearchHitModel {
    GraphSearchHitModel {
        id: hit.id,
        label: hit.label,
        node_type: hit.node_type,
        secondary_label: hit.secondary_label,
    }
}

fn map_runtime_detail(detail: RuntimeGraphNodeDetailResponse) -> GraphNodeDetailModel {
    GraphNodeDetailModel {
        id: detail.id,
        label: detail.label,
        node_type: detail.node_type,
        summary: detail.summary,
        properties: detail.properties,
        related_documents: detail.related_documents.into_iter().map(map_runtime_hit).collect(),
        connected_nodes: detail.connected_nodes.into_iter().map(map_runtime_hit).collect(),
        related_edges: detail
            .related_edges
            .into_iter()
            .map(|edge| GraphRelatedEdgeModel {
                id: edge.id,
                relation_type: edge.relation_type,
                other_node_id: edge.other_node_id,
                other_node_label: edge.other_node_label,
                support_count: edge.support_count,
            })
            .collect(),
        evidence: detail
            .evidence
            .into_iter()
            .map(|evidence| GraphEvidenceModel {
                id: evidence.id,
                document_id: evidence.document_id,
                document_label: evidence.document_label,
                chunk_id: evidence.chunk_id,
                page_ref: evidence.page_ref,
                evidence_text: evidence.evidence_text,
                confidence_score: evidence.confidence_score,
                created_at: evidence.created_at,
                active_provenance_only: evidence.active_provenance_only,
            })
            .collect(),
        relation_count: detail.relation_count,
        reconciliation_status: detail.reconciliation_status,
        convergence_status: Some(detail.convergence_status),
        pending_update_count: detail.pending_update_count,
        pending_delete_count: detail.pending_delete_count,
        active_provenance_only: detail.active_provenance_only,
        filtered_artifact_count: Some(detail.filtered_artifact_count),
        warning: detail.warning,
    }
}

fn map_runtime_diagnostics(diagnostics: RuntimeGraphDiagnosticsResponse) -> GraphDiagnosticsModel {
    GraphDiagnosticsModel {
        graph_status: diagnostics.graph_status,
        reconciliation_status: diagnostics.reconciliation_status,
        convergence_status: Some(diagnostics.convergence_status),
        projection_version: diagnostics.projection_version,
        node_count: diagnostics.node_count,
        edge_count: diagnostics.edge_count,
        projection_freshness: diagnostics.projection_freshness,
        rebuild_backlog_count: diagnostics.rebuild_backlog_count,
        ready_no_graph_count: diagnostics.ready_no_graph_count,
        pending_update_count: diagnostics.pending_update_count,
        pending_delete_count: diagnostics.pending_delete_count,
        filtered_artifact_count: Some(diagnostics.filtered_artifact_count),
        filtered_empty_relation_count: Some(diagnostics.filtered_empty_relation_count),
        filtered_degenerate_loop_count: Some(diagnostics.filtered_degenerate_loop_count),
        provenance_coverage_percent: diagnostics.provenance_coverage_percent,
        last_built_at: diagnostics.last_built_at,
        last_error_message: diagnostics.last_error_message,
        last_mutation_warning: diagnostics.last_mutation_warning,
        active_provenance_only: diagnostics.active_provenance_only,
        blockers: diagnostics.blockers,
        warning: diagnostics.warning,
        graph_backend: diagnostics.graph_backend,
    }
}

fn map_chat_session_detail(
    detail: repositories::ChatSessionDetailRow,
) -> crate::domains::ui_chat::ChatSessionDetailModel {
    crate::domains::ui_chat::ChatSessionDetailModel {
        session_id: detail.id.to_string(),
        title: detail.title,
        message_count: detail.message_count,
        last_message_preview: detail
            .last_message_preview
            .map(|value| value.split_whitespace().collect::<Vec<_>>().join(" ")),
        created_at: detail.created_at.to_rfc3339(),
        updated_at: detail.updated_at.to_rfc3339(),
        prompt_state: detail.prompt_state,
        preferred_mode: detail.preferred_mode,
        is_empty: detail.message_count == 0,
    }
}

fn map_chat_session_settings(
    detail: &repositories::ChatSessionDetailRow,
) -> crate::domains::ui_chat::ChatSessionSettingsModel {
    crate::domains::ui_chat::ChatSessionSettingsModel {
        session_id: detail.id.to_string(),
        system_prompt: detail.system_prompt.clone(),
        prompt_state: detail.prompt_state.clone(),
        preferred_mode: detail.preferred_mode.clone(),
        default_prompt_available: true,
    }
}

fn map_graph_assistant_message(
    id: String,
    role: &str,
    content: String,
    created_at: String,
    query_id: Option<String>,
    mode: Option<String>,
    grounding_status: Option<String>,
    provider: Option<GraphAssistantProviderModel>,
    references: Vec<GraphAssistantReferenceModel>,
    planning: Option<crate::domains::query_intelligence::QueryPlanningMetadata>,
    rerank: Option<crate::domains::query_intelligence::RerankMetadata>,
    context_assembly: Option<crate::domains::query_intelligence::ContextAssemblyMetadata>,
    warning: Option<String>,
    warning_kind: Option<String>,
) -> crate::domains::ui_graph::GraphAssistantMessageModel {
    crate::domains::ui_graph::GraphAssistantMessageModel {
        id,
        role: role.to_string(),
        content,
        created_at,
        query_id,
        mode,
        grounding_status,
        provider,
        references,
        planning,
        rerank,
        context_assembly,
        warning,
        warning_kind,
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/ui/graph/surface", axum::routing::get(get_graph_surface))
        .route("/ui/graph/diagnostics", axum::routing::get(get_graph_diagnostics))
        .route("/ui/graph/search", axum::routing::get(search_graph_nodes))
        .route("/ui/graph/nodes/{id}", axum::routing::get(get_graph_node_detail))
        .route("/ui/graph/ask", axum::routing::post(ask_graph_assistant))
}

async fn get_graph_surface(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
    Query(query): Query<GraphSurfaceQuery>,
) -> Result<Json<GraphSurfaceModel>, ApiError> {
    let active = load_active_ui_context(&state, &ui_session).await?;
    let include_filtered = query.include_filtered.unwrap_or(false);
    let surface = load_runtime_graph_surface(&state, active.project.id, include_filtered).await?;
    let assistant =
        ui_queries::load_graph_assistant(&state.persistence.postgres, active.project.id)
            .await
            .map_err(|_| ApiError::Internal)?;
    Ok(Json(GraphSurfaceModel {
        graph_status: surface.graph_status,
        convergence_status: Some(surface.convergence_status),
        projection_version: surface.projection_version,
        node_count: surface.node_count,
        relation_count: surface.relation_count,
        last_built_at: surface.last_built_at,
        filtered_artifact_count: Some(surface.filtered_artifact_count),
        warning: surface.warning,
        nodes: surface
            .nodes
            .into_iter()
            .map(|node| GraphNodeModel {
                id: node.id,
                label: node.label,
                node_type: node.node_type,
                secondary_label: node.secondary_label,
                support_count: node.support_count,
                filtered_artifact: node.filtered_artifact,
            })
            .collect(),
        edges: surface
            .edges
            .into_iter()
            .map(|edge| GraphEdgeModel {
                id: edge.id,
                source: edge.source,
                target: edge.target,
                relation_type: edge.relation_type,
                support_count: edge.support_count,
                filtered_artifact: edge.filtered_artifact,
            })
            .collect(),
        legend: surface
            .legend
            .into_iter()
            .map(|item| GraphLegendItemModel { key: item.key, label: item.label })
            .collect(),
        assistant,
    }))
}

async fn get_graph_diagnostics(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
) -> Result<Json<GraphDiagnosticsModel>, ApiError> {
    let active = load_active_ui_context(&state, &ui_session).await?;
    let diagnostics = load_runtime_graph_diagnostics(&state, active.project.id).await?;
    Ok(Json(map_runtime_diagnostics(diagnostics)))
}

async fn search_graph_nodes(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
    Query(query): Query<GraphSearchQuery>,
) -> Result<Json<Vec<GraphSearchHitModel>>, ApiError> {
    let active = load_active_ui_context(&state, &ui_session).await?;
    let limit = query.limit.unwrap_or(8).clamp(1, 25);
    let needle = query.q.trim().to_lowercase();
    if needle.is_empty() {
        return Ok(Json(Vec::new()));
    }
    let surface = load_runtime_graph_surface(
        &state,
        active.project.id,
        query.include_filtered.unwrap_or(false),
    )
    .await?;
    let mut hits = surface
        .nodes
        .into_iter()
        .filter(|node| {
            node.label.to_lowercase().contains(&needle)
                || node
                    .secondary_label
                    .as_ref()
                    .is_some_and(|value| value.to_lowercase().contains(&needle))
        })
        .map(|node| GraphSearchHitModel {
            id: node.id,
            label: node.label,
            node_type: node.node_type,
            secondary_label: node.secondary_label,
        })
        .collect::<Vec<_>>();
    hits.sort_by(|left, right| left.label.cmp(&right.label));
    hits.truncate(limit);
    Ok(Json(hits))
}

async fn get_graph_node_detail(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<GraphSurfaceQuery>,
) -> Result<Json<GraphNodeDetailModel>, ApiError> {
    let active = load_active_ui_context(&state, &ui_session).await?;
    let node_id =
        id.parse().map_err(|_| ApiError::BadRequest(format!("invalid graph node id {id}")))?;
    let detail = load_runtime_graph_node_detail(
        &state,
        active.project.id,
        node_id,
        query.include_filtered.unwrap_or(false),
    )
    .await?;
    Ok(Json(map_runtime_detail(detail)))
}

async fn ask_graph_assistant(
    ui_session: UiSessionContext,
    State(state): State<AppState>,
    Json(payload): Json<GraphAskRequest>,
) -> Result<Json<GraphAssistantAnswerModel>, ApiError> {
    let active = load_active_ui_context(&state, &ui_session).await?;
    let question = payload.question.trim();
    if question.is_empty() {
        return Err(ApiError::BadRequest("question is required".into()));
    }

    let focused_detail = match payload.node_id.as_deref() {
        Some(node_id) => {
            let parsed_node_id = node_id
                .parse()
                .map_err(|_| ApiError::BadRequest(format!("invalid graph node id {node_id}")))?;
            Some(map_runtime_detail(
                load_runtime_graph_node_detail(&state, active.project.id, parsed_node_id, false)
                    .await?,
            ))
        }
        None => None,
    };

    let response = run_query_with_workspace(
        &state,
        active.workspace.id,
        &QueryRequest {
            project_id: active.project.id,
            session_id: payload.session_id.as_deref().and_then(|value| value.parse().ok()),
            query_text: augment_question_with_focus(question, focused_detail.as_ref()),
            mode: payload.mode,
            model_profile_id: None,
            embedding_model_profile_id: None,
            top_k: Some(8),
        },
        ui_session.session_id,
    )
    .await?;

    info!(
        user_id = %ui_session.user_id,
        workspace_id = %active.workspace.id,
        project_id = %active.project.id,
        session_id = %response.session_id,
        retrieval_run_id = %response.retrieval_run_id,
        "completed ui graph assistant request"
    );

    let session_detail = repositories::get_chat_session_detail_by_id(
        &state.persistence.postgres,
        response.session_id,
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    let session_summary = session_detail.clone().map(map_chat_session_detail);
    let settings_summary = session_detail.as_ref().map(map_chat_session_settings);
    let structured_references = response
        .structured_references
        .iter()
        .map(|reference| GraphAssistantReferenceModel {
            kind: reference.kind.clone(),
            reference_id: reference.reference_id.to_string(),
            excerpt: reference.excerpt.clone(),
            rank: reference.rank,
            score: reference.score,
        })
        .collect::<Vec<_>>();
    let user_message = map_graph_assistant_message(
        response.user_message_id.to_string(),
        "user",
        question.to_string(),
        chrono::Utc::now().to_rfc3339(),
        None,
        Some(response.mode.clone()),
        None,
        None,
        Vec::new(),
        None,
        None,
        None,
        None,
        None,
    );
    let assistant_message = map_graph_assistant_message(
        response.assistant_message_id.to_string(),
        "assistant",
        response.answer.clone(),
        chrono::Utc::now().to_rfc3339(),
        Some(response.query_id.to_string()),
        Some(response.mode.clone()),
        Some(response.grounding_status.clone()),
        Some(GraphAssistantProviderModel {
            provider_kind: response.provider.provider_kind.clone(),
            model_name: response.provider.model_name.clone(),
        }),
        structured_references.clone(),
        Some(response.planning.clone()),
        Some(response.rerank.clone()),
        Some(response.context_assembly.clone()),
        response.warning.clone(),
        response.warning_kind.clone(),
    );

    Ok(Json(GraphAssistantAnswerModel {
        session_id: response.session_id.to_string(),
        user_message_id: response.user_message_id.to_string(),
        assistant_message_id: response.assistant_message_id.to_string(),
        query_id: response.query_id.to_string(),
        effective_mode: response.mode.clone(),
        session_summary,
        settings_summary,
        user_message,
        assistant_message,
        answer: response.answer,
        references: response.references,
        structured_references,
        mode: response.mode,
        grounding_status: response.grounding_status,
        provider: GraphAssistantProviderModel {
            provider_kind: response.provider.provider_kind,
            model_name: response.provider.model_name,
        },
        planning: response.planning,
        rerank: response.rerank,
        context_assembly: response.context_assembly,
        warning: response.warning,
        warning_kind: response.warning_kind,
    }))
}
