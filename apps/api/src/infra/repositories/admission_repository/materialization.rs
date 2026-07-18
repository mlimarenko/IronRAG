use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::{
    AdmissionError, AdmissionFailpoint, AdmissionOutcome, WebCaptureMaterializationAdmissionBundle,
    WebCaptureMaterializationAdmissionRequest, check_failpoint, lock_library,
};
use crate::infra::repositories::{content_repository, ingest_repository};

#[derive(sqlx::FromRow)]
struct MaterializationRevisionProjectionRow {
    revision_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_number: i64,
    revision_state: String,
    revision_kind: String,
    storage_ref: Option<String>,
    source_uri: Option<String>,
    document_hint: Option<String>,
    mime_type: String,
    checksum: String,
    title: Option<String>,
    byte_size: i64,
    text_state: String,
    vector_state: String,
    graph_state: String,
    superseded_by_revision_id: Option<Uuid>,
}

/// Atomically makes one already-created web revision runnable.
///
/// The exact mutation, active web run, document, revision, and knowledge
/// projection are validated under locks before any owned row is inserted. The
/// mutation item, content-ingest job, and pending head ownership then commit as
/// one unit, so callers never need a mutation-wide compensating reconcile.
pub async fn admit_web_capture_materialization_with_failpoint(
    postgres: &sqlx::PgPool,
    request: &WebCaptureMaterializationAdmissionRequest,
    failpoint: Option<AdmissionFailpoint>,
) -> Result<AdmissionOutcome<WebCaptureMaterializationAdmissionBundle>, AdmissionError> {
    let mut transaction = postgres.begin().await?;
    lock_library(&mut transaction, request.workspace_id, request.library_id).await?;

    let mutation = lock_exact_web_mutation(&mut transaction, request).await?;
    let document = content_repository::lock_document_by_id_with_executor(
        &mut *transaction,
        request.document_id,
    )
    .await?
    .ok_or(AdmissionError::InvariantViolation("materialization document does not exist"))?;
    if document.workspace_id != request.workspace_id
        || document.library_id != request.library_id
        || document.document_state == "deleted"
        || document.deleted_at.is_some()
    {
        return Err(AdmissionError::InvariantViolation(
            "materialization document is outside the active request scope",
        ));
    }
    let head = content_repository::get_document_head_for_update_with_executor(
        &mut *transaction,
        document.id,
    )
    .await?
    .ok_or(AdmissionError::InvariantViolation("materialization document head does not exist"))?;

    let revision = content_repository::get_revision_by_id_with_executor(
        &mut *transaction,
        request.revision_id,
    )
    .await?
    .ok_or(AdmissionError::InvariantViolation("materialization revision does not exist"))?;
    if revision.document_id != document.id
        || revision.workspace_id != request.workspace_id
        || revision.library_id != request.library_id
        || revision.content_source_kind != "web_page"
        || revision.created_by_principal_id != request.requested_by_principal_id
    {
        return Err(AdmissionError::InvariantViolation(
            "materialization revision identity does not match its document",
        ));
    }
    let projection = lock_exact_knowledge_revision(&mut transaction, request).await?;
    ensure_revision_projection_matches(&revision, &projection)?;
    let run_state = lock_exact_web_run(&mut transaction, request).await?;

    if let Some(existing_item) = load_exact_materialization_item(&mut transaction, request).await? {
        let job = load_exact_materialization_job(&mut transaction, existing_item.id).await?.ok_or(
            AdmissionError::InvariantViolation(
                "materialization item exists without its exact ingest job",
            ),
        )?;
        let bundle = WebCaptureMaterializationAdmissionBundle {
            mutation,
            document,
            revision,
            item: existing_item,
            job,
            head,
        };
        if !bundle.is_complete() {
            return Err(AdmissionError::InvariantViolation(
                "replayed materialization admission identities disagree",
            ));
        }
        ensure_replay_owner_state(
            &bundle.mutation.mutation_state,
            &run_state,
            &bundle.item.item_state,
        )?;
        transaction.commit().await?;
        return Ok(AdmissionOutcome::Replayed(bundle));
    }

    ensure_open_materialization_owner(&mutation.mutation_state, &run_state)?;
    ensure_revision_projection_is_pending(&projection)?;
    ensure_head_accepts_mutation(&mut transaction, &head, request.mutation_id).await?;
    let base_revision_id = head.active_revision_id.or(head.readable_revision_id);
    let item = content_repository::create_mutation_item_with_executor(
        &mut *transaction,
        &content_repository::NewContentMutationItem {
            mutation_id: request.mutation_id,
            document_id: Some(request.document_id),
            base_revision_id,
            result_revision_id: Some(request.revision_id),
            item_state: "pending",
            message: Some("web page accepted and queued for ingest"),
        },
    )
    .await?;
    check_failpoint(failpoint, AdmissionFailpoint::AfterMutationItem)?;

    let job = ingest_repository::create_ingest_job_with_executor(
        &mut *transaction,
        &ingest_repository::NewIngestJob {
            workspace_id: request.workspace_id,
            library_id: request.library_id,
            mutation_id: Some(request.mutation_id),
            mutation_item_id: Some(item.id),
            connector_id: None,
            async_operation_id: None,
            knowledge_document_id: Some(request.document_id),
            knowledge_revision_id: Some(request.revision_id),
            job_kind: "content_mutation".to_string(),
            queue_state: "queued".to_string(),
            priority: request.priority,
            dedupe_key: Some(format!("content-mutation:{}:{}", request.mutation_id, item.id)),
            queued_at: None,
            available_at: None,
            completed_at: None,
        },
    )
    .await?;
    check_failpoint(failpoint, AdmissionFailpoint::AfterJob)?;

    let promoted_head = content_repository::upsert_document_head_without_generation_with_executor(
        &mut *transaction,
        &content_repository::NewContentDocumentHead {
            document_id: document.id,
            active_revision_id: head.active_revision_id,
            readable_revision_id: head.readable_revision_id,
            latest_mutation_id: Some(request.mutation_id),
            latest_successful_attempt_id: head.latest_successful_attempt_id,
        },
    )
    .await?;
    check_failpoint(failpoint, AdmissionFailpoint::AfterHead)?;

    let bundle = WebCaptureMaterializationAdmissionBundle {
        mutation,
        document,
        revision,
        item,
        job,
        head: promoted_head,
    };
    if !bundle.is_complete() || bundle.head.latest_mutation_id != Some(request.mutation_id) {
        return Err(AdmissionError::InvariantViolation(
            "new materialization admission is not internally complete",
        ));
    }
    check_failpoint(failpoint, AdmissionFailpoint::BeforeCommit)?;
    transaction.commit().await?;
    Ok(AdmissionOutcome::Created(bundle))
}

async fn lock_exact_web_mutation(
    transaction: &mut Transaction<'_, Postgres>,
    request: &WebCaptureMaterializationAdmissionRequest,
) -> Result<content_repository::ContentMutationRow, AdmissionError> {
    let mutation = sqlx::query_as::<_, content_repository::ContentMutationRow>(
        "select
            id, workspace_id, library_id, operation_kind::text as operation_kind,
            requested_by_principal_id, request_surface::text as request_surface,
            idempotency_key, source_identity, mutation_state::text as mutation_state,
            requested_at, completed_at, failure_code, conflict_code
         from content_mutation
         where id = $1
         for update",
    )
    .bind(request.mutation_id)
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(AdmissionError::InvariantViolation("materialization mutation does not exist"))?;
    if mutation.workspace_id != request.workspace_id
        || mutation.library_id != request.library_id
        || mutation.operation_kind != "web_capture"
        || mutation.requested_by_principal_id != request.requested_by_principal_id
    {
        return Err(AdmissionError::InvariantViolation(
            "materialization mutation is outside the owning web-capture scope",
        ));
    }
    Ok(mutation)
}

async fn lock_exact_web_run(
    transaction: &mut Transaction<'_, Postgres>,
    request: &WebCaptureMaterializationAdmissionRequest,
) -> Result<String, AdmissionError> {
    let run = sqlx::query_as::<_, (Uuid, Uuid, Uuid, Option<Uuid>, String)>(
        "select id, workspace_id, library_id, requested_by_principal_id,
                run_state::text as run_state
         from content_web_ingest_run
         where mutation_id = $1
         for share",
    )
    .bind(request.mutation_id)
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(AdmissionError::InvariantViolation("materialization requires one owning web run"))?;
    if run.1 != request.workspace_id
        || run.2 != request.library_id
        || run.3 != request.requested_by_principal_id
    {
        return Err(AdmissionError::InvariantViolation(
            "materialization web run is outside the owning request scope",
        ));
    }
    Ok(run.4)
}

async fn lock_exact_knowledge_revision(
    transaction: &mut Transaction<'_, Postgres>,
    request: &WebCaptureMaterializationAdmissionRequest,
) -> Result<MaterializationRevisionProjectionRow, AdmissionError> {
    let projection = sqlx::query_as::<_, MaterializationRevisionProjectionRow>(
        "select
            revision_id, workspace_id, library_id, document_id, revision_number,
            revision_state, revision_kind, storage_ref, source_uri, document_hint,
            mime_type, checksum, title, byte_size, text_state, vector_state,
            graph_state, superseded_by_revision_id
         from knowledge_revision
         where revision_id = $1
         for share",
    )
    .bind(request.revision_id)
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(AdmissionError::InvariantViolation(
        "materialization knowledge revision identity is unavailable",
    ))?;
    if projection.document_id != request.document_id
        || projection.workspace_id != request.workspace_id
        || projection.library_id != request.library_id
    {
        return Err(AdmissionError::InvariantViolation(
            "materialization knowledge revision is outside the owning request scope",
        ));
    }
    Ok(projection)
}

fn ensure_revision_projection_matches(
    revision: &content_repository::ContentRevisionRow,
    projection: &MaterializationRevisionProjectionRow,
) -> Result<(), AdmissionError> {
    if projection.revision_id != revision.id
        || projection.document_id != revision.document_id
        || projection.workspace_id != revision.workspace_id
        || projection.library_id != revision.library_id
        || projection.revision_number != i64::from(revision.revision_number)
        || projection.revision_kind != revision.content_source_kind
        || projection.storage_ref != revision.storage_key
        || projection.source_uri != revision.source_uri
        || projection.document_hint != revision.document_hint
        || projection.mime_type != revision.mime_type
        || projection.checksum != revision.checksum
        || projection.title != revision.title
        || projection.byte_size != revision.byte_size
    {
        return Err(AdmissionError::InvariantViolation(
            "materialization canonical revision and knowledge projection disagree",
        ));
    }
    Ok(())
}

fn ensure_revision_projection_is_pending(
    projection: &MaterializationRevisionProjectionRow,
) -> Result<(), AdmissionError> {
    if projection.revision_state != "accepted"
        || projection.text_state != "accepted"
        || projection.vector_state != "accepted"
        || projection.graph_state != "accepted"
        || projection.superseded_by_revision_id.is_some()
    {
        return Err(AdmissionError::InvariantViolation(
            "materialization revision projection is not pending admission",
        ));
    }
    Ok(())
}

fn ensure_open_materialization_owner(
    mutation_state: &str,
    run_state: &str,
) -> Result<(), AdmissionError> {
    if matches!(mutation_state, "accepted" | "running")
        && matches!(run_state, "accepted" | "discovering" | "processing")
    {
        return Ok(());
    }
    Err(AdmissionError::InvariantViolation(
        "materialization requires an active mutation and web run",
    ))
}

fn ensure_replay_owner_state(
    mutation_state: &str,
    run_state: &str,
    item_state: &str,
) -> Result<(), AdmissionError> {
    if matches!(item_state, "pending") {
        return ensure_open_materialization_owner(mutation_state, run_state);
    }
    let consistent = matches!(
        (mutation_state, run_state),
        ("accepted" | "running", "accepted" | "discovering" | "processing")
            | ("applied", "completed" | "completed_partial")
            | ("failed", "failed")
            | ("canceled", "canceled")
    );
    if consistent {
        return Ok(());
    }
    Err(AdmissionError::InvariantViolation("replayed materialization owner states disagree"))
}

async fn load_exact_materialization_item(
    transaction: &mut Transaction<'_, Postgres>,
    request: &WebCaptureMaterializationAdmissionRequest,
) -> Result<Option<content_repository::ContentMutationItemRow>, AdmissionError> {
    let mut items = sqlx::query_as::<_, content_repository::ContentMutationItemRow>(
        "select id, mutation_id, document_id, base_revision_id, result_revision_id,
                item_state::text as item_state, message
         from content_mutation_item
         where mutation_id = $1
           and document_id = $2
           and result_revision_id = $3
         order by id
         for update",
    )
    .bind(request.mutation_id)
    .bind(request.document_id)
    .bind(request.revision_id)
    .fetch_all(&mut **transaction)
    .await?;
    if items.len() > 1 {
        return Err(AdmissionError::InvariantViolation(
            "materialization has multiple items for one exact revision",
        ));
    }
    Ok(items.pop())
}

async fn load_exact_materialization_job(
    transaction: &mut Transaction<'_, Postgres>,
    mutation_item_id: Uuid,
) -> Result<Option<ingest_repository::IngestJobRow>, AdmissionError> {
    Ok(sqlx::query_as::<_, ingest_repository::IngestJobRow>(
        "select
            id, workspace_id, library_id, mutation_id, mutation_item_id,
            connector_id, async_operation_id, knowledge_document_id,
            knowledge_revision_id, job_kind::text as job_kind,
            queue_state::text as queue_state, priority, dedupe_key,
            queued_at, available_at, completed_at,
            queue_leased_at, queue_lease_token, queue_lease_owner
         from ingest_job
         where mutation_item_id = $1
           and job_kind = 'content_mutation'
         for update",
    )
    .bind(mutation_item_id)
    .fetch_optional(&mut **transaction)
    .await?)
}

async fn ensure_head_accepts_mutation(
    transaction: &mut Transaction<'_, Postgres>,
    head: &content_repository::ContentDocumentHeadRow,
    mutation_id: Uuid,
) -> Result<(), AdmissionError> {
    let Some(existing_mutation_id) = head.latest_mutation_id else {
        return Ok(());
    };
    if existing_mutation_id == mutation_id {
        return Ok(());
    }
    let existing_state = sqlx::query_scalar::<_, String>(
        "select mutation_state::text
         from content_mutation
         where id = $1",
    )
    .bind(existing_mutation_id)
    .fetch_optional(&mut **transaction)
    .await?;
    if existing_state.as_deref().is_some_and(|state| matches!(state, "accepted" | "running")) {
        return Err(AdmissionError::ConflictingActiveMutation { document_id: head.document_id });
    }
    Ok(())
}
