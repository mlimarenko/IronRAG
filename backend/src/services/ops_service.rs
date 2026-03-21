use chrono::Utc;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::knowledge::{KnowledgeLibraryGeneration, KnowledgeRevision},
    domains::ops::{OpsAsyncOperation, OpsLibraryState, OpsLibraryWarning},
    infra::repositories::ops_repository,
    interfaces::http::router_support::ApiError,
};

#[derive(Debug, Clone)]
pub struct CreateAsyncOperationCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub operation_kind: String,
    pub surface_kind: String,
    pub requested_by_principal_id: Option<Uuid>,
    pub status: String,
    pub subject_kind: String,
    pub subject_id: Option<Uuid>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub failure_code: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UpdateAsyncOperationCommand {
    pub operation_id: Uuid,
    pub status: String,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub failure_code: Option<String>,
}

#[derive(Clone, Default)]
pub struct OpsService;

#[derive(Debug, Clone)]
pub struct OpsLibraryStateSnapshot {
    pub state: OpsLibraryState,
    pub knowledge_generations: Vec<KnowledgeLibraryGeneration>,
}

impl OpsService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    pub async fn create_async_operation(
        &self,
        state: &AppState,
        command: CreateAsyncOperationCommand,
    ) -> Result<OpsAsyncOperation, ApiError> {
        let row = ops_repository::create_async_operation(
            &state.persistence.postgres,
            &ops_repository::NewOpsAsyncOperation {
                workspace_id: command.workspace_id,
                library_id: command.library_id,
                operation_kind: &command.operation_kind,
                surface_kind: &command.surface_kind,
                requested_by_principal_id: command.requested_by_principal_id,
                status: &command.status,
                subject_kind: &command.subject_kind,
                subject_id: command.subject_id,
                completed_at: command.completed_at,
                failure_code: command.failure_code.as_deref(),
            },
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(map_async_operation_row(row))
    }

    pub async fn update_async_operation(
        &self,
        state: &AppState,
        command: UpdateAsyncOperationCommand,
    ) -> Result<OpsAsyncOperation, ApiError> {
        let row = ops_repository::update_async_operation(
            &state.persistence.postgres,
            command.operation_id,
            &ops_repository::UpdateOpsAsyncOperation {
                status: &command.status,
                completed_at: command.completed_at,
                failure_code: command.failure_code.as_deref(),
            },
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("async_operation", command.operation_id))?;
        Ok(map_async_operation_row(row))
    }

    pub async fn get_async_operation(
        &self,
        state: &AppState,
        operation_id: Uuid,
    ) -> Result<OpsAsyncOperation, ApiError> {
        let row =
            ops_repository::get_async_operation_by_id(&state.persistence.postgres, operation_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("async_operation", operation_id))?;
        Ok(map_async_operation_row(row))
    }

    pub async fn get_latest_async_operation_by_subject(
        &self,
        state: &AppState,
        subject_kind: &str,
        subject_id: Uuid,
    ) -> Result<Option<OpsAsyncOperation>, ApiError> {
        let row = ops_repository::get_latest_async_operation_by_subject(
            &state.persistence.postgres,
            subject_kind,
            subject_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(row.map(map_async_operation_row))
    }

    pub async fn get_library_state_snapshot(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<OpsLibraryStateSnapshot, ApiError> {
        let library = state.canonical_services.catalog.get_library(state, library_id).await?;
        let facts = ops_repository::get_library_facts(&state.persistence.postgres, library_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("library", library_id))?;
        let mut knowledge_generations =
            state.canonical_services.knowledge.list_library_generations(state, library_id).await?;
        knowledge_generations.sort_by(|left, right| {
            right.created_at.cmp(&left.created_at).then_with(|| right.id.cmp(&left.id))
        });
        let revision_health =
            load_library_revision_health(state, library.workspace_id, library_id).await?;
        let failed_attempts = ops_repository::list_recent_failed_ingest_attempts(
            &state.persistence.postgres,
            library_id,
            10,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        let bundle_failures = ops_repository::list_recent_bundle_assembly_failures(
            &state.persistence.postgres,
            library_id,
            10,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        let state = map_library_facts_row(
            &facts,
            &knowledge_generations,
            &revision_health,
            !failed_attempts.is_empty(),
            !bundle_failures.is_empty(),
        );
        Ok(OpsLibraryStateSnapshot { state, knowledge_generations })
    }

    pub async fn list_library_warnings(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<Vec<OpsLibraryWarning>, ApiError> {
        let library = state.canonical_services.catalog.get_library(state, library_id).await?;
        let revision_health =
            load_library_revision_health(state, library.workspace_id, library_id).await?;
        let failed_attempts = ops_repository::list_recent_failed_ingest_attempts(
            &state.persistence.postgres,
            library_id,
            10,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        let bundle_failures = ops_repository::list_recent_bundle_assembly_failures(
            &state.persistence.postgres,
            library_id,
            10,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(build_library_warnings(library_id, &revision_health, &failed_attempts, &bundle_failures))
    }
}

fn map_async_operation_row(row: ops_repository::OpsAsyncOperationRow) -> OpsAsyncOperation {
    OpsAsyncOperation {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        operation_kind: row.operation_kind,
        status: row.status,
        surface_kind: Some(row.surface_kind),
        subject_kind: Some(row.subject_kind),
        subject_id: row.subject_id,
        failure_code: row.failure_code,
        created_at: row.created_at,
        completed_at: row.completed_at,
    }
}

fn map_library_facts_row(
    row: &ops_repository::OpsLibraryFactsRow,
    knowledge_generations: &[KnowledgeLibraryGeneration],
    revision_health: &[KnowledgeRevision],
    has_failed_attempts: bool,
    has_bundle_failures: bool,
) -> OpsLibraryState {
    let latest_knowledge_generation = knowledge_generations.first();
    let readable_document_count =
        revision_health.iter().filter(|revision| is_revision_text_readable(revision)).count();
    let failed_document_count =
        revision_health.iter().filter(|revision| is_revision_failed(revision)).count();
    let stale_vector_count =
        revision_health.iter().filter(|revision| is_revision_vector_stale(revision)).count();
    let stale_relation_count =
        revision_health.iter().filter(|revision| is_revision_graph_stale(revision)).count();

    OpsLibraryState {
        library_id: row.library_id,
        queue_depth: row.queue_depth,
        running_attempts: row.running_attempts,
        readable_document_count: i64::try_from(readable_document_count).unwrap_or(i64::MAX),
        failed_document_count: i64::try_from(failed_document_count).unwrap_or(i64::MAX),
        degraded_state: derive_degraded_state(
            row.queue_depth,
            row.running_attempts,
            failed_document_count,
            stale_vector_count,
            stale_relation_count,
            has_failed_attempts,
            has_bundle_failures,
            latest_knowledge_generation,
        ),
        latest_knowledge_generation_id: latest_knowledge_generation.map(|generation| generation.id),
        knowledge_generation_state: latest_knowledge_generation
            .map(|generation| generation.generation_state.clone()),
        last_recomputed_at: row.last_recomputed_at,
    }
}

async fn load_library_revision_health(
    state: &AppState,
    workspace_id: Uuid,
    library_id: Uuid,
) -> Result<Vec<KnowledgeRevision>, ApiError> {
    let documents = state
        .arango_document_store
        .list_documents_by_library(workspace_id, library_id)
        .await
        .map_err(|_| ApiError::Internal)?;
    let mut revisions = Vec::new();
    for document in documents {
        let Some(revision_id) = document.readable_revision_id.or(document.active_revision_id)
        else {
            continue;
        };
        let Some(revision) = state
            .arango_document_store
            .get_revision(revision_id)
            .await
            .map_err(|_| ApiError::Internal)?
        else {
            continue;
        };
        revisions.push(KnowledgeRevision {
            id: revision.revision_id,
            document_id: revision.document_id,
            revision_number: revision.revision_number,
            revision_state: revision.revision_state,
            source_uri: revision.source_uri,
            mime_type: revision.mime_type,
            checksum: revision.checksum,
            title: revision.title,
            byte_size: revision.byte_size,
            normalized_text: revision.normalized_text,
            text_checksum: revision.text_checksum,
            text_state: revision.text_state,
            vector_state: revision.vector_state,
            graph_state: revision.graph_state,
            text_readable_at: revision.text_readable_at,
            vector_ready_at: revision.vector_ready_at,
            graph_ready_at: revision.graph_ready_at,
            created_at: revision.created_at,
        });
    }
    Ok(revisions)
}

fn is_revision_text_readable(revision: &KnowledgeRevision) -> bool {
    matches!(revision.text_state.as_str(), "readable" | "ready" | "text_readable")
}

fn is_revision_vector_stale(revision: &KnowledgeRevision) -> bool {
    is_revision_text_readable(revision)
        && !matches!(revision.vector_state.as_str(), "ready" | "vector_ready")
}

fn is_revision_graph_stale(revision: &KnowledgeRevision) -> bool {
    is_revision_text_readable(revision)
        && !matches!(revision.graph_state.as_str(), "ready" | "graph_ready")
}

fn is_revision_failed(revision: &KnowledgeRevision) -> bool {
    matches!(revision.text_state.as_str(), "failed" | "unavailable")
        || revision.vector_state == "failed"
        || revision.graph_state == "failed"
}

fn derive_degraded_state(
    queue_depth: i64,
    running_attempts: i64,
    failed_document_count: usize,
    stale_vector_count: usize,
    stale_relation_count: usize,
    has_failed_attempts: bool,
    has_bundle_failures: bool,
    latest_generation: Option<&KnowledgeLibraryGeneration>,
) -> String {
    if failed_document_count > 0 || has_failed_attempts || has_bundle_failures {
        "degraded".to_string()
    } else if stale_vector_count > 0 || stale_relation_count > 0 {
        "rebuilding".to_string()
    } else if queue_depth > 0 || running_attempts > 0 {
        "processing".to_string()
    } else {
        latest_generation
            .map(|generation| generation.generation_state.clone())
            .unwrap_or_else(|| "healthy".to_string())
    }
}

fn build_library_warnings(
    library_id: Uuid,
    revision_health: &[KnowledgeRevision],
    failed_attempts: &[ops_repository::OpsLibraryFailureRow],
    bundle_failures: &[ops_repository::OpsLibraryFailureRow],
) -> Vec<OpsLibraryWarning> {
    let mut warnings = Vec::new();

    let stale_vectors =
        revision_health.iter().filter(|revision| is_revision_vector_stale(revision)).count();
    if stale_vectors > 0 {
        warnings.push(derived_warning(library_id, "stale_vectors", "warning", Utc::now()));
    }

    let stale_relations =
        revision_health.iter().filter(|revision| is_revision_graph_stale(revision)).count();
    if stale_relations > 0 {
        warnings.push(derived_warning(library_id, "stale_relations", "warning", Utc::now()));
    }

    if let Some(latest_failure) = failed_attempts.first() {
        warnings.push(derived_warning(
            library_id,
            "failed_rebuilds",
            "error",
            latest_failure.created_at,
        ));
    }

    if let Some(latest_failure) = bundle_failures.first() {
        warnings.push(derived_warning(
            library_id,
            "bundle_assembly_failures",
            "error",
            latest_failure.created_at,
        ));
    }

    warnings.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.warning_kind.cmp(&right.warning_kind))
    });
    warnings
}

fn derived_warning(
    library_id: Uuid,
    warning_kind: &str,
    severity: &str,
    created_at: chrono::DateTime<chrono::Utc>,
) -> OpsLibraryWarning {
    let warning_id = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("ops-warning:{library_id}:{warning_kind}").as_bytes(),
    );
    OpsLibraryWarning {
        id: warning_id,
        library_id,
        warning_kind: warning_kind.to_string(),
        severity: severity.to_string(),
        created_at,
        resolved_at: None,
    }
}
