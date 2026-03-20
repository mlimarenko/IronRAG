use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use serde::Deserialize;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    domains::{
        graph_quality::{
            CanonicalGraphSummary, GraphSummaryConfidenceStatus, MutationImpactScopeConfidence,
            MutationImpactScopeStatus, MutationImpactScopeSummary,
        },
        query_modes::RuntimeQueryMode,
        ui_admin::{
            AdminMemberModel, AdminOverviewModel, AdminSettingItemModel, AdminTabAvailability,
            AdminTabCounts, ApiTokenRowModel, LibraryAccessRowModel,
        },
        ui_chat::{ChatSessionDetailModel, ChatSessionSettingsModel, ChatSessionSummaryModel},
        ui_documents::{
            DocumentAttemptGroup, DocumentAttemptSummary, DocumentCollectionDiagnostics,
            DocumentCollectionFormatDiagnostics, DocumentCollectionGraphThroughputSummary,
            DocumentCollectionProgressCounters, DocumentCollectionSettlementSummary,
            DocumentCollectionStageDiagnostics, DocumentCollectionWarning, DocumentDetailModel,
            DocumentExtractedStats, DocumentGraphHealthSummary, DocumentGraphStats,
            DocumentGraphThroughputSummary, DocumentHistoryItem, DocumentListItem,
            DocumentMutationState, DocumentProviderFailureSummary, DocumentQueueIsolationSummary,
            DocumentRevisionHistoryItem, DocumentStageAccountingItem, DocumentStageBenchmarkItem,
            DocumentTerminalOutcomeSummary,
        },
        ui_graph::{
            GraphAssistantConfigModel, GraphAssistantMessageModel, GraphAssistantModel,
            GraphAssistantProviderModel, GraphAssistantReferenceModel,
        },
        ui_identity::{UiSession, UiUser},
    },
    infra::repositories::{
        self, ApiTokenRow, AttemptStageAccountingRow, DocumentRevisionRow,
        IngestionExecutionPayload, IngestionJobRow, LogicalDocumentProjectionRow, ProjectRow,
        RuntimeDocumentContributionSummaryRow, RuntimeExtractedContentRow,
        RuntimeGraphProgressCheckpointRow, RuntimeIngestionRunRow, RuntimeIngestionStageEventRow,
        RuntimeLibraryQueueSliceRow, RuntimeQueryReferenceGroupRow, UiSessionRow, UiUserRow,
        WorkspaceRow,
    },
    services::{
        document_accounting::{
            ResolvedStageAccountingView, resolve_attempt_stage_accounting,
            summarize_resolved_attempt_stage_accounting,
        },
        ingest_activity::IngestActivityService,
        query_runtime::{
            hydrate_runtime_query_enrichment, parse_runtime_query_enrichment,
            parse_runtime_query_warning,
        },
        queue_isolation::QueueIsolationService,
        runtime_ingestion::{
            build_runtime_collection_graph_throughput_summary,
            build_runtime_document_graph_throughput_summary,
            classify_runtime_document_activity_with_service,
            rank_runtime_graph_progress_bottlenecks,
        },
    },
    shared::file_extract::{
        EXTRACTED_CONTENT_PREVIEW_LIMIT, build_extracted_content_preview,
        extraction_quality_from_source_map,
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

#[derive(Debug, Clone, Deserialize)]
struct StructuredReferencePayload {
    kind: String,
    reference_id: Uuid,
    excerpt: Option<String>,
    rank: usize,
    score: Option<f32>,
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
    allow_legacy_bootstrap_side_effects: bool,
) -> Result<ResolvedShellContext, sqlx::Error> {
    let mut workspaces = repositories::list_workspaces_for_ui_user(pool, user.id).await?;
    if workspaces.is_empty() && allow_legacy_bootstrap_side_effects {
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
    if projects.is_empty() && allow_legacy_bootstrap_side_effects {
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

pub async fn load_document_row(
    pool: &PgPool,
    ingest_activity: &IngestActivityService,
    run: &RuntimeIngestionRunRow,
    library_name: &str,
    graph_progress: Option<&RuntimeGraphProgressCheckpointRow>,
    graph_resume_rollup: Option<&repositories::RuntimeGraphExtractionResumeRollupRow>,
    bottleneck_rank: Option<usize>,
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
    let graph_throughput = build_runtime_document_graph_throughput_summary(
        graph_progress,
        graph_resume_rollup,
        bottleneck_rank,
    )
    .map(map_document_graph_throughput);

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
        settled_estimated_cost: latest_summary
            .as_ref()
            .and_then(|item| item.settled_estimated_cost.and_then(|value| value.to_f64())),
        in_flight_estimated_cost: latest_summary
            .as_ref()
            .and_then(|item| item.in_flight_estimated_cost.and_then(|value| value.to_f64())),
        currency: latest_summary.as_ref().and_then(|item| item.currency.clone()),
        in_flight_stage_count: latest_summary
            .as_ref()
            .map(|item| item.in_flight_stage_count)
            .unwrap_or(0),
        missing_stage_count: latest_summary
            .as_ref()
            .map(|item| item.missing_stage_count)
            .unwrap_or(0),
        partial_history,
        partial_history_reason: partial_history
            .then_some("Legacy runtime history is incomplete for this attempt.".to_string()),
        graph_throughput,
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
    queue_isolation_service: &QueueIsolationService,
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
    let extraction_recovery = load_document_extraction_recovery_summary(pool, run).await?;
    let graph_stats =
        load_document_graph_stats(pool, run.project_id, run.document_id, run.revision_id).await?;
    let graph_node_id = load_document_graph_node_id(pool, run.project_id, run.document_id).await?;
    let canonical_summary_preview =
        load_document_canonical_summary_preview(pool, run.project_id, graph_node_id.as_deref())
            .await?;
    let reconciliation_scope = load_document_mutation_impact_scope(pool, run.document_id).await?;
    let graph_progress =
        repositories::get_runtime_graph_progress_checkpoint(pool, run.id, run.current_attempt_no)
            .await?;
    let graph_resume_rollup =
        repositories::load_runtime_graph_extraction_resume_rollup_by_run(pool, run.id).await?;
    let workspace_projection =
        repositories::load_documents_workspace_projection_rows(pool, run.project_id).await?;
    let stage_rollups =
        repositories::list_runtime_collection_settlement_rollups(pool, run.project_id, "stage")
            .await?;
    let format_rollups =
        repositories::list_runtime_collection_settlement_rollups(pool, run.project_id, "format")
            .await?;
    let bottleneck_rank = if run.current_stage == "extracting_graph" && run.status == "processing" {
        let rows = repositories::list_active_runtime_graph_progress_checkpoints_by_project(
            pool,
            run.project_id,
        )
        .await?;
        let ranks = rank_runtime_graph_progress_bottlenecks(&rows);
        ranks.get(&(run.id, run.current_attempt_no)).copied()
    } else {
        None
    };
    let requested_by = match run.document_id {
        Some(document_id) => {
            repositories::get_active_document_mutation_workflow_by_document_id(pool, document_id)
                .await?
                .and_then(|workflow| workflow.requested_by)
        }
        None => None,
    };
    let extracted_stats = build_extracted_stats(
        projection.as_ref(),
        extracted.as_ref(),
        contribution.chunk_count,
        extraction_recovery,
    );
    let graph_throughput = build_runtime_document_graph_throughput_summary(
        graph_progress.as_ref(),
        graph_resume_rollup.as_ref(),
        bottleneck_rank,
    )
    .map(map_document_graph_throughput);
    let active_graph_progress =
        repositories::list_active_runtime_graph_progress_checkpoints_by_project(
            pool,
            run.project_id,
        )
        .await?;
    let active_graph_resume_rollups =
        repositories::list_active_runtime_graph_extraction_resume_rollups_by_project(
            pool,
            run.project_id,
        )
        .await?;
    let collection_graph_throughput = build_runtime_collection_graph_throughput_summary(
        &active_graph_progress,
        &active_graph_resume_rollups,
    );
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
        settled_estimated_cost: latest_summary
            .as_ref()
            .and_then(|item| item.settled_estimated_cost.and_then(|value| value.to_f64())),
        in_flight_estimated_cost: latest_summary
            .as_ref()
            .and_then(|item| item.in_flight_estimated_cost.and_then(|value| value.to_f64())),
        currency: latest_summary.as_ref().and_then(|item| item.currency.clone()),
        in_flight_stage_count: latest_summary
            .as_ref()
            .map(|item| item.in_flight_stage_count)
            .unwrap_or(0),
        missing_stage_count: latest_summary
            .as_ref()
            .map(|item| item.missing_stage_count)
            .unwrap_or(0),
        partial_history,
        partial_history_reason,
        mutation: build_mutation_state(projection.as_ref()),
        requested_by,
        error_message: run.latest_error_message.clone(),
        failure_class: derive_detail_failure_class(
            run.latest_error_message.as_deref(),
            graph_progress.as_ref(),
            workspace_projection.terminal_outcome.as_ref(),
        ),
        operator_action: derive_detail_operator_action(
            run.latest_error_message.as_deref(),
            graph_progress.as_ref(),
            workspace_projection.terminal_outcome.as_ref(),
        ),
        summary: format_document_detail_summary(
            &run.status,
            extracted_stats.chunk_count,
            &graph_stats,
        ),
        graph_node_id,
        canonical_summary_preview,
        can_download_text: extracted
            .as_ref()
            .and_then(|item| item.content_text.as_ref())
            .is_some_and(|text| !text.trim().is_empty()),
        can_append: can_update_document(run, projection.as_ref()),
        can_replace: can_update_document(run, projection.as_ref()),
        can_remove: can_remove_document(projection.as_ref()),
        reconciliation_scope,
        provider_failure: graph_progress.as_ref().and_then(map_document_provider_failure_summary),
        graph_throughput,
        extracted_stats,
        graph_stats,
        collection_diagnostics: Some(build_collection_diagnostics(
            queue_isolation_service,
            &workspace_projection.queue_slice,
            workspace_projection.settlement_snapshot.as_ref(),
            workspace_projection.terminal_outcome.as_ref(),
            workspace_projection.graph_diagnostics.as_ref(),
            &stage_rollups,
            &format_rollups,
            &workspace_projection.warnings,
            collection_graph_throughput.as_ref(),
        )),
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

async fn load_document_mutation_impact_scope(
    pool: &PgPool,
    document_id: Option<Uuid>,
) -> Result<Option<MutationImpactScopeSummary>, sqlx::Error> {
    let Some(document_id) = document_id else {
        return Ok(None);
    };
    let row =
        repositories::get_active_document_mutation_impact_scope_by_document_id(pool, document_id)
            .await?;
    Ok(row.map(|row| MutationImpactScopeSummary {
        scope_status: match row.scope_status.as_str() {
            "pending" => MutationImpactScopeStatus::Pending,
            "targeted" => MutationImpactScopeStatus::Targeted,
            "fallback_broad" => MutationImpactScopeStatus::FallbackBroad,
            "completed" => MutationImpactScopeStatus::Completed,
            "failed" => MutationImpactScopeStatus::Failed,
            _ => MutationImpactScopeStatus::Pending,
        },
        confidence_status: match row.confidence_status.as_str() {
            "high" => MutationImpactScopeConfidence::High,
            "medium" => MutationImpactScopeConfidence::Medium,
            "low" => MutationImpactScopeConfidence::Low,
            _ => MutationImpactScopeConfidence::Low,
        },
        affected_node_count: serde_json::from_value::<Vec<Uuid>>(row.affected_node_ids_json)
            .map(|ids| ids.len())
            .unwrap_or_default(),
        affected_relationship_count: serde_json::from_value::<Vec<Uuid>>(
            row.affected_relationship_ids_json,
        )
        .map(|ids| ids.len())
        .unwrap_or_default(),
        fallback_reason: row.fallback_reason,
    }))
}

pub async fn load_graph_assistant(
    pool: &PgPool,
    project_id: Uuid,
    config: Option<GraphAssistantConfigModel>,
) -> Result<GraphAssistantModel, sqlx::Error> {
    let session_rows = repositories::list_chat_sessions_by_project(pool, project_id).await?;
    let recent_sessions = session_rows
        .iter()
        .cloned()
        .map(|row| ChatSessionSummaryModel {
            session_id: row.id.to_string(),
            title: row.title,
            message_count: row.message_count,
            last_message_preview: row
                .last_message_preview
                .map(|value| value.split_whitespace().collect::<Vec<_>>().join(" ")),
            updated_at: row.updated_at.to_rfc3339(),
            prompt_state: row.prompt_state,
            preferred_mode: row.preferred_mode,
            is_empty: row.message_count == 0,
        })
        .collect::<Vec<_>>();

    let active_detail = match session_rows.first() {
        Some(session) => repositories::get_chat_session_detail_by_id(pool, session.id).await?,
        None => None,
    };
    let settings_summary = active_detail.as_ref().map(|detail| ChatSessionSettingsModel {
        session_id: detail.id.to_string(),
        system_prompt: detail.system_prompt.clone(),
        prompt_state: detail.prompt_state.clone(),
        preferred_mode: detail.preferred_mode.clone(),
        default_prompt_available: true,
    });
    let active_session = active_detail.as_ref().map(|detail| ChatSessionDetailModel {
        session_id: detail.id.to_string(),
        title: detail.title.clone(),
        message_count: detail.message_count,
        last_message_preview: detail
            .last_message_preview
            .as_ref()
            .map(|value| value.split_whitespace().collect::<Vec<_>>().join(" ")),
        created_at: detail.created_at.to_rfc3339(),
        updated_at: detail.updated_at.to_rfc3339(),
        prompt_state: detail.prompt_state.clone(),
        preferred_mode: detail.preferred_mode.clone(),
        is_empty: detail.message_count == 0,
    });

    let messages = match active_detail.as_ref() {
        Some(detail) => {
            let rows = repositories::list_chat_thread_messages_by_session(pool, detail.id).await?;
            let query_ids = rows
                .iter()
                .filter_map(|row| {
                    row.retrieval_debug_json
                        .as_ref()
                        .and_then(|debug_json| debug_json.get("query_id"))
                        .and_then(serde_json::Value::as_str)
                        .and_then(|value| Uuid::parse_str(value).ok())
                })
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let enrichment_rows =
                repositories::list_runtime_query_enrichments_by_execution_ids(pool, &query_ids)
                    .await?;
            let grouped_reference_rows =
                repositories::list_runtime_query_reference_groups_by_execution_ids(
                    pool, &query_ids,
                )
                .await?;
            let enrichment_by_query_id = enrichment_rows
                .into_iter()
                .map(|row| (row.query_execution_id, row))
                .collect::<HashMap<_, _>>();
            let mut grouped_references_by_query_id: HashMap<
                Uuid,
                Vec<RuntimeQueryReferenceGroupRow>,
            > = HashMap::new();
            for row in grouped_reference_rows {
                grouped_references_by_query_id.entry(row.query_execution_id).or_default().push(row);
            }

            rows.into_iter()
                .map(|row| {
                    let debug_json =
                        row.retrieval_debug_json.unwrap_or_else(|| serde_json::json!({}));
                    let fallback_mode = debug_json
                        .get("mode")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("hybrid")
                        .parse::<RuntimeQueryMode>()
                        .unwrap_or(RuntimeQueryMode::Hybrid);
                    let query_id = debug_json
                        .get("query_id")
                        .and_then(serde_json::Value::as_str)
                        .and_then(|value| Uuid::parse_str(value).ok());
                    let enrichment = if row.retrieval_run_id.is_some() {
                        Some(match query_id {
                            Some(query_id) => hydrate_runtime_query_enrichment(
                                enrichment_by_query_id.get(&query_id),
                                grouped_references_by_query_id
                                    .get(&query_id)
                                    .map(Vec::as_slice)
                                    .unwrap_or(&[]),
                                &debug_json,
                                fallback_mode,
                            ),
                            None => parse_runtime_query_enrichment(&debug_json, fallback_mode),
                        })
                    } else {
                        None
                    };
                    let references = debug_json
                        .get("structured_references")
                        .cloned()
                        .map(serde_json::from_value::<Vec<StructuredReferencePayload>>)
                        .transpose()
                        .map_err(|error| sqlx::Error::Decode(Box::new(error)))?
                        .unwrap_or_default()
                        .into_iter()
                        .map(|reference| GraphAssistantReferenceModel {
                            kind: reference.kind,
                            reference_id: reference.reference_id.to_string(),
                            excerpt: reference.excerpt,
                            rank: reference.rank,
                            score: reference.score,
                        })
                        .collect::<Vec<_>>();
                    let (warning, warning_kind) = parse_runtime_query_warning(&debug_json);

                    Ok(GraphAssistantMessageModel {
                        id: row.id.to_string(),
                        role: row.role,
                        content: row.content,
                        created_at: row.created_at.to_rfc3339(),
                        query_id: debug_json
                            .get("query_id")
                            .and_then(serde_json::Value::as_str)
                            .map(ToOwned::to_owned),
                        mode: debug_json
                            .get("mode")
                            .and_then(serde_json::Value::as_str)
                            .map(ToOwned::to_owned),
                        grounding_status: debug_json
                            .get("grounding_status")
                            .and_then(serde_json::Value::as_str)
                            .map(ToOwned::to_owned),
                        provider: debug_json
                            .get("provider_kind")
                            .and_then(serde_json::Value::as_str)
                            .map(|provider_kind| GraphAssistantProviderModel {
                                provider_kind: provider_kind.to_string(),
                                model_name: debug_json
                                    .get("model_name")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                            }),
                        references,
                        planning: enrichment.as_ref().map(|value| value.planning.clone()),
                        rerank: enrichment.as_ref().map(|value| value.rerank.clone()),
                        context_assembly: enrichment
                            .as_ref()
                            .map(|value| value.context_assembly.clone()),
                        grouped_references: enrichment
                            .as_ref()
                            .map(|value| value.grouped_references.clone())
                            .unwrap_or_default(),
                        warning,
                        warning_kind,
                    })
                })
                .collect::<Result<Vec<_>, sqlx::Error>>()?
        }
        None => Vec::new(),
    };

    Ok(GraphAssistantModel {
        title: "Ask AI".to_string(),
        subtitle: "Questions stay inside the active library.".to_string(),
        prompts: vec![
            "Summarize the most connected entities in this library.".to_string(),
            "Which documents contribute the strongest graph evidence?".to_string(),
            "What themes are visible in the current graph?".to_string(),
        ],
        disclaimer: "Answers use the active library and its current graph projection.".to_string(),
        config,
        session_id: active_session.as_ref().map(|session| session.session_id.clone()),
        recent_sessions,
        active_session,
        settings_summary,
        focus_context: None,
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
    recovery: Option<crate::domains::graph_quality::ExtractionRecoverySummary>,
) -> DocumentExtractedStats {
    let warnings = extracted
        .and_then(|item| value_to_string_vec(&item.extraction_warnings_json))
        .unwrap_or_default();
    let quality = extracted.map(|item| {
        extraction_quality_from_source_map(
            &item.source_map_json,
            &item.extraction_kind,
            warnings.len(),
        )
    });
    let preview = extracted.map(|item| {
        build_extracted_content_preview(
            item.content_text.as_deref(),
            EXTRACTED_CONTENT_PREVIEW_LIMIT,
        )
    });

    DocumentExtractedStats {
        chunk_count,
        document_id: projection.map(|item| item.id.to_string()),
        checksum: projection.and_then(|item| item.checksum.clone()),
        page_count: extracted.and_then(|item| item.page_count),
        extraction_kind: extracted.map(|item| item.extraction_kind.clone()),
        preview_text: preview.as_ref().and_then(|item| item.text.clone()),
        preview_truncated: preview.as_ref().is_some_and(|item| item.truncated),
        warning_count: quality.as_ref().map(|item| item.warning_count).unwrap_or(0),
        normalization_status: quality
            .as_ref()
            .map(|item| item.normalization_status.as_str().to_string())
            .unwrap_or_else(|| "verbatim".to_string()),
        ocr_source: quality.and_then(|item| item.ocr_source),
        recovery,
        warnings,
    }
}

async fn load_document_extraction_recovery_summary(
    pool: &PgPool,
    run: &RuntimeIngestionRunRow,
) -> Result<Option<crate::domains::graph_quality::ExtractionRecoverySummary>, sqlx::Error> {
    let attempts = repositories::list_runtime_graph_extraction_recovery_attempts_by_run(
        pool,
        run.id,
        run.current_attempt_no,
    )
    .await?;
    Ok(crate::services::extraction_recovery::ExtractionRecoveryService
        .summarize_attempt_rows(&attempts))
}

async fn load_document_canonical_summary_preview(
    pool: &PgPool,
    project_id: Uuid,
    graph_node_id: Option<&str>,
) -> Result<Option<CanonicalGraphSummary>, sqlx::Error> {
    let Some(graph_node_id) = graph_node_id else {
        return Ok(None);
    };
    let Ok(graph_node_uuid) = Uuid::parse_str(graph_node_id) else {
        return Ok(None);
    };
    let summary = repositories::get_active_runtime_graph_canonical_summary_by_target(
        pool,
        project_id,
        "node",
        graph_node_uuid,
    )
    .await?;

    Ok(summary.map(|row| CanonicalGraphSummary {
        text: row.summary_text,
        confidence_status: match row.confidence_status.as_str() {
            "strong" => GraphSummaryConfidenceStatus::Strong,
            "partial" => GraphSummaryConfidenceStatus::Partial,
            "conflicted" => GraphSummaryConfidenceStatus::Conflicted,
            _ => GraphSummaryConfidenceStatus::Weak,
        },
        support_count: usize::try_from(row.support_count).unwrap_or_default(),
        warning: row.warning_text,
    }))
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

    attempt_nos
        .into_iter()
        .rev()
        .map(|attempt_no| {
            let attempt_stage_events =
                stage_events_by_attempt.get(&attempt_no).cloned().unwrap_or_default();
            let resolved_stage_accounting =
                resolve_attempt_stage_accounting(&attempt_stage_events, stage_accounting);
            let accounting_by_event = resolved_stage_accounting
                .iter()
                .filter_map(|row| row.anchor_event_id.map(|event_id| (event_id, row.clone())))
                .collect::<HashMap<_, _>>();
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
                    accounting: accounting_by_event
                        .get(&event.id)
                        .map(map_document_stage_accounting),
                })
                .collect::<Vec<_>>();
            let summary = summarize_attempt_accounting(&resolved_stage_accounting);
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

fn map_document_stage_accounting(row: &ResolvedStageAccountingView) -> DocumentStageAccountingItem {
    DocumentStageAccountingItem {
        accounting_scope: row.accounting_scope.clone(),
        pricing_status: row.pricing_status.clone(),
        usage_event_id: row.usage_event_id.map(|value| value.to_string()),
        cost_ledger_id: row.cost_ledger_id.map(|value| value.to_string()),
        pricing_catalog_entry_id: row.pricing_catalog_entry_id.map(|value| value.to_string()),
        estimated_cost: row.estimated_cost.and_then(|value| value.to_f64()),
        settled_estimated_cost: row.settled_estimated_cost.and_then(|value| value.to_f64()),
        in_flight_estimated_cost: row.in_flight_estimated_cost.and_then(|value| value.to_f64()),
        currency: row.currency.clone(),
        attribution_source: Some(row.attribution_source.clone()),
    }
}

fn build_collection_diagnostics(
    queue_isolation_service: &QueueIsolationService,
    queue_slice: &RuntimeLibraryQueueSliceRow,
    settlement_row: Option<&repositories::RuntimeCollectionSettlementRow>,
    terminal_outcome: Option<&repositories::RuntimeCollectionTerminalOutcomeRow>,
    graph_health: Option<&repositories::RuntimeGraphDiagnosticsSnapshotRow>,
    stage_rollups: &[repositories::RuntimeCollectionSettlementRollupRow],
    format_rollups: &[repositories::RuntimeCollectionSettlementRollupRow],
    warning_rows: &[repositories::RuntimeCollectionWarningRow],
    graph_throughput: Option<
        &crate::domains::runtime_ingestion::RuntimeCollectionGraphThroughputSummary,
    >,
) -> DocumentCollectionDiagnostics {
    let queue_isolation = queue_isolation_service.summarize(
        usize::try_from(queue_slice.queued_count).unwrap_or(usize::MAX),
        usize::try_from(queue_slice.processing_count).unwrap_or(usize::MAX),
        usize::try_from(queue_slice.workspace_processing_count).unwrap_or(usize::MAX),
        usize::try_from(queue_slice.global_processing_count).unwrap_or(usize::MAX),
        queue_slice.last_claimed_at,
        queue_slice.last_progress_at,
        parse_waiting_reason(queue_slice.waiting_reason.as_deref()),
    );
    let settlement_row =
        settlement_row.cloned().unwrap_or_else(|| repositories::RuntimeCollectionSettlementRow {
            project_id: Uuid::nil(),
            progress_state: "fully_settled".to_string(),
            terminal_state: "fully_settled".to_string(),
            terminal_transition_at: chrono::Utc::now(),
            residual_reason: None,
            document_count: 0,
            accepted_count: 0,
            content_extracted_count: 0,
            chunked_count: 0,
            embedded_count: 0,
            graph_active_count: 0,
            graph_ready_count: 0,
            pending_graph_count: 0,
            ready_count: 0,
            failed_count: 0,
            queue_backlog_count: 0,
            processing_backlog_count: 0,
            live_total_estimated_cost: None,
            settled_total_estimated_cost: None,
            missing_total_estimated_cost: None,
            currency: None,
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            priced_stage_count: 0,
            unpriced_stage_count: 0,
            in_flight_stage_count: 0,
            missing_stage_count: 0,
            accounting_status: "unpriced".to_string(),
            is_fully_settled: true,
            settled_at: Some(Utc::now()),
            computed_at: Utc::now(),
        });

    DocumentCollectionDiagnostics {
        progress: DocumentCollectionProgressCounters {
            accepted: usize::try_from(settlement_row.accepted_count).unwrap_or(usize::MAX),
            content_extracted: usize::try_from(settlement_row.content_extracted_count)
                .unwrap_or(usize::MAX),
            chunked: usize::try_from(settlement_row.chunked_count).unwrap_or(usize::MAX),
            embedded: usize::try_from(settlement_row.embedded_count).unwrap_or(usize::MAX),
            extracting_graph: usize::try_from(settlement_row.graph_active_count)
                .unwrap_or(usize::MAX),
            graph_ready: usize::try_from(settlement_row.graph_ready_count).unwrap_or(usize::MAX),
            ready: usize::try_from(settlement_row.ready_count).unwrap_or(usize::MAX),
            failed: usize::try_from(settlement_row.failed_count).unwrap_or(usize::MAX),
        },
        queue_backlog_count: usize::try_from(settlement_row.queue_backlog_count)
            .unwrap_or(usize::MAX),
        processing_backlog_count: usize::try_from(settlement_row.processing_backlog_count)
            .unwrap_or(usize::MAX),
        active_backlog_count: usize::try_from(
            settlement_row.queue_backlog_count + settlement_row.processing_backlog_count,
        )
        .unwrap_or(usize::MAX),
        queue_isolation: Some(DocumentQueueIsolationSummary {
            waiting_reason: repositories::runtime_queue_waiting_reason_key(
                &queue_isolation.waiting_reason,
            )
            .to_string(),
            queued_count: queue_isolation.queued_count,
            processing_count: queue_isolation.processing_count,
            isolated_capacity_count: queue_isolation.isolated_capacity_count,
            available_capacity_count: queue_isolation.available_capacity_count,
            last_claimed_at: queue_isolation.last_claimed_at.map(|value| value.to_rfc3339()),
            last_progress_at: queue_isolation.last_progress_at.map(|value| value.to_rfc3339()),
        }),
        graph_throughput: graph_throughput.map(map_collection_graph_throughput),
        settlement: Some(DocumentCollectionSettlementSummary {
            progress_state: settlement_row.progress_state.clone(),
            live_total_estimated_cost: settlement_row
                .live_total_estimated_cost
                .as_ref()
                .and_then(rust_decimal::Decimal::to_f64),
            settled_total_estimated_cost: settlement_row
                .settled_total_estimated_cost
                .as_ref()
                .and_then(rust_decimal::Decimal::to_f64),
            missing_total_estimated_cost: settlement_row
                .missing_total_estimated_cost
                .as_ref()
                .and_then(rust_decimal::Decimal::to_f64),
            currency: settlement_row.currency.clone(),
            is_fully_settled: settlement_row.is_fully_settled,
            settled_at: settlement_row.settled_at.map(|value| value.to_rfc3339()),
        }),
        terminal_outcome: terminal_outcome.map(map_document_terminal_outcome_summary),
        graph_health: graph_health.map(map_document_graph_health_summary),
        warnings: warning_rows
            .iter()
            .map(|warning| DocumentCollectionWarning {
                warning_kind: warning.warning_kind.clone(),
                warning_scope: warning.warning_scope.clone(),
                warning_message: warning.warning_message.clone(),
                is_degraded: warning.is_degraded,
            })
            .collect(),
        per_stage: build_collection_stage_diagnostics(stage_rollups),
        per_format: build_collection_format_diagnostics(format_rollups),
    }
}

fn map_document_terminal_outcome_summary(
    row: &repositories::RuntimeCollectionTerminalOutcomeRow,
) -> DocumentTerminalOutcomeSummary {
    DocumentTerminalOutcomeSummary {
        terminal_state: row.terminal_state.clone(),
        residual_reason: row.residual_reason.clone(),
        queued_count: usize::try_from(row.queued_count).unwrap_or(usize::MAX),
        processing_count: usize::try_from(row.processing_count).unwrap_or(usize::MAX),
        pending_graph_count: usize::try_from(row.pending_graph_count).unwrap_or(usize::MAX),
        failed_document_count: usize::try_from(row.failed_document_count).unwrap_or(usize::MAX),
        settled_at: row.settled_at.map(|value| value.to_rfc3339()),
    }
}

fn map_document_graph_health_summary(
    row: &repositories::RuntimeGraphDiagnosticsSnapshotRow,
) -> DocumentGraphHealthSummary {
    DocumentGraphHealthSummary {
        projection_health: row.projection_health.clone(),
        active_projection_count: usize::try_from(row.active_projection_count).unwrap_or(usize::MAX),
        retrying_projection_count: usize::try_from(row.retrying_projection_count)
            .unwrap_or(usize::MAX),
        failed_projection_count: usize::try_from(row.failed_projection_count).unwrap_or(usize::MAX),
        pending_node_write_count: usize::try_from(row.pending_node_write_count)
            .unwrap_or(usize::MAX),
        pending_edge_write_count: usize::try_from(row.pending_edge_write_count)
            .unwrap_or(usize::MAX),
        last_failure_kind: row.last_projection_failure_kind.clone(),
        last_failure_at: row.last_projection_failure_at.map(|value| value.to_rfc3339()),
        is_runtime_readable: row.is_runtime_readable,
        snapshot_at: row.snapshot_at.to_rfc3339(),
    }
}

fn map_document_provider_failure_summary(
    row: &RuntimeGraphProgressCheckpointRow,
) -> Option<DocumentProviderFailureSummary> {
    row.provider_failure_class.as_ref().map(|failure_class| DocumentProviderFailureSummary {
        failure_class: failure_class.clone(),
        provider_kind: None,
        model_name: None,
        request_shape_key: row.request_shape_key.clone(),
        request_size_bytes: row.request_size_bytes.and_then(|value| usize::try_from(value).ok()),
        upstream_status: row.upstream_status.clone(),
        elapsed_ms: None,
        retry_decision: row.retry_outcome.clone(),
        usage_visible: row.provider_call_count > 0,
    })
}

fn derive_detail_failure_class(
    latest_error: Option<&str>,
    graph_progress: Option<&RuntimeGraphProgressCheckpointRow>,
    terminal_outcome: Option<&repositories::RuntimeCollectionTerminalOutcomeRow>,
) -> Option<String> {
    graph_progress
        .and_then(|row| row.provider_failure_class.clone())
        .or_else(|| latest_error.and_then(classify_detail_failure_class).map(str::to_string))
        .or_else(|| terminal_outcome.and_then(|row| row.residual_reason.clone()))
}

fn derive_detail_operator_action(
    latest_error: Option<&str>,
    graph_progress: Option<&RuntimeGraphProgressCheckpointRow>,
    terminal_outcome: Option<&repositories::RuntimeCollectionTerminalOutcomeRow>,
) -> Option<String> {
    derive_detail_failure_class(latest_error, graph_progress, terminal_outcome)
        .map(|value| detail_operator_action(&value).to_string())
}

fn classify_detail_failure_class(latest_error: &str) -> Option<&'static str> {
    let normalized = latest_error.to_ascii_lowercase();
    if normalized.contains("upload_limit_exceeded")
        || normalized.contains("upload limit exceeded")
        || normalized.contains("exceeded the size limit")
    {
        return Some("upload_limit_exceeded");
    }
    if normalized.contains("projection contention")
        || normalized.contains("deadlock")
        || normalized.contains("lock timeout")
    {
        return Some("projection_contention");
    }
    if normalized.contains("graph persistence integrity")
        || normalized.contains("foreign key violation")
        || normalized.contains("runtime_graph_edge")
    {
        return Some("graph_persistence_integrity");
    }
    if normalized.contains("settlement refresh failed")
        || normalized.contains("failed to persist collection settlement")
        || normalized.contains("failed to persist collection terminal outcome")
    {
        return Some("settlement_refresh_failed");
    }
    if normalized.contains("could not parse the json body of your request")
        || normalized.contains("expects a json payload")
        || normalized.contains("upstream protocol failure")
    {
        return Some("upstream_protocol_failure");
    }
    if normalized.contains("provider failure")
        || normalized.contains("upstream timeout")
        || normalized.contains("upstream protocol failure")
        || normalized.contains("upstream rejection")
        || normalized.contains("invalid model output")
        || normalized.contains("invalid_request")
        || normalized.contains("invalid request")
    {
        return Some("provider_failure");
    }
    None
}

fn detail_operator_action(failure_class: &str) -> &'static str {
    match failure_class {
        "upload_limit_exceeded" => "reduce_upload_size",
        "projection_contention" => "retry_projection_or_wait",
        "graph_persistence_integrity" => "inspect_graph_integrity",
        "settlement_refresh_failed" => "refresh_settlement",
        "internal_request_invalid" => "inspect_request_shape",
        "upstream_protocol_failure" => "retry_provider_call",
        "upstream_timeout" => "retry_provider_call",
        "upstream_rejection" => "check_provider_limits",
        "invalid_model_output" => "retry_or_relax_schema",
        "recovered_after_retry" => "no_action_required",
        "provider_failure" => "inspect_provider_failure",
        _ => "inspect_failure_logs",
    }
}

fn map_document_graph_throughput(
    summary: crate::domains::runtime_ingestion::RuntimeDocumentGraphThroughputSummary,
) -> DocumentGraphThroughputSummary {
    DocumentGraphThroughputSummary {
        processed_chunks: summary.processed_chunks,
        total_chunks: summary.total_chunks,
        progress_percent: summary.progress_percent,
        provider_call_count: summary.provider_call_count,
        resumed_chunk_count: summary.resumed_chunk_count,
        resume_hit_count: summary.resume_hit_count,
        replayed_chunk_count: summary.replayed_chunk_count,
        duplicate_work_ratio: summary.duplicate_work_ratio,
        max_downgrade_level: summary.max_downgrade_level,
        avg_call_elapsed_ms: summary.avg_call_elapsed_ms,
        avg_chunk_elapsed_ms: summary.avg_chunk_elapsed_ms,
        avg_chars_per_second: summary.avg_chars_per_second,
        avg_tokens_per_second: summary.avg_tokens_per_second,
        last_provider_call_at: summary.last_provider_call_at.map(|value| value.to_rfc3339()),
        last_checkpoint_at: summary.last_checkpoint_at.to_rfc3339(),
        last_checkpoint_elapsed_ms: summary.last_checkpoint_elapsed_ms,
        next_checkpoint_eta_ms: summary.next_checkpoint_eta_ms,
        pressure_kind: summary.pressure_kind,
        cadence: repositories::runtime_graph_progress_cadence_key(&summary.cadence).to_string(),
        recommended_poll_interval_ms: summary.recommended_poll_interval_ms,
        bottleneck_rank: summary.bottleneck_rank,
    }
}

fn map_collection_graph_throughput(
    summary: &crate::domains::runtime_ingestion::RuntimeCollectionGraphThroughputSummary,
) -> DocumentCollectionGraphThroughputSummary {
    DocumentCollectionGraphThroughputSummary {
        tracked_document_count: summary.tracked_document_count,
        active_document_count: summary.active_document_count,
        processed_chunks: summary.processed_chunks,
        total_chunks: summary.total_chunks,
        progress_percent: summary.progress_percent,
        provider_call_count: summary.provider_call_count,
        resumed_chunk_count: summary.resumed_chunk_count,
        resume_hit_count: summary.resume_hit_count,
        replayed_chunk_count: summary.replayed_chunk_count,
        duplicate_work_ratio: summary.duplicate_work_ratio,
        max_downgrade_level: summary.max_downgrade_level,
        avg_call_elapsed_ms: summary.avg_call_elapsed_ms,
        avg_chunk_elapsed_ms: summary.avg_chunk_elapsed_ms,
        avg_chars_per_second: summary.avg_chars_per_second,
        avg_tokens_per_second: summary.avg_tokens_per_second,
        last_provider_call_at: summary.last_provider_call_at.map(|value| value.to_rfc3339()),
        last_checkpoint_at: summary.last_checkpoint_at.to_rfc3339(),
        last_checkpoint_elapsed_ms: summary.last_checkpoint_elapsed_ms,
        next_checkpoint_eta_ms: summary.next_checkpoint_eta_ms,
        pressure_kind: summary.pressure_kind.clone(),
        cadence: repositories::runtime_graph_progress_cadence_key(&summary.cadence).to_string(),
        recommended_poll_interval_ms: summary.recommended_poll_interval_ms,
        bottleneck_rank: summary.bottleneck_rank,
    }
}

fn build_collection_stage_diagnostics(
    stage_rollups: &[repositories::RuntimeCollectionSettlementRollupRow],
) -> Vec<DocumentCollectionStageDiagnostics> {
    let mut diagnostics = stage_rollups
        .iter()
        .map(|rollup| DocumentCollectionStageDiagnostics {
            stage: rollup.scope_key.clone(),
            active_count: usize::try_from(rollup.processing_count).unwrap_or(usize::MAX),
            completed_count: usize::try_from(rollup.completed_count).unwrap_or(usize::MAX),
            failed_count: usize::try_from(rollup.failed_count).unwrap_or(usize::MAX),
            avg_elapsed_ms: rollup.avg_elapsed_ms,
            max_elapsed_ms: rollup.max_elapsed_ms,
            total_estimated_cost: sum_collection_cost_f64(
                rollup.live_estimated_cost,
                rollup.settled_estimated_cost,
            ),
            settled_estimated_cost: rollup.settled_estimated_cost.and_then(|value| value.to_f64()),
            in_flight_estimated_cost: rollup.live_estimated_cost.and_then(|value| value.to_f64()),
            currency: rollup.currency.clone(),
            prompt_tokens: rollup.prompt_tokens,
            completion_tokens: rollup.completion_tokens,
            total_tokens: rollup.total_tokens,
            accounting_status: rollup.accounting_status.clone(),
        })
        .collect::<Vec<_>>();

    diagnostics.sort_by_key(|item| stage_sort_key(&item.stage));
    diagnostics
}

fn build_collection_format_diagnostics(
    format_rollups: &[repositories::RuntimeCollectionSettlementRollupRow],
) -> Vec<DocumentCollectionFormatDiagnostics> {
    format_rollups
        .iter()
        .map(|rollup| DocumentCollectionFormatDiagnostics {
            file_type: humanize_file_type(&rollup.scope_key),
            document_count: usize::try_from(rollup.document_count).unwrap_or(usize::MAX),
            queued_count: usize::try_from(rollup.queued_count).unwrap_or(usize::MAX),
            processing_count: usize::try_from(rollup.processing_count).unwrap_or(usize::MAX),
            ready_count: usize::try_from(rollup.ready_count).unwrap_or(usize::MAX),
            ready_no_graph_count: usize::try_from(rollup.ready_no_graph_count)
                .unwrap_or(usize::MAX),
            failed_count: usize::try_from(rollup.failed_count).unwrap_or(usize::MAX),
            content_extracted_count: usize::try_from(rollup.content_extracted_count)
                .unwrap_or(usize::MAX),
            chunked_count: usize::try_from(rollup.chunked_count).unwrap_or(usize::MAX),
            embedded_count: usize::try_from(rollup.embedded_count).unwrap_or(usize::MAX),
            extracting_graph_count: usize::try_from(rollup.graph_active_count)
                .unwrap_or(usize::MAX),
            graph_ready_count: usize::try_from(rollup.graph_ready_count).unwrap_or(usize::MAX),
            avg_queue_elapsed_ms: None,
            max_queue_elapsed_ms: None,
            avg_total_elapsed_ms: rollup.avg_elapsed_ms,
            max_total_elapsed_ms: rollup.max_elapsed_ms,
            bottleneck_stage: rollup.bottleneck_stage.clone(),
            bottleneck_avg_elapsed_ms: rollup.bottleneck_avg_elapsed_ms,
            bottleneck_max_elapsed_ms: rollup.bottleneck_max_elapsed_ms,
            total_estimated_cost: sum_collection_cost_f64(
                rollup.live_estimated_cost,
                rollup.settled_estimated_cost,
            ),
            settled_estimated_cost: rollup.settled_estimated_cost.and_then(|value| value.to_f64()),
            in_flight_estimated_cost: rollup.live_estimated_cost.and_then(|value| value.to_f64()),
            currency: rollup.currency.clone(),
            prompt_tokens: rollup.prompt_tokens,
            completion_tokens: rollup.completion_tokens,
            total_tokens: rollup.total_tokens,
            accounting_status: rollup.accounting_status.clone(),
        })
        .collect()
}

fn parse_waiting_reason(
    value: Option<&str>,
) -> Option<crate::domains::runtime_ingestion::RuntimeQueueWaitingReason> {
    repositories::parse_runtime_queue_waiting_reason(value)
}

fn stage_sort_key(stage: &str) -> usize {
    match stage {
        "extracting_content" => 0,
        "chunking" => 1,
        "embedding_chunks" => 2,
        "extracting_graph" => 3,
        "merging_graph" => 4,
        "projecting_graph" => 5,
        "finalizing" => 6,
        "failed" => 7,
        _ => 99,
    }
}

fn sum_collection_cost_f64(
    live_estimated_cost: Option<rust_decimal::Decimal>,
    settled_estimated_cost: Option<rust_decimal::Decimal>,
) -> Option<f64> {
    match (live_estimated_cost, settled_estimated_cost) {
        (Some(live), Some(settled)) => (live + settled).to_f64(),
        (Some(live), None) => live.to_f64(),
        (None, Some(settled)) => settled.to_f64(),
        (None, None) => None,
    }
}

fn summarize_attempt_accounting(
    resolved_stage_accounting: &[ResolvedStageAccountingView],
) -> DocumentAttemptSummary {
    let summary = summarize_resolved_attempt_stage_accounting(resolved_stage_accounting);
    DocumentAttemptSummary {
        total_estimated_cost: summary.total_estimated_cost.and_then(|value| value.to_f64()),
        settled_estimated_cost: summary.settled_estimated_cost.and_then(|value| value.to_f64()),
        in_flight_estimated_cost: summary.in_flight_estimated_cost.and_then(|value| value.to_f64()),
        currency: summary.currency,
        priced_stage_count: summary.priced_stage_count,
        unpriced_stage_count: summary.unpriced_stage_count,
        in_flight_stage_count: summary.in_flight_stage_count,
        missing_stage_count: summary.missing_stage_count,
        accounting_status: summary.accounting_status,
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
