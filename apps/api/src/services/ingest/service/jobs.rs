use super::*;

impl IngestService {
    pub async fn list_jobs(
        &self,
        state: &AppState,
        workspace_id: Option<Uuid>,
        library_id: Option<Uuid>,
    ) -> Result<Vec<IngestJob>, ApiError> {
        let rows = ingest_repository::list_ingest_jobs(
            &state.persistence.postgres,
            workspace_id,
            library_id,
            None,
            None,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(rows.into_iter().map(map_job_row).collect())
    }

    /// Keyset-paginated job history for `GET
    /// /v1/ingest/libraries/{libraryId}/jobs`. Returns the built handles for
    /// the page plus whether a further page exists. Unlike `list_jobs`
    /// (unpaginated, still used by the MCP mutation-failure lookup path),
    /// this is the primary REST list surface.
    pub async fn list_job_handles_page(
        &self,
        state: &AppState,
        library_id: Uuid,
        cursor: Option<(chrono::DateTime<Utc>, Uuid)>,
        limit: i64,
        status_filter: &[String],
    ) -> Result<(Vec<IngestJobHandle>, bool), ApiError> {
        let page = ingest_repository::list_ingest_jobs_page(
            &state.persistence.postgres,
            library_id,
            cursor,
            limit,
            status_filter,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let jobs = page.rows.into_iter().map(map_job_row).collect();
        let handles = self.build_job_handles(state, jobs).await?;
        Ok((handles, page.has_more))
    }

    pub async fn list_job_handles_by_mutation_ids(
        &self,
        state: &AppState,
        workspace_id: Uuid,
        library_id: Uuid,
        mutation_ids: &[Uuid],
    ) -> Result<Vec<IngestJobHandle>, ApiError> {
        let rows = ingest_repository::list_ingest_jobs_by_mutation_ids(
            &state.persistence.postgres,
            workspace_id,
            library_id,
            mutation_ids,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let jobs = rows.into_iter().map(map_job_row).collect();
        self.build_job_handles(state, jobs).await
    }

    pub async fn get_job(&self, state: &AppState, job_id: Uuid) -> Result<IngestJob, ApiError> {
        let row = ingest_repository::get_ingest_job_by_id(&state.persistence.postgres, job_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("ingest_job", job_id))?;
        Ok(map_job_row(row))
    }

    pub async fn get_job_handle(
        &self,
        state: &AppState,
        job_id: Uuid,
    ) -> Result<IngestJobHandle, ApiError> {
        let job = self.get_job(state, job_id).await?;
        self.build_job_handle(state, job).await
    }

    pub async fn get_job_handle_by_mutation_id(
        &self,
        state: &AppState,
        mutation_id: Uuid,
    ) -> Result<Option<IngestJobHandle>, ApiError> {
        let row = ingest_repository::get_latest_ingest_job_by_mutation_id(
            &state.persistence.postgres,
            mutation_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        match row {
            Some(row) => Ok(Some(self.build_job_handle(state, map_job_row(row)).await?)),
            None => Ok(None),
        }
    }

    pub async fn get_job_handle_by_async_operation_id(
        &self,
        state: &AppState,
        async_operation_id: Uuid,
    ) -> Result<Option<IngestJobHandle>, ApiError> {
        let row = ingest_repository::get_latest_ingest_job_by_async_operation_id(
            &state.persistence.postgres,
            async_operation_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        match row {
            Some(row) => Ok(Some(self.build_job_handle(state, map_job_row(row)).await?)),
            None => Ok(None),
        }
    }

    pub async fn get_job_handle_by_knowledge_revision_id(
        &self,
        state: &AppState,
        knowledge_revision_id: Uuid,
    ) -> Result<Option<IngestJobHandle>, ApiError> {
        let row = ingest_repository::get_latest_ingest_job_by_knowledge_revision_id(
            &state.persistence.postgres,
            knowledge_revision_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        match row {
            Some(row) => Ok(Some(self.build_job_handle(state, map_job_row(row)).await?)),
            None => Ok(None),
        }
    }

    pub async fn list_job_handles_by_knowledge_document_id(
        &self,
        state: &AppState,
        workspace_id: Uuid,
        library_id: Uuid,
        knowledge_document_id: Uuid,
    ) -> Result<Vec<IngestJobHandle>, ApiError> {
        let rows = ingest_repository::list_ingest_jobs_by_knowledge_document_id(
            &state.persistence.postgres,
            workspace_id,
            library_id,
            knowledge_document_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let jobs = rows.into_iter().map(map_job_row).collect();
        self.build_job_handles(state, jobs).await
    }

    pub async fn admit_job(
        &self,
        state: &AppState,
        command: AdmitIngestJobCommand,
    ) -> Result<IngestJob, ApiError> {
        if let Some(dedupe_key) =
            command.dedupe_key.as_deref().map(str::trim).filter(|value| !value.is_empty())
            && let Some(existing) = ingest_repository::get_ingest_job_by_dedupe_key(
                &state.persistence.postgres,
                command.library_id,
                dedupe_key,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        {
            return Ok(map_job_row(existing));
        }

        let row = ingest_repository::create_ingest_job(
            &state.persistence.postgres,
            &NewIngestJob {
                workspace_id: command.workspace_id,
                library_id: command.library_id,
                mutation_id: command.mutation_id,
                mutation_item_id: command.mutation_item_id,
                connector_id: command.connector_id,
                async_operation_id: command.async_operation_id,
                knowledge_document_id: command.knowledge_document_id,
                knowledge_revision_id: command.knowledge_revision_id,
                job_kind: command.job_kind,
                queue_state: "queued".to_string(),
                priority: command.priority,
                dedupe_key: command.dedupe_key,
                queued_at: None,
                available_at: command.available_at,
                completed_at: None,
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(map_job_row(row))
    }

    async fn build_job_handle(
        &self,
        state: &AppState,
        job: IngestJob,
    ) -> Result<IngestJobHandle, ApiError> {
        let latest_attempt = ingest_repository::get_latest_ingest_attempt_by_job(
            &state.persistence.postgres,
            job.id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .map(map_attempt_row);
        let async_operation = match job.async_operation_id {
            Some(operation_id) => {
                Some(state.application_services.ops.get_async_operation(state, operation_id).await?)
            }
            None => None,
        };
        Ok(IngestJobHandle { job, latest_attempt, async_operation })
    }

    async fn build_job_handles(
        &self,
        state: &AppState,
        jobs: Vec<IngestJob>,
    ) -> Result<Vec<IngestJobHandle>, ApiError> {
        if jobs.is_empty() {
            return Ok(Vec::new());
        }

        let job_ids = jobs.iter().map(|job| job.id).collect::<Vec<_>>();
        let async_operation_ids =
            jobs.iter().filter_map(|job| job.async_operation_id).collect::<Vec<_>>();

        let latest_attempts_by_job_id = ingest_repository::list_latest_ingest_attempts_by_job_ids(
            &state.persistence.postgres,
            &job_ids,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .into_iter()
        .map(|row| (row.job_id, map_attempt_row(row)))
        .collect::<HashMap<_, _>>();

        let async_operation_rows = ops_repository::list_async_operations_by_ids(
            &state.persistence.postgres,
            &async_operation_ids,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let mut async_operations_by_id = HashMap::with_capacity(async_operation_rows.len());
        for row in async_operation_rows {
            async_operations_by_id.insert(row.id, map_async_operation_row(row)?);
        }

        Ok(jobs
            .into_iter()
            .map(|job| IngestJobHandle {
                latest_attempt: latest_attempts_by_job_id.get(&job.id).cloned(),
                async_operation: job
                    .async_operation_id
                    .and_then(|operation_id| async_operations_by_id.get(&operation_id).cloned()),
                job,
            })
            .collect())
    }
}
