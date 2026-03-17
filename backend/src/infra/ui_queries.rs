use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::{
    domains::{
        pricing_catalog::{PricingBillingUnit, PricingCapability},
        query_modes::RuntimeQueryMode,
        ui_admin::{
            AdminMemberModel, AdminOverviewModel, AdminSettingItemModel, AdminTabAvailability,
            AdminTabCounts, ApiTokenRowModel, LibraryAccessRowModel,
        },
        ui_documents::{
            DocumentAttemptGroup, DocumentAttemptSummary, DocumentDetailModel,
            DocumentExtractedStats, DocumentFilterValues, DocumentGraphStats, DocumentHistoryItem,
            DocumentListItem, DocumentMutationState, DocumentRevisionHistoryItem,
            DocumentStageAccountingItem, DocumentStageBenchmarkItem, DocumentSummaryCounters,
            DocumentSurfaceModel,
        },
        ui_graph::{
            GraphAssistantMessageModel, GraphAssistantModel, GraphAssistantProviderModel,
            GraphAssistantReferenceModel,
        },
        ui_identity::{UiSession, UiUser},
        usage_governance::{RuntimeStageBillingPolicy, runtime_stage_billing_policy},
    },
    infra::repositories::{
        self, ApiTokenRow, AttemptStageAccountingRow, DocumentRevisionRow,
        IngestionExecutionPayload, IngestionJobRow, LogicalDocumentProjectionRow, ProjectRow,
        RuntimeDocumentContributionSummaryRow, RuntimeExtractedContentRow, RuntimeIngestionRunRow,
        RuntimeIngestionStageEventRow, UiSessionRow, UiUserRow, WorkspaceRow,
    },
    services::{
        ingest_activity::IngestActivityService,
        query_runtime::{parse_runtime_query_enrichment, parse_runtime_query_warning},
        runtime_ingestion::classify_runtime_document_activity_with_service,
    },
};

#[derive(Debug, Clone)]
pub struct ResolvedShellContext {
    pub session: UiSessionRow,
    pub user: UiUserRow,
    pub workspaces: Vec<WorkspaceRow>,
    pub active_workspace: WorkspaceRow,
    pub projects: Vec<ProjectRow>,
    pub active_project: ProjectRow,
}

#[derive(Debug, Clone, FromRow)]
struct RecentRuntimeQueryExecutionRow {
    id: Uuid,
    mode: String,
    question: String,
    status: String,
    answer_text: Option<String>,
    grounding_status: String,
    provider_kind: String,
    model_name: String,
    debug_json: Value,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct ResolvedDocumentContributionSummary {
    chunk_count: Option<usize>,
    graph_node_count: Option<usize>,
    graph_edge_count: Option<usize>,
}

pub fn map_ui_user(user: &UiUserRow) -> UiUser {
    UiUser {
        id: user.id,
        email: user.email.clone(),
        display_name: user.display_name.clone(),
        role_label: user.role_label.clone(),
        initials: initials_from_display_name(&user.display_name),
        preferred_locale: user.preferred_locale.clone(),
    }
}

pub fn map_ui_session(session: &UiSessionRow, user: &UiUserRow) -> UiSession {
    UiSession {
        id: session.id,
        user: map_ui_user(user),
        active_workspace_id: session.active_workspace_id,
        active_library_id: session.active_project_id,
        locale: session.locale.clone(),
        expires_at: session.expires_at,
    }
}

pub fn workspace_is_visible(workspaces: &[WorkspaceRow], workspace_id: Uuid) -> bool {
    workspaces.iter().any(|workspace| workspace.id == workspace_id)
}

pub fn project_is_visible(projects: &[ProjectRow], project_id: Uuid) -> bool {
    projects.iter().any(|project| project.id == project_id)
}

pub async fn resolve_shell_context(
    pool: &PgPool,
    session: UiSessionRow,
    user: UiUserRow,
) -> Result<ResolvedShellContext, sqlx::Error> {
    let mut workspaces = repositories::list_workspaces_for_ui_user(pool, user.id).await?;
    if workspaces.is_empty() {
        let workspace = repositories::find_or_create_default_workspace(pool).await?;
        repositories::ensure_workspace_member(pool, workspace.id, user.id, &user.role_label)
            .await?;
        workspaces = repositories::list_workspaces_for_ui_user(pool, user.id).await?;
    }

    let active_workspace = session
        .active_workspace_id
        .and_then(|workspace_id| workspaces.iter().find(|workspace| workspace.id == workspace_id))
        .cloned()
        .or_else(|| workspaces.first().cloned())
        .ok_or(sqlx::Error::RowNotFound)?;

    let mut projects =
        repositories::list_projects_for_ui_user(pool, user.id, active_workspace.id).await?;
    if projects.is_empty() {
        let project =
            repositories::find_or_create_default_project(pool, active_workspace.id).await?;
        repositories::ensure_project_access_grant(pool, project.id, user.id, "write").await?;
        projects =
            repositories::list_projects_for_ui_user(pool, user.id, active_workspace.id).await?;
    }

    let active_project = session
        .active_project_id
        .and_then(|project_id| projects.iter().find(|project| project.id == project_id))
        .cloned()
        .or_else(|| projects.first().cloned())
        .ok_or(sqlx::Error::RowNotFound)?;

    let session = if session.active_workspace_id != Some(active_workspace.id)
        || session.active_project_id != Some(active_project.id)
    {
        repositories::touch_ui_session(
            pool,
            session.id,
            Some(active_workspace.id),
            Some(active_project.id),
            &session.locale,
        )
        .await?
        .unwrap_or(session)
    } else {
        session
    };

    Ok(ResolvedShellContext {
        session,
        user,
        workspaces,
        active_workspace,
        projects,
        active_project,
    })
}

pub async fn load_documents_surface(
    pool: &PgPool,
    ingest_activity: &IngestActivityService,
    project_id: Uuid,
    library_name: &str,
    accepted_formats: &[&str],
    max_size_mb: u64,
) -> Result<DocumentSurfaceModel, sqlx::Error> {
    let runs = repositories::list_runtime_ingestion_runs_by_project(pool, project_id).await?;
    let mut rows = Vec::with_capacity(runs.len());
    for run in &runs {
        rows.push(load_document_row(pool, ingest_activity, run, library_name).await?);
    }

    let counters = build_document_counters(&runs);
    let snapshot = repositories::get_runtime_graph_snapshot(pool, project_id).await?;
    let graph_status = resolve_graph_status(snapshot.as_ref(), &counters);
    let rebuild_backlog_count = counters.queued + counters.processing + counters.ready_no_graph;

    Ok(DocumentSurfaceModel {
        accepted_formats: accepted_formats.iter().map(|item| (*item).to_string()).collect(),
        max_size_mb,
        graph_status,
        graph_warning: build_graph_warning(snapshot.as_ref(), rebuild_backlog_count),
        rebuild_backlog_count,
        counters,
        filters: DocumentFilterValues {
            statuses: rows
                .iter()
                .map(|row| row.status.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            file_types: rows
                .iter()
                .map(|row| row.file_type.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        },
        rows,
    })
}

pub async fn load_document_row(
    pool: &PgPool,
    ingest_activity: &IngestActivityService,
    run: &RuntimeIngestionRunRow,
    library_name: &str,
) -> Result<DocumentListItem, sqlx::Error> {
    let projection = load_logical_projection(pool, run.document_id).await?;
    let latest_summary = repositories::get_attempt_stage_cost_summary_by_run(pool, run.id).await?;
    let contribution =
        load_document_contribution_summary(pool, run.project_id, run.document_id, run.revision_id)
            .await?;
    let activity = classify_runtime_document_activity_with_service(ingest_activity, run);
    let activity_status = activity.activity_status;
    let partial_history = matches!(run.status.as_str(), "ready" | "ready_no_graph" | "failed")
        && (run.queue_elapsed_ms.is_none()
            || (run.finished_at.is_some() && run.total_elapsed_ms.is_none())
            || latest_summary.is_none());

    Ok(DocumentListItem {
        id: run.id.to_string(),
        logical_document_id: run.document_id.map(|value| value.to_string()),
        file_name: run.file_name.clone(),
        file_type: humanize_file_type(&run.file_type),
        file_size_label: format_file_size_label(run.file_size_bytes),
        uploaded_at: run.created_at.to_rfc3339(),
        library_name: library_name.to_string(),
        stage: normalize_stage_label(&run.current_stage),
        status: normalize_document_status(&run.status).to_string(),
        progress_percent: normalize_progress(run.progress_percent),
        activity_status: Some(activity_status.clone()),
        last_activity_at: activity.last_activity_at.map(|value| value.to_rfc3339()),
        stalled_reason: activity.stalled_reason,
        active_revision_no: projection.as_ref().and_then(|item| item.active_revision_no),
        active_revision_kind: projection
            .as_ref()
            .and_then(|item| item.active_revision_kind.clone()),
        latest_attempt_no: run.current_attempt_no,
        accounting_status: latest_summary
            .as_ref()
            .map(|item| item.accounting_status.clone())
            .unwrap_or_else(|| "unpriced".to_string()),
        total_estimated_cost: latest_summary
            .as_ref()
            .and_then(|item| item.total_estimated_cost.and_then(|value| value.to_f64())),
        currency: latest_summary.as_ref().and_then(|item| item.currency.clone()),
        partial_history,
        partial_history_reason: partial_history
            .then_some("Legacy runtime history is incomplete for this attempt.".to_string()),
        mutation: build_mutation_state(projection.as_ref()),
        can_retry: run.status == "failed",
        can_append: can_update_document(run, projection.as_ref()),
        can_replace: can_update_document(run, projection.as_ref()),
        can_remove: can_remove_document(projection.as_ref()),
        detail_available: true,
        chunk_count: contribution.chunk_count,
        graph_node_count: contribution.graph_node_count,
        graph_edge_count: contribution.graph_edge_count,
    })
}

pub async fn load_document_detail(
    pool: &PgPool,
    ingest_activity: &IngestActivityService,
    run: &RuntimeIngestionRunRow,
    library_name: &str,
) -> Result<DocumentDetailModel, sqlx::Error> {
    let projection = load_logical_projection(pool, run.document_id).await?;
    let extracted = repositories::get_runtime_extracted_content_by_run(pool, run.id).await?;
    let stage_events = repositories::list_runtime_stage_events_by_run(pool, run.id).await?;
    let stage_accounting = repositories::list_attempt_stage_accounting_by_run(pool, run.id).await?;
    let attempt_jobs =
        repositories::list_ingestion_jobs_by_runtime_ingestion_run_id(pool, run.id).await?;
    let latest_summary = repositories::get_attempt_stage_cost_summary_by_run(pool, run.id).await?;
    let contribution =
        load_document_contribution_summary(pool, run.project_id, run.document_id, run.revision_id)
            .await?;
    let revision_history = match run.document_id {
        Some(document_id) => {
            repositories::list_document_revisions_by_document_id(pool, document_id).await?
        }
        None => Vec::new(),
    };
    let graph_stats =
        load_document_graph_stats(pool, run.project_id, run.document_id, run.revision_id).await?;
    let graph_node_id = load_document_graph_node_id(pool, run.project_id, run.document_id).await?;
    let requested_by = match run.document_id {
        Some(document_id) => {
            repositories::get_active_document_mutation_workflow_by_document_id(pool, document_id)
                .await?
                .and_then(|workflow| workflow.requested_by)
        }
        None => None,
    };
    let extracted_stats =
        build_extracted_stats(projection.as_ref(), extracted.as_ref(), contribution.chunk_count);
    let activity = classify_runtime_document_activity_with_service(ingest_activity, run);
    let attempts = build_document_attempts(
        run,
        &revision_history,
        &stage_events,
        &stage_accounting,
        &attempt_jobs,
        &activity.activity_status,
    );
    let latest_attempt = attempts.first();
    let partial_history = latest_attempt.is_some_and(|attempt| attempt.partial_history);
    let partial_history_reason =
        latest_attempt.and_then(|attempt| attempt.partial_history_reason.clone());

    Ok(DocumentDetailModel {
        id: run.id.to_string(),
        logical_document_id: run.document_id.map(|value| value.to_string()),
        file_name: run.file_name.clone(),
        file_type: humanize_file_type(&run.file_type),
        file_size_label: format_file_size_label(run.file_size_bytes),
        uploaded_at: run.created_at.to_rfc3339(),
        library_name: library_name.to_string(),
        stage: normalize_stage_label(&run.current_stage),
        status: normalize_document_status(&run.status).to_string(),
        progress_percent: normalize_progress(run.progress_percent),
        activity_status: Some(activity.activity_status.clone()),
        last_activity_at: activity.last_activity_at.map(|value| value.to_rfc3339()),
        stalled_reason: activity.stalled_reason,
        active_revision_no: projection.as_ref().and_then(|item| item.active_revision_no),
        active_revision_kind: projection
            .as_ref()
            .and_then(|item| item.active_revision_kind.clone()),
        active_revision_status: projection
            .as_ref()
            .and_then(|item| item.active_revision_status.clone()),
        latest_attempt_no: run.current_attempt_no,
        accounting_status: latest_summary
            .as_ref()
            .map(|item| item.accounting_status.clone())
            .unwrap_or_else(|| "unpriced".to_string()),
        total_estimated_cost: latest_summary
            .as_ref()
            .and_then(|item| item.total_estimated_cost.and_then(|value| value.to_f64())),
        currency: latest_summary.as_ref().and_then(|item| item.currency.clone()),
        partial_history,
        partial_history_reason,
        mutation: build_mutation_state(projection.as_ref()),
        requested_by,
        error_message: run.latest_error_message.clone(),
        summary: format_document_detail_summary(
            &run.status,
            extracted_stats.chunk_count,
            &graph_stats,
        ),
        graph_node_id,
        can_download_text: extracted
            .as_ref()
            .and_then(|item| item.content_text.as_ref())
            .is_some_and(|text| !text.trim().is_empty()),
        can_append: can_update_document(run, projection.as_ref()),
        can_replace: can_update_document(run, projection.as_ref()),
        can_remove: can_remove_document(projection.as_ref()),
        extracted_stats,
        graph_stats,
        revision_history: revision_history
            .into_iter()
            .map(|revision| DocumentRevisionHistoryItem {
                id: revision.id.to_string(),
                revision_no: revision.revision_no,
                revision_kind: revision.revision_kind,
                status: revision.status,
                source_file_name: revision.source_file_name,
                appended_text_excerpt: revision.appended_text_excerpt,
                accepted_at: revision.accepted_at.to_rfc3339(),
                activated_at: revision.activated_at.map(|value| value.to_rfc3339()),
                superseded_at: revision.superseded_at.map(|value| value.to_rfc3339()),
                is_active: projection
                    .as_ref()
                    .and_then(|item| item.current_revision_id)
                    .is_some_and(|current_id| current_id == revision.id),
            })
            .collect(),
        processing_history: stage_events
            .into_iter()
            .map(|event| DocumentHistoryItem {
                attempt_no: event.attempt_no,
                status: event.status,
                stage: normalize_stage_label(&event.stage),
                error_message: event.message,
                started_at: event.started_at.to_rfc3339(),
                finished_at: event.finished_at.map(|value| value.to_rfc3339()),
            })
            .collect(),
        attempts,
    })
}

pub async fn load_graph_assistant(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<GraphAssistantModel, sqlx::Error> {
    let executions = list_recent_runtime_query_executions(pool, project_id, 3).await?;
    let mut messages = Vec::new();

    for execution in executions.into_iter().rev() {
        let parsed_mode = execution.mode.parse::<RuntimeQueryMode>().ok();
        let enrichment =
            parsed_mode.map(|mode| parse_runtime_query_enrichment(&execution.debug_json, mode));
        let (warning, warning_kind) = parse_runtime_query_warning(&execution.debug_json);
        messages.push(GraphAssistantMessageModel {
            id: format!("{}:user", execution.id),
            role: "user".to_string(),
            content: execution.question.clone(),
            created_at: execution.created_at.to_rfc3339(),
            query_id: Some(execution.id.to_string()),
            mode: Some(execution.mode.clone()),
            grounding_status: None,
            provider: None,
            references: Vec::new(),
            planning: None,
            rerank: None,
            context_assembly: None,
            warning: None,
            warning_kind: None,
        });

        if execution.status == "completed" {
            let references =
                repositories::list_runtime_query_references_by_execution(pool, execution.id)
                    .await?
                    .into_iter()
                    .map(|reference| GraphAssistantReferenceModel {
                        kind: reference.reference_kind,
                        reference_id: reference.reference_id.to_string(),
                        excerpt: reference.excerpt,
                        rank: usize::try_from(reference.rank).unwrap_or_default(),
                        score: reference.score.and_then(|value| {
                            if value.is_finite() { Some(value as f32) } else { None }
                        }),
                    })
                    .collect::<Vec<_>>();

            messages.push(GraphAssistantMessageModel {
                id: format!("{}:assistant", execution.id),
                role: "assistant".to_string(),
                content: execution.answer_text.unwrap_or_default(),
                created_at: execution.created_at.to_rfc3339(),
                query_id: Some(execution.id.to_string()),
                mode: Some(execution.mode),
                grounding_status: Some(execution.grounding_status),
                provider: Some(GraphAssistantProviderModel {
                    provider_kind: execution.provider_kind,
                    model_name: execution.model_name,
                }),
                references,
                planning: enrichment.as_ref().map(|value| value.planning.clone()),
                rerank: enrichment.as_ref().map(|value| value.rerank.clone()),
                context_assembly: enrichment.as_ref().map(|value| value.context_assembly.clone()),
                warning,
                warning_kind,
            });
        }
    }

    Ok(GraphAssistantModel {
        title: "Ask AI".to_string(),
        subtitle: "Questions stay inside the active library.".to_string(),
        prompts: vec![
            "Summarize the most connected entities in this library.".to_string(),
            "Which documents contribute the strongest graph evidence?".to_string(),
            "What themes are visible in the current graph?".to_string(),
        ],
        disclaimer: "Answers use the active library and its current graph projection.".to_string(),
        config: None,
        session_id: None,
        messages,
    })
}

pub async fn load_admin_overview(
    pool: &PgPool,
    workspace: &WorkspaceRow,
) -> Result<AdminOverviewModel, sqlx::Error> {
    let api_tokens = repositories::list_api_tokens(pool, Some(workspace.id)).await?;
    let members = repositories::list_workspace_members(pool, workspace.id).await?;
    let library_access = repositories::list_project_access_grants(pool, workspace.id).await?;

    Ok(AdminOverviewModel {
        active_tab: "api_tokens".to_string(),
        workspace_name: workspace.name.clone(),
        counts: AdminTabCounts {
            api_tokens: api_tokens.len(),
            members: members.len(),
            library_access: library_access.len(),
            settings: 5,
        },
        availability: AdminTabAvailability {
            api_tokens: true,
            members: true,
            library_access: true,
            settings: true,
        },
    })
}

pub async fn load_admin_api_tokens(
    pool: &PgPool,
    workspace_id: Uuid,
) -> Result<Vec<ApiTokenRowModel>, sqlx::Error> {
    let rows = repositories::list_api_tokens(pool, Some(workspace_id)).await?;
    Ok(rows.into_iter().map(map_api_token_row).collect())
}

pub async fn load_admin_members(
    pool: &PgPool,
    workspace_id: Uuid,
) -> Result<Vec<AdminMemberModel>, sqlx::Error> {
    let rows = repositories::list_workspace_members(pool, workspace_id).await?;
    Ok(rows
        .into_iter()
        .map(|row| AdminMemberModel {
            id: row.user_id.to_string(),
            display_name: row.display_name,
            email: row.email,
            role_label: row.role_label,
        })
        .collect())
}

pub async fn load_admin_library_access(
    pool: &PgPool,
    workspace_id: Uuid,
) -> Result<Vec<LibraryAccessRowModel>, sqlx::Error> {
    let rows = repositories::list_project_access_grants(pool, workspace_id).await?;
    Ok(rows
        .into_iter()
        .map(|row| LibraryAccessRowModel {
            id: format!("{}:{}", row.project_id, row.user_id),
            library_name: row.project_name,
            principal_label: row.display_name,
            access_level: row.access_level,
        })
        .collect())
}

pub fn build_admin_settings_items(
    workspace: &WorkspaceRow,
    default_locale: &str,
    frontend_origin: &str,
    session_ttl_hours: u64,
    upload_max_size_mb: u64,
) -> Vec<AdminSettingItemModel> {
    vec![
        AdminSettingItemModel {
            id: "workspace_slug".to_string(),
            label: "Workspace slug".to_string(),
            value: workspace.slug.clone(),
        },
        AdminSettingItemModel {
            id: "default_locale".to_string(),
            label: "Default locale".to_string(),
            value: default_locale.to_string(),
        },
        AdminSettingItemModel {
            id: "session_ttl".to_string(),
            label: "Session TTL".to_string(),
            value: format!("{session_ttl_hours} hours"),
        },
        AdminSettingItemModel {
            id: "upload_limit".to_string(),
            label: "Upload limit".to_string(),
            value: format!("{upload_max_size_mb} MB"),
        },
        AdminSettingItemModel {
            id: "frontend_origin".to_string(),
            label: "Frontend origin".to_string(),
            value: frontend_origin.to_string(),
        },
    ]
}

fn initials_from_display_name(display_name: &str) -> String {
    let initials = display_name
        .split_whitespace()
        .filter_map(|part| part.chars().next())
        .take(2)
        .collect::<String>();
    if initials.is_empty() { "RR".to_string() } else { initials.to_uppercase() }
}

fn normalize_document_status(status: &str) -> &'static str {
    match status {
        "accepted" | "queued" => "queued",
        "processing" => "processing",
        "ready" => "ready",
        "ready_no_graph" => "ready_no_graph",
        "failed" => "failed",
        _ => "processing",
    }
}

fn normalize_stage_label(stage: &str) -> String {
    stage.trim().to_lowercase()
}

fn normalize_progress(progress_percent: Option<i32>) -> Option<u8> {
    progress_percent.map(|value| value.clamp(0, 100) as u8)
}

fn humanize_file_type(file_type: &str) -> String {
    match file_type.trim().to_lowercase().as_str() {
        "pdf" => "PDF".to_string(),
        "office_document" | "docx" => "DOCX".to_string(),
        "text_like" | "text" | "txt" => "Text".to_string(),
        "image" | "png" | "jpg" | "jpeg" => "Image".to_string(),
        other if other.is_empty() => "Unknown".to_string(),
        other => other.to_string(),
    }
}

fn format_file_size_label(bytes: Option<i64>) -> String {
    let Some(bytes) = bytes.filter(|value| *value >= 0) else {
        return "—".to_string();
    };
    let bytes = bytes as f64;
    if bytes < 1024.0 {
        format!("{} B", bytes as i64)
    } else if bytes < 1024.0 * 1024.0 {
        format!("{:.1} KB", bytes / 1024.0)
    } else if bytes < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MB", bytes / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", bytes / (1024.0 * 1024.0 * 1024.0))
    }
}

fn build_document_counters(runs: &[RuntimeIngestionRunRow]) -> DocumentSummaryCounters {
    let mut counters = DocumentSummaryCounters {
        queued: 0,
        processing: 0,
        ready: 0,
        ready_no_graph: 0,
        failed: 0,
    };

    for run in runs {
        match normalize_document_status(&run.status) {
            "queued" => counters.queued += 1,
            "processing" => counters.processing += 1,
            "ready" => counters.ready += 1,
            "ready_no_graph" => counters.ready_no_graph += 1,
            "failed" => counters.failed += 1,
            _ => {}
        }
    }

    counters
}

fn resolve_graph_status(
    snapshot: Option<&repositories::RuntimeGraphSnapshotRow>,
    counters: &DocumentSummaryCounters,
) -> String {
    if let Some(snapshot) = snapshot {
        return match snapshot.graph_status.as_str() {
            "empty" | "building" | "ready" | "partial" | "failed" | "stale" => {
                snapshot.graph_status.clone()
            }
            _ => "partial".to_string(),
        };
    }

    if counters.ready_no_graph > 0 || counters.processing > 0 || counters.queued > 0 {
        "building".to_string()
    } else if counters.ready > 0 {
        "partial".to_string()
    } else if counters.failed > 0 {
        "failed".to_string()
    } else {
        "empty".to_string()
    }
}

fn build_graph_warning(
    snapshot: Option<&repositories::RuntimeGraphSnapshotRow>,
    rebuild_backlog_count: usize,
) -> Option<String> {
    if rebuild_backlog_count > 0 {
        return Some(
            "Graph coverage can still change while active processing finishes.".to_string(),
        );
    }
    match snapshot.map(|item| item.graph_status.as_str()) {
        Some("failed") => Some(
            snapshot
                .and_then(|item| item.last_error_message.clone())
                .unwrap_or_else(|| "The last graph build did not complete.".to_string()),
        ),
        Some("stale") => {
            Some("Graph data is being reconciled after recent document changes.".to_string())
        }
        Some("building") => Some("Graph projection is still being built.".to_string()),
        _ => None,
    }
}

fn build_mutation_state(
    projection: Option<&LogicalDocumentProjectionRow>,
) -> DocumentMutationState {
    DocumentMutationState {
        kind: projection.and_then(|item| item.active_mutation_kind.clone()),
        status: projection.and_then(|item| item.active_mutation_status.clone()),
        warning: projection.and_then(|item| match item.active_mutation_status.as_deref() {
            Some("accepted" | "reconciling") => {
                Some("This document is still being reconciled.".to_string())
            }
            _ => None,
        }),
    }
}

fn mutation_locked(projection: Option<&LogicalDocumentProjectionRow>) -> bool {
    matches!(
        projection.and_then(|item| item.active_mutation_status.as_deref()),
        Some("accepted" | "reconciling")
    )
}

fn can_update_document(
    run: &RuntimeIngestionRunRow,
    projection: Option<&LogicalDocumentProjectionRow>,
) -> bool {
    let Some(projection) = projection else {
        return false;
    };
    if projection.deleted_at.is_some() || projection.active_status == "deleted" {
        return false;
    }
    if mutation_locked(Some(projection)) {
        return false;
    }

    !matches!(run.status.as_str(), "queued" | "processing")
}

fn can_remove_document(projection: Option<&LogicalDocumentProjectionRow>) -> bool {
    let Some(projection) = projection else {
        return false;
    };
    if projection.deleted_at.is_some() || projection.active_status == "deleted" {
        return false;
    }

    !mutation_locked(Some(projection))
}

fn build_extracted_stats(
    projection: Option<&LogicalDocumentProjectionRow>,
    extracted: Option<&RuntimeExtractedContentRow>,
    chunk_count: Option<usize>,
) -> DocumentExtractedStats {
    DocumentExtractedStats {
        chunk_count,
        document_id: projection.map(|item| item.id.to_string()),
        checksum: projection.and_then(|item| item.checksum.clone()),
        page_count: extracted.and_then(|item| item.page_count),
        extraction_kind: extracted.map(|item| item.extraction_kind.clone()),
        warnings: extracted
            .and_then(|item| value_to_string_vec(&item.extraction_warnings_json))
            .unwrap_or_default(),
    }
}

fn format_document_detail_summary(
    status: &str,
    chunk_count: Option<usize>,
    graph_stats: &DocumentGraphStats,
) -> String {
    match normalize_document_status(status) {
        "ready" => format!(
            "Processed with {} chunk(s); graph now contains {} node(s) and {} edge(s) linked to this document.",
            chunk_count.unwrap_or(0),
            graph_stats.node_count,
            graph_stats.edge_count
        ),
        "ready_no_graph" => format!(
            "Processed with {} chunk(s); graph projection is still pending.",
            chunk_count.unwrap_or(0)
        ),
        "failed" => "Processing stopped before the document became ready.".to_string(),
        "queued" => "The document is waiting in the processing queue.".to_string(),
        _ => "The document is being processed now.".to_string(),
    }
}

fn map_api_token_row(row: ApiTokenRow) -> ApiTokenRowModel {
    ApiTokenRowModel {
        id: row.id.to_string(),
        label: row.label,
        masked_token: row.token_preview.unwrap_or_else(|| "Stored token".to_string()),
        scopes: serde_json::from_value::<Vec<String>>(row.scope_json).unwrap_or_default(),
        created_at: row.created_at.to_rfc3339(),
        last_used_at: row.last_used_at.map(|value| value.to_rfc3339()),
        expires_at: row.expires_at.map(|value| value.to_rfc3339()),
        can_revoke: row.status == "active",
    }
}

fn build_document_attempts(
    run: &RuntimeIngestionRunRow,
    revisions: &[DocumentRevisionRow],
    stage_events: &[RuntimeIngestionStageEventRow],
    stage_accounting: &[AttemptStageAccountingRow],
    jobs: &[IngestionJobRow],
    current_activity_status: &str,
) -> Vec<DocumentAttemptGroup> {
    let mut attempt_nos = stage_events.iter().map(|item| item.attempt_no).collect::<BTreeSet<_>>();
    attempt_nos.insert(run.current_attempt_no);
    let attempt_nos = attempt_nos.into_iter().collect::<Vec<_>>();
    let revision_no_by_id = revisions
        .iter()
        .map(|revision| (revision.id, revision.revision_no))
        .collect::<HashMap<_, _>>();
    let initial_revision_no = revisions.iter().map(|revision| revision.revision_no).min();
    let payload_by_attempt = map_attempt_payloads(&attempt_nos, jobs);
    let job_by_attempt = map_attempt_jobs(&attempt_nos, jobs);
    let stage_events_by_attempt = group_stage_events_by_attempt(stage_events);
    let accounting_by_event = stage_accounting
        .iter()
        .cloned()
        .map(|row| (row.stage_event_id, row))
        .collect::<HashMap<_, _>>();

    attempt_nos
        .into_iter()
        .rev()
        .map(|attempt_no| {
            let attempt_stage_events =
                stage_events_by_attempt.get(&attempt_no).cloned().unwrap_or_default();
            let payload = payload_by_attempt.get(&attempt_no);
            let job = job_by_attempt.get(&attempt_no);
            let revision_no = payload
                .and_then(|item| item.target_revision_id)
                .and_then(|revision_id| revision_no_by_id.get(&revision_id).copied())
                .or_else(|| {
                    payload
                        .and_then(|item| item.attempt_kind.as_deref())
                        .filter(|kind| *kind == "initial_upload")
                        .and(initial_revision_no)
                });
            let benchmarks = attempt_stage_events
                .iter()
                .map(|event| DocumentStageBenchmarkItem {
                    stage: normalize_stage_label(&event.stage),
                    status: event.status.clone(),
                    message: event.message.clone(),
                    provider_kind: event.provider_kind.clone(),
                    model_name: event.model_name.clone(),
                    started_at: event.started_at.to_rfc3339(),
                    finished_at: event.finished_at.map(|value| value.to_rfc3339()),
                    elapsed_ms: event.elapsed_ms,
                    accounting: accounting_by_event.get(&event.id).and_then(|row| {
                        stage_accounting_belongs_to_billable_stage(row, &event.stage)
                            .then(|| map_document_stage_accounting(row, event))
                    }),
                })
                .collect::<Vec<_>>();
            let summary = summarize_attempt_accounting(&attempt_stage_events, stage_accounting);
            let attempt_status = attempt_status(run, job.copied(), &attempt_stage_events);
            let partial_history_reason =
                attempt_partial_history_reason(attempt_no, run, &attempt_stage_events, payload);
            DocumentAttemptGroup {
                attempt_no,
                revision_no,
                revision_id: payload
                    .and_then(|item| item.target_revision_id)
                    .map(|id| id.to_string()),
                attempt_kind: payload.and_then(|item| item.attempt_kind.clone()),
                status: attempt_status.clone(),
                queue_elapsed_ms: if attempt_no == run.current_attempt_no {
                    run.queue_elapsed_ms.or_else(|| {
                        attempt_started_at(&attempt_stage_events)
                            .zip(attempt_queue_started_at(&attempt_stage_events))
                            .map(|(started_at, queue_started_at)| {
                                started_at
                                    .signed_duration_since(queue_started_at)
                                    .num_milliseconds()
                                    .max(0)
                            })
                    })
                } else {
                    attempt_started_at(&attempt_stage_events)
                        .zip(attempt_queue_started_at(&attempt_stage_events))
                        .map(|(started_at, queue_started_at)| {
                            started_at
                                .signed_duration_since(queue_started_at)
                                .num_milliseconds()
                                .max(0)
                        })
                },
                total_elapsed_ms: if attempt_no == run.current_attempt_no {
                    run.total_elapsed_ms.or_else(|| {
                        attempt_finished_at(&attempt_stage_events)
                            .zip(attempt_queue_started_at(&attempt_stage_events))
                            .map(|(finished_at, queue_started_at)| {
                                finished_at
                                    .signed_duration_since(queue_started_at)
                                    .num_milliseconds()
                                    .max(0)
                            })
                    })
                } else {
                    attempt_finished_at(&attempt_stage_events)
                        .zip(attempt_queue_started_at(&attempt_stage_events))
                        .map(|(finished_at, queue_started_at)| {
                            finished_at
                                .signed_duration_since(queue_started_at)
                                .num_milliseconds()
                                .max(0)
                        })
                },
                started_at: attempt_started_at(&attempt_stage_events)
                    .or(if attempt_no == run.current_attempt_no { run.started_at } else { None })
                    .map(|value| value.to_rfc3339()),
                finished_at: attempt_finished_at(&attempt_stage_events)
                    .or(if attempt_no == run.current_attempt_no { run.finished_at } else { None })
                    .map(|value| value.to_rfc3339()),
                activity_status: Some(
                    attempt_activity_status(
                        run,
                        attempt_no,
                        &attempt_status,
                        current_activity_status,
                    )
                    .to_string(),
                ),
                last_activity_at: attempt_last_activity_at(run, attempt_no, &attempt_stage_events)
                    .map(|value| value.to_rfc3339()),
                partial_history: partial_history_reason.is_some(),
                partial_history_reason,
                summary,
                benchmarks,
            }
        })
        .collect()
}

fn map_document_stage_accounting(
    row: &AttemptStageAccountingRow,
    event: &RuntimeIngestionStageEventRow,
) -> DocumentStageAccountingItem {
    DocumentStageAccountingItem {
        pricing_status: row.pricing_status.clone(),
        usage_event_id: row.usage_event_id.map(|value| value.to_string()),
        cost_ledger_id: row.cost_ledger_id.map(|value| value.to_string()),
        pricing_catalog_entry_id: row.pricing_catalog_entry_id.map(|value| value.to_string()),
        estimated_cost: row.estimated_cost.and_then(|value| value.to_f64()),
        currency: row.currency.clone(),
        attribution_source: Some(stage_attribution_source(row, &event.stage).to_string()),
    }
}

fn summarize_attempt_accounting(
    attempt_stage_events: &[RuntimeIngestionStageEventRow],
    stage_accounting: &[AttemptStageAccountingRow],
) -> DocumentAttemptSummary {
    let stage_event_ids =
        attempt_stage_events.iter().map(|event| event.id).collect::<BTreeSet<_>>();
    let attempt_rows = stage_accounting
        .iter()
        .filter(|row| {
            stage_event_ids.contains(&row.stage_event_id)
                && attempt_stage_events.iter().any(|event| {
                    event.id == row.stage_event_id
                        && stage_accounting_belongs_to_billable_stage(row, &event.stage)
                })
        })
        .collect::<Vec<_>>();
    let total_estimated_cost = attempt_rows
        .iter()
        .filter_map(|row| row.estimated_cost)
        .fold(rust_decimal::Decimal::ZERO, |acc, value| acc + value);
    let priced_stage_count =
        i32::try_from(attempt_rows.iter().filter(|row| row.pricing_status == "priced").count())
            .unwrap_or(i32::MAX);
    let unpriced_stage_count =
        i32::try_from(attempt_rows.iter().filter(|row| row.pricing_status != "priced").count())
            .unwrap_or(i32::MAX);
    DocumentAttemptSummary {
        total_estimated_cost: if attempt_rows.iter().any(|row| row.estimated_cost.is_some()) {
            total_estimated_cost.to_f64()
        } else {
            None
        },
        currency: attempt_rows.iter().find_map(|row| row.currency.clone()),
        priced_stage_count,
        unpriced_stage_count,
        accounting_status: if priced_stage_count > 0 && unpriced_stage_count == 0 {
            "priced".to_string()
        } else if priced_stage_count > 0 {
            "partial".to_string()
        } else {
            "unpriced".to_string()
        },
    }
}

fn map_attempt_payloads(
    attempt_nos: &[i32],
    jobs: &[IngestionJobRow],
) -> HashMap<i32, IngestionExecutionPayload> {
    let mut payloads = HashMap::new();
    for (attempt_no, job) in attempt_nos.iter().copied().zip(jobs.iter()) {
        if let Ok(payload) = repositories::parse_ingestion_execution_payload(job) {
            payloads.insert(attempt_no, payload);
        }
    }
    payloads
}

fn map_attempt_jobs<'a>(
    attempt_nos: &[i32],
    jobs: &'a [IngestionJobRow],
) -> HashMap<i32, &'a IngestionJobRow> {
    attempt_nos.iter().copied().zip(jobs.iter()).collect()
}

fn group_stage_events_by_attempt(
    stage_events: &[RuntimeIngestionStageEventRow],
) -> BTreeMap<i32, Vec<RuntimeIngestionStageEventRow>> {
    let mut grouped = BTreeMap::new();
    for event in stage_events {
        grouped.entry(event.attempt_no).or_insert_with(Vec::new).push(event.clone());
    }
    grouped
}

fn attempt_status(
    run: &RuntimeIngestionRunRow,
    job: Option<&IngestionJobRow>,
    stage_events: &[RuntimeIngestionStageEventRow],
) -> String {
    if let Some(job) = job {
        if let Some(status) =
            job.result_json.get("terminal_status").and_then(serde_json::Value::as_str)
        {
            return status.to_string();
        }
        match job.status.as_str() {
            "retryable_failed" => return "failed".to_string(),
            "queued" => return "queued".to_string(),
            "running" => return "processing".to_string(),
            _ => {}
        }
    }
    if stage_events.iter().any(|event| event.status == "failed") {
        "failed".to_string()
    } else if stage_events
        .iter()
        .any(|event| event.stage == "finalizing" && event.status == "completed")
    {
        normalize_document_status(&run.status).to_string()
    } else if stage_events.iter().any(|event| event.status == "started") {
        "processing".to_string()
    } else {
        "queued".to_string()
    }
}

fn attempt_queue_started_at(
    stage_events: &[RuntimeIngestionStageEventRow],
) -> Option<DateTime<Utc>> {
    stage_events.iter().find(|event| event.stage == "accepted").map(|event| event.started_at)
}

fn attempt_started_at(stage_events: &[RuntimeIngestionStageEventRow]) -> Option<DateTime<Utc>> {
    stage_events
        .iter()
        .find(|event| event.stage != "accepted" && event.status == "started")
        .map(|event| event.started_at)
}

fn attempt_finished_at(stage_events: &[RuntimeIngestionStageEventRow]) -> Option<DateTime<Utc>> {
    stage_events
        .iter()
        .rev()
        .find(|event| matches!(event.status.as_str(), "completed" | "failed" | "skipped"))
        .and_then(|event| event.finished_at)
}

fn attempt_partial_history_reason(
    attempt_no: i32,
    run: &RuntimeIngestionRunRow,
    stage_events: &[RuntimeIngestionStageEventRow],
    payload: Option<&IngestionExecutionPayload>,
) -> Option<String> {
    if payload.is_none() {
        return Some("Attempt metadata predates revision-aware lifecycle snapshots.".to_string());
    }
    if stage_events.is_empty() {
        return Some("Attempt benchmark history is missing stage events.".to_string());
    }
    if stage_events.iter().any(|event| {
        matches!(event.status.as_str(), "completed" | "failed" | "skipped")
            && (event.finished_at.is_none() || event.elapsed_ms.is_none())
    }) {
        return Some("Attempt benchmark history is missing terminal stage timings.".to_string());
    }
    if stage_events.iter().any(|event| {
        matches!(event.stage.as_str(), "embedding_chunks" | "extracting_graph")
            && event.status == "completed"
            && (event.provider_kind.is_none() || event.model_name.is_none())
    }) {
        return Some(
            "Attempt benchmark history is missing provider/model attribution.".to_string(),
        );
    }
    if attempt_no == run.current_attempt_no
        && matches!(run.status.as_str(), "ready" | "ready_no_graph" | "failed")
        && (run.queue_elapsed_ms.is_none()
            || (run.finished_at.is_some() && run.total_elapsed_ms.is_none()))
    {
        return Some(
            "Latest attempt predates persisted queue or total elapsed timings.".to_string(),
        );
    }
    None
}

fn attempt_activity_status(
    run: &RuntimeIngestionRunRow,
    attempt_no: i32,
    attempt_status: &str,
    current_activity_status: &str,
) -> &'static str {
    if attempt_no == run.current_attempt_no {
        return match current_activity_status {
            "queued" => "queued",
            "active" => "active",
            "blocked" => "blocked",
            "retrying" => "retrying",
            "stalled" => "stalled",
            "ready" => "ready",
            "failed" => "failed",
            _ => "active",
        };
    }
    match attempt_status {
        "ready" | "ready_no_graph" => "ready",
        "failed" => "failed",
        "processing" => "active",
        _ => "queued",
    }
}

fn attempt_last_activity_at(
    run: &RuntimeIngestionRunRow,
    attempt_no: i32,
    stage_events: &[RuntimeIngestionStageEventRow],
) -> Option<DateTime<Utc>> {
    if attempt_no == run.current_attempt_no {
        return run.last_activity_at;
    }
    stage_events.iter().rev().find_map(|event| event.finished_at.or(Some(event.started_at)))
}

fn stage_attribution_source(row: &AttemptStageAccountingRow, event_stage: &str) -> &'static str {
    let metadata_source = row
        .pricing_snapshot_json
        .get("stage_ownership")
        .or_else(|| row.token_usage_json.get("stage_ownership"))
        .and_then(|value| value.get("attribution_source"))
        .and_then(Value::as_str);
    match metadata_source {
        Some("stage_native") => "stage_native",
        Some("reconciled") => "reconciled",
        _ if row.stage == event_stage => "stage_native",
        _ => "reconciled",
    }
}

fn stage_accounting_belongs_to_billable_stage(
    row: &AttemptStageAccountingRow,
    event_stage: &str,
) -> bool {
    if row.stage != event_stage {
        return false;
    }
    match runtime_stage_billing_policy(event_stage) {
        RuntimeStageBillingPolicy::Billable { capability, billing_unit } => {
            row.capability == pricing_capability_label(&capability)
                && row.billing_unit == pricing_billing_unit_label(&billing_unit)
        }
        RuntimeStageBillingPolicy::NonBillable => false,
    }
}

fn pricing_capability_label(value: &PricingCapability) -> &'static str {
    match value {
        PricingCapability::Indexing => "indexing",
        PricingCapability::Embedding => "embedding",
        PricingCapability::Answer => "answer",
        PricingCapability::Vision => "vision",
        PricingCapability::GraphExtract => "graph_extract",
    }
}

fn pricing_billing_unit_label(value: &PricingBillingUnit) -> &'static str {
    match value {
        PricingBillingUnit::Per1MInputTokens => "per_1m_input_tokens",
        PricingBillingUnit::Per1MOutputTokens => "per_1m_output_tokens",
        PricingBillingUnit::Per1MTokens => "per_1m_tokens",
        PricingBillingUnit::FixedPerCall => "fixed_per_call",
    }
}

fn value_to_string_vec(value: &Value) -> Option<Vec<String>> {
    value.as_array().map(|items| {
        items.iter().filter_map(|item| item.as_str().map(ToString::to_string)).collect::<Vec<_>>()
    })
}

async fn load_document_contribution_summary(
    pool: &PgPool,
    project_id: Uuid,
    document_id: Option<Uuid>,
    revision_id: Option<Uuid>,
) -> Result<ResolvedDocumentContributionSummary, sqlx::Error> {
    let Some(document_id) = document_id else {
        return Ok(ResolvedDocumentContributionSummary {
            chunk_count: None,
            graph_node_count: None,
            graph_edge_count: None,
        });
    };

    let cached =
        repositories::get_runtime_document_contribution_summary_by_document_id(pool, document_id)
            .await?
            .filter(|row| revision_id.is_none() || row.revision_id == revision_id);
    if let Some(row) = cached {
        return Ok(map_runtime_document_contribution_summary_row(&row));
    }

    let chunk_count = repositories::count_chunks_by_document(pool, document_id).await?;
    let graph_counts = match revision_id {
        Some(revision_id) => {
            repositories::count_runtime_graph_contributions_by_document_revision(
                pool,
                project_id,
                document_id,
                revision_id,
            )
            .await?
        }
        None => {
            repositories::count_runtime_graph_contributions_by_document(
                pool,
                project_id,
                document_id,
            )
            .await?
        }
    };

    Ok(ResolvedDocumentContributionSummary {
        chunk_count: Some(usize::try_from(chunk_count).unwrap_or_default()),
        graph_node_count: Some(usize::try_from(graph_counts.node_count).unwrap_or_default()),
        graph_edge_count: Some(usize::try_from(graph_counts.edge_count).unwrap_or_default()),
    })
}

fn map_runtime_document_contribution_summary_row(
    row: &RuntimeDocumentContributionSummaryRow,
) -> ResolvedDocumentContributionSummary {
    ResolvedDocumentContributionSummary {
        chunk_count: row.chunk_count.and_then(|value| usize::try_from(value).ok()),
        graph_node_count: usize::try_from(row.admitted_graph_node_count).ok(),
        graph_edge_count: usize::try_from(row.admitted_graph_edge_count).ok(),
    }
}

async fn load_logical_projection(
    pool: &PgPool,
    document_id: Option<Uuid>,
) -> Result<Option<LogicalDocumentProjectionRow>, sqlx::Error> {
    match document_id {
        Some(document_id) => {
            repositories::get_logical_document_projection_by_id(pool, document_id).await
        }
        None => Ok(None),
    }
}

async fn load_document_graph_stats(
    pool: &PgPool,
    project_id: Uuid,
    document_id: Option<Uuid>,
    revision_id: Option<Uuid>,
) -> Result<DocumentGraphStats, sqlx::Error> {
    let Some(document_id) = document_id else {
        return Ok(DocumentGraphStats { node_count: 0, edge_count: 0, evidence_count: 0 });
    };
    let counts = match revision_id {
        Some(revision_id) => {
            repositories::count_runtime_graph_contributions_by_document_revision(
                pool,
                project_id,
                document_id,
                revision_id,
            )
            .await?
        }
        None => {
            repositories::count_runtime_graph_contributions_by_document(
                pool,
                project_id,
                document_id,
            )
            .await?
        }
    };

    Ok(DocumentGraphStats {
        node_count: usize::try_from(counts.node_count).unwrap_or_default(),
        edge_count: usize::try_from(counts.edge_count).unwrap_or_default(),
        evidence_count: usize::try_from(counts.evidence_count).unwrap_or_default(),
    })
}

async fn load_document_graph_node_id(
    pool: &PgPool,
    project_id: Uuid,
    document_id: Option<Uuid>,
) -> Result<Option<String>, sqlx::Error> {
    let Some(document_id) = document_id else {
        return Ok(None);
    };
    let Some(snapshot) = repositories::get_runtime_graph_snapshot(pool, project_id).await? else {
        return Ok(None);
    };
    let canonical_key = format!("document:{document_id}");
    let node = repositories::get_runtime_graph_node_by_key(
        pool,
        project_id,
        &canonical_key,
        snapshot.projection_version,
    )
    .await?;
    Ok(node.map(|item| item.id.to_string()))
}

async fn list_recent_runtime_query_executions(
    pool: &PgPool,
    project_id: Uuid,
    limit: i64,
) -> Result<Vec<RecentRuntimeQueryExecutionRow>, sqlx::Error> {
    sqlx::query_as::<_, RecentRuntimeQueryExecutionRow>(
        "select id, mode, question, status, answer_text, grounding_status, provider_kind, model_name, debug_json, created_at
         from runtime_query_execution
         where project_id = $1
         order by created_at desc
         limit $2",
    )
    .bind(project_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}
