use chrono::Utc;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::content::{ContentMutation, ContentMutationItem},
    infra::repositories::content_repository::{self, NewContentMutation, NewContentMutationItem},
    interfaces::http::router_support::ApiError,
    services::{
        ingest::service::AdmitIngestJobCommand,
        ops::service::{CreateAsyncOperationCommand, UpdateAsyncOperationCommand},
    },
};

use super::{
    AcceptMutationCommand, AdmitDocumentCommand, AdmitMutationCommand, ContentMutationAdmission,
    ContentService, CreateDocumentAdmission, CreateDocumentCommand, CreateMutationItemCommand,
    PromoteHeadCommand, ReconcileFailedIngestMutationCommand, UpdateMutationCommand,
    UpdateMutationItemCommand, derive_failed_revision_readiness,
    ensure_existing_mutation_matches_request, is_content_mutation_idempotency_violation,
    map_mutation_item_row, map_mutation_row,
};

impl ContentService {
    pub async fn admit_document(
        &self,
        state: &AppState,
        command: AdmitDocumentCommand,
    ) -> Result<CreateDocumentAdmission, ApiError> {
        let mutation = self
            .accept_mutation(
                state,
                AcceptMutationCommand {
                    workspace_id: command.workspace_id,
                    library_id: command.library_id,
                    operation_kind: "upload".to_string(),
                    requested_by_principal_id: command.created_by_principal_id,
                    request_surface: command.request_surface.clone(),
                    idempotency_key: command.idempotency_key.clone(),
                    source_identity: command.source_identity.clone(),
                },
            )
            .await?;
        let mutation_lock = content_repository::acquire_content_mutation_lock(
            &state.persistence.postgres,
            mutation.id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

        let result = async {
            let existing_admission = self.get_mutation_admission(state, mutation.id).await?;
            if let Some(existing_document_id) =
                existing_admission.items.iter().find_map(|item| item.document_id)
            {
                let document = self.get_document(state, existing_document_id).await?;
                return Ok(CreateDocumentAdmission { document, mutation: existing_admission });
            }

            let document = self
                .create_document(
                    state,
                    CreateDocumentCommand {
                        workspace_id: command.workspace_id,
                        library_id: command.library_id,
                        external_key: command.external_key,
                        file_name: command.file_name,
                        created_by_principal_id: command.created_by_principal_id,
                    },
                )
                .await?;

            let async_operation = state
                .canonical_services
                .ops
                .create_async_operation(
                    state,
                    CreateAsyncOperationCommand {
                        workspace_id: command.workspace_id,
                        library_id: command.library_id,
                        operation_kind: "content_mutation".to_string(),
                        surface_kind: "rest".to_string(),
                        requested_by_principal_id: command.created_by_principal_id,
                        status: "accepted".to_string(),
                        subject_kind: "content_mutation".to_string(),
                        subject_id: Some(mutation.id),
                        completed_at: None,
                        failure_code: None,
                    },
                )
                .await?;

            let (items, job_id, async_operation_id) = if let Some(revision) = command.revision {
                let revision = self
                    .create_revision_from_metadata(
                        state,
                        document.id,
                        command.created_by_principal_id,
                        revision,
                    )
                    .await?;
                let item = self
                    .create_mutation_item(
                        state,
                        CreateMutationItemCommand {
                            mutation_id: mutation.id,
                            document_id: Some(document.id),
                            base_revision_id: None,
                            result_revision_id: Some(revision.id),
                            item_state: "pending".to_string(),
                            message: Some(
                                "document revision accepted and queued for ingest".to_string(),
                            ),
                        },
                    )
                    .await?;
                let job = match state
                    .canonical_services
                    .ingest
                    .admit_job(
                        state,
                        AdmitIngestJobCommand {
                            workspace_id: command.workspace_id,
                            library_id: command.library_id,
                            mutation_id: Some(mutation.id),
                            connector_id: None,
                            async_operation_id: Some(async_operation.id),
                            knowledge_document_id: Some(document.id),
                            knowledge_revision_id: Some(revision.id),
                            job_kind: "content_mutation".to_string(),
                            priority: 100,
                            dedupe_key: command.idempotency_key,
                            available_at: None,
                        },
                    )
                    .await
                {
                    Ok(job) => job,
                    Err(error) => {
                        let _ = self
                            .reconcile_failed_ingest_mutation(
                                state,
                                ReconcileFailedIngestMutationCommand {
                                    mutation_id: mutation.id,
                                    failure_code: "ingest_job_admission_failed".to_string(),
                                    failure_message:
                                        "failed to admit ingest job for uploaded document"
                                            .to_string(),
                                },
                            )
                            .await;
                        return Err(error);
                    }
                };
                let _ = self
                    .promote_pending_document_mutation_head(state, document.id, mutation.id)
                    .await?;
                (vec![item], Some(job.id), Some(async_operation.id))
            } else {
                let _ = self
                    .promote_document_head(
                        state,
                        PromoteHeadCommand {
                            document_id: document.id,
                            active_revision_id: None,
                            readable_revision_id: None,
                            latest_mutation_id: Some(mutation.id),
                            latest_successful_attempt_id: None,
                        },
                    )
                    .await?;
                let _ = self
                    .update_mutation(
                        state,
                        UpdateMutationCommand {
                            mutation_id: mutation.id,
                            mutation_state: "applied".to_string(),
                            completed_at: Some(Utc::now()),
                            failure_code: None,
                            conflict_code: None,
                        },
                    )
                    .await?;
                let ready_operation = state
                    .canonical_services
                    .ops
                    .update_async_operation(
                        state,
                        UpdateAsyncOperationCommand {
                            operation_id: async_operation.id,
                            status: "ready".to_string(),
                            completed_at: Some(Utc::now()),
                            failure_code: None,
                        },
                    )
                    .await?;
                (Vec::new(), None, Some(ready_operation.id))
            };

            let document = self.get_document(state, document.id).await?;
            let mutation = self.get_mutation(state, mutation.id).await?;
            Ok(CreateDocumentAdmission {
                document,
                mutation: ContentMutationAdmission { mutation, items, job_id, async_operation_id },
            })
        }
        .await;
        let release_result =
            content_repository::release_content_mutation_lock(mutation_lock, mutation.id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"));
        match (result, release_result) {
            (Ok(admission), Ok(())) => Ok(admission),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Err(_), Err(error)) => Err(error),
        }
    }

    pub async fn admit_mutation(
        &self,
        state: &AppState,
        command: AdmitMutationCommand,
    ) -> Result<ContentMutationAdmission, ApiError> {
        let accept_command = Self::accept_mutation_command_from_admit(&command);

        if command.operation_kind == "delete" {
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

        if let Some(existing_admission) =
            self.get_existing_mutation_admission_for_request(state, &accept_command).await?
        {
            return Ok(existing_admission);
        }

        self.ensure_document_accepts_new_mutation(
            state,
            command.document_id,
            &command.operation_kind,
        )
        .await?;
        let current_head = self.get_document_head(state, command.document_id).await?;
        let base_revision_id = current_head.as_ref().and_then(|row| row.latest_revision_id());

        let mutation = self.accept_mutation(state, accept_command).await?;
        let async_operation = state
            .canonical_services
            .ops
            .create_async_operation(
                state,
                CreateAsyncOperationCommand {
                    workspace_id: command.workspace_id,
                    library_id: command.library_id,
                    operation_kind: "content_mutation".to_string(),
                    surface_kind: "rest".to_string(),
                    requested_by_principal_id: command.requested_by_principal_id,
                    status: "accepted".to_string(),
                    subject_kind: "content_mutation".to_string(),
                    subject_id: Some(mutation.id),
                    completed_at: None,
                    failure_code: None,
                },
            )
            .await?;

        let revision = self
            .create_revision_from_metadata(
                state,
                command.document_id,
                command.requested_by_principal_id,
                command.revision.ok_or_else(|| {
                    ApiError::BadRequest(
                        "revision metadata is required for non-delete document mutations"
                            .to_string(),
                    )
                })?,
            )
            .await?;

        let item = self
            .create_mutation_item(
                state,
                CreateMutationItemCommand {
                    mutation_id: mutation.id,
                    document_id: Some(command.document_id),
                    base_revision_id,
                    result_revision_id: Some(revision.id),
                    item_state: "pending".to_string(),
                    message: Some("revision accepted and queued for ingest".to_string()),
                },
            )
            .await?;
        let job = match state
            .canonical_services
            .ingest
            .admit_job(
                state,
                AdmitIngestJobCommand {
                    workspace_id: command.workspace_id,
                    library_id: command.library_id,
                    mutation_id: Some(mutation.id),
                    connector_id: None,
                    async_operation_id: Some(async_operation.id),
                    knowledge_document_id: Some(command.document_id),
                    knowledge_revision_id: Some(revision.id),
                    job_kind: "content_mutation".to_string(),
                    priority: 100,
                    dedupe_key: command.idempotency_key,
                    available_at: None,
                },
            )
            .await
        {
            Ok(job) => job,
            Err(error) => {
                let _ = self
                    .reconcile_failed_ingest_mutation(
                        state,
                        ReconcileFailedIngestMutationCommand {
                            mutation_id: mutation.id,
                            failure_code: "ingest_job_admission_failed".to_string(),
                            failure_message: "failed to admit ingest job for mutation".to_string(),
                        },
                    )
                    .await;
                return Err(error);
            }
        };
        let _ = self
            .promote_pending_document_mutation_head(state, command.document_id, mutation.id)
            .await?;
        Ok(ContentMutationAdmission {
            mutation,
            items: vec![item],
            job_id: Some(job.id),
            async_operation_id: Some(async_operation.id),
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
            source_identity: command.source_identity.clone(),
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
        let base_revision_id = current_head.as_ref().and_then(|row| row.latest_revision_id());
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
            let canonical_delete_mutation = match superseded_mutation_id {
                Some(latest_mutation_id) => match content_repository::get_mutation_by_id(
                    &state.persistence.postgres,
                    latest_mutation_id,
                )
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                {
                    Some(existing_row) if existing_row.operation_kind == "delete" => {
                        map_mutation_row(existing_row)
                    }
                    _ => self.accept_mutation(state, accept_command.clone()).await?,
                },
                None => self.accept_mutation(state, accept_command.clone()).await?,
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

        let _ = self
            .delete_document_with_context(state, command.document_id, Some(mutation.id))
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

        if async_operation.status != "ready"
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
        let existing_items = self.list_mutation_items(state, mutation_id).await?;
        let existing_item = existing_items
            .iter()
            .find(|item| item.document_id == Some(document_id))
            .cloned()
            .or_else(|| existing_items.into_iter().next());

        if let Some(existing_item) = existing_item {
            if existing_item.item_state == "applied" && item_state == "pending" {
                return Ok(existing_item);
            }
            if existing_item.base_revision_id == base_revision_id
                && existing_item.result_revision_id.is_none()
                && existing_item.item_state == item_state
                && existing_item.message.as_deref() == Some(message)
            {
                return Ok(existing_item);
            }
            return self
                .update_mutation_item(
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
                    library_id: command.library_id,
                    operation_kind: "content_mutation".to_string(),
                    surface_kind: command.request_surface.clone(),
                    requested_by_principal_id: command.requested_by_principal_id,
                    status: "accepted".to_string(),
                    subject_kind: "content_mutation".to_string(),
                    subject_id: Some(mutation_id),
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
                .arango_document_store
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
                .arango_document_store
                .get_revision(revision_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        {
            let readiness = derive_failed_revision_readiness(&revision, &stage_events);
            let _ = state
                .arango_document_store
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
