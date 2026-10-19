use chrono::Utc;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::content::{ContentMutation, ContentMutationItem},
    domains::ops::{ASYNC_OP_STATUS_READY, MUTATION_KIND_DELETE},
    infra::repositories::{
        admission_repository::{
            self, AdmissionError, ContentAdmissionRequest, ContentAdmissionTarget,
        },
        content_mutation_repository::{self, NewContentMutationItem},
        ingest_repository,
    },
    interfaces::http::router_support::ApiError,
    services::ops::service::{CreateAsyncOperationCommand, UpdateAsyncOperationCommand},
    shared::database::{DatabaseErrorClass, classify_error},
};

mod admission;
mod reconciliation;
mod records;
#[cfg(test)]
mod tests;

use admission::{
    delete_mutation_item_is_reusable, delete_mutation_request_identity, map_atomic_admission_error,
    validate_delete_mutation_item_collection,
};

/// Default priority for content-mutation ingest jobs.
const DEFAULT_JOB_PRIORITY: i32 = 100;

/// Identity and desired state for a delete mutation's single document item.
/// Grouped so the ensure/reuse helpers carry one cohesive value rather than
/// five positional arguments.
struct DeleteMutationItemInput<'a> {
    mutation_id: Uuid,
    document_id: Uuid,
    base_revision_id: Option<Uuid>,
    item_state: &'a str,
    message: &'a str,
}

use super::{
    AdmitDocumentCommand, AdmitMutationCommand, ContentMutationAdmission, ContentService,
    CreateDocumentAdmission, CreateMutationItemCommand, ReconcileFailedIngestMutationCommand,
    UpdateMutationCommand, UpdateMutationItemCommand, derive_failed_revision_readiness,
    map_mutation_item_row, map_mutation_row,
};

impl ContentService {
    /// Accepts an independent visibility obligation without claiming payload ownership.
    pub async fn admit_visibility_intent(
        &self,
        state: &AppState,
        request: admission_repository::VisibilityAdmissionRequest,
    ) -> Result<
        admission_repository::AdmissionOutcome<admission_repository::VisibilityAdmissionBundle>,
        ApiError,
    > {
        admission_repository::admit_visibility_intent_with_failpoint(
            &state.persistence.postgres,
            &request,
            None,
        )
        .await
        .map_err(map_atomic_admission_error)
    }

    pub async fn admit_document(
        &self,
        state: &AppState,
        command: AdmitDocumentCommand,
    ) -> Result<CreateDocumentAdmission, ApiError> {
        self.admit_document_source(state, command, None).await
    }

    pub(super) async fn admit_document_source(
        &self,
        state: &AppState,
        command: AdmitDocumentCommand,
        staged_bytes: Option<&[u8]>,
    ) -> Result<CreateDocumentAdmission, ApiError> {
        let request = ContentAdmissionRequest {
            workspace_id: command.workspace_id,
            library_id: command.library_id,
            operation_kind: "upload".to_string(),
            requested_by_principal_id: command.created_by_principal_id,
            request_surface: command.request_surface,
            idempotency_key: command.idempotency_key,
            source_identity: command.source_identity,
            target: ContentAdmissionTarget::New {
                external_key: command.external_key,
                file_name: command.file_name,
                parent_external_key: command.parent_external_key,
            },
            revision: command.revision,
            parent_async_operation_id: None,
            priority: DEFAULT_JOB_PRIORITY,
        };
        let outcome = match staged_bytes {
            Some(bytes) => {
                admission_repository::admit_staged_upload_with_failpoint(
                    &state.persistence.postgres,
                    &request,
                    bytes,
                    None,
                )
                .await
            }
            None => {
                admission_repository::admit_content_with_failpoint(
                    &state.persistence.postgres,
                    &request,
                    None,
                )
                .await
            }
        }
        .map_err(map_atomic_admission_error)?;
        let replayed = matches!(outcome, admission_repository::AdmissionOutcome::Replayed(_));
        let bundle = outcome.into_bundle();
        let document_id = bundle.document.id;
        let request_fingerprint = bundle.mutation.request_fingerprint.clone();
        let operation_status = bundle.async_operation.status.clone();
        let result_revision_id = bundle.revision.as_ref().map(|revision| revision.id);
        let result_checksum = bundle.revision.as_ref().map(|revision| revision.checksum.clone());
        let mutation = ContentMutationAdmission {
            replayed,
            mutation: map_mutation_row(bundle.mutation),
            items: bundle.item.into_iter().map(map_mutation_item_row).collect(),
            job_id: bundle.job.map(|job| job.id),
            async_operation_id: Some(bundle.async_operation.id),
            operation_status: Some(operation_status),
            result_revision_id,
            result_checksum,
            request_fingerprint,
        };
        let document = self.get_document(state, document_id).await?;
        Ok(CreateDocumentAdmission { document, mutation })
    }

    pub async fn admit_mutation(
        &self,
        state: &AppState,
        mut command: AdmitMutationCommand,
    ) -> Result<ContentMutationAdmission, ApiError> {
        if command.operation_kind == MUTATION_KIND_DELETE && command.idempotency_key.is_none() {
            command.idempotency_key = Some(format!("delete:{}", command.document_id));
        }
        if command.operation_kind == MUTATION_KIND_DELETE {
            let document_lock = content_mutation_repository::acquire_content_document_lock(
                &state.persistence.postgres,
                command.document_id,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
            let result = self.admit_delete_mutation(state, &command).await;
            let release_result = content_mutation_repository::release_content_document_lock(
                document_lock,
                command.document_id,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"));
            return match (result, release_result) {
                (Ok(admission), Ok(())) => Ok(admission),
                (Err(error), Ok(())) => Err(error),
                (Ok(_), Err(error)) => Err(error),
                (Err(_), Err(error)) => Err(error),
            };
        }

        let revision = command.revision.ok_or_else(|| {
            ApiError::BadRequest(
                "revision metadata is required for non-delete document mutations".to_string(),
            )
        })?;
        let request = ContentAdmissionRequest {
            workspace_id: command.workspace_id,
            library_id: command.library_id,
            operation_kind: command.operation_kind,
            requested_by_principal_id: command.requested_by_principal_id,
            request_surface: command.request_surface,
            idempotency_key: command.idempotency_key,
            source_identity: command.source_identity,
            target: ContentAdmissionTarget::Existing { document_id: command.document_id },
            revision: Some(revision),
            parent_async_operation_id: command.parent_async_operation_id,
            priority: DEFAULT_JOB_PRIORITY,
        };
        let outcome = admission_repository::admit_content_with_failpoint(
            &state.persistence.postgres,
            &request,
            None,
        )
        .await
        .map_err(map_atomic_admission_error)?;
        let replayed = matches!(outcome, admission_repository::AdmissionOutcome::Replayed(_));
        let bundle = outcome.into_bundle();
        let request_fingerprint = bundle.mutation.request_fingerprint.clone();
        let operation_status = bundle.async_operation.status.clone();
        let result_revision_id = bundle.revision.as_ref().map(|revision| revision.id);
        let result_checksum = bundle.revision.as_ref().map(|revision| revision.checksum.clone());
        Ok(ContentMutationAdmission {
            replayed,
            mutation: map_mutation_row(bundle.mutation),
            items: bundle.item.into_iter().map(map_mutation_item_row).collect(),
            job_id: bundle.job.map(|job| job.id),
            async_operation_id: Some(bundle.async_operation.id),
            operation_status: Some(operation_status),
            result_revision_id,
            result_checksum,
            request_fingerprint,
        })
    }

    pub async fn settle_deleted_document_mutation(
        &self,
        state: &AppState,
        mutation_id: Uuid,
    ) -> Result<(), ApiError> {
        let admission = self.get_mutation_admission(state, mutation_id).await?;
        if !matches!(admission.mutation.mutation_state.as_str(), "accepted" | "running") {
            return Ok(());
        }
        if let Some(operation_id) = admission.async_operation_id {
            let _ = state
                .application_services
                .ops
                .update_async_operation(
                    state,
                    UpdateAsyncOperationCommand {
                        operation_id,
                        status: "failed".to_string(),
                        completed_at: Some(Utc::now()),
                        failure_code: Some("document_deleted".to_string()),
                    },
                )
                .await?;
        }
        for item in &admission.items {
            if matches!(item.item_state.as_str(), "applied" | "failed" | "skipped") {
                continue;
            }
            let _ = self
                .update_mutation_item(
                    state,
                    UpdateMutationItemCommand {
                        item_id: item.id,
                        document_id: item.document_id,
                        base_revision_id: item.base_revision_id,
                        result_revision_id: item.result_revision_id,
                        item_state: "skipped".to_string(),
                        message: Some("mutation skipped because document was deleted".to_string()),
                    },
                )
                .await?;
        }
        let _ = self
            .update_mutation(
                state,
                UpdateMutationCommand {
                    mutation_id,
                    mutation_state: "canceled".to_string(),
                    completed_at: Some(Utc::now()),
                    failure_code: Some("document_deleted".to_string()),
                    conflict_code: None,
                },
            )
            .await?;
        Ok(())
    }

    async fn admit_delete_mutation(
        &self,
        state: &AppState,
        command: &AdmitMutationCommand,
    ) -> Result<ContentMutationAdmission, ApiError> {
        let current_head = self.get_document_head(state, command.document_id).await?;
        let superseded_mutation_id = current_head.as_ref().and_then(|head| head.latest_mutation_id);
        let request = ContentAdmissionRequest {
            workspace_id: command.workspace_id,
            library_id: command.library_id,
            operation_kind: MUTATION_KIND_DELETE.to_owned(),
            requested_by_principal_id: command.requested_by_principal_id,
            request_surface: command.request_surface.clone(),
            idempotency_key: command.idempotency_key.clone(),
            source_identity: Some(delete_mutation_request_identity(
                command.document_id,
                command.source_identity.as_deref(),
            )),
            target: ContentAdmissionTarget::Existing { document_id: command.document_id },
            revision: None,
            parent_async_operation_id: command.parent_async_operation_id,
            priority: DEFAULT_JOB_PRIORITY,
        };
        let outcome = admission_repository::admit_content_with_failpoint(
            &state.persistence.postgres,
            &request,
            None,
        )
        .await
        .map_err(map_atomic_admission_error)?;
        let replayed = matches!(outcome, admission_repository::AdmissionOutcome::Replayed(_));
        let bundle = outcome.into_bundle();
        if replayed
            && bundle.mutation.mutation_state == "applied"
            && bundle.item.as_ref().is_some_and(|item| item.item_state == "applied")
            && bundle.async_operation.status == ASYNC_OP_STATUS_READY
        {
            self.reconcile_deleted_document_projection(
                state,
                command.document_id,
                bundle.mutation.id,
            )
            .await?;
            return Ok(ContentMutationAdmission {
                replayed,
                request_fingerprint: bundle.mutation.request_fingerprint.clone(),
                mutation: map_mutation_row(bundle.mutation),
                items: bundle.item.into_iter().map(map_mutation_item_row).collect(),
                job_id: None,
                async_operation_id: Some(bundle.async_operation.id),
                operation_status: Some(bundle.async_operation.status),
                result_revision_id: None,
                result_checksum: None,
            });
        }
        let base_revision_id = bundle.item.as_ref().ok_or(ApiError::Internal)?.base_revision_id;
        let operation_id = bundle.async_operation.id;
        let request_fingerprint = bundle.mutation.request_fingerprint.clone();
        let mutation = map_mutation_row(bundle.mutation);
        let mutation_id = mutation.id;
        let mut admission = self
            .finalize_delete_mutation_admission(
                state,
                command,
                mutation,
                base_revision_id,
                superseded_mutation_id.filter(|id| *id != mutation_id),
            )
            .await?;
        admission.replayed = replayed;
        admission.async_operation_id = Some(operation_id);
        admission.request_fingerprint = request_fingerprint;
        admission.operation_status = Some(
            state
                .application_services
                .ops
                .get_async_operation(state, operation_id)
                .await?
                .status
                .as_str()
                .to_owned(),
        );
        Ok(admission)
    }

    async fn finalize_delete_mutation_admission(
        &self,
        state: &AppState,
        command: &AdmitMutationCommand,
        mutation: ContentMutation,
        base_revision_id: Option<Uuid>,
        superseded_mutation_id: Option<Uuid>,
    ) -> Result<ContentMutationAdmission, ApiError> {
        let pending_item = self
            .ensure_delete_mutation_item(
                state,
                DeleteMutationItemInput {
                    mutation_id: mutation.id,
                    document_id: command.document_id,
                    base_revision_id,
                    item_state: "pending",
                    message: "document delete admitted",
                },
            )
            .await?;
        let async_operation =
            self.ensure_delete_async_operation(state, command, mutation.id).await?;
        let mutation_id = mutation.id;
        let completed_at = mutation.completed_at.unwrap_or_else(Utc::now);
        let refresh_graph_projection = command.parent_async_operation_id.is_none();

        let _ = self
            .delete_document_with_context(
                state,
                command.document_id,
                Some(mutation.id),
                refresh_graph_projection,
            )
            .await?;

        if let Some(superseded_mutation_id) =
            superseded_mutation_id.filter(|mutation_id| *mutation_id != mutation.id)
        {
            self.settle_deleted_document_mutation(state, superseded_mutation_id).await?;
        }

        let _ = if pending_item.item_state == "applied"
            && pending_item.base_revision_id == base_revision_id
            && pending_item.result_revision_id.is_none()
        {
            pending_item
        } else {
            self.update_mutation_item(
                state,
                UpdateMutationItemCommand {
                    item_id: pending_item.id,
                    document_id: Some(command.document_id),
                    base_revision_id,
                    result_revision_id: None,
                    item_state: "applied".to_string(),
                    message: Some("document deleted".to_string()),
                },
            )
            .await?
        };

        let _ = if mutation.mutation_state == "applied" && mutation.completed_at.is_some() {
            mutation
        } else {
            self.update_mutation(
                state,
                UpdateMutationCommand {
                    mutation_id: mutation.id,
                    mutation_state: "applied".to_string(),
                    completed_at: Some(completed_at),
                    failure_code: None,
                    conflict_code: None,
                },
            )
            .await?
        };

        if async_operation.status.as_str() != ASYNC_OP_STATUS_READY
            || async_operation.completed_at.is_none()
            || async_operation.failure_code.is_some()
        {
            let _ = state
                .application_services
                .ops
                .update_async_operation(
                    state,
                    UpdateAsyncOperationCommand {
                        operation_id: async_operation.id,
                        status: "ready".to_string(),
                        completed_at: Some(completed_at),
                        failure_code: None,
                    },
                )
                .await?;
        }

        self.get_mutation_admission(state, mutation_id).await
    }

    async fn ensure_delete_mutation_item(
        &self,
        state: &AppState,
        input: DeleteMutationItemInput<'_>,
    ) -> Result<ContentMutationItem, ApiError> {
        let mutation_id = input.mutation_id;
        let mutation_lock = content_mutation_repository::acquire_content_mutation_lock(
            &state.persistence.postgres,
            mutation_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let result = self.ensure_delete_mutation_item_with_lock_held(state, input).await;
        let release_result =
            content_mutation_repository::release_content_mutation_lock(mutation_lock, mutation_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"));
        match (result, release_result) {
            (Ok(item), Ok(())) => Ok(item),
            (Err(error), Ok(())) => Err(error),
            (_, Err(error)) => Err(error),
        }
    }

    async fn ensure_delete_mutation_item_with_lock_held(
        &self,
        state: &AppState,
        input: DeleteMutationItemInput<'_>,
    ) -> Result<ContentMutationItem, ApiError> {
        let DeleteMutationItemInput {
            mutation_id,
            document_id,
            base_revision_id,
            item_state,
            message,
        } = input;
        let existing_items = self.list_mutation_items(state, mutation_id).await?;
        validate_delete_mutation_item_collection(
            &existing_items,
            document_id,
            "the delete mutation already targets a different or incomplete document",
        )?;
        if let Some(existing_item) =
            existing_items.iter().find(|item| item.document_id == Some(document_id)).cloned()
        {
            return self
                .reuse_or_update_delete_mutation_item(
                    state,
                    existing_item,
                    DeleteMutationItemInput {
                        mutation_id,
                        document_id,
                        base_revision_id,
                        item_state,
                        message,
                    },
                )
                .await;
        }

        self.create_mutation_item(
            state,
            CreateMutationItemCommand {
                mutation_id,
                document_id,
                base_revision_id,
                result_revision_id: None,
                item_state: item_state.to_string(),
                message: Some(message.to_string()),
            },
        )
        .await
    }

    async fn reuse_or_update_delete_mutation_item(
        &self,
        state: &AppState,
        existing_item: ContentMutationItem,
        input: DeleteMutationItemInput<'_>,
    ) -> Result<ContentMutationItem, ApiError> {
        let DeleteMutationItemInput { document_id, base_revision_id, item_state, message, .. } =
            input;
        if delete_mutation_item_is_reusable(
            &existing_item,
            document_id,
            base_revision_id,
            item_state,
            message,
        ) {
            return Ok(existing_item);
        }
        self.update_mutation_item(
            state,
            UpdateMutationItemCommand {
                item_id: existing_item.id,
                document_id: Some(document_id),
                base_revision_id,
                result_revision_id: None,
                item_state: item_state.to_string(),
                message: Some(message.to_string()),
            },
        )
        .await
    }

    async fn ensure_delete_async_operation(
        &self,
        state: &AppState,
        command: &AdmitMutationCommand,
        mutation_id: Uuid,
    ) -> Result<crate::domains::ops::OpsAsyncOperation, ApiError> {
        if let Some(existing) = state
            .application_services
            .ops
            .get_latest_async_operation_by_subject(state, "content_mutation", mutation_id)
            .await?
        {
            return Ok(existing);
        }

        state
            .application_services
            .ops
            .create_async_operation(
                state,
                CreateAsyncOperationCommand {
                    workspace_id: command.workspace_id,
                    library_id: Some(command.library_id),
                    operation_kind: "content_mutation".to_string(),
                    surface_kind: command.request_surface.clone(),
                    requested_by_principal_id: command.requested_by_principal_id,
                    status: "accepted".to_string(),
                    subject_kind: "content_mutation".to_string(),
                    subject_id: Some(mutation_id),
                    parent_async_operation_id: command.parent_async_operation_id,
                    completed_at: None,
                    failure_code: None,
                },
            )
            .await
    }
}
