use sqlx::{Executor, PgPool, Postgres};
use uuid::Uuid;

use super::types::{RuntimeGraphEvidenceLifecycleRow, RuntimeGraphEvidenceTargetRow};

/// Lists active runtime graph evidence lifecycle rows for one target.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the evidence rows.
pub async fn list_active_runtime_graph_evidence_lifecycle_by_target(
    pool: &PgPool,
    library_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
) -> Result<Vec<RuntimeGraphEvidenceLifecycleRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEvidenceLifecycleRow>(
        "select id, library_id, target_kind, target_id, document_id, revision_id,
            activated_by_attempt_id, chunk_id, source_file_name,
            page_ref, evidence_text, confidence_score, created_at
         from runtime_graph_evidence
         where library_id = $1
           and target_kind = $2
           and target_id = $3
         order by created_at desc, id desc",
    )
    .bind(library_id)
    .bind(target_kind)
    .bind(target_id)
    .fetch_all(pool)
    .await
}
/// Deletes graph evidence for the just-deleted document and self-heals any
/// orphan rows still surviving in the library.
///
/// Required contract: every `runtime_graph_evidence` row points at an
/// active `content_document`. The single-doc cleanup explicitly removes the
/// just-deleted doc's rows AND sweeps any rows in the same library whose
/// `document_id` is null (FK `ON DELETE SET NULL` orphan debris) or whose
/// referenced document is in `deleted` state — for example, evidence rows
/// stranded by an earlier delete whose graph-refresh failed soft and never
/// retried. Without this sweep those rows keep nodes alive forever via the
/// support-count recalculation.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the evidence rows.
pub async fn deactivate_runtime_graph_evidence_by_document(
    pool: &PgPool,
    library_id: Uuid,
    document_id: Uuid,
) -> Result<Vec<RuntimeGraphEvidenceTargetRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEvidenceTargetRow>(
        "delete from runtime_graph_evidence as evidence
         where evidence.library_id = $1
           and (
             evidence.document_id = $2
             or evidence.document_id is null
             or exists (
                 select 1 from content_document as document
                 where document.id = evidence.document_id
                   and document.library_id = $1
                   and (document.document_state = 'deleted' or document.deleted_at is not null)
             )
           )
         returning target_kind, target_id",
    )
    .bind(library_id)
    .bind(document_id)
    .fetch_all(pool)
    .await
}

/// Deletes graph evidence for a batch of just-deleted documents and self-heals
/// any orphan rows still surviving in the library.
///
/// Same shared contract as [`deactivate_runtime_graph_evidence_by_document`]:
/// the orphan sweep makes batch delete idempotent against past failures so a
/// once-stranded document cannot keep its supported nodes/edges visible.
pub async fn deactivate_runtime_graph_evidence_by_documents(
    pool: &PgPool,
    library_id: Uuid,
    document_ids: &[Uuid],
) -> Result<Vec<RuntimeGraphEvidenceTargetRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEvidenceTargetRow>(
        "delete from runtime_graph_evidence as evidence
         where evidence.library_id = $1
           and (
             evidence.document_id = any($2)
             or evidence.document_id is null
             or exists (
                 select 1 from content_document as document
                 where document.id = evidence.document_id
                   and document.library_id = $1
                   and (document.document_state = 'deleted' or document.deleted_at is not null)
             )
           )
         returning target_kind, target_id",
    )
    .bind(library_id)
    .bind(document_ids)
    .fetch_all(pool)
    .await
}

/// Lists document graph nodes for soft-deleted documents, including nodes
/// created before evidence was flushed.
///
/// Failed graph rebuilds can leave the source-document node without a
/// corresponding `runtime_graph_evidence` row. Delete convergence still must
/// target that node so the file leaves no graph trace.
///
/// Returns the document-typed nodes for the explicit `document_ids` AND any
/// document-typed node in the library whose backing `content_document` is in
/// `deleted` state. The latter self-heals stranded nodes from previously
/// failed cleanup runs.
pub async fn list_runtime_graph_document_node_targets_by_documents(
    pool: &PgPool,
    library_id: Uuid,
    document_ids: &[Uuid],
) -> Result<Vec<RuntimeGraphEvidenceTargetRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEvidenceTargetRow>(
        "select 'node'::text as target_kind, node.id as target_id
         from runtime_graph_node as node
         where node.library_id = $1
           and node.node_type = 'document'
           and (
             node.document_id = any($2)
             or exists (
                 select 1 from content_document as document
                 where document.id = node.document_id
                   and document.library_id = $1
                   and (document.document_state = 'deleted' or document.deleted_at is not null)
             )
           )",
    )
    .bind(library_id)
    .bind(document_ids)
    .fetch_all(pool)
    .await
}
/// Lists active graph evidence rows for one logical content revision.
///
/// # Errors
/// Returns any `SQLx` error raised while querying revision-scoped evidence rows.
pub async fn list_active_runtime_graph_evidence_by_content_revision(
    pool: &PgPool,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
) -> Result<Vec<RuntimeGraphEvidenceLifecycleRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEvidenceLifecycleRow>(
        "select id, library_id, target_kind, target_id, document_id, revision_id,
            activated_by_attempt_id, chunk_id, source_file_name,
            page_ref, evidence_text, confidence_score, created_at
         from runtime_graph_evidence
         where library_id = $1
           and document_id = $2
           and (revision_id = $3 or revision_id is null)
         order by created_at desc, id desc",
    )
    .bind(library_id)
    .bind(document_id)
    .bind(revision_id)
    .fetch_all(pool)
    .await
}

/// Lists target ids that still have active evidence outside one content revision.
///
/// # Errors
/// Returns any `SQLx` error raised while querying surviving evidence lineage.
pub async fn list_active_runtime_graph_target_ids_excluding_content_revision(
    pool: &PgPool,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    target_kind: &str,
    target_ids: &[Uuid],
) -> Result<Vec<Uuid>, sqlx::Error> {
    if target_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_scalar::<_, Uuid>(
        "select distinct target_id
         from runtime_graph_evidence
         where library_id = $1
           and target_kind = $4
           and target_id = any($5)
           and not (
                document_id = $2
                and (revision_id = $3 or revision_id is null)
           )
         order by target_id asc",
    )
    .bind(library_id)
    .bind(document_id)
    .bind(revision_id)
    .bind(target_kind)
    .bind(target_ids)
    .fetch_all(pool)
    .await
}

pub async fn deactivate_runtime_graph_evidence_by_content_revision_with_executor<'e, E>(
    executor: E,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
) -> Result<Vec<RuntimeGraphEvidenceTargetRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, RuntimeGraphEvidenceTargetRow>(
        "delete from runtime_graph_evidence
         where library_id = $1
           and document_id = $2
           and (revision_id = $3 or revision_id is null)
         returning target_kind, target_id",
    )
    .bind(library_id)
    .bind(document_id)
    .bind(revision_id)
    .fetch_all(executor)
    .await
}

/// One evidence row selected for retagging, with every field the identity key
/// is built from.
#[derive(sqlx::FromRow)]
struct RetaggableEvidenceRow {
    id: Uuid,
    target_kind: String,
    target_id: Uuid,
    document_id: Option<Uuid>,
    activated_by_attempt_id: Option<Uuid>,
    chunk_id: Option<Uuid>,
    page_ref: Option<String>,
    source_file_name: Option<String>,
    evidence_identity_key: String,
}

/// Named inputs for [`retag_runtime_graph_evidence_to_revision`].
///
/// The pair of kept-chunk slices is order-significant and positional, so the
/// named payload keeps every identifier and slice distinct at the call site.
pub struct RetagRuntimeGraphEvidenceInput<'a> {
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub previous_revision_id: Uuid,
    pub revision_id: Uuid,
    pub activated_by_attempt_id: Uuid,
    pub kept_previous_chunk_ids: &'a [Uuid],
    pub kept_revision_chunk_ids: &'a [Uuid],
}

/// Retags one document's evidence from a superseded revision to the revision
/// that now carries the same unchanged chunks, without deleting or re-inserting
/// it. Kept-chunk evidence moves to the new chunk id; evidence whose `chunk_id`
/// is null (document-level rows) only moves to the new revision and attempt.
///
/// The identity key is rebuilt in Rust from the row's own columns plus the
/// context tail recovered from the previous key, so the retagged row occupies
/// exactly the identity a full rebuild would produce for that target. The table
/// stores the context only inside the key, so it is the one field that cannot
/// be recovered by a plain SQL concatenation.
///
/// Returns the `(target_kind, target_id)` pairs that were carried over, so the
/// caller can account for their contribution and refresh summaries.
///
/// # Errors
/// Returns any `SQLx` error raised while selecting or updating the evidence rows.
pub async fn retag_runtime_graph_evidence_to_revision(
    connection: &mut sqlx::PgConnection,
    input: RetagRuntimeGraphEvidenceInput<'_>,
) -> Result<Vec<RuntimeGraphEvidenceTargetRow>, sqlx::Error> {
    let RetagRuntimeGraphEvidenceInput {
        library_id,
        document_id,
        previous_revision_id,
        revision_id,
        activated_by_attempt_id,
        kept_previous_chunk_ids,
        kept_revision_chunk_ids,
    } = input;
    let new_chunk_by_previous = kept_previous_chunk_ids
        .iter()
        .copied()
        .zip(kept_revision_chunk_ids.iter().copied())
        .collect::<std::collections::HashMap<_, _>>();
    let rows = sqlx::query_as::<_, RetaggableEvidenceRow>(
        "select id, target_kind, target_id, document_id, activated_by_attempt_id, chunk_id,
                page_ref, source_file_name, evidence_identity_key
         from runtime_graph_evidence
         where library_id = $1
           and document_id = $2
           and revision_id = $3
           and (chunk_id = any($4) or chunk_id is null)",
    )
    .bind(library_id)
    .bind(document_id)
    .bind(previous_revision_id)
    .bind(kept_previous_chunk_ids)
    .fetch_all(&mut *connection)
    .await?;

    let mut updates = Vec::with_capacity(rows.len());
    for row in rows {
        let previous_prefix = super::evidence_writes::runtime_graph_evidence_identity_prefix(
            super::evidence_writes::RuntimeGraphEvidenceIdentityInput {
                target_kind: &row.target_kind,
                target_id: row.target_id,
                document_id: row.document_id,
                revision_id: Some(previous_revision_id),
                activated_by_attempt_id: row.activated_by_attempt_id,
                chunk_id: row.chunk_id,
                page_ref: row.page_ref.as_deref(),
                source_file_name: row.source_file_name.as_deref(),
            },
        );
        let Some(evidence_context_key) = row.evidence_identity_key.strip_prefix(&previous_prefix)
        else {
            // A key that does not match its own columns is not ours to rewrite.
            continue;
        };
        let new_chunk_id =
            row.chunk_id.and_then(|previous| new_chunk_by_previous.get(&previous).copied());
        let new_key = super::evidence_writes::runtime_graph_evidence_identity_key(
            super::evidence_writes::RuntimeGraphEvidenceIdentityInput {
                target_kind: &row.target_kind,
                target_id: row.target_id,
                document_id: row.document_id,
                revision_id: Some(revision_id),
                activated_by_attempt_id: Some(activated_by_attempt_id),
                chunk_id: new_chunk_id,
                page_ref: row.page_ref.as_deref(),
                source_file_name: row.source_file_name.as_deref(),
            },
            evidence_context_key,
        );
        updates.push((row.id, new_key, new_chunk_id));
    }
    if updates.is_empty() {
        return Ok(Vec::new());
    }

    let mut builder = sqlx::QueryBuilder::<Postgres>::new(
        "update runtime_graph_evidence as evidence
         set revision_id = ",
    );
    builder.push_bind(revision_id);
    builder.push(", activated_by_attempt_id = ").push_bind(activated_by_attempt_id);
    builder
        .push(", chunk_id = updated.new_chunk_id, evidence_identity_key = updated.new_key from (");
    builder.push_values(&updates, |mut row, (id, new_key, new_chunk_id)| {
        row.push_bind(id).push_bind(new_key).push_bind(new_chunk_id);
    });
    builder.push(
        ") as updated(id, new_key, new_chunk_id)
         where evidence.id = updated.id
         returning evidence.target_kind, evidence.target_id",
    );
    builder.build_query_as::<RuntimeGraphEvidenceTargetRow>().fetch_all(&mut *connection).await
}
