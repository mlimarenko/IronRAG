use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::{
    AdmissionError, AdmissionFailpoint, AdmissionOutcome, ContentAdmissionBundle,
    ContentAdmissionRequest, ContentAdmissionTarget, NewJob, NewMutation, NewOperation,
    RevisionAdmission, check_failpoint, find_mutation_for_update, fingerprint, idempotency_scope,
    insert_job, insert_mutation, insert_operation, load_job, load_operation_by_subject,
    lock_library,
};
use crate::domains::content::derive_document_role;
use crate::infra::repositories::{catalog_repository, content_repository};

pub async fn admit_content_with_failpoint(
    postgres: &sqlx::PgPool,
    request: &ContentAdmissionRequest,
    failpoint: Option<AdmissionFailpoint>,
) -> Result<AdmissionOutcome<ContentAdmissionBundle>, AdmissionError> {
    let request_fingerprint = content_fingerprint(request)?;
    let scope = idempotency_scope(request.workspace_id, request.requested_by_principal_id);
    let mut transaction = postgres.begin().await?;
    lock_library(&mut transaction, request.workspace_id, request.library_id).await?;

    if let Some(existing) = find_mutation_for_update(
        &mut transaction,
        &scope,
        &request.request_surface,
        request.idempotency_key.as_deref(),
    )
    .await?
    {
        if existing.request_fingerprint.as_deref() != Some(request_fingerprint.as_str()) {
            return Err(AdmissionError::IdempotencyConflict { mutation_id: existing.id });
        }
        let mutation = existing.into_row();
        let (bundle, repaired) =
            load_or_repair_content_bundle(&mut transaction, request, mutation, failpoint).await?;
        transaction.commit().await?;
        return Ok(if repaired {
            AdmissionOutcome::RepairedLegacy(bundle)
        } else {
            AdmissionOutcome::Replayed(bundle)
        });
    }

    let mutation = insert_mutation(
        &mut transaction,
        NewMutation {
            workspace_id: request.workspace_id,
            library_id: request.library_id,
            operation_kind: &request.operation_kind,
            requested_by_principal_id: request.requested_by_principal_id,
            request_surface: &request.request_surface,
            idempotency_key: request.idempotency_key.as_deref(),
            source_identity: request.source_identity.as_deref(),
            idempotency_scope: &scope,
            request_fingerprint: &request_fingerprint,
        },
    )
    .await?;
    check_failpoint(failpoint, AdmissionFailpoint::AfterMutation)?;

    let document = create_or_lock_document(&mut transaction, request).await?;
    check_failpoint(failpoint, AdmissionFailpoint::AfterDocument)?;

    let async_operation = insert_operation(
        &mut transaction,
        NewOperation {
            workspace_id: request.workspace_id,
            library_id: request.library_id,
            operation_kind: "content_mutation",
            surface_kind: &request.request_surface,
            requested_by_principal_id: request.requested_by_principal_id,
            subject_kind: "content_mutation",
            subject_id: mutation.id,
            parent_async_operation_id: request.parent_async_operation_id,
        },
    )
    .await?;
    check_failpoint(failpoint, AdmissionFailpoint::AfterAsyncOperation)?;

    let revision = match request.revision.as_ref() {
        Some(revision) => {
            Some(create_revision_projection(&mut transaction, request, &document, revision).await?)
        }
        None => None,
    };
    check_failpoint(failpoint, AdmissionFailpoint::AfterRevision)?;

    let item = insert_mutation_item(
        &mut transaction,
        mutation.id,
        document.id,
        revision.as_ref().and_then(|revision| revision.parent_revision_id),
        revision.as_ref().map(|revision| revision.id),
        if revision.is_some() { "pending" } else { "applied" },
    )
    .await?;
    check_failpoint(failpoint, AdmissionFailpoint::AfterMutationItem)?;

    let job = if let Some(revision) = revision.as_ref() {
        let dedupe_key = format!("content-mutation:{}:{}", mutation.id, item.id);
        let job = insert_job(
            &mut transaction,
            NewJob {
                workspace_id: request.workspace_id,
                library_id: request.library_id,
                mutation_id: mutation.id,
                mutation_item_id: Some(item.id),
                async_operation_id: async_operation.id,
                knowledge_document_id: Some(document.id),
                knowledge_revision_id: Some(revision.id),
                job_kind: "content_mutation",
                priority: request.priority,
                dedupe_key: &dedupe_key,
            },
        )
        .await?;
        Some(job)
    } else {
        settle_metadata_only_admission(&mut transaction, mutation.id, async_operation.id).await?;
        None
    };
    check_failpoint(failpoint, AdmissionFailpoint::AfterJob)?;

    promote_pending_head(&mut transaction, document.id, mutation.id).await?;
    check_failpoint(failpoint, AdmissionFailpoint::AfterHead)?;
    catalog_repository::touch_library_source_truth_version_with_executor(
        &mut *transaction,
        request.library_id,
    )
    .await?;

    let bundle = ContentAdmissionBundle {
        mutation,
        document,
        revision,
        item: Some(item),
        job,
        async_operation,
    };
    if !bundle.is_complete() {
        return Err(AdmissionError::InvariantViolation(
            "new content admission is not internally complete",
        ));
    }
    check_failpoint(failpoint, AdmissionFailpoint::BeforeCommit)?;
    transaction.commit().await?;
    Ok(AdmissionOutcome::Created(bundle))
}

fn content_fingerprint(request: &ContentAdmissionRequest) -> Result<String, AdmissionError> {
    let target = match &request.target {
        ContentAdmissionTarget::New { external_key, file_name, parent_external_key } => {
            serde_json::json!({
                "kind": "new",
                "externalKey": external_key,
                "fileName": file_name,
                "parentExternalKey": parent_external_key,
            })
        }
        ContentAdmissionTarget::Existing { document_id } => serde_json::json!({
            "kind": "existing",
            "documentId": document_id,
        }),
    };
    let revision = request.revision.as_ref().map(|revision| {
        serde_json::json!({
            "contentSourceKind": revision.content_source_kind,
            "checksum": revision.checksum,
            "mimeType": revision.mime_type,
            "byteSize": revision.byte_size,
            "title": revision.title,
            "languageCode": revision.language_code,
            "sourceUri": revision.source_uri,
            "documentHint": revision.document_hint,
            "storageKey": revision.storage_key,
        })
    });
    fingerprint(&serde_json::json!({
        "workspaceId": request.workspace_id,
        "libraryId": request.library_id,
        "operationKind": request.operation_kind,
        "requestSurface": request.request_surface,
        "sourceIdentity": request.source_identity,
        "target": target,
        "revision": revision,
    }))
}

async fn create_or_lock_document(
    transaction: &mut Transaction<'_, Postgres>,
    request: &ContentAdmissionRequest,
) -> Result<content_repository::ContentDocumentRow, AdmissionError> {
    match &request.target {
        ContentAdmissionTarget::Existing { document_id } => {
            let document = content_repository::lock_document_by_id_with_executor(
                &mut **transaction,
                *document_id,
            )
            .await?
            .ok_or(AdmissionError::TargetDocumentNotFound { document_id: *document_id })?;
            if document.workspace_id != request.workspace_id
                || document.library_id != request.library_id
            {
                return Err(AdmissionError::TargetDocumentScopeConflict {
                    document_id: *document_id,
                });
            }
            if document.deleted_at.is_some() || document.document_state == "deleted" {
                return Err(AdmissionError::TargetDocumentDeleted { document_id: *document_id });
            }
            let active_mutation = sqlx::query_scalar::<_, Uuid>(
                "select mutation.id
                 from content_document_head as head
                 join content_mutation as mutation on mutation.id = head.latest_mutation_id
                 where head.document_id = $1
                   and mutation.mutation_state in ('accepted', 'running')
                 limit 1",
            )
            .bind(document.id)
            .fetch_optional(&mut **transaction)
            .await?;
            if active_mutation.is_some() {
                return Err(AdmissionError::ConflictingActiveMutation { document_id: document.id });
            }
            Ok(document)
        }
        ContentAdmissionTarget::New { external_key, file_name, parent_external_key } => {
            let external_key = external_key
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map_or_else(|| Uuid::now_v7().to_string(), ToOwned::to_owned);
            let parent_external_key =
                parent_external_key.as_deref().map(str::trim).filter(|value| !value.is_empty());
            let parent_document_id = if let Some(parent_external_key) = parent_external_key {
                sqlx::query_scalar::<_, Uuid>(
                    "select id
                     from content_document
                     where workspace_id = $1 and library_id = $2 and external_key = $3
                       and document_state <> 'deleted' and deleted_at is null
                     order by created_at asc, id asc
                     limit 1",
                )
                .bind(request.workspace_id)
                .bind(request.library_id)
                .bind(parent_external_key)
                .fetch_optional(&mut **transaction)
                .await?
            } else {
                None
            };
            let role = derive_document_role(parent_external_key.is_some(), false);
            // A failed statement aborts the whole Postgres transaction (every
            // later command errors with 25P02 "current transaction is
            // aborted") until it is rolled back to a savepoint — so the
            // existing-document lookup below, run on this same transaction
            // after a unique-violation, needs the abort cleared first.
            sqlx::query("SAVEPOINT document_insert").execute(&mut **transaction).await?;
            let document = match content_repository::create_document_with_executor(
                &mut **transaction,
                &content_repository::NewContentDocument {
                    workspace_id: request.workspace_id,
                    library_id: request.library_id,
                    external_key: &external_key,
                    document_state: "active",
                    created_by_principal_id: request.requested_by_principal_id,
                    parent_external_key,
                    parent_document_id,
                    document_role: role,
                },
            )
            .await
            {
                Ok(document) => document,
                Err(sqlx::Error::Database(database_error))
                    if database_error.is_unique_violation()
                        && database_error.constraint()
                            == Some("content_document_library_id_external_key_active_idx") =>
                {
                    sqlx::query("ROLLBACK TO SAVEPOINT document_insert")
                        .execute(&mut **transaction)
                        .await?;
                    let existing_document_id = sqlx::query_scalar::<_, Uuid>(
                        "select id
                         from content_document
                         where library_id = $1 and external_key = $2
                           and document_state = 'active'
                         limit 1",
                    )
                    .bind(request.library_id)
                    .bind(&external_key)
                    .fetch_one(&mut **transaction)
                    .await?;
                    return Err(AdmissionError::DuplicateExternalKey { existing_document_id });
                }
                Err(error) => return Err(error.into()),
            };
            let source_file_name = content_repository::canonical_file_name_component(
                file_name.as_deref().unwrap_or(&external_key),
            );
            sqlx::query("update content_document set source_file_name = $2 where id = $1")
                .bind(document.id)
                .bind(&source_file_name)
                .execute(&mut **transaction)
                .await?;
            content_repository::upsert_document_head_without_generation_with_executor(
                &mut **transaction,
                &content_repository::NewContentDocumentHead {
                    document_id: document.id,
                    active_revision_id: None,
                    readable_revision_id: None,
                    latest_mutation_id: None,
                    latest_successful_attempt_id: None,
                },
            )
            .await?;
            sqlx::query(
                "insert into knowledge_document (
                    document_id, workspace_id, library_id, external_key, file_name, title,
                    document_state, active_revision_id, readable_revision_id,
                    latest_revision_no, parent_document_id, document_role,
                    created_at, updated_at, deleted_at
                 ) values ($1, $2, $3, $4, $5, null, $6, null, null, null, $7, $8, $9, $9, null)",
            )
            .bind(document.id)
            .bind(document.workspace_id)
            .bind(document.library_id)
            .bind(&document.external_key)
            .bind(&source_file_name)
            .bind(&document.document_state)
            .bind(document.parent_document_id)
            .bind(&document.document_role)
            .bind(document.created_at)
            .execute(&mut **transaction)
            .await?;
            Ok(document)
        }
    }
}

async fn create_revision_projection(
    transaction: &mut Transaction<'_, Postgres>,
    request: &ContentAdmissionRequest,
    document: &content_repository::ContentDocumentRow,
    input: &RevisionAdmission,
) -> Result<content_repository::ContentRevisionRow, AdmissionError> {
    let latest = sqlx::query_as::<_, (Uuid, i32)>(
        "select id, revision_number
         from content_revision
         where document_id = $1
         order by revision_number desc, created_at desc, id desc
         limit 1",
    )
    .bind(document.id)
    .fetch_optional(&mut **transaction)
    .await?;
    let revision_number = latest.as_ref().map_or(1, |(_, number)| number.saturating_add(1));
    let revision = content_repository::create_revision_with_executor(
        &mut **transaction,
        &content_repository::NewContentRevision {
            document_id: document.id,
            workspace_id: request.workspace_id,
            library_id: request.library_id,
            revision_number,
            parent_revision_id: latest.map(|(id, _)| id),
            content_source_kind: &input.content_source_kind,
            checksum: &input.checksum,
            mime_type: &input.mime_type,
            byte_size: input.byte_size,
            title: input.title.as_deref(),
            language_code: input.language_code.as_deref(),
            source_uri: input.source_uri.as_deref(),
            document_hint: input.document_hint.as_deref(),
            storage_key: input.storage_key.as_deref(),
            created_by_principal_id: request.requested_by_principal_id,
        },
    )
    .await?;
    sqlx::query(
        "insert into knowledge_revision (
            revision_id, workspace_id, library_id, document_id, revision_number,
            revision_state, revision_kind, storage_ref, source_uri, document_hint,
            mime_type, checksum, title, byte_size, normalized_text, text_checksum,
            image_checksum, text_state, vector_state, graph_state, text_readable_at,
            vector_ready_at, graph_ready_at, superseded_by_revision_id, created_at
         ) values (
            $1, $2, $3, $4, $5, 'accepted', $6, $7, $8, $9, $10, $11, $12, $13,
            null, null, null, 'accepted', 'accepted', 'accepted', null, null, null, null, $14
         )",
    )
    .bind(revision.id)
    .bind(revision.workspace_id)
    .bind(revision.library_id)
    .bind(revision.document_id)
    .bind(i64::from(revision.revision_number))
    .bind(&revision.content_source_kind)
    .bind(&revision.storage_key)
    .bind(&revision.source_uri)
    .bind(&revision.document_hint)
    .bind(&revision.mime_type)
    .bind(&revision.checksum)
    .bind(&revision.title)
    .bind(revision.byte_size)
    .bind(revision.created_at)
    .execute(&mut **transaction)
    .await?;
    Ok(revision)
}

async fn insert_mutation_item(
    transaction: &mut Transaction<'_, Postgres>,
    mutation_id: Uuid,
    document_id: Uuid,
    base_revision_id: Option<Uuid>,
    result_revision_id: Option<Uuid>,
    item_state: &str,
) -> Result<content_repository::ContentMutationItemRow, AdmissionError> {
    Ok(sqlx::query_as::<_, content_repository::ContentMutationItemRow>(
        "insert into content_mutation_item (
            id, mutation_id, document_id, base_revision_id,
            result_revision_id, item_state, message
         ) values ($1, $2, $3, $4, $5, $6::content_mutation_item_state, $7)
         returning id, mutation_id, document_id, base_revision_id, result_revision_id,
                   item_state::text as item_state, message",
    )
    .bind(Uuid::now_v7())
    .bind(mutation_id)
    .bind(document_id)
    .bind(base_revision_id)
    .bind(result_revision_id)
    .bind(item_state)
    .bind(if result_revision_id.is_some() {
        "revision accepted and queued for ingest"
    } else {
        "metadata-only admission applied"
    })
    .fetch_one(&mut **transaction)
    .await?)
}

async fn promote_pending_head(
    transaction: &mut Transaction<'_, Postgres>,
    document_id: Uuid,
    mutation_id: Uuid,
) -> Result<(), AdmissionError> {
    let head = content_repository::get_document_head_for_update_with_executor(
        &mut **transaction,
        document_id,
    )
    .await?
    .ok_or(AdmissionError::InvariantViolation("document head is missing"))?;
    let outcome = content_repository::upsert_document_head_without_generation_outcome(
        transaction,
        &content_repository::NewContentDocumentHead {
            document_id,
            active_revision_id: head.active_revision_id,
            readable_revision_id: head.readable_revision_id,
            latest_mutation_id: Some(mutation_id),
            latest_successful_attempt_id: head.latest_successful_attempt_id,
        },
    )
    .await?;
    match outcome {
        content_repository::ValidatedDocumentHeadWriteOutcome::Updated(_) => Ok(()),
        content_repository::ValidatedDocumentHeadWriteOutcome::DocumentNotFound => {
            Err(AdmissionError::TargetDocumentNotFound { document_id })
        }
        content_repository::ValidatedDocumentHeadWriteOutcome::ReferenceIntegrityViolation => {
            Err(AdmissionError::TargetDocumentHeadIntegrity { document_id })
        }
    }
}

async fn settle_metadata_only_admission(
    transaction: &mut Transaction<'_, Postgres>,
    mutation_id: Uuid,
    operation_id: Uuid,
) -> Result<(), AdmissionError> {
    sqlx::query(
        "update content_mutation
         set mutation_state = 'applied', completed_at = now()
         where id = $1",
    )
    .bind(mutation_id)
    .execute(&mut **transaction)
    .await?;
    sqlx::query(
        "update ops_async_operation
         set status = 'ready', completed_at = now()
         where id = $1",
    )
    .bind(operation_id)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn load_or_repair_content_bundle(
    transaction: &mut Transaction<'_, Postgres>,
    request: &ContentAdmissionRequest,
    mutation: content_repository::ContentMutationRow,
    failpoint: Option<AdmissionFailpoint>,
) -> Result<(ContentAdmissionBundle, bool), AdmissionError> {
    let mut items = sqlx::query_as::<_, content_repository::ContentMutationItemRow>(
        "select id, mutation_id, document_id, base_revision_id, result_revision_id,
                item_state::text as item_state, message
         from content_mutation_item
         where mutation_id = $1
         order by id asc
         for update",
    )
    .bind(mutation.id)
    .fetch_all(&mut **transaction)
    .await?;
    if items.len() != 1 {
        return Err(AdmissionError::IncompleteLegacy {
            mutation_id: mutation.id,
            reason: "expected exactly one mutation item",
        });
    }
    let item = items.pop().ok_or(AdmissionError::IncompleteLegacy {
        mutation_id: mutation.id,
        reason: "mutation item disappeared while locked",
    })?;
    let document_id = item.document_id.ok_or(AdmissionError::IncompleteLegacy {
        mutation_id: mutation.id,
        reason: "mutation item has no document",
    })?;
    let document =
        content_repository::lock_document_by_id_with_executor(&mut **transaction, document_id)
            .await?
            .ok_or(AdmissionError::IncompleteLegacy {
                mutation_id: mutation.id,
                reason: "linked document is missing",
            })?;
    let operation = load_operation_by_subject(transaction, "content_mutation", mutation.id)
        .await?
        .ok_or(AdmissionError::IncompleteLegacy {
            mutation_id: mutation.id,
            reason: "linked async operation is missing",
        })?;
    let revision = match item.result_revision_id {
        Some(revision_id) => Some(
            sqlx::query_as::<_, content_repository::ContentRevisionRow>(
                "select id, document_id, workspace_id, library_id, revision_number,
                        parent_revision_id, content_source_kind::text as content_source_kind,
                        checksum, mime_type, byte_size, title, language_code, source_uri,
                        document_hint, storage_key, created_by_principal_id, created_at
                 from content_revision where id = $1 for update",
            )
            .bind(revision_id)
            .fetch_optional(&mut **transaction)
            .await?
            .ok_or(AdmissionError::IncompleteLegacy {
                mutation_id: mutation.id,
                reason: "linked revision is missing",
            })?,
        ),
        None => None,
    };
    let mut job = load_job(transaction, mutation.id, "content_mutation").await?;
    let repaired = if job.is_none() {
        if let Some(revision) = revision.as_ref() {
            let dedupe_key = format!("content-mutation:{}:{}", mutation.id, item.id);
            job = Some(
                insert_job(
                    transaction,
                    NewJob {
                        workspace_id: request.workspace_id,
                        library_id: request.library_id,
                        mutation_id: mutation.id,
                        mutation_item_id: Some(item.id),
                        async_operation_id: operation.id,
                        knowledge_document_id: Some(document.id),
                        knowledge_revision_id: Some(revision.id),
                        job_kind: "content_mutation",
                        priority: request.priority,
                        dedupe_key: &dedupe_key,
                    },
                )
                .await?,
            );
            check_failpoint(failpoint, AdmissionFailpoint::AfterJob)?;
            true
        } else {
            false
        }
    } else {
        false
    };
    let bundle = ContentAdmissionBundle {
        mutation,
        document,
        revision,
        item: Some(item),
        job,
        async_operation: operation,
    };
    if !bundle.is_complete() {
        return Err(AdmissionError::IncompleteLegacy {
            mutation_id: bundle.mutation.id,
            reason: "linked rows disagree on admission identity",
        });
    }
    Ok((bundle, repaired))
}
