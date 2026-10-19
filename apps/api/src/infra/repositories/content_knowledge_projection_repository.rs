use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::domains::content::{derive_document_role, revision_is_raster_image};

use super::{
    content_document_head_repository::{
        ContentDocumentHeadRow, NewContentDocumentHead, ValidatedDocumentHeadWriteOutcome,
        get_document_head_for_update_with_executor, upsert_document_head_with_executor,
        upsert_document_head_without_generation_outcome,
    },
    content_document_repository::{
        ContentDocumentRow, NewContentDocument, create_document_with_executor,
        lock_document_by_id_with_executor,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaterializeKnowledgeDocumentOutcome {
    Materialized,
    DocumentNotFound,
    DocumentHeadNotFound,
    RevisionNotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializeKnowledgeDocumentResult {
    pub outcome: MaterializeKnowledgeDocumentOutcome,
    pub changed: bool,
}
#[derive(Debug, Clone)]
pub enum PromoteDocumentHeadOutcome {
    Promoted(ContentDocumentHeadRow),
    DocumentNotFound,
    RevisionNotFound,
    ReferenceIntegrityViolation,
    SuppressedUntilReady,
}

/// Rows removed by one knowledge-plane projection purge.
#[derive(Debug, Default, Clone, Copy)]
pub struct PurgedKnowledgeProjection {
    pub documents: u64,
    pub chunk_vector_rows: u64,
    pub graph_evidence_rows: u64,
}

/// Deletes the chunk vectors of one knowledge-projection scope and maps the
/// vector-plane error into the protocol error both projection purges share.
async fn delete_projection_chunk_vectors_for_chunks(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    chunk_ids: &[Uuid],
) -> Result<u64, sqlx::Error> {
    crate::infra::postgres::pg_search_store::delete_projection_chunk_vectors(
        transaction,
        library_id,
        chunk_ids,
    )
    .await
    .map_err(|error| {
        sqlx::Error::Protocol(format!(
            "failed to delete projection chunk vectors atomically: {error:#}"
        ))
    })
}

/// Removes knowledge-plane documents together with everything that does not
/// cascade with them.
///
/// Revisions, chunks, evidence, structured blocks and technical facts cascade
/// from `knowledge_document`. Chunk vectors live in dynamic per-dim relations
/// without foreign keys, and `runtime_graph_evidence` references documents by
/// plain columns, so both are swept explicitly in the same transaction — a
/// purge that skips them leaves vectors keyed to chunk ids that no longer
/// exist. Physical vector deletion and exact manifest decrements are atomic.
/// The caller owns the library lifecycle lock, the source-truth generation
/// bump, and any graph re-projection.
pub async fn purge_knowledge_projection_with_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    document_ids: &[Uuid],
) -> Result<PurgedKnowledgeProjection, sqlx::Error> {
    if document_ids.is_empty() {
        return Ok(PurgedKnowledgeProjection::default());
    }

    let chunk_ids: Vec<Uuid> = sqlx::query_scalar(
        "select chunk_id from knowledge_chunk
         where library_id = $1 and document_id = any($2)",
    )
    .bind(library_id)
    .bind(document_ids)
    .fetch_all(&mut **transaction)
    .await?;

    let chunk_vector_rows =
        delete_projection_chunk_vectors_for_chunks(transaction, library_id, &chunk_ids).await?;

    let graph_evidence_rows = sqlx::query(
        "delete from runtime_graph_evidence
         where library_id = $1 and document_id = any($2)",
    )
    .bind(library_id)
    .bind(document_ids)
    .execute(&mut **transaction)
    .await?
    .rows_affected();

    let documents = sqlx::query(
        "delete from knowledge_document
         where library_id = $1 and document_id = any($2)",
    )
    .bind(library_id)
    .bind(document_ids)
    .execute(&mut **transaction)
    .await?
    .rows_affected();

    Ok(PurgedKnowledgeProjection { documents, chunk_vector_rows, graph_evidence_rows })
}

/// Rows removed by one revision-scoped historical projection purge.
#[derive(Debug, Default, Clone, Copy)]
pub struct PurgedHistoricalProjection {
    pub knowledge_revisions: u64,
    pub chunk_vector_rows: u64,
    pub graph_evidence_rows: u64,
}

/// Removes historical knowledge-plane revisions of one document while leaving
/// the document's current projection in place.
///
/// This is the derived knowledge plane only. Authoritative source history
/// (`content_revision` and its blob), `content_chunk` and
/// `query_chunk_reference` are never referenced: saved citations and source
/// history resolve against the source plane and are untouched here. The caller
/// owns the library lifecycle lock, the source-truth generation bump, and
/// graph re-projection.
///
/// The guarded `knowledge_revision` delete is the single authority. Its
/// `WITH ... removed RETURNING` statement evaluates the head and in-flight
/// ingest guards exactly once against one pre-statement snapshot and deletes
/// only the revisions it admits; it returns each removed revision together with
/// the chunk ids that revision owned. Chunk vectors and `runtime_graph_evidence`
/// are then swept for exactly that returned set, so a revision the guard refuses
/// survives with its derived planes intact instead of losing them before the
/// refusal. `knowledge_chunk` rows cascade away with `knowledge_revision`, so
/// the chunk ids are captured by a subquery in the same statement, which reads
/// the pre-statement snapshot. Chunk, structured, fact, evidence and candidate
/// rows cascade from `knowledge_revision`, and the existing FK turns a
/// `superseded_by_revision_id` that pointed at a removed revision into NULL.
pub async fn purge_historical_projection_revisions_with_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    document_id: Uuid,
    revision_ids: &[Uuid],
) -> Result<PurgedHistoricalProjection, sqlx::Error> {
    if revision_ids.is_empty() {
        return Ok(PurgedHistoricalProjection::default());
    }

    let removed = sqlx::query_as::<_, (Uuid, Vec<Uuid>)>(
        "with removable as (
             select kr.revision_id
             from knowledge_revision kr
             join knowledge_document kd on kd.document_id = kr.document_id
             where kr.library_id = $1
               and kr.document_id = $2
               and kr.revision_id = any($3)
               and kr.revision_id is distinct from kd.readable_revision_id
               and kr.revision_id is distinct from kd.active_revision_id
               and not exists (
                   select 1 from ingest_job job
                   where job.library_id = kr.library_id
                     and job.knowledge_document_id = kr.document_id
                     and job.queue_state in ('queued', 'leased', 'paused')
                     and job.completed_at is null
               )
         ),
         removed as (
             delete from knowledge_revision kr
             using removable r
             where kr.revision_id = r.revision_id
             returning kr.revision_id
         )
         select r.revision_id,
                coalesce(
                    (select array_agg(kc.chunk_id)
                     from knowledge_chunk kc
                     where kc.library_id = $1
                       and kc.document_id = $2
                       and kc.revision_id = r.revision_id),
                    '{}'::uuid[]
                ) as chunk_ids
         from removed r",
    )
    .bind(library_id)
    .bind(document_id)
    .bind(revision_ids)
    .fetch_all(&mut **transaction)
    .await?;

    let removed_revision_ids =
        removed.iter().map(|(revision_id, _)| *revision_id).collect::<Vec<_>>();
    let removed_chunk_ids =
        removed.iter().flat_map(|(_, chunk_ids)| chunk_ids.iter().copied()).collect::<Vec<_>>();

    let chunk_vector_rows =
        delete_projection_chunk_vectors_for_chunks(transaction, library_id, &removed_chunk_ids)
            .await?;

    let graph_evidence_rows = if removed_revision_ids.is_empty() {
        0
    } else {
        sqlx::query(
            "delete from runtime_graph_evidence
             where library_id = $1 and document_id = $2 and revision_id = any($3)",
        )
        .bind(library_id)
        .bind(document_id)
        .bind(&removed_revision_ids)
        .execute(&mut **transaction)
        .await?
        .rows_affected()
    };

    Ok(PurgedHistoricalProjection {
        knowledge_revisions: u64::try_from(removed.len()).unwrap_or(u64::MAX),
        chunk_vector_rows,
        graph_evidence_rows,
    })
}

/// Releases `external_key` from the query projection when the row holding it
/// belongs to a document that is no longer active.
///
/// The two uniqueness rules over an external key disagree about deletion.
/// `content_document` frees the key — its unique index is partial on
/// `document_state = 'active'` — while the projection's index carries no such
/// predicate. The projection row of a deleted document would therefore hold the
/// key forever, and every later ingest of that key would fail as a duplicate.
/// Because the projection is derived from `content_document` and a delete is
/// terminal, that row serves no reader; reclaiming it is what lets a source
/// re-publish a page that was once deleted here.
///
/// An *active* holder is never touched: that is a genuine duplicate key, owned
/// by the caller's authoritative guard on `content_document`. Neither is a
/// holder with no `content_document` at all — that is a corrupt projection, and
/// absorbing it here would let ingest quietly paper over the drift that
/// `repair orphan-knowledge-documents` exists to surface and remove.
///
/// The caller must already hold the library lifecycle lock, which is what
/// serializes this against a concurrent delete or materialization.
pub async fn reclaim_external_key_from_inactive_projection_with_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    library_id: Uuid,
    external_key: &str,
) -> Result<Option<Uuid>, sqlx::Error> {
    let holder = sqlx::query_scalar::<_, Uuid>(
        "select kd.document_id
         from knowledge_document kd
         join content_document cd on cd.id = kd.document_id
         where kd.workspace_id = $1
           and kd.library_id = $2
           and kd.external_key = $3
           and cd.document_state <> 'active'",
    )
    .bind(workspace_id)
    .bind(library_id)
    .bind(external_key)
    .fetch_optional(&mut **transaction)
    .await?;
    let Some(document_id) = holder else {
        return Ok(None);
    };

    purge_knowledge_projection_with_transaction(transaction, library_id, &[document_id]).await?;
    // Retrieval caches key off the library generation, and the purge removed
    // chunks and vectors they may still be holding.
    super::catalog_repository::touch_library_source_truth_version_with_executor(
        &mut **transaction,
        library_id,
    )
    .await?;
    Ok(Some(document_id))
}

pub async fn create_document_with_projection(
    postgres: &PgPool,
    new_document: &NewContentDocument<'_>,
    source_file_name: Option<&str>,
) -> Result<ContentDocumentRow, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    if !super::catalog_repository::lock_library_for_lifecycle_event_with_executor(
        &mut *transaction,
        new_document.workspace_id,
        new_document.library_id,
    )
    .await?
    {
        transaction.rollback().await?;
        return Err(sqlx::Error::RowNotFound);
    }
    let document = create_document_with_executor(&mut *transaction, new_document).await?;
    let source_file_name = source_file_name.map_or_else(
        || storage_file_name_component(&document.external_key),
        storage_file_name_component,
    );
    sqlx::query("update content_document set source_file_name = $2 where id = $1")
        .bind(document.id)
        .bind(&source_file_name)
        .execute(&mut *transaction)
        .await?;
    upsert_document_head_with_executor(
        &mut *transaction,
        &NewContentDocumentHead {
            document_id: document.id,
            publishing_mutation_item_id: None,
            active_revision_id: None,
            readable_revision_id: None,
            latest_mutation_id: None,
            latest_successful_attempt_id: None,
        },
    )
    .await?;
    let file_name =
        normalized_projection_file_name(&document.external_key, Some(&source_file_name), None);
    // The authoritative insert above proves no *active* document holds this
    // key. A projection row left by a deleted one still would, so release it
    // before the projection insert can collide with it.
    reclaim_external_key_from_inactive_projection_with_transaction(
        &mut transaction,
        document.workspace_id,
        document.library_id,
        &document.external_key,
    )
    .await?;
    sqlx::query(
        "insert into knowledge_document (
            document_id, workspace_id, library_id, external_key, file_name, title,
            document_state, active_revision_id, readable_revision_id, latest_revision_no,
            parent_document_id, document_role, created_at, updated_at, deleted_at
         ) values (
            $1, $2, $3, $4, $5, null, $6, null, null, null,
            $7, $8, $9, $9, $10
         )",
    )
    .bind(document.id)
    .bind(document.workspace_id)
    .bind(document.library_id)
    .bind(&document.external_key)
    .bind(file_name)
    .bind(&document.document_state)
    .bind(document.parent_document_id)
    .bind(&document.document_role)
    .bind(document.created_at)
    .bind(document.deleted_at)
    .execute(&mut *transaction)
    .await?;

    transaction.commit().await?;
    Ok(document)
}

fn normalized_projection_file_name(
    external_key: &str,
    source_file_name: Option<&str>,
    storage_key: Option<&str>,
) -> String {
    storage_key
        .and_then(persisted_file_name_from_storage_key)
        .or_else(|| source_file_name.map(storage_file_name_component))
        .unwrap_or_else(|| storage_file_name_component(external_key))
}

fn persisted_file_name_from_storage_key(storage_key: &str) -> Option<String> {
    let basename = storage_key
        .trim()
        .split(['/', '\\'])
        .next_back()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    basename
        .split_once('-')
        .and_then(|(prefix, suffix)| {
            (prefix.len() == 64
                && prefix.bytes().all(|byte| byte.is_ascii_hexdigit())
                && !suffix.trim().is_empty())
            .then_some(suffix)
        })
        .map(storage_file_name_component)
}

pub(crate) fn storage_file_name_component(value: &str) -> String {
    let basename = value
        .trim()
        .split(['/', '\\'])
        .next_back()
        .unwrap_or(value)
        .chars()
        .map(|character| if character.is_ascii_control() { '_' } else { character })
        .collect::<String>()
        .replace('"', "")
        .trim()
        .trim_matches('.')
        .to_string();
    if basename.is_empty() { "document".to_string() } else { basename }
}

/// Materializes one knowledge document exclusively from its locked source
/// document and head state.
///
/// The library parent is locked before the source document, head, and
/// knowledge row. A delayed promotion therefore never trusts the head pointers
/// carried in its request: after it acquires the lock it observes the newest
/// committed current document head and writes that state. The projection write, typed
/// role finalization, and durable generation fence commit atomically.
pub async fn materialize_knowledge_document_from_readable_head(
    postgres: &PgPool,
    document_id: Uuid,
) -> Result<MaterializeKnowledgeDocumentOutcome, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let outcome = materialize_knowledge_document_from_current_head_with_transaction(
        &mut transaction,
        document_id,
    )
    .await?;
    if outcome.outcome == MaterializeKnowledgeDocumentOutcome::Materialized {
        if outcome.changed {
            let library_id = sqlx::query_scalar::<_, Uuid>(
                "select library_id from content_document where id = $1",
            )
            .bind(document_id)
            .fetch_one(&mut *transaction)
            .await?;
            super::catalog_repository::touch_library_source_truth_version_with_executor(
                &mut *transaction,
                library_id,
            )
            .await?;
        }
        transaction.commit().await?;
    } else {
        transaction.rollback().await?;
    }
    Ok(outcome.outcome)
}

/// Transaction-scoped form used by lifecycle operations so the
/// authoritative write and its query projection cannot commit independently.
/// Content owns `document_state` and `deleted_at`; an existing knowledge row
/// copies both, including tombstones, rather than defining a second lifecycle.
pub async fn materialize_knowledge_document_from_current_head_with_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    document_id: Uuid,
) -> Result<MaterializeKnowledgeDocumentResult, sqlx::Error> {
    let Some((workspace_id, library_id)) = sqlx::query_as::<_, (Uuid, Uuid)>(
        "select workspace_id, library_id
         from content_document
         where id = $1",
    )
    .bind(document_id)
    .fetch_optional(&mut **transaction)
    .await?
    else {
        return Ok(MaterializeKnowledgeDocumentResult {
            outcome: MaterializeKnowledgeDocumentOutcome::DocumentNotFound,
            changed: false,
        });
    };

    if !super::catalog_repository::lock_library_for_lifecycle_event_with_executor(
        &mut **transaction,
        workspace_id,
        library_id,
    )
    .await?
    {
        return Ok(MaterializeKnowledgeDocumentResult {
            outcome: MaterializeKnowledgeDocumentOutcome::DocumentNotFound,
            changed: false,
        });
    }

    let Some((
        external_key,
        source_file_name,
        document_state,
        created_at,
        deleted_at,
        parent_document_id,
    )) = sqlx::query_as::<
        _,
        (String, Option<String>, String, DateTime<Utc>, Option<DateTime<Utc>>, Option<Uuid>),
    >(
        "select
                external_key,
                source_file_name,
                document_state::text,
                created_at,
                deleted_at,
                parent_document_id
             from content_document
             where id = $1
               and workspace_id = $2
               and library_id = $3
             for no key update",
    )
    .bind(document_id)
    .bind(workspace_id)
    .bind(library_id)
    .fetch_optional(&mut **transaction)
    .await?
    else {
        return Ok(MaterializeKnowledgeDocumentResult {
            outcome: MaterializeKnowledgeDocumentOutcome::DocumentNotFound,
            changed: false,
        });
    };

    // A non-active document whose projection row was reclaimed must not be
    // rebuilt. The projection's unique index over the external key has no
    // state predicate, so re-inserting the tombstone would take the key back
    // from the live document that now owns it — moving the duplicate-key loop
    // from ingest into the promote and repair paths. Deleting is terminal, so
    // a rebuilt tombstone would serve no reader either. An existing row is
    // still updated: that is the ordinary delete, which writes the tombstone.
    if document_state != "active"
        && !sqlx::query_scalar::<_, bool>(
            "select exists (select 1 from knowledge_document where document_id = $1)",
        )
        .bind(document_id)
        .fetch_one(&mut **transaction)
        .await?
    {
        return Ok(MaterializeKnowledgeDocumentResult {
            outcome: MaterializeKnowledgeDocumentOutcome::Materialized,
            changed: false,
        });
    }

    let Some((active_revision_id, readable_revision_id)) =
        sqlx::query_as::<_, (Option<Uuid>, Option<Uuid>)>(
            "select active_revision_id, readable_revision_id
             from content_document_head
             where document_id = $1
             for no key update",
        )
        .bind(document_id)
        .fetch_optional(&mut **transaction)
        .await?
    else {
        return Ok(MaterializeKnowledgeDocumentResult {
            outcome: MaterializeKnowledgeDocumentOutcome::DocumentHeadNotFound,
            changed: false,
        });
    };

    let head_revision_id = readable_revision_id.or(active_revision_id);
    let (head_mime_type, head_title, head_storage_key) = if let Some(revision_id) = head_revision_id
    {
        let revision = sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
            "select mime_type, title, storage_key
             from content_revision
             where id = $1
               and document_id = $2
               and workspace_id = $3
               and library_id = $4",
        )
        .bind(revision_id)
        .bind(document_id)
        .bind(workspace_id)
        .bind(library_id)
        .fetch_optional(&mut **transaction)
        .await?;
        let Some(revision) = revision else {
            return Ok(MaterializeKnowledgeDocumentResult {
                outcome: MaterializeKnowledgeDocumentOutcome::RevisionNotFound,
                changed: false,
            });
        };
        (Some(revision.0), revision.1, revision.2)
    } else {
        (None, None, None)
    };
    let latest_revision_no = sqlx::query_scalar::<_, Option<i64>>(
        "select max(revision_number)::bigint
         from content_revision
         where document_id = $1
           and workspace_id = $2
           and library_id = $3",
    )
    .bind(document_id)
    .bind(workspace_id)
    .bind(library_id)
    .fetch_one(&mut **transaction)
    .await?;

    let file_name = normalized_projection_file_name(
        &external_key,
        source_file_name.as_deref(),
        head_storage_key.as_deref(),
    );
    let is_raster_image = head_revision_id.is_some()
        && revision_is_raster_image(Some(&file_name), head_mime_type.as_deref());
    let document_role =
        derive_document_role(parent_document_id.is_some(), is_raster_image).to_string();

    let document_role_update = sqlx::query(
        "update content_document
         set document_role = $2
         where id = $1
           and workspace_id = $3
           and library_id = $4
           and document_role is distinct from $2",
    )
    .bind(document_id)
    .bind(&document_role)
    .bind(workspace_id)
    .bind(library_id)
    .execute(&mut **transaction)
    .await?;
    let projection_changed = sqlx::query_scalar::<_, Uuid>(
        "insert into knowledge_document (
            document_id, workspace_id, library_id, external_key, file_name, title,
            document_state, active_revision_id, readable_revision_id, latest_revision_no,
            parent_document_id, document_role, created_at, updated_at, deleted_at
         ) values (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
            $11, $12, $13, now(), $14
         )
         on conflict (document_id) do update
         set workspace_id = excluded.workspace_id,
             library_id = excluded.library_id,
             external_key = excluded.external_key,
             file_name = excluded.file_name,
             title = excluded.title,
             document_state = excluded.document_state,
             active_revision_id = excluded.active_revision_id,
             readable_revision_id = excluded.readable_revision_id,
             latest_revision_no = excluded.latest_revision_no,
             parent_document_id = excluded.parent_document_id,
             document_role = excluded.document_role,
             updated_at = excluded.updated_at,
             deleted_at = excluded.deleted_at
         where knowledge_document.workspace_id is distinct from excluded.workspace_id
            or knowledge_document.library_id is distinct from excluded.library_id
            or knowledge_document.external_key is distinct from excluded.external_key
            or knowledge_document.file_name is distinct from excluded.file_name
            or knowledge_document.title is distinct from excluded.title
            or knowledge_document.document_state is distinct from excluded.document_state
            or knowledge_document.active_revision_id is distinct from excluded.active_revision_id
            or knowledge_document.readable_revision_id is distinct from excluded.readable_revision_id
            or knowledge_document.latest_revision_no is distinct from excluded.latest_revision_no
            or knowledge_document.parent_document_id is distinct from excluded.parent_document_id
            or knowledge_document.document_role is distinct from excluded.document_role
            or knowledge_document.deleted_at is distinct from excluded.deleted_at
         returning document_id",
    )
    .bind(document_id)
    .bind(workspace_id)
    .bind(library_id)
    .bind(&external_key)
    .bind(&file_name)
    .bind(&head_title)
    .bind(&document_state)
    .bind(active_revision_id)
    .bind(readable_revision_id)
    .bind(latest_revision_no)
    .bind(parent_document_id)
    .bind(&document_role)
    .bind(created_at)
    .bind(deleted_at)
    .fetch_optional(&mut **transaction)
    .await?;

    Ok(MaterializeKnowledgeDocumentResult {
        outcome: MaterializeKnowledgeDocumentOutcome::Materialized,
        changed: document_role_update.rows_affected() > 0 || projection_changed.is_some(),
    })
}

/// Promotes the current document head and rematerializes its query projection in one
/// parent-first transaction. Callers supply authoritative promotion pointers;
/// null-head maintenance uses the conditional repair entry point below.
pub async fn promote_document_head_with_projection(
    postgres: &PgPool,
    new_head: &NewContentDocumentHead,
) -> Result<PromoteDocumentHeadOutcome, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let Some(scope) = resolve_promotion_scope(&mut transaction, new_head.document_id).await? else {
        transaction.rollback().await?;
        return Ok(PromoteDocumentHeadOutcome::DocumentNotFound);
    };
    let previous_head =
        get_document_head_for_update_with_executor(&mut *transaction, new_head.document_id).await?;
    // Fenced READY publication owns the applied item and its own transaction;
    // this unfenced promotion API is never allowed to release suppression.
    if new_head.active_revision_id.is_some() || new_head.readable_revision_id.is_some() {
        let suppressed = sqlx::query_scalar::<_, bool>(concat!(
            "select exists (select 1 from content_document_head h where h.document_id = $1 and (",
            super::content_document_head_repository::unresolved_head_suppression!(),
            "))",
        ))
        .bind(new_head.document_id)
        .fetch_one(&mut *transaction)
        .await?;
        if suppressed {
            return Ok(PromoteDocumentHeadOutcome::SuppressedUntilReady);
        }
    }
    promote_locked_head_with_projection(transaction, scope, previous_head.as_ref(), new_head).await
}

/// Reconciles failure without replaying an unlocked snapshot over a READY winner.
/// The failed mutation must still own the locked head; its pointers and successful
/// attempt identity are read only after the library/document/head locks are held.
///
/// # Errors
/// Returns database errors from locking or materializing the current projection.
pub async fn reconcile_failed_document_head_with_projection(
    postgres: &PgPool,
    document_id: Uuid,
    mutation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let Some(scope) = resolve_promotion_scope(&mut transaction, document_id).await? else {
        return Ok(());
    };
    let Some(head) =
        get_document_head_for_update_with_executor(&mut *transaction, document_id).await?
    else {
        return Ok(());
    };
    if head.latest_mutation_id != Some(mutation_id) {
        return Ok(());
    }
    let new_head = NewContentDocumentHead {
        publishing_mutation_item_id: None,
        document_id,
        active_revision_id: head.active_revision_id,
        readable_revision_id: head.readable_revision_id,
        latest_mutation_id: head.latest_mutation_id,
        latest_successful_attempt_id: head.latest_successful_attempt_id,
    };
    match promote_locked_head_with_projection(transaction, scope, Some(&head), &new_head).await? {
        PromoteDocumentHeadOutcome::Promoted(_) => Ok(()),
        other => {
            Err(sqlx::Error::Protocol(format!("locked failure reconciliation failed: {other:?}")))
        }
    }
}

/// Repairs a NULL head only if it is still eligible under the lifecycle locks.
/// Re-select the latest chunk-bearing revision here rather than trusting the
/// sweeper snapshot, and preserve operational identities on the locked head.
pub async fn repair_null_document_head_with_projection(
    postgres: &PgPool,
    document_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let Some(_scope) = resolve_promotion_scope(&mut transaction, document_id).await? else {
        return Ok(false);
    };
    let Some(head) =
        get_document_head_for_update_with_executor(&mut *transaction, document_id).await?
    else {
        return Ok(false);
    };
    let eligible = sqlx::query_scalar::<_, bool>(concat!(
        "select exists (
             select 1 from content_document_head h
             join content_document d on d.id = h.document_id
             where h.document_id = $1 and d.deleted_at is null
               and d.document_state <> 'deleted' and (",
        super::content_document_head_repository::null_head_repair_eligibility!(),
        ") )",
    ))
    .bind(document_id)
    .fetch_one(&mut *transaction)
    .await?;
    if !eligible {
        return Ok(false);
    }
    // A null readable head has no published item owner. Maintenance must not
    // manufacture one from the newest chunk-bearing historical revision.
    let _ = head;
    Ok(false)
}

async fn promote_locked_head_with_projection(
    mut transaction: Transaction<'_, Postgres>,
    scope: PromotionScope,
    previous_head: Option<&ContentDocumentHeadRow>,
    new_head: &NewContentDocumentHead,
) -> Result<PromoteDocumentHeadOutcome, sqlx::Error> {
    let head =
        match upsert_document_head_without_generation_outcome(&mut transaction, new_head).await? {
            ValidatedDocumentHeadWriteOutcome::Updated(head) => head,
            ValidatedDocumentHeadWriteOutcome::DocumentNotFound => {
                transaction.rollback().await?;
                return Ok(PromoteDocumentHeadOutcome::DocumentNotFound);
            }
            ValidatedDocumentHeadWriteOutcome::ReferenceIntegrityViolation => {
                transaction.rollback().await?;
                return Ok(PromoteDocumentHeadOutcome::ReferenceIntegrityViolation);
            }
        };
    let materialized = materialize_knowledge_document_from_current_head_with_transaction(
        &mut transaction,
        new_head.document_id,
    )
    .await?;
    match materialized.outcome {
        MaterializeKnowledgeDocumentOutcome::Materialized => {
            let readable_changed = previous_head.map_or_else(
                || head.readable_revision_id.is_some(),
                |previous| previous.readable_revision_id != head.readable_revision_id,
            );
            if readable_changed || materialized.changed {
                super::catalog_repository::touch_library_source_truth_version_with_executor(
                    &mut *transaction,
                    scope.library_id,
                )
                .await?;
            }
            transaction.commit().await?;
            Ok(PromoteDocumentHeadOutcome::Promoted(head))
        }
        MaterializeKnowledgeDocumentOutcome::RevisionNotFound => {
            transaction.rollback().await?;
            Ok(PromoteDocumentHeadOutcome::RevisionNotFound)
        }
        MaterializeKnowledgeDocumentOutcome::DocumentNotFound
        | MaterializeKnowledgeDocumentOutcome::DocumentHeadNotFound => {
            transaction.rollback().await?;
            Ok(PromoteDocumentHeadOutcome::DocumentNotFound)
        }
    }
}

struct PromotionScope {
    library_id: Uuid,
}

/// Locks the owning document, its library lifecycle row and the document row
/// itself before any head write. `None` means the document or its library lock
/// could not be resolved, so the caller rolls back and reports `DocumentNotFound`.
async fn resolve_promotion_scope(
    transaction: &mut Transaction<'_, Postgres>,
    document_id: Uuid,
) -> Result<Option<PromotionScope>, sqlx::Error> {
    let Some((workspace_id, library_id)) = sqlx::query_as::<_, (Uuid, Uuid)>(
        "select workspace_id, library_id from content_document where id = $1",
    )
    .bind(document_id)
    .fetch_optional(&mut **transaction)
    .await?
    else {
        return Ok(None);
    };
    if !super::catalog_repository::lock_library_for_lifecycle_event_with_executor(
        &mut **transaction,
        workspace_id,
        library_id,
    )
    .await?
    {
        return Ok(None);
    }
    if lock_document_by_id_with_executor(&mut **transaction, document_id).await?.is_none() {
        return Ok(None);
    }
    Ok(Some(PromotionScope { library_id }))
}
