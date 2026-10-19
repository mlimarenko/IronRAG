use super::*;

impl ContentService {
    pub async fn create_document(
        &self,
        state: &AppState,
        command: CreateDocumentCommand,
    ) -> Result<ContentDocument, ApiError> {
        let external_key = command
            .external_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map_or_else(|| Uuid::now_v7().to_string(), ToString::to_string);
        let parent_external_key = command
            .parent_external_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        // Resolve the declared parent key to a concrete parent document when the
        // sibling already exists in this library. When it does not exist yet
        // (out-of-order connector sync), the key is recorded as pending and the
        // deferred resolver reconciles it later.
        let parent_document_id = match parent_external_key.as_deref() {
            Some(key) => content_document_repository::get_document_by_external_key(
                &state.persistence.postgres,
                command.library_id,
                key,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .map(|row| row.id),
            None => None,
        };
        // The role is finalized once a revision lands and its media class is
        // known (promote path / backfill). At shell-creation the media class is
        // unknown, so a child gets the provisional peer role `attachment`: never
        // a wrong demotion to `attached_context`, and `primary` when it has no
        // declared parent at all.
        let document_role =
            crate::domains::content::derive_document_role(parent_external_key.is_some(), false);
        let row = content_knowledge_projection_repository::create_document_with_projection(
            &state.persistence.postgres,
            &NewContentDocument {
                workspace_id: command.workspace_id,
                library_id: command.library_id,
                external_key: &external_key,
                document_state: "active",
                created_by_principal_id: command.created_by_principal_id,
                parent_external_key: parent_external_key.as_deref(),
                parent_document_id,
                document_role,
            },
            command.file_name.as_deref(),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(ContentDocument {
            id: row.id,
            workspace_id: row.workspace_id,
            library_id: row.library_id,
            external_key: row.external_key.clone(),
            document_state: row.document_state.clone(),
            created_at: row.created_at,
        })
    }

    pub async fn create_revision(
        &self,
        state: &AppState,
        command: CreateRevisionCommand,
    ) -> Result<ContentRevision, ApiError> {
        let document = content_document_repository::get_document_by_id(
            &state.persistence.postgres,
            command.document_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("document", command.document_id))?;
        if document.document_state == "deleted" || document.deleted_at.is_some() {
            return Err(ApiError::BadRequest(
                "deleted documents do not accept new revisions".to_string(),
            ));
        }
        let outcome = content_revision_repository::create_revision_with_projection(
            &state.persistence.postgres,
            &NewContentRevisionProjection {
                admission_identity: None,
                document_id: document.id,
                workspace_id: document.workspace_id,
                library_id: document.library_id,
                content_source_kind: &command.content_source_kind,
                checksum: &command.checksum,
                mime_type: &command.mime_type,
                byte_size: command.byte_size,
                title: command.title.as_deref(),
                language_code: command.language_code.as_deref(),
                source_uri: command.source_uri.as_deref(),
                document_hint: command.document_hint.as_deref(),
                storage_key: command.storage_key.as_deref(),
                created_by_principal_id: command.created_by_principal_id,
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        match outcome {
            CreateContentRevisionOutcome::Created(row) => Ok(map_revision_row(*row)),
            CreateContentRevisionOutcome::DocumentNotFound => {
                Err(ApiError::resource_not_found("document", command.document_id))
            }
            CreateContentRevisionOutcome::DocumentDeleted => Err(ApiError::BadRequest(
                "deleted documents do not accept new revisions".to_string(),
            )),
            CreateContentRevisionOutcome::ProjectionUnavailable => {
                Err(ApiError::service_unavailable(
                    "document projection is converging; retry the revision shortly",
                    "document_projection_converging",
                ))
            }
        }
    }

    pub async fn append_revision(
        &self,
        state: &AppState,
        command: CreateRevisionCommand,
    ) -> Result<ContentRevision, ApiError> {
        self.create_revision(state, command).await
    }

    pub async fn replace_revision(
        &self,
        state: &AppState,
        command: CreateRevisionCommand,
    ) -> Result<ContentRevision, ApiError> {
        self.create_revision(state, command).await
    }

    pub async fn promote_document_head(
        &self,
        state: &AppState,
        command: PromoteHeadCommand,
    ) -> Result<ContentDocumentHead, ApiError> {
        if command.readable_revision_id.is_none() && command.publishing_mutation_item_id.is_some() {
            return Err(ApiError::VisibilityBindingConflict(
                "a publishing item requires a readable revision".to_owned(),
            ));
        }
        if let Some(active_revision_id) = command.active_revision_id {
            content_revision_repository::get_revision_by_id(
                &state.persistence.postgres,
                active_revision_id,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("revision", active_revision_id))?;
        }
        if let Some(readable_revision_id) = command.readable_revision_id {
            content_revision_repository::get_revision_by_id(
                &state.persistence.postgres,
                readable_revision_id,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("revision", readable_revision_id))?;
        }

        let outcome =
            content_knowledge_projection_repository::promote_document_head_with_projection(
                &state.persistence.postgres,
                &NewContentDocumentHead {
                    publishing_mutation_item_id: command.publishing_mutation_item_id,
                    document_id: command.document_id,
                    active_revision_id: command.active_revision_id,
                    readable_revision_id: command.readable_revision_id,
                    latest_mutation_id: command.latest_mutation_id,
                    latest_successful_attempt_id: command.latest_successful_attempt_id,
                },
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let row = match outcome {
            PromoteDocumentHeadOutcome::Promoted(row) => row,
            PromoteDocumentHeadOutcome::DocumentNotFound => {
                return Err(ApiError::resource_not_found("document", command.document_id));
            }
            PromoteDocumentHeadOutcome::RevisionNotFound => {
                return Err(ApiError::service_unavailable(
                    "document projection is converging; retry the promotion shortly",
                    "document_projection_converging",
                ));
            }
            PromoteDocumentHeadOutcome::SuppressedUntilReady => {
                return Err(ApiError::BadRequest(
                    "document visibility is suppressed until fenced READY publication".to_string(),
                ));
            }
            integrity @ PromoteDocumentHeadOutcome::ReferenceIntegrityViolation => {
                return Err(ApiError::internal_with_log(
                    integrity,
                    "document head reference integrity failure",
                ));
            }
        };
        Ok(ContentDocumentHead {
            visibility_intent_id: row.visibility_intent_id,
            document_id: row.document_id,
            active_revision_id: row.active_revision_id,
            readable_revision_id: row.readable_revision_id,
            latest_mutation_id: row.latest_mutation_id,
            latest_successful_attempt_id: row.latest_successful_attempt_id,
            head_updated_at: row.head_updated_at,
            document_summary: row.document_summary,
        })
    }
}
