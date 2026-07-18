use chrono::Utc;
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::content::{ContentMutation, ContentMutationItem},
    domains::ops::{ASYNC_OP_STATUS_READY, MUTATION_KIND_DELETE},
    infra::repositories::{
        admission_repository::{
            self, AdmissionError, ContentAdmissionRequest, ContentAdmissionTarget,
            RevisionAdmission,
        },
        content_repository::{self, NewContentMutation, NewContentMutationItem},
    },
    interfaces::http::router_support::ApiError,
    services::ops::service::{CreateAsyncOperationCommand, UpdateAsyncOperationCommand},
};

/// Default priority for content-mutation ingest jobs.
const DEFAULT_JOB_PRIORITY: i32 = 100;

fn map_atomic_admission_error(error: AdmissionError) -> ApiError {
    match error {
        AdmissionError::IdempotencyConflict { .. } => ApiError::Conflict(
            "idempotency key was already used for a different content request".to_string(),
        ),
        AdmissionError::TargetDocumentNotFound { document_id } => {
            ApiError::resource_not_found("document", document_id)
        }
        AdmissionError::TargetDocumentDeleted { .. } => {
            ApiError::BadRequest("deleted documents do not accept new mutations".to_string())
        }
        AdmissionError::TargetDocumentScopeConflict { .. } => {
            ApiError::Conflict("document scope changed before mutation admission".to_string())
        }
        AdmissionError::ConflictingActiveMutation { .. } => ApiError::ConflictingMutation(
            "document is still processing a previous mutation".to_string(),
        ),
        AdmissionError::DuplicateExternalKey { existing_document_id } => {
            ApiError::DuplicateContent {
                message: "an active document with this external key already exists".to_string(),
                existing_document_id,
            }
        }
        error @ AdmissionError::TargetDocumentHeadIntegrity { .. } => {
            ApiError::internal_with_log(error, "content admission head integrity failure")
        }
        other => ApiError::internal_with_log(other, "content admission failed"),
    }
}

fn delete_mutation_item_is_reusable(
    item: &ContentMutationItem,
    document_id: Uuid,
    base_revision_id: Option<Uuid>,
    requested_state: &str,
    requested_message: &str,
) -> bool {
    let owns_target = item.document_id == Some(document_id)
        && item.base_revision_id == base_revision_id
        && item.result_revision_id.is_none();
    owns_target
        && ((item.item_state == "applied" && requested_state == "pending")
            || (item.item_state == requested_state
                && item.message.as_deref() == Some(requested_message)))
}

fn validate_delete_mutation_item_collection(
    items: &[ContentMutationItem],
    document_id: Uuid,
    multiple_unbound_message: &str,
    foreign_document_message: &str,
) -> Result<(), ApiError> {
    if items.iter().filter(|item| item.document_id.is_none()).count() > 1 {
        return Err(ApiError::idempotency_conflict(multiple_unbound_message));
    }
    if items.iter().any(|item| {
        item.document_id.is_some_and(|existing_document_id| existing_document_id != document_id)
    }) {
        return Err(ApiError::idempotency_conflict(foreign_document_message));
    }
    Ok(())
}

fn delete_mutation_request_identity(document_id: Uuid, source_identity: Option<&str>) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ironrag:content-delete:v1\0");
    digest.update(document_id.as_bytes());
    match source_identity {
        Some(source_identity) => {
            digest.update([1]);
            digest.update(source_identity.as_bytes());
        }
        None => digest.update([0]),
    }
    format!("v1:sha256:{}", hex::encode(digest.finalize()))
}

use super::{
    AcceptMutationCommand, AdmitDocumentCommand, AdmitMutationCommand, ContentMutationAdmission,
    ContentService, CreateDocumentAdmission, CreateMutationItemCommand, PromoteHeadCommand,
    ReconcileFailedIngestMutationCommand, UpdateMutationCommand, UpdateMutationItemCommand,
    derive_failed_revision_readiness, ensure_existing_mutation_matches_request,
    is_content_mutation_idempotency_violation, map_mutation_item_row, map_mutation_row,
};

impl ContentService {
    pub async fn admit_document(
        &self,
        state: &AppState,
        command: AdmitDocumentCommand,
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
            revision: command.revision.map(|revision| RevisionAdmission {
                content_source_kind: revision.content_source_kind,
                checksum: revision.checksum,
                mime_type: revision.mime_type,
                byte_size: revision.byte_size,
                title: revision.title,
                language_code: revision.language_code,
                source_uri: revision.source_uri,
                document_hint: revision.document_hint,
                storage_key: revision.storage_key,
            }),
            parent_async_operation_id: None,
            priority: DEFAULT_JOB_PRIORITY,
        };
        let bundle = admission_repository::admit_content_with_failpoint(
            &state.persistence.postgres,
            &request,
            None,
        )
        .await
        .map_err(map_atomic_admission_error)?
        .into_bundle();
        let document_id = bundle.document.id;
        let mutation = ContentMutationAdmission {
            mutation: map_mutation_row(bundle.mutation),
            items: bundle.item.into_iter().map(map_mutation_item_row).collect(),
            job_id: bundle.job.map(|job| job.id),
            async_operation_id: Some(bundle.async_operation.id),
        };
        let document = self.get_document(state, document_id).await?;
        Ok(CreateDocumentAdmission { document, mutation })
    }

    pub async fn admit_mutation(
        &self,
        state: &AppState,
        command: AdmitMutationCommand,
    ) -> Result<ContentMutationAdmission, ApiError> {
        let accept_command = Self::accept_mutation_command_from_admit(&command);

        if command.operation_kind == MUTATION_KIND_DELETE {
            let document_lock = content_repository::acquire_content_document_lock(
                &state.persistence.postgres,
                command.document_id,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
            let result = self.admit_delete_mutation(state, &command, &accept_command).await;
            let release_result = content_repository::release_content_document_lock(
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

        self.ensure_document_accepts_new_mutation(
            state,
            command.document_id,
            &command.operation_kind,
        )
        .await?;
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
            revision: Some(RevisionAdmission {
                content_source_kind: revision.content_source_kind,
                checksum: revision.checksum,
                mime_type: revision.mime_type,
                byte_size: revision.byte_size,
                title: revision.title,
                language_code: revision.language_code,
                source_uri: revision.source_uri,
                document_hint: revision.document_hint,
                storage_key: revision.storage_key,
            }),
            parent_async_operation_id: command.parent_async_operation_id,
            priority: DEFAULT_JOB_PRIORITY,
        };
        let bundle = admission_repository::admit_content_with_failpoint(
            &state.persistence.postgres,
            &request,
            None,
        )
        .await
        .map_err(map_atomic_admission_error)?
        .into_bundle();
        Ok(ContentMutationAdmission {
            mutation: map_mutation_row(bundle.mutation),
            items: bundle.item.into_iter().map(map_mutation_item_row).collect(),
            job_id: bundle.job.map(|job| job.id),
            async_operation_id: Some(bundle.async_operation.id),
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
                .canonical_services
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

    pub async fn accept_mutation(
        &self,
        state: &AppState,
        command: AcceptMutationCommand,
    ) -> Result<ContentMutation, ApiError> {
        self.create_mutation_record(state, &command, "accepted").await
    }

    fn accept_mutation_command_from_admit(command: &AdmitMutationCommand) -> AcceptMutationCommand {
        AcceptMutationCommand {
            workspace_id: command.workspace_id,
            library_id: command.library_id,
            operation_kind: command.operation_kind.clone(),
            requested_by_principal_id: command.requested_by_principal_id,
            request_surface: command.request_surface.clone(),
            idempotency_key: command.idempotency_key.clone(),
            source_identity: if command.operation_kind == MUTATION_KIND_DELETE {
                Some(delete_mutation_request_identity(
                    command.document_id,
                    command.source_identity.as_deref(),
                ))
            } else {
                command.source_identity.clone()
            },
        }
    }

    async fn admit_delete_mutation(
        &self,
        state: &AppState,
        command: &AdmitMutationCommand,
        accept_command: &AcceptMutationCommand,
    ) -> Result<ContentMutationAdmission, ApiError> {
        let current_document = content_repository::get_document_by_id(
            &state.persistence.postgres,
            command.document_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("document", command.document_id))?;
        let current_head = self.get_document_head(state, command.document_id).await?;
        let base_revision_id = current_head
            .as_ref()
            .and_then(crate::domains::content::ContentDocumentHead::latest_revision_id);
        let superseded_mutation_id = current_head.as_ref().and_then(|head| head.latest_mutation_id);

        if let Some(existing_mutation) =
            self.find_existing_mutation_for_request(state, accept_command).await?
        {
            let existing_mutation_id = existing_mutation.id;
            return self
                .finalize_delete_mutation_admission(
                    state,
                    command,
                    existing_mutation,
                    base_revision_id,
                    superseded_mutation_id
                        .filter(|mutation_id| *mutation_id != existing_mutation_id),
                )
                .await;
        }

        if current_document.document_state == "deleted" || current_document.deleted_at.is_some() {
            let has_idempotency_key = accept_command
                .idempotency_key
                .as_deref()
                .map(str::trim)
                .is_some_and(|value| !value.is_empty());
            let canonical_delete_mutation = if has_idempotency_key {
                self.accept_mutation(state, accept_command.clone()).await?
            } else {
                match superseded_mutation_id {
                    Some(latest_mutation_id) => match content_repository::get_mutation_by_id(
                        &state.persistence.postgres,
                        latest_mutation_id,
                    )
                    .await
                    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                    {
                        Some(existing_row)
                            if existing_row.operation_kind == MUTATION_KIND_DELETE =>
                        {
                            map_mutation_row(existing_row)
                        }
                        _ => self.accept_mutation(state, accept_command.clone()).await?,
                    },
                    None => self.accept_mutation(state, accept_command.clone()).await?,
                }
            };
            return self
                .finalize_delete_mutation_admission(
                    state,
                    command,
                    canonical_delete_mutation.clone(),
                    base_revision_id,
                    superseded_mutation_id
                        .filter(|mutation_id| *mutation_id != canonical_delete_mutation.id),
                )
                .await;
        }

        self.ensure_document_accepts_new_mutation(
            state,
            command.document_id,
            &command.operation_kind,
        )
        .await?;
        let mutation = self.accept_mutation(state, accept_command.clone()).await?;
        self.finalize_delete_mutation_admission(
            state,
            command,
            mutation,
            base_revision_id,
            superseded_mutation_id,
        )
        .await
    }

    async fn find_existing_mutation_for_request(
        &self,
        state: &AppState,
        command: &AcceptMutationCommand,
    ) -> Result<Option<ContentMutation>, ApiError> {
        let (Some(principal_id), Some(idempotency_key)) = (
            command.requested_by_principal_id,
            command.idempotency_key.as_deref().map(str::trim).filter(|value| !value.is_empty()),
        ) else {
            return Ok(None);
        };
        let request_source_identity =
            command.source_identity.as_deref().map(str::trim).filter(|value| !value.is_empty());
        let existing = content_repository::find_mutation_by_idempotency(
            &state.persistence.postgres,
            principal_id,
            &command.request_surface,
            idempotency_key,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        if let Some(existing) = existing {
            ensure_existing_mutation_matches_request(
                &existing,
                command.workspace_id,
                command.library_id,
                &command.operation_kind,
                request_source_identity,
            )?;
            return Ok(Some(map_mutation_row(existing)));
        }
        Ok(None)
    }

    pub(crate) async fn get_existing_mutation_admission_for_request(
        &self,
        state: &AppState,
        command: &AcceptMutationCommand,
    ) -> Result<Option<ContentMutationAdmission>, ApiError> {
        let Some(existing) = self.find_existing_mutation_for_request(state, command).await? else {
            return Ok(None);
        };
        let admission = self.get_mutation_admission(state, existing.id).await?;
        Ok(Some(admission))
    }

    async fn create_mutation_record(
        &self,
        state: &AppState,
        command: &AcceptMutationCommand,
        mutation_state: &str,
    ) -> Result<ContentMutation, ApiError> {
        if let (Some(principal_id), Some(idempotency_key)) = (
            command.requested_by_principal_id,
            command.idempotency_key.as_deref().map(str::trim).filter(|value| !value.is_empty()),
        ) {
            let request_source_identity =
                command.source_identity.as_deref().map(str::trim).filter(|value| !value.is_empty());
            if let Some(existing) = content_repository::find_mutation_by_idempotency(
                &state.persistence.postgres,
                principal_id,
                &command.request_surface,
                idempotency_key,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            {
                ensure_existing_mutation_matches_request(
                    &existing,
                    command.workspace_id,
                    command.library_id,
                    &command.operation_kind,
                    request_source_identity,
                )?;
                return Ok(map_mutation_row(existing));
            }

            let row = content_repository::create_mutation(
                &state.persistence.postgres,
                &NewContentMutation {
                    workspace_id: command.workspace_id,
                    library_id: command.library_id,
                    operation_kind: &command.operation_kind,
                    requested_by_principal_id: command.requested_by_principal_id,
                    request_surface: &command.request_surface,
                    idempotency_key: command.idempotency_key.as_deref(),
                    source_identity: command.source_identity.as_deref(),
                    mutation_state,
                    failure_code: None,
                    conflict_code: None,
                },
            )
            .await;
            return match row {
                Ok(row) => Ok(map_mutation_row(row)),
                Err(error) if is_content_mutation_idempotency_violation(&error) => {
                    let existing = content_repository::find_mutation_by_idempotency(
                        &state.persistence.postgres,
                        principal_id,
                        &command.request_surface,
                        idempotency_key,
                    )
                    .await
                    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                    .ok_or(ApiError::Internal)?;
                    ensure_existing_mutation_matches_request(
                        &existing,
                        command.workspace_id,
                        command.library_id,
                        &command.operation_kind,
                        request_source_identity,
                    )?;
                    Ok(map_mutation_row(existing))
                }
                Err(_) => Err(ApiError::Internal),
            };
        }

        let row = content_repository::create_mutation(
            &state.persistence.postgres,
            &NewContentMutation {
                workspace_id: command.workspace_id,
                library_id: command.library_id,
                operation_kind: &command.operation_kind,
                requested_by_principal_id: command.requested_by_principal_id,
                request_surface: &command.request_surface,
                idempotency_key: command.idempotency_key.as_deref(),
                source_identity: command.source_identity.as_deref(),
                mutation_state,
                failure_code: None,
                conflict_code: None,
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(map_mutation_row(row))
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
                mutation.id,
                command.document_id,
                base_revision_id,
                "pending",
                "document delete admitted",
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
                .canonical_services
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
        mutation_id: Uuid,
        document_id: Uuid,
        base_revision_id: Option<Uuid>,
        item_state: &str,
        message: &str,
    ) -> Result<ContentMutationItem, ApiError> {
        let mutation_lock = content_repository::acquire_content_mutation_lock(
            &state.persistence.postgres,
            mutation_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let result = self
            .ensure_delete_mutation_item_with_lock_held(
                state,
                mutation_id,
                document_id,
                base_revision_id,
                item_state,
                message,
            )
            .await;
        let release_result =
            content_repository::release_content_mutation_lock(mutation_lock, mutation_id)
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
        mutation_id: Uuid,
        document_id: Uuid,
        base_revision_id: Option<Uuid>,
        item_state: &str,
        message: &str,
    ) -> Result<ContentMutationItem, ApiError> {
        let existing_items = self.list_mutation_items(state, mutation_id).await?;
        validate_delete_mutation_item_collection(
            &existing_items,
            document_id,
            "the delete mutation has multiple unbound items and cannot be repaired safely",
            "the delete mutation already targets a different document",
        )?;
        if let Some(existing_item) =
            existing_items.iter().find(|item| item.document_id == Some(document_id)).cloned()
        {
            return self
                .reuse_or_update_delete_mutation_item(
                    state,
                    existing_item,
                    document_id,
                    base_revision_id,
                    item_state,
                    message,
                )
                .await;
        }

        if let Some(unbound_item) = existing_items.iter().find(|item| item.document_id.is_none()) {
            return self
                .claim_delete_mutation_item(
                    state,
                    mutation_id,
                    unbound_item.id,
                    document_id,
                    base_revision_id,
                    item_state,
                    message,
                )
                .await;
        }

        self.create_mutation_item(
            state,
            CreateMutationItemCommand {
                mutation_id,
                document_id: Some(document_id),
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
        document_id: Uuid,
        base_revision_id: Option<Uuid>,
        item_state: &str,
        message: &str,
    ) -> Result<ContentMutationItem, ApiError> {
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

    async fn claim_delete_mutation_item(
        &self,
        state: &AppState,
        mutation_id: Uuid,
        unbound_item_id: Uuid,
        document_id: Uuid,
        base_revision_id: Option<Uuid>,
        item_state: &str,
        message: &str,
    ) -> Result<ContentMutationItem, ApiError> {
        let claimed = content_repository::claim_unbound_mutation_item(
            &state.persistence.postgres,
            mutation_id,
            unbound_item_id,
            document_id,
            base_revision_id,
            None,
            item_state,
            Some(message),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        if let Some(claimed) = claimed {
            return Ok(map_mutation_item_row(claimed));
        }

        let refreshed_items = self.list_mutation_items(state, mutation_id).await?;
        validate_delete_mutation_item_collection(
            &refreshed_items,
            document_id,
            "the delete mutation has multiple unbound items and cannot be repaired safely",
            "the delete mutation item was claimed by a different document",
        )?;
        let Some(existing_item) =
            refreshed_items.into_iter().find(|item| item.document_id == Some(document_id))
        else {
            return Err(ApiError::idempotency_conflict(
                "the delete mutation item changed while its document target was being claimed",
            ));
        };
        self.reuse_or_update_delete_mutation_item(
            state,
            existing_item,
            document_id,
            base_revision_id,
            item_state,
            message,
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
            .canonical_services
            .ops
            .get_latest_async_operation_by_subject(state, "content_mutation", mutation_id)
            .await?
        {
            return Ok(existing);
        }

        state
            .canonical_services
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

    pub async fn list_mutations(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<Vec<ContentMutation>, ApiError> {
        let rows =
            content_repository::list_mutations_by_library(&state.persistence.postgres, library_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(rows.into_iter().map(map_mutation_row).collect())
    }

    pub async fn list_mutation_admissions(
        &self,
        state: &AppState,
        workspace_id: Uuid,
        library_id: Uuid,
    ) -> Result<Vec<ContentMutationAdmission>, ApiError> {
        let mutations = self.list_mutations(state, library_id).await?;
        let mutation_ids = mutations.iter().map(|mutation| mutation.id).collect::<Vec<_>>();
        let job_handles = state
            .canonical_services
            .ingest
            .list_job_handles_by_mutation_ids(state, workspace_id, library_id, &mutation_ids)
            .await?;

        let mut admissions = Vec::with_capacity(mutations.len());
        for mutation in mutations {
            let items = self.list_mutation_items(state, mutation.id).await?;
            let job_handle =
                job_handles.iter().find(|handle| handle.job.mutation_id == Some(mutation.id));
            let async_operation_id = job_handle
                .and_then(|handle| handle.async_operation.as_ref().map(|operation| operation.id))
                .or_else(|| job_handle.and_then(|handle| handle.job.async_operation_id));
            admissions.push(ContentMutationAdmission {
                mutation,
                items,
                job_id: job_handle.map(|handle| handle.job.id),
                async_operation_id,
            });
        }
        Ok(admissions)
    }

    pub async fn get_mutation(
        &self,
        state: &AppState,
        mutation_id: Uuid,
    ) -> Result<ContentMutation, ApiError> {
        let row = content_repository::get_mutation_by_id(&state.persistence.postgres, mutation_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("mutation", mutation_id))?;
        Ok(map_mutation_row(row))
    }

    pub async fn find_mutation_by_idempotency(
        &self,
        state: &AppState,
        principal_id: Uuid,
        request_surface: &str,
        idempotency_key: &str,
    ) -> Result<Option<ContentMutation>, ApiError> {
        let row = content_repository::find_mutation_by_idempotency(
            &state.persistence.postgres,
            principal_id,
            request_surface,
            idempotency_key,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(row.map(map_mutation_row))
    }

    pub async fn get_mutation_admission(
        &self,
        state: &AppState,
        mutation_id: Uuid,
    ) -> Result<ContentMutationAdmission, ApiError> {
        let mutation = self.get_mutation(state, mutation_id).await?;
        let items = self.list_mutation_items(state, mutation_id).await?;
        let job_handle = state
            .canonical_services
            .ingest
            .get_job_handle_by_mutation_id(state, mutation_id)
            .await?;
        let mut async_operation_id = job_handle
            .as_ref()
            .and_then(|handle| handle.async_operation.as_ref().map(|operation| operation.id))
            .or_else(|| job_handle.as_ref().and_then(|handle| handle.job.async_operation_id));
        if async_operation_id.is_none()
            && let Some(operation) = state
                .canonical_services
                .ops
                .get_latest_async_operation_by_subject(state, "content_mutation", mutation_id)
                .await?
        {
            async_operation_id = Some(operation.id);
        }
        Ok(ContentMutationAdmission {
            mutation,
            items,
            job_id: job_handle.as_ref().map(|handle| handle.job.id),
            async_operation_id,
        })
    }

    pub async fn list_mutation_items(
        &self,
        state: &AppState,
        mutation_id: Uuid,
    ) -> Result<Vec<ContentMutationItem>, ApiError> {
        let rows =
            content_repository::list_mutation_items(&state.persistence.postgres, mutation_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(rows.into_iter().map(map_mutation_item_row).collect())
    }

    pub async fn create_mutation_item(
        &self,
        state: &AppState,
        command: CreateMutationItemCommand,
    ) -> Result<ContentMutationItem, ApiError> {
        let row = content_repository::create_mutation_item(
            &state.persistence.postgres,
            &NewContentMutationItem {
                mutation_id: command.mutation_id,
                document_id: command.document_id,
                base_revision_id: command.base_revision_id,
                result_revision_id: command.result_revision_id,
                item_state: &command.item_state,
                message: command.message.as_deref(),
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(map_mutation_item_row(row))
    }

    pub async fn update_mutation(
        &self,
        state: &AppState,
        command: UpdateMutationCommand,
    ) -> Result<ContentMutation, ApiError> {
        let row = content_repository::update_mutation_status(
            &state.persistence.postgres,
            command.mutation_id,
            &command.mutation_state,
            command.completed_at,
            command.failure_code.as_deref(),
            command.conflict_code.as_deref(),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("mutation", command.mutation_id))?;
        Ok(map_mutation_row(row))
    }

    pub async fn update_mutation_item(
        &self,
        state: &AppState,
        command: UpdateMutationItemCommand,
    ) -> Result<ContentMutationItem, ApiError> {
        let row = content_repository::update_mutation_item(
            &state.persistence.postgres,
            command.item_id,
            command.document_id,
            command.base_revision_id,
            command.result_revision_id,
            &command.item_state,
            command.message.as_deref(),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("mutation_item", command.item_id))?;
        Ok(map_mutation_item_row(row))
    }

    pub async fn reconcile_failed_ingest_mutation(
        &self,
        state: &AppState,
        command: ReconcileFailedIngestMutationCommand,
    ) -> Result<ContentMutationAdmission, ApiError> {
        let admission = self.get_mutation_admission(state, command.mutation_id).await?;
        let job_handle = state
            .canonical_services
            .ingest
            .get_job_handle_by_mutation_id(state, command.mutation_id)
            .await?;
        let async_operation_id = admission.async_operation_id.or_else(|| {
            job_handle
                .as_ref()
                .and_then(|handle| handle.async_operation.as_ref().map(|operation| operation.id))
                .or_else(|| job_handle.as_ref().and_then(|handle| handle.job.async_operation_id))
        });
        let stage_events = if let Some(attempt) =
            job_handle.as_ref().and_then(|handle| handle.latest_attempt.as_ref())
        {
            state.canonical_services.ingest.list_stage_events(state, attempt.id).await?
        } else {
            Vec::new()
        };

        if let Some(operation_id) = async_operation_id {
            let _ = state
                .canonical_services
                .ops
                .update_async_operation(
                    state,
                    UpdateAsyncOperationCommand {
                        operation_id,
                        status: "failed".to_string(),
                        completed_at: Some(Utc::now()),
                        failure_code: Some(command.failure_code.clone()),
                    },
                )
                .await?;
        }

        for item in &admission.items {
            if matches!(item.item_state.as_str(), "applied" | "failed") {
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
                        item_state: "failed".to_string(),
                        message: Some(command.failure_message.clone()),
                    },
                )
                .await?;
        }

        if matches!(admission.mutation.mutation_state.as_str(), "accepted" | "running") {
            let _ = self
                .update_mutation(
                    state,
                    UpdateMutationCommand {
                        mutation_id: command.mutation_id,
                        mutation_state: "failed".to_string(),
                        completed_at: Some(Utc::now()),
                        failure_code: Some(command.failure_code.clone()),
                        conflict_code: None,
                    },
                )
                .await?;
        }

        let document_id =
            admission.items.iter().find_map(|item| item.document_id).or_else(|| {
                job_handle.as_ref().and_then(|handle| handle.job.knowledge_document_id)
            });
        let revision_id =
            admission.items.iter().find_map(|item| item.result_revision_id).or_else(|| {
                job_handle.as_ref().and_then(|handle| handle.job.knowledge_revision_id)
            });

        if let Some(document_id) = document_id
            && let Some(document) = state
                .document_store
                .get_document(document_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        {
            let head =
                content_repository::get_document_head(&state.persistence.postgres, document_id)
                    .await
                    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
            let _ = self
                .promote_document_head(
                    state,
                    PromoteHeadCommand {
                        document_id,
                        active_revision_id: document.active_revision_id,
                        readable_revision_id: document.readable_revision_id,
                        latest_mutation_id: Some(command.mutation_id),
                        latest_successful_attempt_id: head
                            .as_ref()
                            .and_then(|current_head| current_head.latest_successful_attempt_id),
                    },
                )
                .await?;
        }

        if let Some(revision_id) = revision_id
            && let Some(revision) = state
                .document_store
                .get_revision(revision_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        {
            let readiness = derive_failed_revision_readiness(&revision, &stage_events);
            let _ = state
                .document_store
                .update_revision_readiness(
                    revision_id,
                    &readiness.text_state,
                    &readiness.vector_state,
                    &readiness.graph_state,
                    readiness.text_readable_at,
                    readiness.vector_ready_at,
                    readiness.graph_ready_at,
                    revision.superseded_by_revision_id,
                )
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        }

        self.get_mutation_admission(state, command.mutation_id).await
    }
}

#[cfg(test)]
mod admission_error_mapping_tests {
    use super::*;

    #[test]
    fn target_state_errors_map_without_reclassifying_unrelated_database_failures() {
        let document_id = Uuid::now_v7();

        assert!(matches!(
            map_atomic_admission_error(AdmissionError::TargetDocumentNotFound { document_id }),
            ApiError::NotFound(_)
        ));
        assert!(matches!(
            map_atomic_admission_error(AdmissionError::TargetDocumentDeleted { document_id }),
            ApiError::BadRequest(_)
        ));
        assert!(matches!(
            map_atomic_admission_error(AdmissionError::TargetDocumentScopeConflict { document_id }),
            ApiError::Conflict(_)
        ));
        assert!(matches!(
            map_atomic_admission_error(AdmissionError::TargetDocumentHeadIntegrity { document_id }),
            ApiError::Internal
        ));
        assert!(matches!(
            map_atomic_admission_error(AdmissionError::ConflictingActiveMutation { document_id }),
            ApiError::ConflictingMutation(_)
        ));
        assert!(matches!(
            map_atomic_admission_error(AdmissionError::Database(sqlx::Error::RowNotFound)),
            ApiError::Internal
        ));
    }

    #[test]
    fn delete_item_reuse_requires_the_exact_document_and_revision_scope() {
        let document_id = Uuid::now_v7();
        let base_revision_id = Uuid::now_v7();
        let mut item = ContentMutationItem {
            id: Uuid::now_v7(),
            mutation_id: Uuid::now_v7(),
            document_id: None,
            base_revision_id: Some(base_revision_id),
            result_revision_id: None,
            item_state: "pending".to_string(),
            message: Some("document delete admitted".to_string()),
        };

        assert!(!delete_mutation_item_is_reusable(
            &item,
            document_id,
            Some(base_revision_id),
            "pending",
            "document delete admitted",
        ));
        item.item_state = "applied".to_string();
        assert!(!delete_mutation_item_is_reusable(
            &item,
            document_id,
            Some(base_revision_id),
            "pending",
            "document delete admitted",
        ));
        item.document_id = Some(document_id);
        assert!(delete_mutation_item_is_reusable(
            &item,
            document_id,
            Some(base_revision_id),
            "pending",
            "document delete admitted",
        ));
    }
}
