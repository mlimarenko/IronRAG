use super::*;

/// What a committed document-deletion transaction leaves for the post-commit
/// convergence steps: the deleted document row and whether this call made the
/// real non-deleted -> deleted transition (which owns the parentage cascade).
struct DocumentDeletionOutcome {
    document: content_document_repository::ContentDocumentRow,
    transitioned_to_deleted: bool,
}

fn warn_child_cascade_failure(
    parent_document_id: Uuid,
    child_document_id: Uuid,
    library_id: Uuid,
    error: impl std::fmt::Debug,
    message: &str,
) {
    tracing::warn!(%parent_document_id, %child_document_id, %library_id, ?error, "{message}");
}

impl ContentService {
    // --- Document lifecycle ---

    /// Repairs durable projections on an exact terminal receipt without rerunning
    /// post-commit graph cleanup or child lifecycle effects.
    pub(crate) async fn reconcile_deleted_document_projection(
        &self,
        state: &AppState,
        document_id: Uuid,
        mutation_id: Uuid,
    ) -> Result<(), ApiError> {
        self.persist_document_deletion(state, document_id, Some(mutation_id)).await?;
        Ok(())
    }

    pub async fn delete_document(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<ContentDocument, ApiError> {
        self.delete_document_with_context(state, document_id, None, true).await
    }

    pub(crate) async fn delete_document_with_context(
        &self,
        state: &AppState,
        document_id: Uuid,
        latest_mutation_id: Option<Uuid>,
        refresh_graph_projection: bool,
    ) -> Result<ContentDocument, ApiError> {
        let deletion =
            self.persist_document_deletion(state, document_id, latest_mutation_id).await?;
        let document = deletion.document;

        self.converge_deleted_document_technical_facts(state, document.library_id, document_id)
            .await;
        self.converge_deleted_document_graph_artifacts(
            state,
            document.library_id,
            document_id,
            refresh_graph_projection,
        )
        .await;

        // Only the transaction that observed the non-deleted row owns the
        // parentage cascade. The same transaction already persisted the
        // durable lifecycle event beside the state transition.
        if deletion.transitioned_to_deleted {
            // Cascade the parentage lifecycle to this document's children. Only
            // runs on the real delete transition (guarded by the non-deleted
            // precondition above), so an idempotent re-delete never re-cascades.
            // `attached_context` children are subordinate context whose value
            // depends on the parent — they are soft-deleted through the normal
            // per-document delete flow so each fires its own webhook/audit
            // event. `attachment` children are peer documents that survive; they
            // are detached (parent pointer cleared) but keep their declared
            // `parent_external_key` so the resolver can re-attach if the parent
            // reappears.
            self.cascade_document_parentage_on_delete(
                state,
                document.library_id,
                document_id,
                refresh_graph_projection,
            )
            .await;
        }

        Ok(ContentDocument {
            id: document.id,
            workspace_id: document.workspace_id,
            library_id: document.library_id,
            external_key: document.external_key,
            document_state: document.document_state,
            created_at: document.created_at,
        })
    }

    async fn converge_deleted_document_technical_facts(
        &self,
        state: &AppState,
        library_id: Uuid,
        document_id: Uuid,
    ) {
        if let Err(error) = self.converge_document_technical_facts(state, document_id, None).await {
            tracing::warn!(
                %document_id,
                %library_id,
                ?error,
                "post-delete technical fact convergence failed after document delete committed"
            );
        }
    }

    async fn converge_deleted_document_graph_artifacts(
        &self,
        state: &AppState,
        library_id: Uuid,
        document_id: Uuid,
        refresh_graph_projection: bool,
    ) {
        let graph_refresh = if refresh_graph_projection {
            self.refresh_deleted_document_graph_state(state, library_id, document_id).await
        } else {
            self.cleanup_deleted_document_local_graph_artifacts(state, library_id, document_id)
                .await
        };
        if let Err(error) = graph_refresh {
            tracing::warn!(
                %document_id,
                %library_id,
                ?error,
                "post-delete graph convergence failed after document delete committed"
            );
        }
    }

    /// Commits the single transaction that soft-deletes the document, cancels
    /// its jobs, rewrites its head, enqueues the lifecycle webhook, and
    /// materializes the knowledge projection. Returns what the post-commit
    /// steps still need.
    /// Content owns the lifecycle: its tombstone and the knowledge copy commit
    /// together. A failed projection write must roll back the source deletion.
    async fn persist_document_deletion(
        &self,
        state: &AppState,
        document_id: Uuid,
        latest_mutation_id: Option<Uuid>,
    ) -> Result<DocumentDeletionOutcome, ApiError> {
        let mut transaction = state
            .persistence
            .postgres
            .begin()
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        // Establish the same parent-before-child lock order used by catalog
        // deletion. This initial read does not lock the document; the library
        // parent-generation lock is therefore the first row lock held by this
        // transaction and fences a concurrent library/workspace delete before
        // we touch document, head, job, or outbox rows.
        let document_scope = content_document_repository::get_document_by_id_with_executor(
            &mut *transaction,
            document_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;
        let parent_locked = catalog_repository::lock_library_for_lifecycle_event_with_executor(
            &mut *transaction,
            document_scope.workspace_id,
            document_scope.library_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        if !parent_locked {
            return Err(ApiError::resource_not_found("document", document_id));
        }
        let locked_document = content_document_repository::lock_document_by_id_with_executor(
            &mut *transaction,
            document_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;
        if locked_document.workspace_id != document_scope.workspace_id
            || locked_document.library_id != document_scope.library_id
        {
            return Err(ApiError::Conflict(
                "document scope changed while acquiring the lifecycle lock".to_string(),
            ));
        }
        let head = content_document_head_repository::get_document_head_for_update_with_executor(
            &mut *transaction,
            document_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let readable_revision_id = head.as_ref().and_then(|row| row.readable_revision_id);
        let resolved_latest_mutation_id =
            latest_mutation_id.or_else(|| head.as_ref().and_then(|row| row.latest_mutation_id));
        let latest_successful_attempt_id =
            head.as_ref().and_then(|row| row.latest_successful_attempt_id);
        let transitioned_to_deleted = locked_document.document_state != "deleted";
        let deleted_at = if transitioned_to_deleted {
            Utc::now()
        } else {
            locked_document.deleted_at.unwrap_or_else(Utc::now)
        };
        let _ = ingest_repository::cancel_jobs_for_document_with_executor(
            &mut *transaction,
            document_id,
            ingest_repository::CancelDisposition::DOCUMENT_DELETED,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let document = content_document_repository::update_document_state_with_executor(
            &mut *transaction,
            document_id,
            "deleted",
            Some(deleted_at),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;
        let head_outcome =
            content_document_head_repository::upsert_document_head_without_generation_outcome(
                &mut transaction,
                &content_document_head_repository::NewContentDocumentHead {
                    publishing_mutation_item_id: None,
                    document_id,
                    active_revision_id: None,
                    readable_revision_id,
                    latest_mutation_id: resolved_latest_mutation_id,
                    latest_successful_attempt_id,
                },
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        match head_outcome {
            content_document_head_repository::ValidatedDocumentHeadWriteOutcome::Updated(_) => {}
            content_document_head_repository::ValidatedDocumentHeadWriteOutcome::DocumentNotFound => {
                return Err(ApiError::resource_not_found("document", document_id));
            }
            integrity
            @ content_document_head_repository::ValidatedDocumentHeadWriteOutcome::ReferenceIntegrityViolation => {
                return Err(ApiError::internal_with_log(
                    integrity,
                    "document delete head integrity failure",
                ));
            }
        }
        if transitioned_to_deleted {
            let event = WebhookEvent {
                event_type: "document.deleted".to_string(),
                event_id: document_deleted_event_id(document_id, deleted_at),
                occurred_at: deleted_at,
                workspace_id: locked_document.workspace_id,
                library_id: locked_document.library_id,
                payload_json: serde_json::json!({
                    "document_id": document_id,
                    "library_id": locked_document.library_id,
                }),
            };
            webhook_outbox_repository::enqueue_webhook_lifecycle_event_with_executor(
                &mut *transaction,
                &event,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        }
        let materialized =
            content_knowledge_projection_repository::materialize_knowledge_document_from_current_head_with_transaction(
                &mut transaction,
                document_id,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        if materialized.outcome
            != content_knowledge_projection_repository::MaterializeKnowledgeDocumentOutcome::Materialized
        {
            return Err(ApiError::service_unavailable(
                "document projection is converging; retry the delete shortly",
                "document_projection_converging",
            ));
        }
        if transitioned_to_deleted || materialized.changed {
            catalog_repository::touch_library_source_truth_version_with_executor(
                &mut *transaction,
                document.library_id,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        }
        transaction.commit().await.map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(DocumentDeletionOutcome { document, transitioned_to_deleted })
    }

    /// Applies the parentage lifecycle to a deleted document's children:
    /// soft-delete `attached_context` children through the normal per-document
    /// delete flow, and detach `attachment` children (clear the parent pointer,
    /// keep the declared key). Failures are logged and never fail the parent
    /// delete, which is already committed by the time this runs.
    async fn cascade_document_parentage_on_delete(
        &self,
        state: &AppState,
        library_id: Uuid,
        parent_document_id: Uuid,
        refresh_graph_projection: bool,
    ) {
        let children = match content_document_repository::list_active_children_by_parent(
            &state.persistence.postgres,
            library_id,
            parent_document_id,
        )
        .await
        {
            Ok(children) => children,
            Err(error) => {
                tracing::warn!(
                    parent_document_id = %parent_document_id,
                    %library_id,
                    ?error,
                    "failed to load child documents for delete cascade; children left untouched"
                );
                return;
            }
        };

        for child in children {
            self.cascade_child_document(
                state,
                library_id,
                parent_document_id,
                &child,
                refresh_graph_projection,
            )
            .await;
        }
    }

    /// Applies the parentage lifecycle to one deleted document's child.
    ///
    /// `attached_context` children are soft-deleted through the normal
    /// per-document flow (so each fires its own webhook/audit event);
    /// `attachment` children are peers that survive and are only detached
    /// (parent pointer cleared, declared `parent_external_key` kept). Failures
    /// are logged and never fail the parent delete, which is already committed.
    async fn cascade_child_document(
        &self,
        state: &AppState,
        library_id: Uuid,
        parent_document_id: Uuid,
        child: &content_document_repository::ContentDocumentRow,
        refresh_graph_projection: bool,
    ) {
        match child.document_role.as_str() {
            DOCUMENT_ROLE_ATTACHED_CONTEXT => {
                // Boxed: `delete_document_with_context` -> cascade ->
                // `delete_document_with_context` is an async recursion cycle
                // (a parent's attached_context child is itself deleted
                // through the full per-document flow), which Rust requires to
                // be heap-pinned.
                if let Err(error) = Box::pin(self.delete_document_with_context(
                    state,
                    child.id,
                    None,
                    refresh_graph_projection,
                ))
                .await
                {
                    warn_child_cascade_failure(
                        parent_document_id,
                        child.id,
                        library_id,
                        error,
                        "failed to cascade-delete attached_context child after parent delete",
                    );
                }
            }
            DOCUMENT_ROLE_ATTACHMENT => {
                if let Err(error) = content_document_repository::detach_document_parent(
                    &state.persistence.postgres,
                    library_id,
                    child.id,
                )
                .await
                {
                    warn_child_cascade_failure(
                        parent_document_id,
                        child.id,
                        library_id,
                        error,
                        "failed to detach attachment child after parent delete",
                    );
                }
            }
            _ => {}
        }
    }
}
