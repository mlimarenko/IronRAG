use super::*;

use crate::infra::knowledge_plane::RevisionReadinessUpdate;

impl ContentService {
    pub async fn reconcile_failed_ingest_mutation(
        &self,
        state: &AppState,
        command: ReconcileFailedIngestMutationCommand,
    ) -> Result<ContentMutationAdmission, ApiError> {
        let admission = self.get_mutation_admission(state, command.mutation_id).await?;
        let job_handle = state
            .application_services
            .ingest
            .get_job_handle_by_mutation_id(state, command.mutation_id)
            .await?;
        let async_operation_id = resolve_reconciled_async_operation_id(
            admission.async_operation_id,
            job_handle
                .as_ref()
                .and_then(|handle| handle.async_operation.as_ref().map(|operation| operation.id)),
            job_handle.as_ref().and_then(|handle| handle.job.async_operation_id),
        );
        let stage_events = if let Some(attempt) =
            job_handle.as_ref().and_then(|handle| handle.latest_attempt.as_ref())
        {
            state.application_services.ingest.list_stage_events(state, attempt.id).await?
        } else {
            Vec::new()
        };

        // Retire the job before the operation goes terminal. A job left
        // claimable behind a terminal operation can never lease an attempt
        // again, and it keeps occupying a per-library concurrency slot every
        // time the stale-lease reaper hands it back to the queue.
        ingest_repository::fail_jobs_for_mutation(
            &state.persistence.postgres,
            command.mutation_id,
            &command.failure_code,
            &command.failure_message,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

        if let Some(operation_id) = async_operation_id {
            let _ = state
                .application_services
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

        self.fail_reconciled_mutation_items(state, &admission, &command).await?;

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

        self.promote_reconciled_document_head(state, &command, document_id).await?;
        self.fail_reconciled_revision_readiness(state, revision_id, &stage_events).await?;

        self.get_mutation_admission(state, command.mutation_id).await
    }

    async fn fail_reconciled_mutation_items(
        &self,
        state: &AppState,
        admission: &ContentMutationAdmission,
        command: &ReconcileFailedIngestMutationCommand,
    ) -> Result<(), ApiError> {
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

        Ok(())
    }

    /// Reconciles only the failed mutation's still-owned, locked current head.
    async fn promote_reconciled_document_head(
        &self,
        state: &AppState,
        command: &ReconcileFailedIngestMutationCommand,
        document_id: Option<Uuid>,
    ) -> Result<(), ApiError> {
        let Some(document_id) = document_id else {
            return Ok(());
        };
        crate::infra::repositories::content_knowledge_projection_repository::reconcile_failed_document_head_with_projection(
            &state.persistence.postgres,
            document_id,
            command.mutation_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(())
    }

    /// Marks the failed revision's readiness from its stage events.
    async fn fail_reconciled_revision_readiness(
        &self,
        state: &AppState,
        revision_id: Option<Uuid>,
        stage_events: &[crate::domains::ingest::IngestStageEvent],
    ) -> Result<(), ApiError> {
        let Some(revision_id) = revision_id else {
            return Ok(());
        };
        let Some(revision) = state
            .document_store
            .get_revision(revision_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        else {
            return Ok(());
        };
        let readiness = derive_failed_revision_readiness(&revision, stage_events);
        let _ = state
            .document_store
            .update_revision_readiness(RevisionReadinessUpdate {
                revision_id,
                text_state: &readiness.text_state,
                vector_state: &readiness.vector_state,
                graph_state: &readiness.graph_state,
                text_readable_at: readiness.text_readable_at,
                vector_ready_at: readiness.vector_ready_at,
                graph_ready_at: readiness.graph_ready_at,
                superseded_by_revision_id: revision.superseded_by_revision_id,
            })
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(())
    }
}

fn resolve_reconciled_async_operation_id(
    admission_operation_id: Option<Uuid>,
    handle_operation_id: Option<Uuid>,
    job_operation_id: Option<Uuid>,
) -> Option<Uuid> {
    admission_operation_id.or(handle_operation_id).or(job_operation_id)
}

#[cfg(test)]
mod tests {
    use super::resolve_reconciled_async_operation_id;
    use uuid::Uuid;

    #[test]
    fn reconciled_operation_identity_prefers_admission_then_handle_then_job() {
        let admission = Some(Uuid::from_u128(1));
        let handle = Some(Uuid::from_u128(2));
        let job = Some(Uuid::from_u128(3));
        for (admission_id, handle_id, job_id, expected) in [
            (None, None, None, None),
            (None, None, job, job),
            (None, handle, None, handle),
            (None, handle, job, handle),
            (admission, None, None, admission),
            (admission, None, job, admission),
            (admission, handle, None, admission),
            (admission, handle, job, admission),
        ] {
            assert_eq!(
                resolve_reconciled_async_operation_id(admission_id, handle_id, job_id),
                expected,
            );
        }
    }
}
