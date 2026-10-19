use super::*;

impl ContentService {
    /// Resolves the revision that retry should re-run against.
    ///
    /// Authoritative source is Postgres `content_revision`, NOT the
    /// derived knowledge projection. The retry ultimately writes a
    /// `content_mutation_item` row whose `base_revision_id` FK points into
    /// `content_revision`; if we pick a revision that only exists in
    /// the knowledge projection (projection drift after a crashed ingest) the admit fails
    /// with a raw FK violation and the document stays stuck.
    ///
    /// Selects the latest revision by `(revision_number desc, created_at
    /// desc)`. This covers both the healthy case (revision exists with a
    /// `storage_key`) and the failed-mid-pipeline case (revision was
    /// created before the crash, `content_document_head.active_revision_id`
    /// was never promoted). When the document has literally zero
    /// revisions in PG (orphan debris from an earlier crash that never
    /// persisted any source bytes), this function force-finalizes the
    /// document — marks any inflight mutation as `failed`, flips
    /// queued/leased ingest jobs to `canceled`, tombstones the document
    /// — and reports `NotFound` so the caller buckets it under
    /// `skipped_count` instead of looping forever.
    pub async fn resolve_reprocess_revision(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<ContentRevision, ApiError> {
        let rows = content_revision_repository::list_revisions_by_document(
            &state.persistence.postgres,
            document_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        if let Some(row) = rows.into_iter().next() {
            return Ok(map_revision_row(row));
        }
        self.force_fail_unrecoverable_document(state, document_id).await?;
        Err(ApiError::resource_not_found("document", document_id))
    }

    /// Permanently retires an orphan document that has no recoverable source.
    /// Cancels any inflight ingest jobs, flips an inflight mutation to
    /// `failed` (if any), and tombstones the document itself by setting
    /// `document_state='deleted'`. The document then disappears from the
    /// active list and stops jamming the retry loop. Idempotent — safe to
    /// call repeatedly.
    async fn force_fail_unrecoverable_document(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<(), ApiError> {
        if let Err(error) = ingest_repository::cancel_jobs_for_document(
            &state.persistence.postgres,
            document_id,
            ingest_repository::CancelDisposition::UNRECOVERABLE_NO_SOURCE,
        )
        .await
        {
            return Err(ApiError::internal_with_log(error, "internal"));
        }
        if let Some(head) = self.get_document_head(state, document_id).await?
            && let Some(latest_mutation_id) = head.latest_mutation_id
            && let Some(latest_mutation) = content_mutation_repository::get_mutation_by_id(
                &state.persistence.postgres,
                latest_mutation_id,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            && matches!(latest_mutation.mutation_state.as_str(), "accepted" | "running")
        {
            self.reconcile_failed_ingest_mutation(
                state,
                ReconcileFailedIngestMutationCommand {
                    mutation_id: latest_mutation_id,
                    failure_code: "unrecoverable_no_source".to_string(),
                    failure_message:
                        "document has no content_revision rows; nothing to ingest from".to_string(),
                },
            )
            .await?;
        }
        // Tombstone the document so it leaves the active listing. Without
        // this the retry caller would see the same orphan back on the next
        // page refresh and try again forever.
        let _ = content_document_repository::update_document_state(
            &state.persistence.postgres,
            document_id,
            "deleted",
            Some(chrono::Utc::now()),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(())
    }

    /// Force-aborts any ingest work still attached to `document_id` and
    /// finalizes the latest mutation so a subsequent `admit_mutation` call
    /// can proceed. Called from the retry path — a user-initiated retry is
    /// an explicit "stop whatever was happening and start over", which the
    /// automatic `reconcile_stale_inflight_mutation_if_terminal` refuses to
    /// do because it only acts on jobs that reached a terminal failure state
    /// on their own.
    ///
    /// Sequence:
    /// 1. `cancel_jobs_for_document` — transitions every queued/leased
    ///    `ingest_job` for this document to `queue_state='canceled'`. Queued
    ///    rows are atomically terminal. Leased rows become a signal that the
    ///    worker's heartbeat observer picks up within `≤15s`, aborting the
    ///    pipeline cooperatively and finalizing the attempt as canceled.
    /// 2. `reconcile_failed_ingest_mutation` — flips the stuck mutation
    ///    (`accepted`/`running`) to `failed` with
    ///    `failure_code='superseded_by_retry'`, updates mutation items and
    ///    async operation, and re-promotes the document head. From this
    ///    point `ensure_document_accepts_new_mutation` no longer blocks
    ///    a fresh mutation on this document.
    ///
    /// Terminal mutations (`failed`, `canceled`, `applied`) are left alone.
    pub async fn force_reset_inflight_for_retry(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<(), ApiError> {
        self.force_reset_inflight_for_retry_inner(state, document_id, std::future::ready(())).await
    }

    /// Inner form of [`Self::force_reset_inflight_for_retry`] with an await
    /// point between the two halves of the reset, so the HTTP retry path can
    /// supply a deterministic test gate (IRG-203 SITE-2). Production calls this
    /// with an already-ready future, which the compiler folds away.
    ///
    /// `after_cancel` runs after the first commit (`cancel_jobs_for_document`)
    /// and before the mutation reconciliation. That is exactly the window the
    /// cancellation class is about: dropping the caller there used to leave the
    /// head mutation `accepted|running` with its jobs already `canceled`.
    pub(crate) async fn force_reset_inflight_for_retry_inner<F>(
        &self,
        state: &AppState,
        document_id: Uuid,
        after_cancel: F,
    ) -> Result<(), ApiError>
    where
        F: std::future::Future<Output = ()> + Send,
    {
        if let Err(error) = ingest_repository::cancel_jobs_for_document(
            &state.persistence.postgres,
            document_id,
            ingest_repository::CancelDisposition::SUPERSEDED_BY_RETRY,
        )
        .await
        {
            return Err(ApiError::internal_with_log(error, "internal"));
        }

        after_cancel.await;

        let Some(latest_mutation) = self.latest_inflight_mutation(state, document_id).await? else {
            return Ok(());
        };

        self.reconcile_failed_ingest_mutation(
            state,
            ReconcileFailedIngestMutationCommand {
                mutation_id: latest_mutation.id,
                failure_code: "superseded_by_retry".to_string(),
                failure_message:
                    "document retry requested by user while previous ingest was still inflight"
                        .to_string(),
            },
        )
        .await?;
        Ok(())
    }

    /// The document head's latest mutation while it is still non-terminal
    /// (`accepted`/`running`); `None` when there is no head, no mutation, or the
    /// mutation already reached a terminal state.
    ///
    /// Both the retry reset and the "accepts a new mutation" gate need exactly
    /// this resolution; keeping it in one place is what stops them drifting on
    /// which states count as inflight.
    async fn latest_inflight_mutation(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<Option<content_mutation_repository::ContentMutationRow>, ApiError> {
        let Some(head) = self.get_document_head(state, document_id).await? else {
            return Ok(None);
        };
        let Some(latest_mutation_id) = head.latest_mutation_id else {
            return Ok(None);
        };
        let Some(latest_mutation) = content_mutation_repository::get_mutation_by_id(
            &state.persistence.postgres,
            latest_mutation_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        else {
            return Ok(None);
        };
        if matches!(latest_mutation.mutation_state.as_str(), "accepted" | "running") {
            Ok(Some(latest_mutation))
        } else {
            Ok(None)
        }
    }

    pub async fn ensure_document_accepts_new_mutation(
        &self,
        state: &AppState,
        document_id: Uuid,
        operation_kind: &str,
    ) -> Result<(), ApiError> {
        let document = content_document_repository::get_document_by_id(
            &state.persistence.postgres,
            document_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;
        if document.document_state == "deleted" || document.deleted_at.is_some() {
            return Err(ApiError::BadRequest(if operation_kind == MUTATION_KIND_DELETE {
                "document is already deleted".to_string()
            } else {
                "deleted documents do not accept new mutations".to_string()
            }));
        }
        let Some(latest_mutation) = self.latest_inflight_mutation(state, document_id).await? else {
            return Ok(());
        };
        // The reconcile returns the state it set when it acted; `None` means it
        // left the mutation alone, and the helper only ever returns an inflight
        // row, so either way an unreconciled row still blocks a new mutation.
        let still_inflight = self
            .reconcile_stale_inflight_mutation_if_terminal(state, &latest_mutation)
            .await?
            .is_none_or(|state| matches!(state.as_str(), "accepted" | "running"));
        if operation_kind != MUTATION_KIND_DELETE && still_inflight {
            return Err(ApiError::ConflictingMutation(
                "document is still processing a previous mutation".to_string(),
            ));
        }
        Ok(())
    }

    async fn reconcile_stale_inflight_mutation_if_terminal(
        &self,
        state: &AppState,
        latest_mutation: &content_mutation_repository::ContentMutationRow,
    ) -> Result<Option<String>, ApiError> {
        let admission = self.get_mutation_admission(state, latest_mutation.id).await?;
        let job_handle = state
            .application_services
            .ingest
            .get_job_handle_by_mutation_id(state, latest_mutation.id)
            .await?;
        let job_failed =
            job_handle.as_ref().is_some_and(|handle| handle.job.queue_state == "failed");
        let attempt_failed = job_handle
            .as_ref()
            .and_then(|handle| handle.latest_attempt.as_ref())
            .is_some_and(|attempt| {
                matches!(attempt.attempt_state.as_str(), "failed" | "abandoned" | "canceled")
            });
        let async_operation_failed = admission.async_operation_id.and_then(|operation_id| {
            job_handle
                .as_ref()
                .and_then(|handle| handle.async_operation.as_ref())
                .filter(|operation| operation.id == operation_id)
                .map(|operation| operation.status.as_str() == ASYNC_OP_STATUS_FAILED)
        }) == Some(true);

        if !(job_failed || attempt_failed || async_operation_failed) {
            return Ok(None);
        }

        let failure_code = job_handle
            .as_ref()
            .and_then(|handle| handle.latest_attempt.as_ref())
            .and_then(|attempt| attempt.failure_code.clone())
            .or_else(|| {
                job_handle
                    .as_ref()
                    .and_then(|handle| handle.async_operation.as_ref())
                    .and_then(|operation| operation.failure_code.clone())
            })
            .unwrap_or_else(|| "\u{63}anonical_pipeline_failed".to_string());
        let failure_message = format!(
            "terminal ingest failure left mutation {} in {}",
            latest_mutation.id, latest_mutation.mutation_state
        );
        let reconciled = self
            .reconcile_failed_ingest_mutation(
                state,
                ReconcileFailedIngestMutationCommand {
                    mutation_id: latest_mutation.id,
                    failure_code,
                    failure_message,
                },
            )
            .await?;
        Ok(Some(reconciled.mutation.mutation_state))
    }
}
