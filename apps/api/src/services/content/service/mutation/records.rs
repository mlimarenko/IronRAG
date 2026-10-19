use super::*;

impl ContentService {
    pub async fn list_mutations(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<Vec<ContentMutation>, ApiError> {
        let rows = content_mutation_repository::list_mutations_by_library(
            &state.persistence.postgres,
            library_id,
        )
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
        let subject_ids: Vec<Uuid> = sqlx::query_scalar("select subject_id from ops_async_operation
            where subject_kind='content_mutation' and workspace_id=$1 and library_id=$2 and subject_id is not null")
            .bind(workspace_id).bind(library_id).fetch_all(&state.persistence.postgres).await
            .map_err(|error| ApiError::internal_with_log(error, "content receipt inventory failed"))?;
        let subjects = subject_ids.into_iter().collect::<std::collections::HashSet<_>>();
        let mut admissions = Vec::with_capacity(mutations.len());
        for mutation in mutations {
            if !subjects.contains(&mutation.id) {
                continue;
            }
            if mutation.workspace_id != workspace_id {
                return Err(ApiError::Internal);
            }
            admissions.push(self.get_mutation_admission(state, mutation.id).await?);
        }
        Ok(admissions)
    }

    pub async fn get_mutation(
        &self,
        state: &AppState,
        mutation_id: Uuid,
    ) -> Result<ContentMutation, ApiError> {
        let row = content_mutation_repository::get_mutation_by_id(
            &state.persistence.postgres,
            mutation_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("mutation", mutation_id))?;
        Ok(map_mutation_row(row))
    }

    pub async fn get_mutation_admission(
        &self,
        state: &AppState,
        mutation_id: Uuid,
    ) -> Result<ContentMutationAdmission, ApiError> {
        let row = content_mutation_repository::get_mutation_by_id(
            &state.persistence.postgres,
            mutation_id,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "content receipt lookup failed"))?
        .ok_or_else(|| ApiError::resource_not_found("mutation", mutation_id))?;
        let request_fingerprint = row.request_fingerprint.clone();
        let mutation = map_mutation_row(row);
        let items = self.list_mutation_items(state, mutation_id).await?;
        let job_handle = state
            .application_services
            .ingest
            .get_job_handle_by_mutation_id(state, mutation_id)
            .await?;
        let linked_operation_id = job_handle
            .as_ref()
            .and_then(|handle| handle.async_operation.as_ref().map(|operation| operation.id))
            .or_else(|| job_handle.as_ref().and_then(|handle| handle.job.async_operation_id));
        let operations: Vec<(Uuid, String)> = sqlx::query_as(
            "select id, status::text from ops_async_operation where subject_kind='content_mutation'
               and subject_id=$1 and workspace_id=$2 and library_id=$3",
        )
        .bind(mutation_id)
        .bind(mutation.workspace_id)
        .bind(mutation.library_id)
        .fetch_all(&state.persistence.postgres)
        .await
        .map_err(|error| {
            ApiError::internal_with_log(error, "content receipt operation lookup failed")
        })?;
        let operation = match operations.as_slice() {
            [operation] if linked_operation_id.is_none_or(|id| id == operation.0) => {
                Some(operation)
            }
            [] if linked_operation_id.is_none() => None,
            _ => return Err(ApiError::Internal),
        };
        let result_revision_id =
            if let [item] = items.as_slice() { item.result_revision_id } else { None };
        let result_checksum = if let Some(revision_id) = result_revision_id {
            sqlx::query_scalar(
                "select r.checksum from content_revision r join content_mutation_item i
                on i.result_revision_id=r.id join content_document d on d.id=r.document_id
                where r.id=$1 and i.mutation_id=$2 and d.workspace_id=$3 and d.library_id=$4",
            )
            .bind(revision_id)
            .bind(mutation_id)
            .bind(mutation.workspace_id)
            .bind(mutation.library_id)
            .fetch_optional(&state.persistence.postgres)
            .await
            .map_err(|error| {
                ApiError::internal_with_log(error, "content receipt revision lookup failed")
            })?
        } else {
            None
        };
        Ok(ContentMutationAdmission {
            replayed: true,
            mutation,
            items,
            job_id: job_handle.as_ref().map(|handle| handle.job.id),
            async_operation_id: operation.map(|operation| operation.0),
            operation_status: operation.map(|operation| operation.1.clone()),
            result_revision_id,
            result_checksum,
            request_fingerprint,
        })
    }

    pub async fn list_mutation_items(
        &self,
        state: &AppState,
        mutation_id: Uuid,
    ) -> Result<Vec<ContentMutationItem>, ApiError> {
        let rows = content_mutation_repository::list_mutation_items(
            &state.persistence.postgres,
            mutation_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(rows.into_iter().map(map_mutation_item_row).collect())
    }

    pub async fn create_mutation_item(
        &self,
        state: &AppState,
        command: CreateMutationItemCommand,
    ) -> Result<ContentMutationItem, ApiError> {
        let row = content_mutation_repository::create_mutation_item(
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
        let row = content_mutation_repository::update_mutation_status(
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
        let row = content_mutation_repository::update_mutation_item(
            &state.persistence.postgres,
            content_mutation_repository::MutationItemUpdate {
                item_id: command.item_id,
                document_id: command.document_id,
                base_revision_id: command.base_revision_id,
                result_revision_id: command.result_revision_id,
                item_state: &command.item_state,
                message: command.message.as_deref(),
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("mutation_item", command.item_id))?;
        Ok(map_mutation_item_row(row))
    }
}
