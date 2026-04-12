use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::ingest_repository,
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_DOCUMENTS_WRITE, load_canonical_content_document_and_authorize},
        router_support::ApiError,
    },
    services::content::service::{AdmitMutationCommand, ContentMutationAdmission},
};

use super::types::{
    ContentMutationDetailResponse, build_reprocess_revision_metadata, map_mutation_admission,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BatchDeleteRequest {
    pub document_ids: Vec<Uuid>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BatchDeleteResponse {
    pub deleted_count: usize,
    pub failed_count: usize,
    pub results: Vec<BatchDeleteResult>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BatchDeleteResult {
    pub document_id: Uuid,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BatchCancelRequest {
    pub document_ids: Vec<Uuid>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BatchCancelResponse {
    pub cancelled_count: usize,
    pub failed_count: usize,
    pub results: Vec<BatchCancelResult>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BatchCancelResult {
    pub document_id: Uuid,
    pub jobs_cancelled: u64,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BatchReprocessRequest {
    pub document_ids: Vec<Uuid>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BatchReprocessResponse {
    pub reprocessed_count: usize,
    pub failed_count: usize,
    pub results: Vec<BatchReprocessResult>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BatchReprocessResult {
    pub document_id: Uuid,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutation: Option<ContentMutationDetailResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub(super) const BATCH_MAX_DOCUMENTS: usize = 1000;

pub(super) fn ensure_batch_document_limit(document_count: usize) -> Result<(), ApiError> {
    if document_count > BATCH_MAX_DOCUMENTS {
        return Err(ApiError::BadRequest(format!(
            "batch size exceeds maximum of {BATCH_MAX_DOCUMENTS} documents"
        )));
    }
    Ok(())
}

pub(super) async fn batch_delete_documents(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(request): Json<BatchDeleteRequest>,
) -> Result<Json<BatchDeleteResponse>, ApiError> {
    ensure_batch_document_limit(request.document_ids.len())?;

    let mut results = Vec::with_capacity(request.document_ids.len());
    let mut deleted_count = 0usize;
    let mut failed_count = 0usize;

    for document_id in &request.document_ids {
        match load_canonical_content_document_and_authorize(
            &auth,
            &state,
            *document_id,
            POLICY_DOCUMENTS_WRITE,
        )
        .await
        {
            Ok(document) => {
                match state
                    .canonical_services
                    .content
                    .admit_mutation(
                        &state,
                        AdmitMutationCommand {
                            workspace_id: document.workspace_id,
                            library_id: document.library_id,
                            document_id: *document_id,
                            operation_kind: "delete".to_string(),
                            idempotency_key: None,
                            requested_by_principal_id: Some(auth.principal_id),
                            request_surface: "rest".to_string(),
                            source_identity: None,
                            revision: None,
                        },
                    )
                    .await
                {
                    Ok(_) => {
                        deleted_count += 1;
                        results.push(BatchDeleteResult {
                            document_id: *document_id,
                            success: true,
                            error: None,
                        });
                    }
                    Err(error) => {
                        failed_count += 1;
                        results.push(BatchDeleteResult {
                            document_id: *document_id,
                            success: false,
                            error: Some(error.to_string()),
                        });
                    }
                }
            }
            Err(error) => {
                failed_count += 1;
                results.push(BatchDeleteResult {
                    document_id: *document_id,
                    success: false,
                    error: Some(error.to_string()),
                });
            }
        }
    }

    Ok(Json(BatchDeleteResponse { deleted_count, failed_count, results }))
}

pub(super) async fn batch_cancel_documents(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(request): Json<BatchCancelRequest>,
) -> Result<Json<BatchCancelResponse>, ApiError> {
    ensure_batch_document_limit(request.document_ids.len())?;

    let mut results = Vec::with_capacity(request.document_ids.len());
    let mut cancelled_count = 0usize;
    let mut failed_count = 0usize;

    for document_id in &request.document_ids {
        match load_canonical_content_document_and_authorize(
            &auth,
            &state,
            *document_id,
            POLICY_DOCUMENTS_WRITE,
        )
        .await
        {
            Ok(_) => {
                match ingest_repository::cancel_queued_jobs_for_document(
                    &state.persistence.postgres,
                    *document_id,
                )
                .await
                {
                    Ok(jobs_cancelled) => {
                        cancelled_count += 1;
                        results.push(BatchCancelResult {
                            document_id: *document_id,
                            jobs_cancelled,
                            success: true,
                            error: None,
                        });
                    }
                    Err(error) => {
                        failed_count += 1;
                        results.push(BatchCancelResult {
                            document_id: *document_id,
                            jobs_cancelled: 0,
                            success: false,
                            error: Some(error.to_string()),
                        });
                    }
                }
            }
            Err(error) => {
                failed_count += 1;
                results.push(BatchCancelResult {
                    document_id: *document_id,
                    jobs_cancelled: 0,
                    success: false,
                    error: Some(error.to_string()),
                });
            }
        }
    }

    Ok(Json(BatchCancelResponse { cancelled_count, failed_count, results }))
}

pub(super) async fn batch_reprocess_documents(
    auth: AuthContext,
    State(state): State<AppState>,
    Json(request): Json<BatchReprocessRequest>,
) -> Result<Json<BatchReprocessResponse>, ApiError> {
    ensure_batch_document_limit(request.document_ids.len())?;

    let mut results = Vec::with_capacity(request.document_ids.len());
    let mut reprocessed_count = 0usize;
    let mut failed_count = 0usize;

    for document_id in &request.document_ids {
        match reprocess_single_document(&auth, &state, *document_id).await {
            Ok(admission) => {
                reprocessed_count += 1;
                results.push(BatchReprocessResult {
                    document_id: *document_id,
                    success: true,
                    mutation: Some(map_mutation_admission(admission)),
                    error: None,
                });
            }
            Err(error) => {
                failed_count += 1;
                results.push(BatchReprocessResult {
                    document_id: *document_id,
                    success: false,
                    mutation: None,
                    error: Some(error.to_string()),
                });
            }
        }
    }

    Ok(Json(BatchReprocessResponse { reprocessed_count, failed_count, results }))
}

async fn reprocess_single_document(
    auth: &AuthContext,
    state: &AppState,
    document_id: Uuid,
) -> Result<ContentMutationAdmission, ApiError> {
    let document = load_canonical_content_document_and_authorize(
        auth,
        state,
        document_id,
        POLICY_DOCUMENTS_WRITE,
    )
    .await?;
    let summary = state.canonical_services.content.get_document(state, document_id).await?;
    let active_revision = summary.active_revision.ok_or_else(|| {
        ApiError::BadRequest("document has no active revision to reprocess".to_string())
    })?;
    let resolved_storage_key = state
        .canonical_services
        .content
        .resolve_revision_storage_key(state, active_revision.id)
        .await?;
    if active_revision.storage_key.is_none() && resolved_storage_key.is_none() {
        return Err(ApiError::BadRequest("document has no stored source to reprocess".to_string()));
    }
    state
        .canonical_services
        .content
        .admit_mutation(
            state,
            AdmitMutationCommand {
                workspace_id: document.workspace_id,
                library_id: document.library_id,
                document_id,
                operation_kind: "reprocess".to_string(),
                idempotency_key: None,
                requested_by_principal_id: Some(auth.principal_id),
                request_surface: "rest".to_string(),
                source_identity: None,
                revision: Some(build_reprocess_revision_metadata(
                    &active_revision,
                    resolved_storage_key,
                )),
            },
        )
        .await
}
