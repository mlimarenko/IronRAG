use super::*;

fn log_deleted_revision_chunk_cleanup_failure(
    document_id: Uuid,
    revision_id: Uuid,
    e: impl std::fmt::Debug,
) {
    tracing::error!(
        %document_id,
        %revision_id,
        ?e,
        "failed to delete chunks and vectors for deleted document"
    );
}

fn warn_deleted_revision_artifact_cleanup_failure(
    document_id: Uuid,
    revision_id: Uuid,
    e: impl std::fmt::Debug,
    message: &str,
) {
    tracing::warn!(%document_id, %revision_id, ?e, "{message}");
}

impl ContentService {
    pub async fn converge_document_technical_facts(
        &self,
        state: &AppState,
        document_id: Uuid,
        retained_revision_id: Option<Uuid>,
    ) -> Result<(), ApiError> {
        let revisions = content_revision_repository::list_revisions_by_document(
            &state.persistence.postgres,
            document_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        for revision in revisions {
            if Some(revision.id) == retained_revision_id {
                continue;
            }
            if let Err(error) =
                state.document_store.delete_technical_facts_by_revision(revision.id).await
            {
                tracing::warn!(
                    %document_id,
                    revision_id = %revision.id,
                    ?error,
                    "failed to delete technical facts for document revision"
                );
            }
        }
        Ok(())
    }

    pub(crate) async fn refresh_deleted_document_graph_state(
        &self,
        state: &AppState,
        library_id: Uuid,
        document_id: Uuid,
    ) -> Result<(), ApiError> {
        self.cleanup_deleted_document_local_graph_artifacts(state, library_id, document_id).await?;
        let cleanup =
            self.cleanup_deleted_document_graph_evidence(state, library_id, document_id).await?;
        self.refresh_deleted_library_graph_projection_for_cleanup(state, library_id, cleanup).await
    }

    pub(crate) async fn cleanup_deleted_document_local_graph_artifacts(
        &self,
        state: &AppState,
        library_id: Uuid,
        document_id: Uuid,
    ) -> Result<(), ApiError> {
        repositories::query_repository::delete_query_execution_references_by_document(
            &state.persistence.postgres,
            library_id,
            document_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

        // Clean up knowledge-plane artifacts for all revisions of the deleted document.
        let revisions = state
            .document_store
            .list_revisions_by_document(document_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        for revision in &revisions {
            self.cleanup_revision_local_graph_artifacts(state, document_id, revision.revision_id)
                .await;
        }

        Ok(())
    }

    /// Best-effort cleanup of one revision's knowledge-plane artifacts.
    ///
    /// Post-commit cleanup: the document delete already committed, so a failure
    /// here must stay a loud, operator-visible error rather than abort the
    /// settled mutation — but it must never be downgraded to a routine warning
    /// either. The revision keeps its chunks and vectors until the delete is
    /// retried.
    async fn cleanup_revision_local_graph_artifacts(
        &self,
        state: &AppState,
        document_id: Uuid,
        revision_id: Uuid,
    ) {
        if let Err(e) =
            state.application_services.knowledge.delete_revision_chunks(state, revision_id).await
        {
            log_deleted_revision_chunk_cleanup_failure(document_id, revision_id, e);
        }
        if let Err(e) = state.document_store.delete_structured_blocks_by_revision(revision_id).await
        {
            warn_deleted_revision_artifact_cleanup_failure(
                document_id,
                revision_id,
                e,
                "failed to delete structured blocks for deleted document",
            );
        }
        if let Err(e) = state.graph_store.delete_entity_candidates_by_revision(revision_id).await {
            warn_deleted_revision_artifact_cleanup_failure(
                document_id,
                revision_id,
                e,
                "failed to delete entity candidates for deleted document",
            );
        }
        if let Err(e) = state.graph_store.delete_relation_candidates_by_revision(revision_id).await
        {
            warn_deleted_revision_artifact_cleanup_failure(
                document_id,
                revision_id,
                e,
                "failed to delete relation candidates for deleted document",
            );
        }
    }

    pub(crate) async fn cleanup_deleted_document_graph_evidence(
        &self,
        state: &AppState,
        library_id: Uuid,
        document_id: Uuid,
    ) -> Result<DeletedDocumentGraphCleanup, ApiError> {
        let targets = repositories::deactivate_runtime_graph_evidence_by_document(
            &state.persistence.postgres,
            library_id,
            document_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let mut targets = targets;
        targets.extend(
            repositories::list_runtime_graph_document_node_targets_by_documents(
                &state.persistence.postgres,
                library_id,
                &[document_id],
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?,
        );
        Ok(DeletedDocumentGraphCleanup::from_targets(targets))
    }

    pub(crate) async fn cleanup_deleted_documents_graph_evidence(
        &self,
        state: &AppState,
        library_id: Uuid,
        document_ids: &[Uuid],
    ) -> Result<DeletedDocumentGraphCleanup, ApiError> {
        let targets = repositories::deactivate_runtime_graph_evidence_by_documents(
            &state.persistence.postgres,
            library_id,
            document_ids,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let mut targets = targets;
        targets.extend(
            repositories::list_runtime_graph_document_node_targets_by_documents(
                &state.persistence.postgres,
                library_id,
                document_ids,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?,
        );
        Ok(DeletedDocumentGraphCleanup::from_targets(targets))
    }

    pub(crate) async fn refresh_deleted_library_graph_projection_for_cleanup(
        &self,
        state: &AppState,
        library_id: Uuid,
        cleanup: DeletedDocumentGraphCleanup,
    ) -> Result<(), ApiError> {
        if !cleanup.requires_graph_convergence() {
            return Ok(());
        }

        // IRG-218 test seam: a DB test forces this background step to fail to
        // prove a projection failure does not become the parent's status. A
        // no-op in production builds.
        #[cfg(feature = "test-support")]
        if crate::shared::cancellation::graph_convergence_fault::take() {
            return Err(ApiError::SettlementRefreshFailed(
                "injected graph projection failure".to_string(),
            ));
        }

        let projection_scope = crate::services::graph::projection::resolve_projection_scope(
            state, library_id,
        )
        .await
        .map_err(|error| {
            ApiError::SettlementRefreshFailed(format!(
                "settlement refresh failed: document delete graph projection scope failed: {error}"
            ))
        })?;
        repositories::recalculate_runtime_graph_node_support_counts_by_ids(
            &state.persistence.postgres,
            library_id,
            projection_scope.projection_version,
            &cleanup.node_ids,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        repositories::recalculate_runtime_graph_edge_support_counts_by_ids(
            &state.persistence.postgres,
            library_id,
            projection_scope.projection_version,
            &cleanup.edge_ids,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let deleted_edge_keys = repositories::delete_runtime_graph_edges_without_support_by_ids(
            &state.persistence.postgres,
            library_id,
            projection_scope.projection_version,
            &cleanup.edge_ids,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let deleted_node_keys = repositories::delete_runtime_graph_nodes_without_support_by_ids(
            &state.persistence.postgres,
            library_id,
            projection_scope.projection_version,
            &cleanup.node_ids,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        // Active graph summaries have no FK back to runtime_graph_node /
        // runtime_graph_edge, so the prune above does not cascade. Drop any
        // summary whose target was just deleted (or had been orphaned by an
        // earlier failed cleanup that did not run this sweep).
        let _ = repositories::delete_runtime_graph_summaries_for_orphan_targets(
            &state.persistence.postgres,
            library_id,
            &cleanup.node_ids,
            &cleanup.edge_ids,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        if !deleted_edge_keys.is_empty() {
            let _ = state
                .graph_store
                .delete_relations_by_identity_keys(library_id, &deleted_edge_keys)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        }
        if !deleted_node_keys.is_empty() {
            let _ = state
                .graph_store
                .delete_entities_by_identity_keys(library_id, &deleted_node_keys)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        }
        let projection_scope =
            projection_scope.with_targeted_refresh(cleanup.node_ids, cleanup.edge_ids);
        state
            .application_services
            .graph
            .project_normalized_graph(state, &projection_scope)
            .await
            .map_err(|error| {
                ApiError::SettlementRefreshFailed(format!(
                    "settlement refresh failed: document delete graph projection refresh failed: {error}"
                ))
            })?;
        Ok(())
    }
}
