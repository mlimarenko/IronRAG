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
use crate::infra::repositories::{
    catalog_repository, content_document_head_repository, content_document_repository,
    content_knowledge_projection_repository, content_mutation_repository,
    content_revision_repository,
};

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct VisibilityRequestIdentity<'a> {
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    requested_by_principal_id: Option<Uuid>,
    request_surface: &'a str,
    operation_kind: &'static str,
    source_identity: &'a str,
    materialization_token: &'a Option<String>,
    replacement: &'a ironrag_contracts::documents::ReplacementIdentity,
}

/// Admits a complete independent visibility bundle without claiming payload ownership.
/// Replay returns the original item/floor and never clears a newer publication.
///
/// # Errors
/// Rejects invalid descriptors, scope/deletion/idempotency conflicts and incomplete
/// original bundles. Injected failures roll back every visibility effect.
pub async fn admit_visibility_intent_with_failpoint(
    postgres: &sqlx::PgPool,
    request: &super::VisibilityAdmissionRequest,
    failpoint: Option<AdmissionFailpoint>,
) -> Result<AdmissionOutcome<super::VisibilityAdmissionBundle>, AdmissionError> {
    let intent = request.intent.clone().normalize()?;
    let identity = serde_json::to_value(VisibilityRequestIdentity {
        workspace_id: request.workspace_id,
        library_id: request.library_id,
        document_id: request.document_id,
        requested_by_principal_id: request.requested_by_principal_id,
        request_surface: &request.request_surface,
        operation_kind: "visibility",
        source_identity: &intent.source_identity,
        materialization_token: &intent.materialization_token,
        replacement: &intent.replacement,
    })?;
    let digest = fingerprint(&identity)?;
    let scope = idempotency_scope(request.workspace_id, request.requested_by_principal_id);
    let mut transaction = postgres.begin().await?;
    lock_library(&mut transaction, request.workspace_id, request.library_id).await?;
    // Compare immutable identity before taking any foreign mutation/document lock.
    let existing = find_mutation_for_update(
        &mut transaction,
        super::MutationKey {
            workspace_id: request.workspace_id,
            principal_id: request.requested_by_principal_id,
            scope: &scope,
            surface: &request.request_surface,
            key: Some(&intent.idempotency_key),
        },
    )
    .await?;
    if let Some(stored) = existing.as_ref() {
        let stored_identity: serde_json::Value =
            sqlx::query_scalar("select request_identity from content_mutation where id=$1")
                .bind(stored.id)
                .fetch_one(&mut *transaction)
                .await?;
        if stored_identity != identity {
            return Err(AdmissionError::IdempotencyConflict { mutation_id: stored.id });
        }
    }
    let replayed = existing.is_some();
    let document = lock_visibility_document(&mut transaction, request).await?;
    let mut mutation = if let Some(existing) = existing {
        existing.into_row()
    } else {
        create_visibility_effect(
            &mut transaction,
            request,
            &intent,
            AdmissionIdentity { scope: &scope, digest: &digest, identity: &identity },
            failpoint,
        )
        .await?
    };
    if mutation.request_fingerprint.as_deref() != Some(&digest)
        || mutation.operation_kind != "visibility"
    {
        return Err(AdmissionError::IdempotencyConflict { mutation_id: mutation.id });
    }
    let items: Vec<(Uuid, i32)> = sqlx::query_as(
        "select i.id, v.revision_floor from content_mutation_item i
         join content_visibility_intent v on v.mutation_item_id = i.id and v.document_id = i.document_id
         where i.mutation_id = $1 and i.document_id = $2 and i.item_state = 'applied'
           and i.base_revision_id is null and i.result_revision_id is null and i.visibility_intent_id is null
           and not exists (select 1 from content_mutation_item other where other.mutation_id = $1 and other.id <> i.id)",
    ).bind(mutation.id).bind(document.id).fetch_all(&mut *transaction).await?;
    let [(item_id, floor)] = items.as_slice() else {
        return Err(AdmissionError::InvariantViolation("visibility must own exactly one item"));
    };
    let item = sqlx::query_as::<_, content_mutation_repository::ContentMutationItemRow>(
        "select id, mutation_id, document_id, base_revision_id, result_revision_id, item_state::text as item_state, message
         from content_mutation_item where id = $1 for update",
    ).bind(item_id).fetch_one(&mut *transaction).await?;
    let operation = load_operation_by_subject(&mut transaction, "content_mutation", mutation.id)
        .await?
        .ok_or(AdmissionError::InvariantViolation("visibility operation missing"))?;
    let has_job: bool =
        sqlx::query_scalar("select exists (select 1 from ingest_job where mutation_id = $1)")
            .bind(mutation.id)
            .fetch_one(&mut *transaction)
            .await?;
    validate_visibility_bundle_state(request, &mutation, &operation, has_job)?;
    mutation.completed_at = operation.completed_at;
    let receipt = ironrag_contracts::documents::VisibilityIntentReceipt {
        visibility_intent_id: item.id,
        mutation_id: mutation.id,
        mutation_item_id: item.id,
        async_operation_id: operation.id,
        document_id: document.id,
        revision_floor: *floor,
        source_identity: intent.source_identity,
        materialization_token: intent.materialization_token,
        replacement: intent.replacement,
        request_fingerprint: digest,
    };
    let bundle = super::VisibilityAdmissionBundle {
        mutation,
        document,
        item,
        async_operation: operation,
        receipt,
    };
    check_failpoint(failpoint, AdmissionFailpoint::BeforeCommit)?;
    transaction.commit().await?;
    Ok(if replayed {
        AdmissionOutcome::Replayed(bundle)
    } else {
        AdmissionOutcome::Created(bundle)
    })
}

struct AdmissionIdentity<'a> {
    scope: &'a str,
    digest: &'a str,
    identity: &'a serde_json::Value,
}

/// Writes the independent denial effect after its exact document/head were locked.
/// This phase never writes the foreign payload owner or a job/attempt row.
async fn create_visibility_effect(
    transaction: &mut Transaction<'_, Postgres>,
    request: &super::VisibilityAdmissionRequest,
    intent: &ironrag_contracts::documents::CreateVisibilityIntentRequest,
    binding: AdmissionIdentity<'_>,
    failpoint: Option<AdmissionFailpoint>,
) -> Result<content_mutation_repository::ContentMutationRow, AdmissionError> {
    let mut mutation = insert_mutation(
        transaction,
        NewMutation {
            workspace_id: request.workspace_id,
            library_id: request.library_id,
            operation_kind: "visibility",
            requested_by_principal_id: request.requested_by_principal_id,
            request_surface: &request.request_surface,
            idempotency_key: Some(&intent.idempotency_key),
            source_identity: Some(&intent.source_identity),
            idempotency_scope: binding.scope,
            request_fingerprint: binding.digest,
            request_identity: binding.identity,
        },
    )
    .await?;
    check_failpoint(failpoint, AdmissionFailpoint::AfterMutation)?;
    let operation = insert_operation(
        transaction,
        NewOperation {
            workspace_id: request.workspace_id,
            library_id: request.library_id,
            operation_kind: "content_mutation",
            surface_kind: &request.request_surface,
            requested_by_principal_id: request.requested_by_principal_id,
            subject_kind: "content_mutation",
            subject_id: mutation.id,
            parent_async_operation_id: None,
        },
    )
    .await?;
    check_failpoint(failpoint, AdmissionFailpoint::AfterAsyncOperation)?;
    let item =
        insert_mutation_item(transaction, mutation.id, request.document_id, None, None, "applied")
            .await?;
    check_failpoint(failpoint, AdmissionFailpoint::AfterMutationItem)?;
    sqlx::query(
        "insert into content_visibility_intent (mutation_item_id, document_id, revision_floor)
         select $1, id, revision_sequence from content_document where id = $2",
    )
    .bind(item.id)
    .bind(request.document_id)
    .execute(&mut **transaction)
    .await?;
    sqlx::query(
        "update content_document_head set visibility_intent_id = $2, active_revision_id = null,
         readable_revision_id = null, document_summary = null,
         head_updated_at = now() where document_id = $1",
    )
    .bind(request.document_id)
    .bind(item.id)
    .execute(&mut **transaction)
    .await?;
    sqlx::query(
        "update knowledge_document set active_revision_id = null, readable_revision_id = null,
         title = null, file_name = document_id::text where document_id = $1",
    )
    .bind(request.document_id)
    .execute(&mut **transaction)
    .await?;
    settle_metadata_only_admission(transaction, mutation.id, operation.id).await?;
    catalog_repository::touch_library_source_truth_version_with_executor(
        &mut **transaction,
        request.library_id,
    )
    .await?;
    check_failpoint(failpoint, AdmissionFailpoint::AfterHead)?;
    mutation.mutation_state = "applied".to_owned();
    Ok(mutation)
}

fn validate_visibility_bundle_state(
    request: &super::VisibilityAdmissionRequest,
    mutation: &content_mutation_repository::ContentMutationRow,
    operation: &super::ops_repository::OpsAsyncOperationRow,
    has_job: bool,
) -> Result<(), AdmissionError> {
    if has_job
        || mutation.mutation_state != "applied"
        || operation.status != "ready"
        || operation.completed_at.is_none()
        || operation.workspace_id != request.workspace_id
        || operation.library_id != Some(request.library_id)
    {
        return Err(AdmissionError::InvariantViolation("visibility bundle is incomplete"));
    }
    Ok(())
}

/// Locks the exact intent owner, without inspecting or claiming its payload mutation.
async fn lock_visibility_document(
    transaction: &mut Transaction<'_, Postgres>,
    request: &super::VisibilityAdmissionRequest,
) -> Result<content_document_repository::ContentDocumentRow, AdmissionError> {
    let document = content_document_repository::lock_document_by_id_with_executor(
        &mut **transaction,
        request.document_id,
    )
    .await?
    .ok_or(AdmissionError::TargetDocumentNotFound { document_id: request.document_id })?;
    if (document.workspace_id, document.library_id) != (request.workspace_id, request.library_id) {
        return Err(AdmissionError::TargetDocumentScopeConflict { document_id: document.id });
    }
    if document.deleted_at.is_some() || document.document_state == "deleted" {
        return Err(AdmissionError::TargetDocumentDeleted { document_id: document.id });
    }
    content_document_head_repository::get_document_head_for_update_with_executor(
        &mut **transaction,
        document.id,
    )
    .await?
    .ok_or(AdmissionError::TargetDocumentHeadIntegrity { document_id: document.id })?;
    Ok(document)
}

pub async fn admit_content_with_failpoint(
    postgres: &sqlx::PgPool,
    request: &ContentAdmissionRequest,
    failpoint: Option<AdmissionFailpoint>,
) -> Result<AdmissionOutcome<ContentAdmissionBundle>, AdmissionError> {
    let mut transaction = postgres.begin().await?;
    let outcome = admit_content_in_transaction(&mut transaction, request, failpoint).await?;
    transaction.commit().await?;
    Ok(outcome)
}

/// Only the guarded upload service calls this after persisting these actual bytes.
/// JSON source metadata alone never authorizes completion of an unowned document.
pub(crate) async fn admit_staged_upload_with_failpoint(
    postgres: &sqlx::PgPool,
    request: &ContentAdmissionRequest,
    bytes: &[u8],
    failpoint: Option<AdmissionFailpoint>,
) -> Result<AdmissionOutcome<ContentAdmissionBundle>, AdmissionError> {
    use sha2::{Digest as _, Sha256};
    let source = request
        .revision
        .as_ref()
        .ok_or(AdmissionError::InvariantViolation("staged upload requires a source"))?;
    if source.content_source_kind != "upload"
        || source.storage_key.as_deref().is_none_or(str::is_empty)
        || source.replacement.checksum != format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
        || usize::try_from(source.replacement.byte_size).ok() != Some(bytes.len())
    {
        return Err(AdmissionError::InvariantViolation("staged upload identity mismatch"));
    }
    let mut transaction = postgres.begin().await?;
    let outcome = admit_content_transaction(&mut transaction, request, failpoint, true).await?;
    transaction.commit().await?;
    Ok(outcome)
}

/// The canonical admission flow with commit ownership retained by its caller.
/// Web capture locks the immutable parent in this transaction before admitting
/// its independently owned page; it does not duplicate content writes.
pub(super) async fn admit_content_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    request: &ContentAdmissionRequest,
    failpoint: Option<AdmissionFailpoint>,
) -> Result<AdmissionOutcome<ContentAdmissionBundle>, AdmissionError> {
    admit_content_transaction(transaction, request, failpoint, false).await
}

async fn admit_content_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    request: &ContentAdmissionRequest,
    failpoint: Option<AdmissionFailpoint>,
    staged_upload: bool,
) -> Result<AdmissionOutcome<ContentAdmissionBundle>, AdmissionError> {
    let mut normalized = request.clone();
    if let Some(revision) = normalized.revision.as_mut() {
        revision.replacement = revision.replacement.clone().normalize()?;
    }
    let request = &normalized;
    let scope = idempotency_scope(request.workspace_id, request.requested_by_principal_id);
    lock_library(transaction, request.workspace_id, request.library_id).await?;
    let existing_owner = find_mutation_for_update(
        transaction,
        super::MutationKey {
            workspace_id: request.workspace_id,
            principal_id: request.requested_by_principal_id,
            scope: &scope,
            surface: &request.request_surface,
            key: request.idempotency_key.as_deref(),
        },
    )
    .await?;
    let intent_identity = load_payload_intent_identity(transaction, request).await?;
    if let Some(identity) = intent_identity.as_ref()
        && request
            .source_identity
            .as_deref()
            .is_some_and(|source| Some(source) != identity["sourceIdentity"].as_str())
    {
        let ContentAdmissionTarget::Existing { document_id } = request.target else {
            return Err(AdmissionError::InvariantViolation(
                "bound identity has no existing document",
            ));
        };
        return Err(AdmissionError::VisibilityBindingConflict { document_id });
    }
    let mut request_identity = content_request_identity(request);
    if let Some(identity) = intent_identity.as_ref() {
        request_identity["sourceIdentity"] = identity["sourceIdentity"].clone();
        request_identity["materializationToken"] = identity["materializationToken"].clone();
    }
    let request_fingerprint = fingerprint(&request_identity)?;

    if let Some(existing) = existing_owner {
        if existing.request_fingerprint.as_deref() != Some(request_fingerprint.as_str()) {
            return Err(AdmissionError::IdempotencyConflict { mutation_id: existing.id });
        }
        let mutation = existing.into_row();
        let bundle = load_content_bundle(transaction, mutation).await?;
        return Ok(AdmissionOutcome::Replayed(bundle));
    }

    if staged_upload {
        reject_duplicate_staged_upload(transaction, request).await?;
    }
    let bundle = create_content_admission(
        transaction,
        request,
        failpoint,
        AdmissionIdentity {
            scope: &scope,
            digest: &request_fingerprint,
            identity: &request_identity,
        },
        staged_upload,
    )
    .await?;
    check_failpoint(failpoint, AdmissionFailpoint::BeforeCommit)?;
    Ok(AdmissionOutcome::Created(bundle))
}

/// Writes one new content admission on an already-scoped transaction: the
/// mutation, document, operation, revision projection, item, job and pending
/// head in the order the failpoints observe. The caller owns the commit.
async fn create_content_admission(
    transaction: &mut Transaction<'_, Postgres>,
    request: &ContentAdmissionRequest,
    failpoint: Option<AdmissionFailpoint>,
    binding: AdmissionIdentity<'_>,
    staged_upload: bool,
) -> Result<ContentAdmissionBundle, AdmissionError> {
    let document = create_or_lock_document(transaction, request, staged_upload).await?;
    validate_current_payload_binding(transaction, request, document.id).await?;
    if request.operation_kind != "delete" {
        ensure_no_active_payload_mutation(transaction, document.id).await?;
    }
    let mutation = insert_mutation(
        transaction,
        NewMutation {
            workspace_id: request.workspace_id,
            library_id: request.library_id,
            operation_kind: &request.operation_kind,
            requested_by_principal_id: request.requested_by_principal_id,
            request_surface: &request.request_surface,
            idempotency_key: request.idempotency_key.as_deref(),
            source_identity: binding.identity["sourceIdentity"].as_str(),
            idempotency_scope: binding.scope,
            request_fingerprint: binding.digest,
            request_identity: binding.identity,
        },
    )
    .await?;
    check_failpoint(failpoint, AdmissionFailpoint::AfterMutation)?;

    check_failpoint(failpoint, AdmissionFailpoint::AfterDocument)?;

    let async_operation = insert_operation(
        transaction,
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
        Some(revision) => Some(
            create_revision_projection(transaction, request, &document, revision, binding.identity)
                .await?,
        ),
        None => None,
    };
    check_failpoint(failpoint, AdmissionFailpoint::AfterRevision)?;

    let item = insert_mutation_item(
        transaction,
        mutation.id,
        document.id,
        revision.as_ref(),
        request.revision.as_ref().and_then(|revision| revision.visibility_intent_id),
        if revision.is_some() || request.operation_kind == "delete" {
            "pending"
        } else {
            "applied"
        },
    )
    .await?;
    check_failpoint(failpoint, AdmissionFailpoint::AfterMutationItem)?;

    let job = queue_content_admission_payload(
        transaction,
        request,
        mutation.id,
        item.id,
        async_operation.id,
        revision.as_ref(),
    )
    .await?;
    check_failpoint(failpoint, AdmissionFailpoint::AfterJob)?;

    promote_pending_head(transaction, document.id, mutation.id).await?;
    check_failpoint(failpoint, AdmissionFailpoint::AfterHead)?;
    catalog_repository::touch_library_source_truth_version_with_executor(
        &mut **transaction,
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
    Ok(bundle)
}

/// Queues only admitted payload work. Payloadless metadata settles immediately;
/// DELETE owns its separate lifecycle finalization rather than an ingest job.
async fn queue_content_admission_payload(
    transaction: &mut Transaction<'_, Postgres>,
    request: &ContentAdmissionRequest,
    mutation_id: Uuid,
    item_id: Uuid,
    operation_id: Uuid,
    revision: Option<&content_revision_repository::ContentRevisionRow>,
) -> Result<Option<super::AdmissionIngestJobRow>, AdmissionError> {
    if let Some(revision) = revision {
        let dedupe_key = format!("content-mutation:{mutation_id}:{item_id}");
        let job = insert_job(
            transaction,
            NewJob {
                workspace_id: request.workspace_id,
                library_id: request.library_id,
                mutation_id,
                mutation_item_id: Some(item_id),
                async_operation_id: operation_id,
                knowledge_document_id: Some(revision.document_id),
                knowledge_revision_id: Some(revision.id),
                job_kind: "content_mutation",
                priority: request.priority,
                dedupe_key: &dedupe_key,
            },
        )
        .await?;
        Ok(Some(job))
    } else {
        if request.operation_kind != "delete" {
            settle_metadata_only_admission(transaction, mutation_id, operation_id).await?;
        }
        Ok(None)
    }
}

#[cfg(test)]
fn content_fingerprint(request: &ContentAdmissionRequest) -> Result<String, AdmissionError> {
    fingerprint(&content_request_identity(request))
}

fn content_request_identity(request: &ContentAdmissionRequest) -> serde_json::Value {
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
            "visibilityIntentId": revision.visibility_intent_id,
            "baseRevisionId": revision.base_revision_id,
        })
    });
    serde_json::json!({
        "workspaceId": request.workspace_id,
        "libraryId": request.library_id,
        "operationKind": request.operation_kind,
        "sourceIdentity": request.source_identity,
        "materializationToken": null,
        "target": target,
        "revision": revision,
        "replacement": request.revision.as_ref().map(|revision| &revision.replacement),
    })
}

/// Locks the authenticated target before a replay lookup and reads binding only
/// from its immutable intent record. A stale exact replay returns its original
/// bundle; new submissions must separately prove the current head obligation.
async fn load_payload_intent_identity(
    transaction: &mut Transaction<'_, Postgres>,
    request: &ContentAdmissionRequest,
) -> Result<Option<serde_json::Value>, AdmissionError> {
    let intent_id = request.revision.as_ref().and_then(|revision| revision.visibility_intent_id);
    let ContentAdmissionTarget::Existing { document_id } = request.target else {
        if intent_id.is_some() {
            return Err(AdmissionError::InvariantViolation(
                "new document cannot bind another document's intent",
            ));
        }
        return Ok(None);
    };
    let document = content_document_repository::lock_document_by_id_with_executor(
        &mut **transaction,
        document_id,
    )
    .await?
    .ok_or(AdmissionError::TargetDocumentNotFound { document_id })?;
    if (document.workspace_id, document.library_id) != (request.workspace_id, request.library_id) {
        return Err(AdmissionError::TargetDocumentScopeConflict { document_id });
    }
    if request.operation_kind != "delete"
        && (document.document_state == "deleted" || document.deleted_at.is_some())
    {
        return Err(AdmissionError::TargetDocumentDeleted { document_id });
    }
    content_document_head_repository::get_document_head_for_update_with_executor(
        &mut **transaction,
        document_id,
    )
    .await?
    .ok_or(AdmissionError::TargetDocumentHeadIntegrity { document_id })?;
    let integrity: bool = sqlx::query_scalar(
        "select not exists (select 1 from content_document_head h where h.document_id=$1 and (
            (h.active_revision_id is not null and not exists (select 1 from content_revision r
                where r.id=h.active_revision_id and r.document_id=$1 and r.workspace_id=$2 and r.library_id=$3))
            or (h.readable_revision_id is not null and not exists (select 1 from content_revision r
                where r.id=h.readable_revision_id and r.document_id=$1 and r.workspace_id=$2 and r.library_id=$3
                  and r.admission_identity is not null))
        ))",
    ).bind(document_id).bind(request.workspace_id).bind(request.library_id)
        .fetch_one(&mut **transaction).await?;
    if !integrity {
        return Err(AdmissionError::TargetDocumentHeadIntegrity { document_id });
    }
    let Some(intent_id) = intent_id else {
        return Ok(None);
    };
    let identity: Option<serde_json::Value> = sqlx::query_scalar(
        "select m.request_identity from content_visibility_intent v
         join content_mutation_item i on i.id = v.mutation_item_id and i.document_id = v.document_id
         join content_mutation m on m.id = i.mutation_id
         where v.mutation_item_id = $1 and v.document_id = $2
           and m.workspace_id = $3 and m.library_id = $4 and m.operation_kind = 'visibility'
           and m.mutation_state = 'applied' and i.item_state = 'applied'
           and i.base_revision_id is null and i.result_revision_id is null",
    )
    .bind(intent_id)
    .bind(document_id)
    .bind(request.workspace_id)
    .bind(request.library_id)
    .fetch_optional(&mut **transaction)
    .await?;
    identity.map(Some).ok_or(AdmissionError::VisibilityIntentSuperseded { document_id })
}

async fn validate_current_payload_binding(
    transaction: &mut Transaction<'_, Postgres>,
    request: &ContentAdmissionRequest,
    document_id: Uuid,
) -> Result<(), AdmissionError> {
    let current: Option<Uuid> = sqlx::query_scalar(
        "select visibility_intent_id from content_document_head where document_id = $1 for update",
    )
    .bind(document_id)
    .fetch_one(&mut **transaction)
    .await?;
    let Some(revision) = request.revision.as_ref() else {
        if current.is_some() && request.operation_kind != "delete" {
            return Err(AdmissionError::VisibilityIntentRequired { document_id });
        }
        return Ok(());
    };
    match (current, revision.visibility_intent_id) {
        (Some(_), None) => return Err(AdmissionError::VisibilityIntentRequired { document_id }),
        (current, Some(bound)) if current != Some(bound) => {
            return Err(AdmissionError::VisibilityIntentSuperseded { document_id });
        }
        _ => {}
    }
    if let Some(identity) = load_payload_intent_identity(transaction, request).await? {
        let expected: ironrag_contracts::documents::ReplacementIdentity =
            serde_json::from_value(identity["replacement"].clone())?;
        if expected != revision.replacement || request.operation_kind != "replace" {
            return Err(AdmissionError::VisibilityBindingConflict { document_id });
        }
    }
    if let Some(base_id) = revision.base_revision_id {
        let base_is_current: bool = sqlx::query_scalar(
            "select exists(select 1 from content_current_revision where document_id=$1 and revision_id=$2)",
        ).bind(document_id).bind(base_id).fetch_one(&mut **transaction).await?;
        if !base_is_current {
            return Err(AdmissionError::VisibilityBindingConflict { document_id });
        }
    }
    if request.operation_kind == "append" && revision.base_revision_id.is_none() {
        return Err(AdmissionError::VisibilityBindingConflict { document_id });
    }
    Ok(())
}

async fn ensure_no_active_payload_mutation(
    transaction: &mut Transaction<'_, Postgres>,
    document_id: Uuid,
) -> Result<(), AdmissionError> {
    let active: bool = sqlx::query_scalar(
        "select exists(select 1 from content_document_head h join content_mutation m on m.id = h.latest_mutation_id
         where h.document_id = $1 and m.mutation_state in ('accepted', 'running'))",
    ).bind(document_id).fetch_one(&mut **transaction).await?;
    if active {
        return Err(AdmissionError::ConflictingActiveMutation { document_id });
    }
    Ok(())
}

/// Exact-key resolution takes precedence over cross-key checksum deduplication.
/// A source-less, ownerless historical revision is not an admitted duplicate.
async fn reject_duplicate_staged_upload(
    transaction: &mut Transaction<'_, Postgres>,
    request: &ContentAdmissionRequest,
) -> Result<(), AdmissionError> {
    let ContentAdmissionTarget::New { external_key, .. } = &request.target else {
        return Err(AdmissionError::InvariantViolation("staged CREATE target required"));
    };
    let occupied: bool = sqlx::query_scalar("select exists(select 1 from content_document where workspace_id=$1 and library_id=$2 and external_key=$3 and document_state='active')")
        .bind(request.workspace_id).bind(request.library_id).bind(external_key.as_deref().map(str::trim)).fetch_one(&mut **transaction).await?;
    if !occupied {
        let revision = request
            .revision
            .as_ref()
            .ok_or(AdmissionError::InvariantViolation("staged revision missing"))?;
        if let Some(existing_document_id) =
            content_document_repository::find_active_document_by_library_checksum(
                &mut **transaction,
                request.library_id,
                &revision.replacement.checksum,
            )
            .await?
        {
            return Err(AdmissionError::DuplicateChecksum { existing_document_id });
        }
    }
    Ok(())
}

/// Completes only the known unadmitted shape while retaining all historical identity.
/// Library, document and head locks are held through the normal admission commit.
async fn complete_unowned_document(
    transaction: &mut Transaction<'_, Postgres>,
    request: &ContentAdmissionRequest,
    document_id: Uuid,
    staged_upload: bool,
) -> Result<content_document_repository::ContentDocumentRow, AdmissionError> {
    let document = content_document_repository::lock_document_by_id_with_executor(
        &mut **transaction,
        document_id,
    )
    .await?
    .ok_or(AdmissionError::TargetDocumentNotFound { document_id })?;
    let head = content_document_head_repository::get_document_head_for_update_with_executor(
        &mut **transaction,
        document_id,
    )
    .await?;
    let descriptor: bool =
        sqlx::query_scalar("select exists(select 1 from knowledge_document where document_id=$1)")
            .bind(document_id)
            .fetch_one(&mut **transaction)
            .await?;
    if descriptor {
        let admitted: bool = sqlx::query_scalar(
            "select exists(select 1 from content_mutation_item i where i.document_id=$1
              and (i.result_revision_id is not null or (i.item_state='applied'
                and not exists(select 1 from content_revision r where r.document_id=$1))))",
        )
        .bind(document_id)
        .fetch_one(&mut **transaction)
        .await?;
        return Err(if admitted {
            AdmissionError::DuplicateExternalKey { existing_document_id: document_id }
        } else {
            AdmissionError::DocumentAdmissionIncomplete { document_id }
        });
    }
    let refused = || AdmissionError::DocumentAdmissionIncomplete { document_id };
    let ContentAdmissionTarget::New { parent_external_key, .. } = &request.target else {
        return Err(refused());
    };
    let Some(source) = request.revision.as_ref() else {
        return Err(refused());
    };
    if !staged_upload
        || request.operation_kind != "upload"
        || parent_external_key.is_some()
        || document.workspace_id != request.workspace_id
        || document.library_id != request.library_id
        || document.document_state != "active"
        || document.deleted_at.is_some()
        || document.parent_document_id.is_some()
        || document.parent_external_key.is_some()
        || document.document_role != "primary"
        || head.is_none()
        || source.base_revision_id.is_some()
        || source.visibility_intent_id.is_some()
    {
        return Err(refused());
    }
    let replacement = &source.replacement;
    // All persisted source identity must agree; absent historical optional metadata
    // is not invented. Dependencies/owners are exclusion criteria, not repair targets.
    let eligible: bool = sqlx::query_scalar(
        "select
          (select count(*)=1 from content_revision where document_id=$1)
          and exists(select 1 from content_revision r where r.document_id=$1
            and r.workspace_id=$2 and r.library_id=$3 and r.content_source_kind='upload'
            and r.checksum=$4 and r.byte_size=$5 and r.mime_type=$6 and r.storage_key is null
            and r.parent_revision_id is null and r.admission_identity is null
            and (r.source_uri is null or r.source_uri is not distinct from $7)
            and (r.title is null or r.title is not distinct from $8)
            and (r.language_code is null or r.language_code is not distinct from $9)
            and (r.document_hint is null or r.document_hint is not distinct from $10)
            and not exists(select 1 from content_revision child where child.parent_revision_id=r.id)
            and not exists(select 1 from content_mutation_item i where i.base_revision_id=r.id or i.result_revision_id=r.id)
            and not exists(select 1 from content_document_head h where h.active_revision_id=r.id or h.readable_revision_id=r.id)
            and not exists(select 1 from knowledge_revision k where k.revision_id=r.id)
            and not exists(select 1 from content_chunk c where c.revision_id=r.id)
            and not exists(select 1 from content_revision_ingest_unit u where u.revision_id=r.id)
            and not exists(select 1 from ingest_job j where j.knowledge_revision_id=r.id)
            and not exists(select 1 from content_web_discovered_page p where p.result_revision_id=r.id)
            and not exists(select 1 from runtime_graph_evidence e where e.revision_id=r.id)
            and not exists(select 1 from runtime_graph_filtered_artifact a where a.revision_id=r.id))
          and exists(select 1 from content_document d where d.id=$1
            and (d.source_file_name is null or d.source_file_name=$11)
            and d.revision_sequence=(select max(revision_number) from content_revision where document_id=$1)
            and d.revision_sequence<2147483647)
          and exists(select 1 from content_document_head h where h.document_id=$1
            and h.active_revision_id is null and h.readable_revision_id is null
            and h.latest_mutation_id is null
            and h.latest_successful_attempt_id is null and h.visibility_intent_id is null
            and h.document_summary is null
            and h.recovery_attempts_count=0 and h.dead_letter_at is null
            and h.last_recovery_error_code is null and h.last_recovery_attempt_at is null)
          and not exists(select 1 from content_document c where c.parent_document_id=$1 or
            (c.workspace_id=$2 and c.library_id=$3 and c.parent_external_key=$12))
          and not exists(select 1 from content_mutation_item i where i.document_id=$1)
          and not exists(select 1 from content_mutation m where m.workspace_id=$2 and m.library_id=$3
            and (m.request_identity->'target'->>'documentId'=$1::text
              or m.request_identity->'target'->>'externalKey'=$12))
          and not exists(select 1 from content_visibility_intent v where v.document_id=$1)
          and not exists(select 1 from ingest_job j where j.knowledge_document_id=$1)
          and not exists(select 1 from content_web_discovered_page p where p.document_id=$1)
          and not exists(select 1 from runtime_graph_extraction e where e.document_id=$1)
          and not exists(select 1 from runtime_graph_node n where n.document_id=$1)
          and not exists(select 1 from runtime_graph_evidence e where e.document_id=$1)"
    ).bind(document_id).bind(request.workspace_id).bind(request.library_id)
        .bind(&replacement.checksum).bind(replacement.byte_size).bind(&replacement.mime_type)
        .bind(&replacement.source_uri).bind(&replacement.title).bind(&replacement.language_code)
        .bind(&replacement.document_hint).bind(&replacement.file_name).bind(&document.external_key)
        .fetch_one(&mut **transaction).await?;
    if !eligible {
        return Err(refused());
    }
    Ok(document)
}

async fn create_or_lock_document(
    transaction: &mut Transaction<'_, Postgres>,
    request: &ContentAdmissionRequest,
    staged_upload: bool,
) -> Result<content_document_repository::ContentDocumentRow, AdmissionError> {
    match &request.target {
        ContentAdmissionTarget::Existing { document_id } => {
            let document = content_document_repository::lock_document_by_id_with_executor(
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
            if request.operation_kind != "delete"
                && (document.deleted_at.is_some() || document.document_state == "deleted")
            {
                return Err(AdmissionError::TargetDocumentDeleted { document_id: *document_id });
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
            let document = match content_document_repository::create_document_with_executor(
                &mut **transaction,
                &content_document_repository::NewContentDocument {
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
                        "/* paired-document-readability: mirrors the state-only active unique index after its conflict */
                         select id
                         from content_document
                         where library_id = $1 and external_key = $2
                           and document_state = 'active'
                         limit 1",
                    )
                    .bind(request.library_id)
                    .bind(&external_key)
                    .fetch_one(&mut **transaction)
                    .await?;
                    return complete_unowned_document(
                        transaction,
                        request,
                        existing_document_id,
                        staged_upload,
                    )
                    .await;
                }
                Err(error) => return Err(error.into()),
            };
            let source_file_name =
                content_knowledge_projection_repository::storage_file_name_component(
                    file_name.as_deref().unwrap_or(&external_key),
                );
            sqlx::query("update content_document set source_file_name = $2 where id = $1")
                .bind(document.id)
                .bind(&source_file_name)
                .execute(&mut **transaction)
                .await?;
            content_document_head_repository::upsert_document_head_without_generation_with_executor(
                &mut **transaction,
                &content_document_head_repository::NewContentDocumentHead {
                    publishing_mutation_item_id: None,
                    document_id: document.id,
                    active_revision_id: None,
                    readable_revision_id: None,
                    latest_mutation_id: None,
                    latest_successful_attempt_id: None,
                },
            )
            .await?;
            // The authoritative insert above proves no *active* document holds
            // this key. A projection row left by a deleted one still would, so
            // release it before the projection insert can collide with it —
            // without this, a periodic connector re-sending a page that was
            // once deleted here fails with a duplicate key on every pass.
            content_knowledge_projection_repository::reclaim_external_key_from_inactive_projection_with_transaction(
                transaction,
                document.workspace_id,
                document.library_id,
                &document.external_key,
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
    document: &content_document_repository::ContentDocumentRow,
    input: &RevisionAdmission,
    request_identity: &serde_json::Value,
) -> Result<content_revision_repository::ContentRevisionRow, AdmissionError> {
    let materialized = content_knowledge_projection_repository::materialize_knowledge_document_from_current_head_with_transaction(transaction, document.id).await?;
    if materialized.outcome != content_knowledge_projection_repository::MaterializeKnowledgeDocumentOutcome::Materialized {
        return Err(AdmissionError::InvariantViolation("document projection unavailable"));
    }
    let admission_identity = ironrag_contracts::documents::RevisionAdmissionIdentity::Mutation {
        source_identity: request_identity["sourceIdentity"].as_str().map(str::to_owned),
        materialization_token: request_identity["materializationToken"].as_str().map(str::to_owned),
        visibility_intent_id: input.visibility_intent_id,
        replacement: input.replacement.clone(),
    };
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
    let revision_number = content_revision_repository::allocate_revision_number_with_executor(
        &mut **transaction,
        document.id,
    )
    .await?;
    let revision = content_revision_repository::create_revision_with_executor(
        &mut **transaction,
        &content_revision_repository::NewContentRevision {
            admission_identity: Some(&admission_identity),
            document_id: document.id,
            workspace_id: request.workspace_id,
            library_id: request.library_id,
            revision_number,
            parent_revision_id: input.base_revision_id.or_else(|| latest.map(|(id, _)| id)),
            content_source_kind: &input.content_source_kind,
            checksum: &input.replacement.checksum,
            mime_type: &input.replacement.mime_type,
            byte_size: input.replacement.byte_size,
            title: input.replacement.title.as_deref(),
            language_code: input.replacement.language_code.as_deref(),
            source_uri: input.replacement.source_uri.as_deref(),
            document_hint: input.replacement.document_hint.as_deref(),
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
    revision: Option<&content_revision_repository::ContentRevisionRow>,
    visibility_intent_id: Option<Uuid>,
    item_state: &str,
) -> Result<content_mutation_repository::ContentMutationItemRow, AdmissionError> {
    Ok(sqlx::query_as::<_, content_mutation_repository::ContentMutationItemRow>(
        "insert into content_mutation_item (
            id, mutation_id, document_id, base_revision_id,
            result_revision_id, visibility_intent_id, item_state, message
         ) select $1, $2, $3,
             case when m.operation_kind = 'delete' then coalesce(h.active_revision_id, h.readable_revision_id) else $4 end,
             $5, $6, $7::content_mutation_item_state, $8
           from content_mutation m join content_document_head h on h.document_id = $3
           where m.id = $2
         returning id, mutation_id, document_id, base_revision_id, result_revision_id,
                   item_state::text as item_state, message",
    )
    .bind(Uuid::now_v7())
    .bind(mutation_id)
    .bind(document_id)
    .bind(revision.and_then(|revision| revision.parent_revision_id))
    .bind(revision.map(|revision| revision.id))
    .bind(visibility_intent_id)
    .bind(item_state)
    .bind(if revision.is_some() {
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
    let head = content_document_head_repository::get_document_head_for_update_with_executor(
        &mut **transaction,
        document_id,
    )
    .await?
    .ok_or(AdmissionError::InvariantViolation("document head is missing"))?;
    // Explicit delete admission denies public reads immediately, but its
    // independent intent is retained until the document lifecycle finishes.
    let deleting: bool =
        sqlx::query_scalar("select operation_kind = 'delete' from content_mutation where id=$1")
            .bind(mutation_id)
            .fetch_one(&mut **transaction)
            .await?;
    let (active_revision_id, readable_revision_id) =
        if deleting { (None, None) } else { (head.active_revision_id, head.readable_revision_id) };
    let updated = sqlx::query(
        "update content_document_head set active_revision_id = $2, readable_revision_id = $3,
         document_summary = case when $3::uuid is null then null else document_summary end,
         latest_successful_attempt_id = case when exists
            (select 1 from ingest_attempt where id = latest_successful_attempt_id)
            then latest_successful_attempt_id else null end,
         latest_mutation_id = $4, head_updated_at = now() where document_id = $1",
    )
    .bind(document_id)
    .bind(active_revision_id)
    .bind(readable_revision_id)
    .bind(mutation_id)
    .execute(&mut **transaction)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(AdmissionError::TargetDocumentNotFound { document_id });
    }
    Ok(())
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

async fn load_content_bundle(
    transaction: &mut Transaction<'_, Postgres>,
    mutation: content_mutation_repository::ContentMutationRow,
) -> Result<ContentAdmissionBundle, AdmissionError> {
    let mut items = sqlx::query_as::<_, content_mutation_repository::ContentMutationItemRow>(
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
        return Err(AdmissionError::InvariantViolation("expected exactly one mutation item"));
    }
    let item = items
        .pop()
        .ok_or(AdmissionError::InvariantViolation("mutation item disappeared while locked"))?;
    let document_id = item
        .document_id
        .ok_or(AdmissionError::InvariantViolation("mutation item has no document"))?;
    let document = content_document_repository::lock_document_by_id_with_executor(
        &mut **transaction,
        document_id,
    )
    .await?
    .ok_or(AdmissionError::InvariantViolation("linked document is missing"))?;
    let operation = load_operation_by_subject(transaction, "content_mutation", mutation.id)
        .await?
        .ok_or(AdmissionError::InvariantViolation("linked async operation is missing"))?;
    let revision = match item.result_revision_id {
        Some(revision_id) => Some(
            sqlx::query_as::<_, content_revision_repository::ContentRevisionRow>(
                "select id, document_id, workspace_id, library_id, revision_number,
                        parent_revision_id, content_source_kind::text as content_source_kind,
                        checksum, mime_type, byte_size, title, language_code, source_uri,
                        document_hint, storage_key, created_by_principal_id, created_at
                 from content_revision where id = $1 for update",
            )
            .bind(revision_id)
            .fetch_optional(&mut **transaction)
            .await?
            .ok_or(AdmissionError::InvariantViolation("linked revision is missing"))?,
        ),
        None => None,
    };
    let job = load_job(transaction, mutation.id, "content_mutation").await?;
    if revision.is_some() != job.is_some() {
        return Err(AdmissionError::InvariantViolation("linked job must match payload presence"));
    }
    let bundle = ContentAdmissionBundle {
        mutation,
        document,
        revision,
        item: Some(item),
        job,
        async_operation: operation,
    };
    if !bundle.is_complete() {
        return Err(AdmissionError::InvariantViolation(
            "linked content admission rows disagree on identity",
        ));
    }
    Ok(bundle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_request() -> ContentAdmissionRequest {
        ContentAdmissionRequest {
            workspace_id: Uuid::nil(),
            library_id: Uuid::max(),
            operation_kind: "content_update".to_owned(),
            requested_by_principal_id: None,
            request_surface: "api".to_owned(),
            idempotency_key: Some("request-001".to_owned()),
            source_identity: Some("neutral-source".to_owned()),
            target: ContentAdmissionTarget::New {
                external_key: Some("alpha-document".to_owned()),
                file_name: Some("alpha.txt".to_owned()),
                parent_external_key: None,
            },
            revision: Some(RevisionAdmission {
                content_source_kind: "file".to_owned(),
                replacement: ironrag_contracts::documents::ReplacementIdentity {
                    file_name: "alpha.txt".to_owned(),
                    checksum: format!("sha256:{}", "a".repeat(64)),
                    mime_type: "text/plain".to_owned(),
                    byte_size: 42,
                    title: Some("Alpha document".to_owned()),
                    language_code: None,
                    source_uri: None,
                    document_hint: None,
                },
                visibility_intent_id: None,
                base_revision_id: None,
                storage_key: Some("storage/alpha".to_owned()),
            }),
            parent_async_operation_id: None,
            priority: 0,
        }
    }

    #[test]
    fn content_fingerprint_is_stable_for_equivalent_requests() {
        let request = new_request();

        assert_eq!(content_fingerprint(&request).unwrap(), content_fingerprint(&request).unwrap());
    }

    #[test]
    fn content_fingerprint_changes_when_revision_identity_changes() {
        let request = new_request();
        let mut changed = request.clone();
        changed.revision.as_mut().unwrap().replacement.checksum =
            format!("sha256:{}", "b".repeat(64));

        assert_ne!(content_fingerprint(&request).unwrap(), content_fingerprint(&changed).unwrap());
    }

    #[test]
    fn content_fingerprint_distinguishes_new_and_existing_targets() {
        let request = new_request();
        let mut existing = request.clone();
        existing.target = ContentAdmissionTarget::Existing { document_id: Uuid::nil() };

        assert_ne!(content_fingerprint(&request).unwrap(), content_fingerprint(&existing).unwrap());
    }

    #[test]
    fn visibility_contract_payload_fingerprint_excludes_artifact_and_actor_surface() {
        let request = new_request();
        let mut retransmission = request.clone();
        retransmission.request_surface = "mcp".to_owned();
        retransmission.requested_by_principal_id = Some(Uuid::now_v7());
        retransmission.revision.as_mut().unwrap().storage_key =
            Some("different/staged/location".to_owned());
        assert_eq!(
            content_fingerprint(&request).unwrap(),
            content_fingerprint(&retransmission).unwrap()
        );
    }

    #[test]
    fn visibility_contract_payload_fingerprint_binds_explicit_request_metadata() {
        let request = new_request();
        for field in
            ["mime", "title", "language", "uri", "hint", "filename", "size", "intent", "base"]
        {
            let mut changed = request.clone();
            let revision = changed.revision.as_mut().unwrap();
            match field {
                "mime" => revision.replacement.mime_type = "text/markdown".to_owned(),
                "filename" => revision.replacement.file_name = "other.txt".to_owned(),
                "size" => revision.replacement.byte_size += 1,
                "intent" => revision.visibility_intent_id = Some(Uuid::now_v7()),
                "base" => revision.base_revision_id = Some(Uuid::now_v7()),
                "title" => revision.replacement.title = Some("new title".to_owned()),
                "language" => revision.replacement.language_code = Some("ru".to_owned()),
                "uri" => {
                    revision.replacement.source_uri =
                        Some("https://example.invalid/source".to_owned())
                }
                "hint" => revision.replacement.document_hint = Some("new hint".to_owned()),
                _ => unreachable!(),
            }
            assert_ne!(
                content_fingerprint(&request).unwrap(),
                content_fingerprint(&changed).unwrap(),
                "{field}"
            );
        }
    }

    #[test]
    fn content_fingerprint_binds_independent_visibility_intent() {
        let request = new_request();
        let mut bound = request.clone();
        bound.revision.as_mut().unwrap().visibility_intent_id = Some(Uuid::now_v7());
        assert_ne!(
            content_fingerprint(&request).unwrap(),
            content_fingerprint(&bound).unwrap(),
            "the idempotency fingerprint must bind the independent intent",
        );
    }

    #[test]
    fn content_identity_contains_no_retired_visibility_authority() {
        let identity = content_request_identity(&new_request());
        assert!(identity.get("visibilityPolicy").is_none());
        assert!(identity.get("visibility_policy").is_none());
        assert!(identity["revision"].get("visibilityIntentId").is_some());
    }
}
