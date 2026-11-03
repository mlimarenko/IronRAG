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
        let request_identity: Option<serde_json::Value> =
            sqlx::query_scalar("select request_identity from content_mutation where id=$1")
                .bind(mutation_id)
                .fetch_one(&state.persistence.postgres)
                .await
                .map_err(|error| {
                    ApiError::internal_with_log(error, "content receipt identity lookup failed")
                })?;
        match (request_identity.as_ref(), request_fingerprint.as_deref()) {
            (Some(identity), Some(digest)) => {
                crate::infra::repositories::admission_repository::verify_fingerprint(
                    identity, digest,
                )
                .map_err(|_| ApiError::Internal)?;
            }
            (None, None) => {}
            _ => return Err(ApiError::Internal),
        }
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
        let payload_kind =
            matches!(mutation.operation_kind.as_str(), "upload" | "replace" | "append" | "edit");
        if payload_kind
            && result_revision_id.is_none()
            && (mutation.operation_kind != "upload"
                || items.iter().any(|item| item.result_revision_id.is_some()))
        {
            return Err(ApiError::Internal);
        }
        let payload_admission = payload_kind && result_revision_id.is_some();
        if payload_admission
            && (operation.is_none() || request_fingerprint.as_deref().is_none_or(str::is_empty))
        {
            return Err(ApiError::Internal);
        }
        let result_checksum = if let Some(revision_id) = result_revision_id {
            sqlx::query_scalar(
                "select r.checksum from content_revision r join content_mutation_item i
                on i.result_revision_id=r.id join content_document d on d.id=r.document_id
                 where r.id=$1 and i.mutation_id=$2 and d.workspace_id=$3 and d.library_id=$4
                 and r.workspace_id=$3 and r.library_id=$4 and i.document_id=r.document_id",
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
            .ok_or(ApiError::Internal)
            .map(Some)?
        } else {
            None
        };
        let visibility_intent_id =
            if let Some(revision_id) = result_revision_id.filter(|_| payload_admission) {
                let identity = content_revision_repository::get_revision_admission_identity(
                    &state.persistence.postgres,
                    revision_id,
                )
                .await
                .map_err(|error| {
                    ApiError::internal_with_log(error, "content receipt identity lookup failed")
                })?
                .ok_or(ApiError::Internal)?;
                mutation_receipt_visibility_binding(identity, result_checksum.as_deref())?
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
            visibility_intent_id,
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

fn mutation_receipt_visibility_binding(
    identity: ironrag_contracts::documents::RevisionAdmissionIdentity,
    checksum: Option<&str>,
) -> Result<Option<Uuid>, ApiError> {
    let identity = identity.normalize().map_err(|_| ApiError::Internal)?;
    match identity {
        ironrag_contracts::documents::RevisionAdmissionIdentity::Mutation {
            visibility_intent_id,
            replacement,
            ..
        } if Some(replacement.checksum.as_str()) == checksum => Ok(visibility_intent_id),
        _ => Err(ApiError::Internal),
    }
}

#[cfg(test)]
mod content_receipt_tests {
    use super::*;
    use ironrag_contracts::documents::{ReplacementIdentity, RevisionAdmissionIdentity};

    fn identity(binding: Option<Uuid>) -> RevisionAdmissionIdentity {
        RevisionAdmissionIdentity::Mutation {
            source_identity: Some("source".to_owned()),
            materialization_token: None,
            visibility_intent_id: binding,
            replacement: ReplacementIdentity {
                checksum: format!("sha256:{}", "a".repeat(64)),
                byte_size: 3,
                mime_type: "text/plain".to_owned(),
                file_name: "page.txt".to_owned(),
                title: None,
                language_code: None,
                source_uri: None,
                document_hint: None,
            },
        }
    }

    #[test]
    fn content_receipt_projects_explicit_null_or_exact_binding() {
        let checksum = format!("sha256:{}", "a".repeat(64));
        for binding in [None, Some(Uuid::from_u128(9))] {
            assert_eq!(
                mutation_receipt_visibility_binding(identity(binding), Some(&checksum)).unwrap(),
                binding
            );
        }
    }

    #[test]
    fn content_receipt_rejects_checksum_and_origin_disagreement() {
        assert!(mutation_receipt_visibility_binding(identity(None), None).is_err());
        assert!(mutation_receipt_visibility_binding(identity(None), Some("different")).is_err());
        let certified = RevisionAdmissionIdentity::CertifiedImport {
            replacement: identity(None).replacement().clone(),
            provenance: ironrag_contracts::documents::ImportAdmissionProvenance {
                recovery_snapshot_id: "a".repeat(64),
                identity_certificate_sha256: "b".repeat(64),
                source_baseline_sha384: "c".repeat(96),
                admitted_by: "operator".to_owned(),
            },
        };
        assert!(
            mutation_receipt_visibility_binding(
                certified,
                Some(identity(None).replacement().checksum.as_str())
            )
            .is_err()
        );
    }
}
