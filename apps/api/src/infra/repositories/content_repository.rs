use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use sqlx::{Executor, FromRow, PgPool, Postgres, QueryBuilder, Transaction};
use uuid::Uuid;

use crate::shared::versioning::dotted_version_terms;

/// Canonical CASE expression that derives the five status buckets the
/// documents surface exposes (`canceled` / `failed` / `processing` /
/// `queued` / `ready`) from Postgres-only signals. One source of
/// truth so the list page, the status-count aggregate, and every
/// ad-hoc caller stay aligned.
///
/// Priority (top row wins):
/// 1. Mutation is terminally failed / conflicted → `failed`.
///    The head itself is broken; the operator must see this.
/// 2. Latest ingest_job is `failed` → `failed`.
/// 3. Latest ingest_job is `leased` → `processing`. A worker is
///    actively running this document; surface it regardless of
///    whether a previous readable revision exists, so the operator
///    can see the pipeline moving even during bulk re-ingest.
/// 4. `content_document_head.readable_revision_id` is set → `ready`.
///    The document has a usable revision the user can consume
///    right now. `ready` wins over `canceled` / `queued`:
///    a canceled or queued re-ingest over a still-readable
///    document should not hide it from the ready bucket. Otherwise
///    canceled fan-out jobs can dominate the pick during bulk
///    re-ingest.
/// 5. Latest ingest_job or mutation is `canceled` → `canceled` (no
///    readable, work was canceled before finishing).
/// 6. Latest ingest_job is `queued` → `queued` (new document
///    waiting for its first ingest; no readable yet).
/// 7. Mutation state is `accepted` / `running` → `processing`.
/// 8. Latest ingest_job is `completed` but no readable → `failed`
///    (post-completion head update did not land; surface the anomaly).
/// 9. Everything else → `queued`.
///
/// Requires the hosting query to expose `ij.queue_state`,
/// `m.mutation_state`, and `h.readable_revision_id` under exactly
/// those aliases (both current callers do). `ij.queue_state` must
/// be picked from this document's newest mutation, with state
/// priority only as a retry tie-breaker inside that mutation — see
/// `list_document_page_rows` for the reference implementation.
pub(crate) const DERIVED_STATUS_CASE_SQL: &str = "case
    when m.mutation_state in ('failed','conflicted') then 'failed'
    when ij.queue_state = 'failed' then 'failed'
    when ij.queue_state = 'leased' then 'processing'
    when h.readable_revision_id is not null then 'ready'
    when ij.queue_state = 'canceled' then 'canceled'
    when m.mutation_state = 'canceled' then 'canceled'
    when ij.queue_state = 'queued' then 'queued'
    when m.mutation_state in ('accepted','running') then 'processing'
    when ij.queue_state = 'completed' then 'failed'
    else 'queued'
end";

#[derive(Debug, Clone, FromRow)]
pub struct ContentDocumentRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub external_key: String,
    pub document_state: String,
    pub created_by_principal_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct ContentDocumentHeadRow {
    pub document_id: Uuid,
    pub active_revision_id: Option<Uuid>,
    pub readable_revision_id: Option<Uuid>,
    pub latest_mutation_id: Option<Uuid>,
    pub latest_successful_attempt_id: Option<Uuid>,
    pub head_updated_at: DateTime<Utc>,
    pub document_summary: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
pub struct ContentRevisionRow {
    pub id: Uuid,
    pub document_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub revision_number: i32,
    pub parent_revision_id: Option<Uuid>,
    pub content_source_kind: String,
    pub checksum: String,
    pub mime_type: String,
    pub byte_size: i64,
    pub title: Option<String>,
    pub language_code: Option<String>,
    pub source_uri: Option<String>,
    pub storage_key: Option<String>,
    pub created_by_principal_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct ContentChunkRow {
    pub id: Uuid,
    pub revision_id: Uuid,
    pub chunk_index: i32,
    pub start_offset: i32,
    pub end_offset: i32,
    pub token_count: Option<i32>,
    pub normalized_text: String,
    pub text_checksum: String,
    /// Earliest record timestamp aggregated into this chunk (JSONL ingest
    /// only; NULL for non-temporal sources like PDF/image/markdown).
    pub occurred_at: Option<DateTime<Utc>>,
    /// Latest record timestamp aggregated into this chunk. For
    /// single-record chunks `occurred_until == occurred_at`. NULL when
    /// `occurred_at` is NULL.
    pub occurred_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct ContentMutationRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub operation_kind: String,
    pub requested_by_principal_id: Option<Uuid>,
    pub request_surface: String,
    pub idempotency_key: Option<String>,
    pub source_identity: Option<String>,
    pub mutation_state: String,
    pub requested_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub failure_code: Option<String>,
    pub conflict_code: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
pub struct ContentMutationItemRow {
    pub id: Uuid,
    pub mutation_id: Uuid,
    pub document_id: Option<Uuid>,
    pub base_revision_id: Option<Uuid>,
    pub result_revision_id: Option<Uuid>,
    pub item_state: String,
    pub message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewContentDocument<'a> {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub external_key: &'a str,
    pub document_state: &'a str,
    pub created_by_principal_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct NewContentDocumentHead {
    pub document_id: Uuid,
    pub active_revision_id: Option<Uuid>,
    pub readable_revision_id: Option<Uuid>,
    pub latest_mutation_id: Option<Uuid>,
    pub latest_successful_attempt_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct NewContentRevision<'a> {
    pub document_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub revision_number: i32,
    pub parent_revision_id: Option<Uuid>,
    pub content_source_kind: &'a str,
    pub checksum: &'a str,
    pub mime_type: &'a str,
    pub byte_size: i64,
    pub title: Option<&'a str>,
    pub language_code: Option<&'a str>,
    pub source_uri: Option<&'a str>,
    pub storage_key: Option<&'a str>,
    pub created_by_principal_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct NewContentChunk<'a> {
    pub revision_id: Uuid,
    pub chunk_index: i32,
    pub start_offset: i32,
    pub end_offset: i32,
    pub token_count: Option<i32>,
    pub normalized_text: &'a str,
    pub text_checksum: &'a str,
    /// Earliest record timestamp aggregated into this chunk (JSONL ingest
    /// only; None for non-temporal sources). Computed via the canonical
    /// `record_jsonl::extract_chunk_temporal_bounds` helper.
    pub occurred_at: Option<DateTime<Utc>>,
    /// Latest record timestamp aggregated into this chunk. Equals
    /// `occurred_at` for single-record chunks; None when `occurred_at`
    /// is None.
    pub occurred_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewContentMutation<'a> {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub operation_kind: &'a str,
    pub requested_by_principal_id: Option<Uuid>,
    pub request_surface: &'a str,
    pub idempotency_key: Option<&'a str>,
    pub source_identity: Option<&'a str>,
    pub mutation_state: &'a str,
    pub failure_code: Option<&'a str>,
    pub conflict_code: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct NewContentMutationItem<'a> {
    pub mutation_id: Uuid,
    pub document_id: Option<Uuid>,
    pub base_revision_id: Option<Uuid>,
    pub result_revision_id: Option<Uuid>,
    pub item_state: &'a str,
    pub message: Option<&'a str>,
}

pub async fn list_documents_by_library(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<Vec<ContentDocumentRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentDocumentRow>(
        "select
            id,
            workspace_id,
            library_id,
            external_key,
            document_state::text as document_state,
            created_by_principal_id,
            created_at,
            deleted_at
         from content_document
         where library_id = $1
         order by created_at desc",
    )
    .bind(library_id)
    .fetch_all(postgres)
    .await
}

pub async fn get_document_by_id(
    postgres: &PgPool,
    document_id: Uuid,
) -> Result<Option<ContentDocumentRow>, sqlx::Error> {
    get_document_by_id_with_executor(postgres, document_id).await
}

pub async fn get_document_by_id_with_executor<'e, E>(
    executor: E,
    document_id: Uuid,
) -> Result<Option<ContentDocumentRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, ContentDocumentRow>(
        "select
            id,
            workspace_id,
            library_id,
            external_key,
            document_state::text as document_state,
            created_by_principal_id,
            created_at,
            deleted_at
         from content_document
         where id = $1",
    )
    .bind(document_id)
    .fetch_optional(executor)
    .await
}

pub async fn get_document_by_external_key(
    postgres: &PgPool,
    library_id: Uuid,
    external_key: &str,
) -> Result<Option<ContentDocumentRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentDocumentRow>(
        "select
            id,
            workspace_id,
            library_id,
            external_key,
            document_state::text as document_state,
            created_by_principal_id,
            created_at,
            deleted_at
         from content_document
         where library_id = $1
           and external_key = $2
         order by created_at desc, id desc
         limit 1",
    )
    .bind(library_id)
    .bind(external_key)
    .fetch_optional(postgres)
    .await
}

pub async fn create_document(
    postgres: &PgPool,
    new_document: &NewContentDocument<'_>,
) -> Result<ContentDocumentRow, sqlx::Error> {
    create_document_with_executor(postgres, new_document).await
}

pub async fn create_document_with_executor<'e, E>(
    executor: E,
    new_document: &NewContentDocument<'_>,
) -> Result<ContentDocumentRow, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, ContentDocumentRow>(
        "insert into content_document (
            id,
            workspace_id,
            library_id,
            external_key,
            document_state,
            created_by_principal_id,
            created_at,
            deleted_at
        )
        values ($1, $2, $3, $4, $5::content_document_state, $6, now(), null)
        returning
            id,
            workspace_id,
            library_id,
            external_key,
            document_state::text as document_state,
            created_by_principal_id,
            created_at,
            deleted_at",
    )
    .bind(Uuid::now_v7())
    .bind(new_document.workspace_id)
    .bind(new_document.library_id)
    .bind(new_document.external_key)
    .bind(new_document.document_state)
    .bind(new_document.created_by_principal_id)
    .fetch_one(executor)
    .await
}

pub async fn update_document_state(
    postgres: &PgPool,
    document_id: Uuid,
    document_state: &str,
    deleted_at: Option<DateTime<Utc>>,
) -> Result<Option<ContentDocumentRow>, sqlx::Error> {
    update_document_state_with_executor(postgres, document_id, document_state, deleted_at).await
}

pub async fn update_document_state_with_executor<'e, E>(
    executor: E,
    document_id: Uuid,
    document_state: &str,
    deleted_at: Option<DateTime<Utc>>,
) -> Result<Option<ContentDocumentRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, ContentDocumentRow>(
        "update content_document
         set document_state = $2::content_document_state,
             deleted_at = $3
         where id = $1
         returning
            id,
            workspace_id,
            library_id,
            external_key,
            document_state::text as document_state,
            created_by_principal_id,
            created_at,
            deleted_at",
    )
    .bind(document_id)
    .bind(document_state)
    .bind(deleted_at)
    .fetch_optional(executor)
    .await
}

/// Dedup lookup used by upload and web-ingest paths: is there already a
/// non-deleted document in this library whose content hashes to
/// `checksum`? Returns the canonical "winner" — the document with a
/// healthy `readable_revision_id` if one exists, falling back to the
/// earliest candidate. Relies on `idx_content_revision_library_checksum`.
///
/// Best-effort: not wrapped in an advisory lock. Two concurrent ingests
/// of the same bytes within a ~100ms window can both see "no
/// duplicate" and both admit — but that race is dominated by the
/// normal case (sequential re-uploads, web-crawl worker is
/// single-threaded per run) and not what operators were hitting. If a
/// race-proof variant is needed later, wrap this in
/// `pg_advisory_xact_lock(hash(library_id, checksum))` and move the
/// subsequent document create into the same transaction.
pub async fn find_active_document_by_library_checksum(
    postgres: &PgPool,
    library_id: Uuid,
    checksum: &str,
) -> Result<Option<Uuid>, sqlx::Error> {
    // Match against each document's LATEST revision only. Matching any
    // historical revision produced false positives: a document whose
    // older revision briefly equalled another body (e.g. a site's
    // login-required placeholder served transiently for many URLs)
    // would collide forever, even after its own content diverged. The
    // DISTINCT ON pins us to "is this the same body RIGHT NOW".
    sqlx::query_scalar::<_, Uuid>(
        "with latest_revision as (
             select distinct on (r.document_id)
                 r.document_id,
                 r.checksum
             from content_revision r
             order by r.document_id, r.created_at desc
         )
         select d.id
         from content_document d
         join latest_revision lr on lr.document_id = d.id
         left join content_document_head h on h.document_id = d.id
         where d.library_id = $1
           and lr.checksum = $2
           and d.document_state <> 'deleted'
           and d.deleted_at is null
         order by (h.readable_revision_id is not null) desc,
                  d.created_at asc,
                  d.id asc
         limit 1",
    )
    .bind(library_id)
    .bind(checksum)
    .fetch_optional(postgres)
    .await
}

pub async fn get_document_head(
    postgres: &PgPool,
    document_id: Uuid,
) -> Result<Option<ContentDocumentHeadRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentDocumentHeadRow>(
        "select
            document_id,
            active_revision_id,
            readable_revision_id,
            latest_mutation_id,
            latest_successful_attempt_id,
            head_updated_at,
            document_summary
         from content_document_head
         where document_id = $1",
    )
    .bind(document_id)
    .fetch_optional(postgres)
    .await
}

pub async fn list_document_heads_by_document_ids(
    postgres: &PgPool,
    document_ids: &[Uuid],
) -> Result<Vec<ContentDocumentHeadRow>, sqlx::Error> {
    if document_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, ContentDocumentHeadRow>(
        "select
            document_id,
            active_revision_id,
            readable_revision_id,
            latest_mutation_id,
            latest_successful_attempt_id,
            head_updated_at,
            document_summary
         from content_document_head
         where document_id = any($1)",
    )
    .bind(document_ids)
    .fetch_all(postgres)
    .await
}

pub async fn upsert_document_head(
    postgres: &PgPool,
    new_head: &NewContentDocumentHead,
) -> Result<ContentDocumentHeadRow, sqlx::Error> {
    upsert_document_head_with_executor(postgres, new_head).await
}

pub async fn upsert_document_head_with_executor<'e, E>(
    executor: E,
    new_head: &NewContentDocumentHead,
) -> Result<ContentDocumentHeadRow, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, ContentDocumentHeadRow>(
        "insert into content_document_head (
            document_id,
            active_revision_id,
            readable_revision_id,
            latest_mutation_id,
            latest_successful_attempt_id,
            head_updated_at
        )
        values ($1, $2, $3, $4, $5, now())
        on conflict (document_id) do update
        set active_revision_id = excluded.active_revision_id,
            readable_revision_id = excluded.readable_revision_id,
            latest_mutation_id = excluded.latest_mutation_id,
            latest_successful_attempt_id = excluded.latest_successful_attempt_id,
            head_updated_at = now()
        returning
            document_id,
            active_revision_id,
            readable_revision_id,
            latest_mutation_id,
            latest_successful_attempt_id,
            head_updated_at,
            document_summary",
    )
    .bind(new_head.document_id)
    .bind(new_head.active_revision_id)
    .bind(new_head.readable_revision_id)
    .bind(new_head.latest_mutation_id)
    .bind(new_head.latest_successful_attempt_id)
    .fetch_one(executor)
    .await
}

pub async fn update_document_summary(
    postgres: &PgPool,
    document_id: Uuid,
    summary: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update content_document_head
         set document_summary = $2
         where document_id = $1",
    )
    .bind(document_id)
    .bind(summary)
    .execute(postgres)
    .await?;
    Ok(())
}

pub async fn get_library_readable_content_fingerprint(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<String, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        "with readable_heads as (
            select
                document.id as document_id,
                document.external_key,
                coalesce(head.readable_revision_id, head.active_revision_id) as revision_id
            from content_document as document
            left join content_document_head as head
              on head.document_id = document.id
            where document.library_id = $1
              and document.document_state = 'active'
              and document.deleted_at is null
        ),
        chunk_fingerprints as (
            select
                chunk.revision_id,
                count(*)::bigint as chunk_count,
                md5(string_agg(
                    array_to_string(
                        array[
                            chunk.chunk_index::text,
                            chunk.text_checksum
                        ],
                        chr(31),
                        ''
                    ),
                    chr(30)
                    order by chunk.chunk_index, chunk.id
                )) as chunk_fingerprint
            from content_chunk as chunk
            where chunk.revision_id in (
                select revision_id
                from readable_heads
                where revision_id is not null
            )
            group by chunk.revision_id
        ),
        document_fingerprints as (
            select
                head.document_id,
                array_to_string(
                    array[
                        head.document_id::text,
                        head.external_key,
                        coalesce(head.revision_id::text, ''),
                        coalesce(revision.revision_number::text, ''),
                        coalesce(revision.checksum, ''),
                        coalesce(revision.mime_type, ''),
                        coalesce(revision.byte_size::text, ''),
                        coalesce(revision.title, ''),
                        coalesce(revision.source_uri, ''),
                        coalesce(chunks.chunk_count::text, '0'),
                        coalesce(chunks.chunk_fingerprint, '')
                    ],
                    chr(31),
                    ''
                ) as fingerprint_part
            from readable_heads as head
            left join content_revision as revision
              on revision.id = head.revision_id
            left join chunk_fingerprints as chunks
              on chunks.revision_id = head.revision_id
        )
        select coalesce(
            md5(string_agg(
                fingerprint_part,
                chr(30)
                order by document_id
            )),
            md5('empty')
        )
        from document_fingerprints",
    )
    .bind(library_id)
    .fetch_one(postgres)
    .await
}

pub async fn list_revisions_by_document(
    postgres: &PgPool,
    document_id: Uuid,
) -> Result<Vec<ContentRevisionRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentRevisionRow>(
        "select
            id,
            document_id,
            workspace_id,
            library_id,
            revision_number,
            parent_revision_id,
            content_source_kind::text as content_source_kind,
            checksum,
            mime_type,
            byte_size,
            title,
            language_code,
            source_uri,
            storage_key,
            created_by_principal_id,
            created_at
         from content_revision
         where document_id = $1
         order by revision_number desc, created_at desc",
    )
    .bind(document_id)
    .fetch_all(postgres)
    .await
}

pub async fn get_revision_by_id(
    postgres: &PgPool,
    revision_id: Uuid,
) -> Result<Option<ContentRevisionRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentRevisionRow>(
        "select
            id,
            document_id,
            workspace_id,
            library_id,
            revision_number,
            parent_revision_id,
            content_source_kind::text as content_source_kind,
            checksum,
            mime_type,
            byte_size,
            title,
            language_code,
            source_uri,
            storage_key,
            created_by_principal_id,
            created_at
         from content_revision
         where id = $1",
    )
    .bind(revision_id)
    .fetch_optional(postgres)
    .await
}

pub async fn update_revision_storage_key(
    postgres: &PgPool,
    revision_id: Uuid,
    storage_key: Option<&str>,
) -> Result<Option<ContentRevisionRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentRevisionRow>(
        "update content_revision
         set storage_key = $2
         where id = $1
         returning
            id,
            document_id,
            workspace_id,
            library_id,
            revision_number,
            parent_revision_id,
            content_source_kind::text as content_source_kind,
            checksum,
            mime_type,
            byte_size,
            title,
            language_code,
            source_uri,
            storage_key,
            created_by_principal_id,
            created_at",
    )
    .bind(revision_id)
    .bind(storage_key)
    .fetch_optional(postgres)
    .await
}

pub async fn list_revisions_by_ids(
    postgres: &PgPool,
    revision_ids: &[Uuid],
) -> Result<Vec<ContentRevisionRow>, sqlx::Error> {
    if revision_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, ContentRevisionRow>(
        "select
            id,
            document_id,
            workspace_id,
            library_id,
            revision_number,
            parent_revision_id,
            content_source_kind::text as content_source_kind,
            checksum,
            mime_type,
            byte_size,
            title,
            language_code,
            source_uri,
            storage_key,
            created_by_principal_id,
            created_at
         from content_revision
         where id = any($1)",
    )
    .bind(revision_ids)
    .fetch_all(postgres)
    .await
}

pub async fn get_latest_revision_for_document(
    postgres: &PgPool,
    document_id: Uuid,
) -> Result<Option<ContentRevisionRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentRevisionRow>(
        "select
            id,
            document_id,
            workspace_id,
            library_id,
            revision_number,
            parent_revision_id,
            content_source_kind::text as content_source_kind,
            checksum,
            mime_type,
            byte_size,
            title,
            language_code,
            source_uri,
            storage_key,
            created_by_principal_id,
            created_at
         from content_revision
         where document_id = $1
         order by revision_number desc, created_at desc
         limit 1",
    )
    .bind(document_id)
    .fetch_optional(postgres)
    .await
}

pub async fn create_revision(
    postgres: &PgPool,
    new_revision: &NewContentRevision<'_>,
) -> Result<ContentRevisionRow, sqlx::Error> {
    sqlx::query_as::<_, ContentRevisionRow>(
        "insert into content_revision (
            id,
            document_id,
            workspace_id,
            library_id,
            revision_number,
            parent_revision_id,
            content_source_kind,
            checksum,
            mime_type,
            byte_size,
            title,
            language_code,
            source_uri,
            storage_key,
            created_by_principal_id,
            created_at
        )
        values (
            $1,
            $2,
            $3,
            $4,
            $5,
            $6,
            $7::content_source_kind,
            $8,
            $9,
            $10,
            $11,
            $12,
            $13,
            $14,
            $15,
            now()
        )
        returning
            id,
            document_id,
            workspace_id,
            library_id,
            revision_number,
            parent_revision_id,
            content_source_kind::text as content_source_kind,
            checksum,
            mime_type,
            byte_size,
            title,
            language_code,
            source_uri,
            storage_key,
            created_by_principal_id,
            created_at",
    )
    .bind(Uuid::now_v7())
    .bind(new_revision.document_id)
    .bind(new_revision.workspace_id)
    .bind(new_revision.library_id)
    .bind(new_revision.revision_number)
    .bind(new_revision.parent_revision_id)
    .bind(new_revision.content_source_kind)
    .bind(new_revision.checksum)
    .bind(new_revision.mime_type)
    .bind(new_revision.byte_size)
    .bind(new_revision.title)
    .bind(new_revision.language_code)
    .bind(new_revision.source_uri)
    .bind(new_revision.storage_key)
    .bind(new_revision.created_by_principal_id)
    .fetch_one(postgres)
    .await
}

pub async fn list_chunks_by_revision(
    postgres: &PgPool,
    revision_id: Uuid,
) -> Result<Vec<ContentChunkRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentChunkRow>(
        "select
            id,
            revision_id,
            chunk_index,
            start_offset,
            end_offset,
            token_count,
            normalized_text,
            text_checksum,
            occurred_at,
            occurred_until
         from content_chunk
         where revision_id = $1
         order by chunk_index asc",
    )
    .bind(revision_id)
    .fetch_all(postgres)
    .await
}

pub async fn get_chunk_by_id(
    postgres: &PgPool,
    chunk_id: Uuid,
) -> Result<Option<ContentChunkRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentChunkRow>(
        "select
            id,
            revision_id,
            chunk_index,
            start_offset,
            end_offset,
            token_count,
            normalized_text,
            text_checksum,
            occurred_at,
            occurred_until
         from content_chunk
         where id = $1",
    )
    .bind(chunk_id)
    .fetch_optional(postgres)
    .await
}

pub async fn create_chunk(
    postgres: &PgPool,
    new_chunk: &NewContentChunk<'_>,
) -> Result<ContentChunkRow, sqlx::Error> {
    sqlx::query_as::<_, ContentChunkRow>(
        "insert into content_chunk (
            id,
            revision_id,
            chunk_index,
            start_offset,
            end_offset,
            token_count,
            normalized_text,
            text_checksum,
            occurred_at,
            occurred_until
        )
        values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
        returning
            id,
            revision_id,
            chunk_index,
            start_offset,
            end_offset,
            token_count,
            normalized_text,
            text_checksum,
            occurred_at,
            occurred_until",
    )
    .bind(Uuid::now_v7())
    .bind(new_chunk.revision_id)
    .bind(new_chunk.chunk_index)
    .bind(new_chunk.start_offset)
    .bind(new_chunk.end_offset)
    .bind(new_chunk.token_count)
    .bind(new_chunk.normalized_text)
    .bind(new_chunk.text_checksum)
    .bind(new_chunk.occurred_at)
    .bind(new_chunk.occurred_until)
    .fetch_one(postgres)
    .await
}

pub async fn create_chunks(
    postgres: &PgPool,
    new_chunks: &[NewContentChunk<'_>],
) -> Result<Vec<ContentChunkRow>, sqlx::Error> {
    if new_chunks.is_empty() {
        return Ok(Vec::new());
    }

    const POSTGRES_MAX_BIND_PARAMETERS: usize = 65_535;
    const CONTENT_CHUNK_INSERT_BIND_COUNT: usize = 10;
    const CONTENT_CHUNK_INSERT_BATCH_SIZE: usize =
        POSTGRES_MAX_BIND_PARAMETERS / CONTENT_CHUNK_INSERT_BIND_COUNT;

    let mut created_chunks = Vec::with_capacity(new_chunks.len());
    for chunk_batch in new_chunks.chunks(CONTENT_CHUNK_INSERT_BATCH_SIZE) {
        let mut batch_rows = create_chunk_batch(postgres, chunk_batch).await?;
        created_chunks.append(&mut batch_rows);
    }
    created_chunks.sort_by_key(|chunk| chunk.chunk_index);
    Ok(created_chunks)
}

async fn create_chunk_batch(
    postgres: &PgPool,
    new_chunks: &[NewContentChunk<'_>],
) -> Result<Vec<ContentChunkRow>, sqlx::Error> {
    let mut builder = QueryBuilder::<Postgres>::new(
        "insert into content_chunk (
            id,
            revision_id,
            chunk_index,
            start_offset,
            end_offset,
            token_count,
            normalized_text,
            text_checksum,
            occurred_at,
            occurred_until
        ) ",
    );

    builder.push_values(new_chunks.iter(), |mut row, new_chunk| {
        row.push_bind(Uuid::now_v7())
            .push_bind(new_chunk.revision_id)
            .push_bind(new_chunk.chunk_index)
            .push_bind(new_chunk.start_offset)
            .push_bind(new_chunk.end_offset)
            .push_bind(new_chunk.token_count)
            .push_bind(new_chunk.normalized_text)
            .push_bind(new_chunk.text_checksum)
            .push_bind(new_chunk.occurred_at)
            .push_bind(new_chunk.occurred_until);
    });

    builder.push(
        " returning
            id,
            revision_id,
            chunk_index,
            start_offset,
            end_offset,
            token_count,
            normalized_text,
            text_checksum,
            occurred_at,
            occurred_until",
    );

    builder.build_query_as::<ContentChunkRow>().fetch_all(postgres).await
}

pub async fn delete_chunks_by_revision(
    postgres: &PgPool,
    revision_id: Uuid,
) -> Result<u64, sqlx::Error> {
    sqlx::query("delete from content_chunk where revision_id = $1")
        .bind(revision_id)
        .execute(postgres)
        .await
        .map(|result| result.rows_affected())
}

pub async fn list_mutations_by_library(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<Vec<ContentMutationRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentMutationRow>(
        "select
            id,
            workspace_id,
            library_id,
            operation_kind::text as operation_kind,
            requested_by_principal_id,
            request_surface::text as request_surface,
            idempotency_key,
            source_identity,
            mutation_state::text as mutation_state,
            requested_at,
            completed_at,
            failure_code,
            conflict_code
         from content_mutation
         where library_id = $1
         order by requested_at desc",
    )
    .bind(library_id)
    .fetch_all(postgres)
    .await
}

pub async fn get_mutation_by_id(
    postgres: &PgPool,
    mutation_id: Uuid,
) -> Result<Option<ContentMutationRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentMutationRow>(
        "select
            id,
            workspace_id,
            library_id,
            operation_kind::text as operation_kind,
            requested_by_principal_id,
            request_surface::text as request_surface,
            idempotency_key,
            source_identity,
            mutation_state::text as mutation_state,
            requested_at,
            completed_at,
            failure_code,
            conflict_code
         from content_mutation
         where id = $1",
    )
    .bind(mutation_id)
    .fetch_optional(postgres)
    .await
}

pub async fn list_mutations_by_ids(
    postgres: &PgPool,
    mutation_ids: &[Uuid],
) -> Result<Vec<ContentMutationRow>, sqlx::Error> {
    if mutation_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, ContentMutationRow>(
        "select
            id,
            workspace_id,
            library_id,
            operation_kind::text as operation_kind,
            requested_by_principal_id,
            request_surface::text as request_surface,
            idempotency_key,
            source_identity,
            mutation_state::text as mutation_state,
            requested_at,
            completed_at,
            failure_code,
            conflict_code
         from content_mutation
         where id = any($1)",
    )
    .bind(mutation_ids)
    .fetch_all(postgres)
    .await
}

pub async fn find_mutation_by_idempotency(
    postgres: &PgPool,
    requested_by_principal_id: Uuid,
    request_surface: &str,
    idempotency_key: &str,
) -> Result<Option<ContentMutationRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentMutationRow>(
        "select
            id,
            workspace_id,
            library_id,
            operation_kind::text as operation_kind,
            requested_by_principal_id,
            request_surface::text as request_surface,
            idempotency_key,
            source_identity,
            mutation_state::text as mutation_state,
            requested_at,
            completed_at,
            failure_code,
            conflict_code
         from content_mutation
         where requested_by_principal_id = $1
           and request_surface = $2::surface_kind
           and idempotency_key = $3",
    )
    .bind(requested_by_principal_id)
    .bind(request_surface)
    .bind(idempotency_key)
    .fetch_optional(postgres)
    .await
}

pub async fn create_mutation(
    postgres: &PgPool,
    new_mutation: &NewContentMutation<'_>,
) -> Result<ContentMutationRow, sqlx::Error> {
    sqlx::query_as::<_, ContentMutationRow>(
        "insert into content_mutation (
            id,
            workspace_id,
            library_id,
            operation_kind,
            requested_by_principal_id,
            request_surface,
            idempotency_key,
            source_identity,
            mutation_state,
            requested_at,
            completed_at,
            failure_code,
            conflict_code
        )
        values (
            $1,
            $2,
            $3,
            $4::content_mutation_operation_kind,
            $5,
            $6::surface_kind,
            $7,
            $8,
            $9::content_mutation_state,
            now(),
            null,
            $10,
            $11
        )
        returning
            id,
            workspace_id,
            library_id,
            operation_kind::text as operation_kind,
            requested_by_principal_id,
            request_surface::text as request_surface,
            idempotency_key,
            source_identity,
            mutation_state::text as mutation_state,
            requested_at,
            completed_at,
            failure_code,
            conflict_code",
    )
    .bind(Uuid::now_v7())
    .bind(new_mutation.workspace_id)
    .bind(new_mutation.library_id)
    .bind(new_mutation.operation_kind)
    .bind(new_mutation.requested_by_principal_id)
    .bind(new_mutation.request_surface)
    .bind(new_mutation.idempotency_key)
    .bind(new_mutation.source_identity)
    .bind(new_mutation.mutation_state)
    .bind(new_mutation.failure_code)
    .bind(new_mutation.conflict_code)
    .fetch_one(postgres)
    .await
}

pub async fn acquire_content_mutation_lock(
    postgres: &PgPool,
    mutation_id: Uuid,
) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1::text, 0))")
        .bind(format!("content.mutation:{mutation_id}"))
        .execute(&mut *transaction)
        .await?;
    Ok(transaction)
}

pub async fn acquire_content_document_lock(
    postgres: &PgPool,
    document_id: Uuid,
) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1::text, 0))")
        .bind(format!("content.document:{document_id}"))
        .execute(&mut *transaction)
        .await?;
    Ok(transaction)
}

pub async fn release_content_mutation_lock(
    transaction: Transaction<'static, Postgres>,
    _mutation_id: Uuid,
) -> Result<(), sqlx::Error> {
    transaction.commit().await
}

pub async fn release_content_document_lock(
    transaction: Transaction<'static, Postgres>,
    _document_id: Uuid,
) -> Result<(), sqlx::Error> {
    transaction.commit().await
}

pub async fn update_mutation_status(
    postgres: &PgPool,
    mutation_id: Uuid,
    mutation_state: &str,
    completed_at: Option<DateTime<Utc>>,
    failure_code: Option<&str>,
    conflict_code: Option<&str>,
) -> Result<Option<ContentMutationRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentMutationRow>(
        "update content_mutation
         set mutation_state = $2::content_mutation_state,
             completed_at = $3,
             failure_code = $4,
             conflict_code = $5
         where id = $1
         returning
            id,
            workspace_id,
            library_id,
            operation_kind::text as operation_kind,
            requested_by_principal_id,
            request_surface::text as request_surface,
            idempotency_key,
            source_identity,
            mutation_state::text as mutation_state,
            requested_at,
            completed_at,
            failure_code,
            conflict_code",
    )
    .bind(mutation_id)
    .bind(mutation_state)
    .bind(completed_at)
    .bind(failure_code)
    .bind(conflict_code)
    .fetch_optional(postgres)
    .await
}

pub async fn list_mutation_items(
    postgres: &PgPool,
    mutation_id: Uuid,
) -> Result<Vec<ContentMutationItemRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentMutationItemRow>(
        "select
            id,
            mutation_id,
            document_id,
            base_revision_id,
            result_revision_id,
            item_state::text as item_state,
            message
         from content_mutation_item
         where mutation_id = $1
         order by id asc",
    )
    .bind(mutation_id)
    .fetch_all(postgres)
    .await
}

pub async fn get_mutation_item_by_id(
    postgres: &PgPool,
    item_id: Uuid,
) -> Result<Option<ContentMutationItemRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentMutationItemRow>(
        "select
            id,
            mutation_id,
            document_id,
            base_revision_id,
            result_revision_id,
            item_state::text as item_state,
            message
         from content_mutation_item
         where id = $1",
    )
    .bind(item_id)
    .fetch_optional(postgres)
    .await
}

pub async fn create_mutation_item(
    postgres: &PgPool,
    new_item: &NewContentMutationItem<'_>,
) -> Result<ContentMutationItemRow, sqlx::Error> {
    sqlx::query_as::<_, ContentMutationItemRow>(
        "insert into content_mutation_item (
            id,
            mutation_id,
            document_id,
            base_revision_id,
            result_revision_id,
            item_state,
            message
        )
        values ($1, $2, $3, $4, $5, $6::content_mutation_item_state, $7)
        returning
            id,
            mutation_id,
            document_id,
            base_revision_id,
            result_revision_id,
            item_state::text as item_state,
            message",
    )
    .bind(Uuid::now_v7())
    .bind(new_item.mutation_id)
    .bind(new_item.document_id)
    .bind(new_item.base_revision_id)
    .bind(new_item.result_revision_id)
    .bind(new_item.item_state)
    .bind(new_item.message)
    .fetch_one(postgres)
    .await
}

pub async fn update_mutation_item(
    postgres: &PgPool,
    item_id: Uuid,
    document_id: Option<Uuid>,
    base_revision_id: Option<Uuid>,
    result_revision_id: Option<Uuid>,
    item_state: &str,
    message: Option<&str>,
) -> Result<Option<ContentMutationItemRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentMutationItemRow>(
        "update content_mutation_item
         set document_id = $2,
             base_revision_id = $3,
             result_revision_id = $4,
             item_state = $5::content_mutation_item_state,
             message = $6
         where id = $1
         returning
            id,
            mutation_id,
            document_id,
            base_revision_id,
            result_revision_id,
            item_state::text as item_state,
            message",
    )
    .bind(item_id)
    .bind(document_id)
    .bind(base_revision_id)
    .bind(result_revision_id)
    .bind(item_state)
    .bind(message)
    .fetch_optional(postgres)
    .await
}

// ============================================================================
// Canonical slim-list query for /v1/content/documents (list_documents_page).
// ============================================================================

/// One row of the paginated document-list query. Joins the minimum set of
/// tables required to render the document list card server-side (status,
/// readiness, file_name fallback, source access) without any per-document
/// round-trips. Readiness signals that live exclusively in ArangoDB
/// (`knowledge_revision.text_state` etc.) are merged in by the caller.
#[derive(Debug, Clone, FromRow)]
pub struct ContentDocumentListRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub external_key: String,
    pub document_state: String,
    pub created_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,

    // head pointers — may be absent while the document is still in flight
    pub active_revision_id: Option<Uuid>,
    pub readable_revision_id: Option<Uuid>,

    // active revision metadata (Postgres copy)
    pub revision_title: Option<String>,
    pub revision_mime_type: Option<String>,
    pub revision_byte_size: Option<i64>,
    pub revision_source_uri: Option<String>,
    pub revision_content_source_kind: Option<String>,
    pub revision_storage_key: Option<String>,

    // latest mutation
    pub mutation_id: Option<Uuid>,
    pub mutation_state: Option<String>,
    pub mutation_failure_code: Option<String>,
    pub mutation_requested_at: Option<DateTime<Utc>>,

    // latest ingest job (only one per mutation)
    pub job_id: Option<Uuid>,
    pub job_queue_state: Option<String>,
    pub job_queued_at: Option<DateTime<Utc>>,
    pub job_completed_at: Option<DateTime<Utc>>,

    // latest attempt on that job
    pub attempt_current_stage: Option<String>,
    pub attempt_started_at: Option<DateTime<Utc>>,
    pub attempt_finished_at: Option<DateTime<Utc>>,
    pub attempt_failure_code: Option<String>,
    pub attempt_retryable: Option<bool>,
    pub attempt_heartbeat_at: Option<DateTime<Utc>>,

    // per-document billing rollup — summed across every execution
    // attributed to this document (ingest_attempt + graph_extraction_attempt).
    // Surfaced on the canonical list response so the frontend never has
    // to issue a library-wide `/billing/library-document-costs` fetch to
    // fill in the cost column.
    pub cost_total: rust_decimal::Decimal,
    pub cost_currency_code: String,
}

#[derive(Debug, Clone, FromRow)]
pub struct ContentDocumentMetadataSearchRow {
    pub document_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub external_key: String,
    pub readable_revision_id: Uuid,
    pub revision_title: Option<String>,
    pub metadata_score: f64,
    pub matched_text: String,
}

/// Ordering key for the canonical document-list keyset.
#[derive(Debug, Clone, Copy)]
pub enum DocumentListSortColumn {
    /// Default: upload time, matching the frontend "Uploaded" column.
    CreatedAt,
    /// Lexicographic on `content_document.external_key` (the UI file name
    /// fallback).
    ExternalKey,
    /// Sort by `content_revision.mime_type` (file type column).
    MimeType,
    /// Sort by `content_revision.byte_size` (file size column).
    ByteSize,
    /// Sort by `derived_status` — same CASE expression used for the
    /// status pills, so operators can group ready/failed/processing.
    DerivedStatus,
}

/// One page worth of document-list rows. `cursor_*` fields describe the
/// `(created_at, id)` tuple of the last row returned, allowing the caller to
/// construct an opaque continuation token without re-reading the result.
pub struct ContentDocumentListPage {
    pub rows: Vec<ContentDocumentListRow>,
    pub has_more: bool,
}

/// Keyset-paginated fetch for the document list surface.
///
/// * `limit` is clamped to 1..=200 by the caller.
/// * `cursor` is `(created_at, id)` of the last row on the previous page.
///   Rows strictly older than the cursor on the `(created_at desc, id desc)`
///   keyset are returned.
/// * `include_deleted` mirrors the query parameter on the HTTP surface.
/// * `search` applies a lower(ILIKE) filter on `external_key` using the
///   pg_trgm index. Case-insensitive.
/// * The join strategy is:
///   ```text
///   content_document
///     LEFT JOIN content_document_head ON (document_id)
///     LEFT JOIN content_revision       ON (active or readable)
///     LEFT JOIN content_mutation       ON (latest_mutation_id)
///     LEFT JOIN ingest_job             ON (mutation_id)
///     LEFT JOIN LATERAL ingest_attempt ON (job_id, attempt_number DESC)
///   ```
///   Every join is LEFT so documents without a head/mutation/job still show.
#[allow(clippy::too_many_arguments)]
pub async fn list_document_page_rows(
    postgres: &PgPool,
    library_id: Uuid,
    include_deleted: bool,
    cursor: Option<(DateTime<Utc>, Uuid)>,
    limit: u32,
    search: Option<&str>,
    sort: DocumentListSortColumn,
    sort_desc: bool,
    status_filter: &[String],
) -> Result<ContentDocumentListPage, sqlx::Error> {
    // Fetch `limit + 1` rows so we can report `has_more` without a COUNT(*).
    let fetch_limit = i64::from(limit) + 1;
    let search_pattern = search
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("%{}%", value.to_lowercase()));

    // Separate ORDER BY strings for the inner CTE (on `j.` alias inside
    // `joined`) and the outer SELECT (on `p.` alias inside `page`). They
    // share the same column set but live in different alias namespaces.
    // Every non-canonical sort falls back through `j.created_at desc,
    // j.id desc` as the secondary key so pagination stays deterministic
    // even when the primary column is NULL / tied.
    let (joined_order_sql, page_order_sql) = match (sort, sort_desc) {
        (DocumentListSortColumn::CreatedAt, true) => {
            ("j.created_at desc, j.id desc", "p.created_at desc, p.id desc")
        }
        (DocumentListSortColumn::CreatedAt, false) => {
            ("j.created_at asc, j.id asc", "p.created_at asc, p.id asc")
        }
        (DocumentListSortColumn::ExternalKey, true) => (
            "lower(j.external_key) desc, j.created_at desc, j.id desc",
            "lower(p.external_key) desc, p.created_at desc, p.id desc",
        ),
        (DocumentListSortColumn::ExternalKey, false) => (
            "lower(j.external_key) asc, j.created_at asc, j.id asc",
            "lower(p.external_key) asc, p.created_at asc, p.id asc",
        ),
        (DocumentListSortColumn::MimeType, true) => (
            "j.revision_mime_type desc nulls last, j.created_at desc, j.id desc",
            "p.revision_mime_type desc nulls last, p.created_at desc, p.id desc",
        ),
        (DocumentListSortColumn::MimeType, false) => (
            "j.revision_mime_type asc nulls last, j.created_at desc, j.id desc",
            "p.revision_mime_type asc nulls last, p.created_at desc, p.id desc",
        ),
        (DocumentListSortColumn::ByteSize, true) => (
            "j.revision_byte_size desc nulls last, j.created_at desc, j.id desc",
            "p.revision_byte_size desc nulls last, p.created_at desc, p.id desc",
        ),
        (DocumentListSortColumn::ByteSize, false) => (
            "j.revision_byte_size asc nulls last, j.created_at desc, j.id desc",
            "p.revision_byte_size asc nulls last, p.created_at desc, p.id desc",
        ),
        (DocumentListSortColumn::DerivedStatus, true) => (
            "j.derived_status desc, j.created_at desc, j.id desc",
            "p.derived_status desc, p.created_at desc, p.id desc",
        ),
        (DocumentListSortColumn::DerivedStatus, false) => (
            "j.derived_status asc, j.created_at desc, j.id desc",
            "p.derived_status asc, p.created_at desc, p.id desc",
        ),
    };

    // Keyset is only well-defined for the canonical created_at path; for
    // every other sort we fall back to a regular offset/limit on the
    // joined CTE. The cursor clause is always bound (as NULL when absent)
    // so Postgres can infer the parameter types during query prepare.
    let keyset_sql = match (sort, sort_desc) {
        (DocumentListSortColumn::CreatedAt, true) => {
            "and ($4::timestamptz is null or (j.created_at, j.id) < ($4, $5))"
        }
        (DocumentListSortColumn::CreatedAt, false) => {
            "and ($4::timestamptz is null or (j.created_at, j.id) > ($4, $5))"
        }
        _ => "and ($4::timestamptz is null or $5::uuid is null or true)",
    };

    // `derived_status` mirrors apps/web/src/pages/documents/mappers.ts priority
    // chain on the Postgres-only signals we have in the list path. The 5
    // buckets the frontend filter surface exposes are:
    //   canceled / failed / processing / queued / ready
    // The graph_ready vs readable vs graph_sparse split is NOT part of this
    // derivation — that requires the ArangoDB revision state which isn't in
    // the CTE. `ready` here means "readable revision exists and no terminal
    // failure signal" — the inspector panel surfaces the finer split.
    // Same LATERAL protection as `aggregate_document_list_status_counts`:
    // a content_mutation can own many ingest_job rows (retry, requeue,
    // one bulk-import mutation can carry many document jobs), so
    // the join must return at most one job per document. The selected job
    // is from the newest mutation for this document; state priority is a
    // retry tie-breaker inside that mutation. The active revision is also
    // joined in the inner CTE so `ORDER BY revision_mime_type` /
    // `revision_byte_size` (the file-type / file-size column headers)
    // can push down into keyset sort.
    let sql = format!(
        "with joined as (
            select
                d.id,
                d.workspace_id,
                d.library_id,
                d.external_key,
                d.document_state::text as document_state,
                d.created_at,
                d.deleted_at,
                h.active_revision_id,
                h.readable_revision_id,
                h.latest_mutation_id,
                m.mutation_state::text as mutation_state,
                m.failure_code as mutation_failure_code,
                m.requested_at as mutation_requested_at,
                m.id as mutation_id,
                r.title as revision_title,
                r.mime_type as revision_mime_type,
                r.byte_size as revision_byte_size,
                r.source_uri as revision_source_uri,
                r.content_source_kind::text as revision_content_source_kind,
                r.storage_key as revision_storage_key,
                ij.id as job_id,
                ij.queue_state::text as job_queue_state,
                ij.queued_at as job_queued_at,
                ij.completed_at as job_completed_at,
                {DERIVED_STATUS_CASE_SQL} as derived_status
            from content_document d
            left join content_document_head h on h.document_id = d.id
            left join content_revision r
                on r.id = coalesce(h.readable_revision_id, h.active_revision_id)
            left join content_mutation m on m.id = h.latest_mutation_id
            left join lateral (
                -- Filter by knowledge_document_id, NOT by mutation_id.
                -- Bulk-import mutations can carry ingest_job rows
                -- shared across many documents, so filtering by
                -- mutation_id can resolve unrelated documents to the
                -- same state. ingest_job has a direct document
                -- pointer; using it guarantees the lateral pick
                -- reflects this document only.
                --
                -- Across one document's jobs, the newest mutation wins.
                -- Within that mutation, state priority surfaces the
                -- active retry over older terminal attempts.
                select ij_inner.*
                from ingest_job ij_inner
                left join content_mutation m_inner on m_inner.id = ij_inner.mutation_id
                where ij_inner.knowledge_document_id = d.id
                order by coalesce(m_inner.requested_at, ij_inner.queued_at) desc,
                    case ij_inner.queue_state::text
                        when 'leased' then 1
                        when 'failed' then 2
                        when 'canceled' then 3
                        when 'queued' then 4
                        when 'completed' then 5
                        else 6
                    end,
                    ij_inner.queued_at desc
                limit 1
            ) ij on true
            where d.library_id = $1
              and ($2::bool or d.document_state = 'active')
              and ($3::text is null or lower(d.external_key) like $3)
        ),
        page as (
            select * from joined j
            where true
              {keyset_sql}
              and (cardinality($7::text[]) = 0 or j.derived_status = any($7))
            order by {joined_order_sql}
            limit $6
        )
        select
            p.id,
            p.workspace_id,
            p.library_id,
            p.external_key,
            p.document_state,
            p.created_at,
            p.deleted_at,
            p.active_revision_id,
            p.readable_revision_id,
            p.revision_title,
            p.revision_mime_type,
            p.revision_byte_size,
            p.revision_source_uri,
            p.revision_content_source_kind,
            p.revision_storage_key,
            p.mutation_id,
            p.mutation_state,
            p.mutation_failure_code,
            p.mutation_requested_at,
            p.job_id,
            p.job_queue_state,
            p.job_queued_at,
            p.job_completed_at,
            a.current_stage as attempt_current_stage,
            a.started_at as attempt_started_at,
            a.finished_at as attempt_finished_at,
            a.failure_code as attempt_failure_code,
            a.retryable as attempt_retryable,
            a.heartbeat_at as attempt_heartbeat_at,
            coalesce(c.cost_total, 0) as cost_total,
            coalesce(c.cost_currency_code, 'USD') as cost_currency_code
        from page p
        left join lateral (
            select ia.*
            from ingest_attempt ia
            where ia.job_id = p.job_id
            order by ia.attempt_number desc
            limit 1
        ) a on true
        left join lateral (
            -- Per-document cost rollup. `billing_execution_cost` carries
            -- library_id and knowledge_document_id directly, so this is
            -- a single indexed aggregate via
            -- `idx_billing_execution_cost_library_document`. Lateral
            -- keeps the cost column optional — documents with no
            -- billable execution just get 0.
            select
                coalesce(sum(bec.total_cost), 0) as cost_total,
                coalesce(max(bec.currency_code), 'USD') as cost_currency_code
            from billing_execution_cost bec
            where bec.library_id = p.library_id
              and bec.knowledge_document_id = p.id
        ) c on true
        order by {page_order_sql}",
        keyset_sql = keyset_sql,
        joined_order_sql = joined_order_sql,
        page_order_sql = page_order_sql,
    );

    // Bind order: $1 library_id, $2 include_deleted, $3 search,
    //             $4 cursor_ts, $5 cursor_id, $6 fetch_limit, $7 status_filter.
    //
    // `persistent(false)` forces each execution to re-plan using
    // concrete parameter values. Postgres caches prepared-statement
    // plans per connection and, after ~5 executions, switches to a
    // "generic plan" that ignores parameter values — on this query
    // (with highly selective `status_filter` / sort-column variants)
    // the generic plan collapses to a full sequential scan and ran
    // at ~4 s on the reference library even though the custom plan
    // finishes in 3 ms. Re-planning per call costs a few hundred µs
    // and keeps latency deterministic.
    let (cursor_ts, cursor_id) = cursor.unzip();
    let mut query = sqlx::query_as::<_, ContentDocumentListRow>(&sql)
        .persistent(false)
        .bind(library_id)
        .bind(include_deleted)
        .bind(search_pattern);
    query = query.bind(cursor_ts);
    query = query.bind(cursor_id);
    query = query.bind(fetch_limit);
    query = query.bind(status_filter);

    let mut rows = query.fetch_all(postgres).await?;
    let has_more = rows.len() > limit as usize;
    if has_more {
        rows.truncate(limit as usize);
    }
    Ok(ContentDocumentListPage { rows, has_more })
}

/// Per-bucket counts matching the `derived_status` column the list CTE
/// emits. Used by the documents page filter strip to populate pill
/// badges without an extra endpoint round-trip.
#[derive(Debug, Clone, Default, FromRow)]
pub struct DocumentListStatusCountsRow {
    pub total: Option<i64>,
    pub ready: Option<i64>,
    pub processing: Option<i64>,
    pub queued: Option<i64>,
    pub failed: Option<i64>,
    pub canceled: Option<i64>,
}

/// One pass over the same CASE derivation used by `list_document_page_rows`
/// producing the 5 bucket counts plus the overall total. Called only when
/// the caller opts in via `includeTotal=true` — otherwise every page-flip
/// would pay for an unbounded aggregate.
pub async fn aggregate_document_list_status_counts(
    postgres: &PgPool,
    library_id: Uuid,
    include_deleted: bool,
    search: Option<&str>,
) -> Result<DocumentListStatusCountsRow, sqlx::Error> {
    let search_pattern = search
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("%{}%", value.to_lowercase()));
    // The `ingest_job` join MUST be a LATERAL pick-one to prevent
    // Cartesian fanout: one mutation can own many ingest_job rows
    // across retries and bulk imports. A straight `left join ingest_job`
    // multiplies document rows and corrupts counts. The lateral subquery
    // returns at most one job per document, from the newest mutation, with
    // state priority as the retry tie-breaker inside that mutation.
    let sql = format!(
        "with joined as (
            select
                d.id,
                {DERIVED_STATUS_CASE_SQL} as derived_status
            from content_document d
            left join content_document_head h on h.document_id = d.id
            left join content_mutation m on m.id = h.latest_mutation_id
            left join lateral (
                -- Same per-document filter used in
                -- list_document_page_rows — see the comment there
                -- for why mutation_id cannot be trusted on
                -- stacks with bulk-import mutations.
                select ij_inner.queue_state
                from ingest_job ij_inner
                left join content_mutation m_inner on m_inner.id = ij_inner.mutation_id
                where ij_inner.knowledge_document_id = d.id
                order by coalesce(m_inner.requested_at, ij_inner.queued_at) desc,
                    case ij_inner.queue_state::text
                        when 'leased' then 1
                        when 'failed' then 2
                        when 'canceled' then 3
                        when 'queued' then 4
                        when 'completed' then 5
                        else 6
                    end,
                    ij_inner.queued_at desc
                limit 1
            ) ij on true
            where d.library_id = $1
              and ($2::bool or d.document_state = 'active')
              and ($3::text is null or lower(d.external_key) like $3)
        )
        select
            count(*)::bigint as total,
            count(*) filter (where derived_status = 'ready')::bigint as ready,
            count(*) filter (where derived_status = 'processing')::bigint as processing,
            count(*) filter (where derived_status = 'queued')::bigint as queued,
            count(*) filter (where derived_status = 'failed')::bigint as failed,
            count(*) filter (where derived_status = 'canceled')::bigint as canceled
        from joined"
    );
    sqlx::query_as::<_, DocumentListStatusCountsRow>(&sql)
        .bind(library_id)
        .bind(include_deleted)
        .bind(search_pattern)
        .fetch_one(postgres)
        .await
}

/// Canonical per-library document metrics row. This is the ONE
/// function every surface (`/ops/libraries/{id}/dashboard`,
/// `/content/libraries/{id}/documents?includeTotal=true`,
/// `/knowledge/libraries/{id}/summary`) should route through for
/// document-count numbers. It runs the status-bucket aggregate and
/// the graph-ready count concurrently via `tokio::try_join!` and
/// clamps `graph_ready` to `ready` so the invariant
/// `graph_ready + graph_sparse == ready` always holds on the wire,
/// even during a graph rebuild where the two halves are briefly
/// out-of-sync.
///
/// Contract:
///   * `total == ready + processing + queued + failed + canceled`
///   * `graph_ready + graph_sparse == ready`
///
/// Scoped to `document_state = 'active'` (deleted documents are not
/// reflected in any of the metrics). Search filtering and
/// include-deleted live only on the list surface — metrics are a
/// library-wide summary.
pub async fn aggregate_library_document_metrics(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<ironrag_contracts::documents::LibraryDocumentMetrics, sqlx::Error> {
    // Run the status-bucket CASE aggregate and the graph-snapshot
    // lookup in parallel. The graph count itself is version-scoped,
    // so we pull the active projection_version from the snapshot row
    // and only then hit `runtime_graph_node`.
    let status_future = aggregate_document_list_status_counts(postgres, library_id, false, None);
    let snapshot_future =
        crate::infra::repositories::get_runtime_graph_snapshot(postgres, library_id);
    let (status_row, snapshot_row) = tokio::try_join!(status_future, snapshot_future)?;
    let graph_ready_raw = if let Some(snapshot) = snapshot_row.as_ref() {
        if snapshot.graph_status == "empty" || snapshot.node_count <= 0 {
            0
        } else {
            crate::infra::repositories::count_runtime_graph_document_nodes_by_library(
                postgres,
                library_id,
                snapshot.projection_version.max(1),
            )
            .await?
        }
    } else {
        0
    };
    let total = status_row.total.unwrap_or(0);
    let ready = status_row.ready.unwrap_or(0);
    let processing = status_row.processing.unwrap_or(0);
    let queued = status_row.queued.unwrap_or(0);
    let failed = status_row.failed.unwrap_or(0);
    let canceled = status_row.canceled.unwrap_or(0);
    // Clamp: `runtime_graph_node` may transiently report more document
    // nodes than the active set (e.g. an old projection still lingers
    // while a new rebuild is staging). We never report a graph_ready
    // greater than the ready bucket — that would violate the published
    // invariant and make the dashboard look nonsensical.
    let graph_ready = graph_ready_raw.clamp(0, ready);
    let graph_sparse = ready.saturating_sub(graph_ready);
    Ok(ironrag_contracts::documents::LibraryDocumentMetrics {
        total,
        ready,
        processing,
        queued,
        failed,
        canceled,
        graph_ready,
        graph_sparse,
        recomputed_at: chrono::Utc::now(),
    })
}

pub async fn search_document_metadata_rows(
    postgres: &PgPool,
    library_id: Uuid,
    query: &str,
    limit: u32,
) -> Result<Vec<ContentDocumentMetadataSearchRow>, sqlx::Error> {
    let search_terms = metadata_search_terms(query);
    if search_terms.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let like_patterns =
        search_terms.generic.iter().map(|term| format!("%{term}%")).collect::<Vec<_>>();
    let version_like_patterns =
        search_terms.version.iter().map(|term| format!("%{term}%")).collect::<Vec<_>>();

    sqlx::query_as::<_, ContentDocumentMetadataSearchRow>(
        r#"with candidate as (
         select
            d.id as document_id,
            d.workspace_id,
            d.library_id,
            d.external_key,
            h.readable_revision_id,
            r.title as revision_title,
            case
                when lower(coalesce(r.title, '')) = any($2) then 1400::double precision
                when lower(d.external_key) = any($2) then 1380::double precision
                when cardinality($4::text[]) > 0
                    and lower(coalesce(r.title, '')) like any($4) then 1320::double precision
                when cardinality($4::text[]) > 0
                    and lower(d.external_key) like any($4) then 1280::double precision
                when lower(coalesce(r.title, '')) like any($3) then 1120::double precision
                when lower(d.external_key) like any($3) then 1080::double precision
                else 1050::double precision
            end as metadata_score,
            case
                when lower(coalesce(r.title, '')) = any($2) then coalesce(r.title, d.external_key)
                when lower(d.external_key) = any($2) then d.external_key
                when cardinality($4::text[]) > 0
                    and lower(coalesce(r.title, '')) like any($4) then coalesce(r.title, d.external_key)
                when cardinality($4::text[]) > 0
                    and lower(d.external_key) like any($4) then d.external_key
                when lower(coalesce(r.title, '')) like any($3) then coalesce(r.title, d.external_key)
                when lower(d.external_key) like any($3) then d.external_key
                else coalesce(r.title, d.external_key)
            end as matched_text,
            regexp_match(
                coalesce(r.title, d.external_key),
                '([0-9]+)\.([0-9]+)(?:\.([0-9]+))?(?:\.([0-9]+))?'
            ) as version_parts
         from content_document d
         join content_document_head h on h.document_id = d.id
         join content_revision r on r.id = h.readable_revision_id
         where d.library_id = $1
           and d.document_state = 'active'
           and (
                lower(d.external_key) = any($2)
                or lower(coalesce(r.title, '')) = any($2)
                or lower(d.external_key) like any($3)
                or lower(coalesce(r.title, '')) like any($3)
                or (
                    cardinality($4::text[]) > 0
                    and (
                        lower(d.external_key) like any($4)
                        or lower(coalesce(r.title, '')) like any($4)
                    )
                )
           )
        )
         select
            document_id,
            workspace_id,
            library_id,
            external_key,
            readable_revision_id,
            revision_title,
            metadata_score,
            matched_text
         from candidate
         order by
            metadata_score desc,
            coalesce((version_parts[1])::integer, -1) desc,
            coalesce((version_parts[2])::integer, -1) desc,
            coalesce((version_parts[3])::integer, -1) desc,
            coalesce((version_parts[4])::integer, -1) desc,
            document_id desc
         limit $5"#,
    )
    .bind(library_id)
    .bind(search_terms.generic)
    .bind(like_patterns)
    .bind(version_like_patterns)
    .bind(i64::from(limit))
    .fetch_all(postgres)
    .await
}

#[derive(Debug, Default, PartialEq, Eq)]
struct MetadataSearchTerms {
    generic: Vec<String>,
    version: Vec<String>,
}

impl MetadataSearchTerms {
    fn is_empty(&self) -> bool {
        self.generic.is_empty() && self.version.is_empty()
    }
}

fn metadata_search_terms(query: &str) -> MetadataSearchTerms {
    let normalized_query = query.trim().to_lowercase();
    if normalized_query.is_empty() {
        return MetadataSearchTerms::default();
    }

    let mut seen = BTreeSet::new();
    let mut generic = Vec::new();
    if seen.insert(normalized_query.clone()) {
        generic.push(normalized_query.clone());
    }
    for token in normalized_query.split_whitespace() {
        let normalized_token = token
            .trim_matches(|character: char| {
                !character.is_alphanumeric() && !matches!(character, '.' | '_' | '-' | '/' | '\\')
            })
            .trim();
        if normalized_token.chars().count() >= 2 {
            push_metadata_search_term(&mut generic, &mut seen, normalized_token.to_string());
        }
        if generic.len() >= 8 {
            break;
        }
    }

    MetadataSearchTerms { generic, version: metadata_version_terms(&normalized_query) }
}

fn push_metadata_search_term(terms: &mut Vec<String>, seen: &mut BTreeSet<String>, term: String) {
    if seen.insert(term.clone()) {
        terms.push(term);
    }
}

fn metadata_version_terms(normalized_query: &str) -> Vec<String> {
    let has_word_context = normalized_query
        .split(|character: char| {
            !character.is_alphanumeric() && character != '_' && character != '-' && character != '/'
        })
        .any(|token| token.chars().count() >= 2 && token.chars().any(char::is_alphabetic));

    dotted_version_terms(normalized_query)
        .into_iter()
        .filter(|term| term.matches('.').count() >= 2 || has_word_context)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::metadata_search_terms;

    #[test]
    fn metadata_search_terms_extracts_filename_token_from_mixed_query() {
        let terms = metadata_search_terms("audit_repository.rs filters events");
        assert!(terms.generic.iter().any(|term| term == "audit_repository.rs"));
        assert!(terms.generic.iter().any(|term| term == "filters"));
        assert!(terms.generic.iter().any(|term| term == "events"));
    }

    #[test]
    fn metadata_search_terms_normalizes_unicode_and_deduplicates() {
        let terms = metadata_search_terms("AUDIT_REPOSITORY.RS CAFÉ café");
        assert!(terms.generic.iter().any(|term| term == "audit_repository.rs"));
        assert_eq!(terms.generic.iter().filter(|term| term.as_str() == "café").count(), 1);
    }

    #[test]
    fn metadata_search_terms_extracts_version_prefix_with_word_context() {
        let terms = metadata_search_terms("\"Version 4.6.\" \"Alpha Suite Administrator Guide\"");
        assert!(terms.version.iter().any(|term| term == "4.6"));
    }

    #[test]
    fn metadata_search_terms_requires_context_for_two_part_numbers() {
        let terms = metadata_search_terms("1.2");
        assert!(terms.version.is_empty());
        let terms = metadata_search_terms("Alpha 1.2");
        assert_eq!(terms.version, vec!["1.2"]);
    }
}
