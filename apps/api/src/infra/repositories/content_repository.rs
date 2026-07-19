use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use chrono::{DateTime, Utc};
use sqlx::{Executor, FromRow, PgPool, Postgres, QueryBuilder, Transaction};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::{
    domains::content::{derive_document_role, revision_is_raster_image},
    shared::versioning::dotted_version_terms,
};

/// Canonical CASE expression that derives the five status buckets the
/// documents surface exposes (`canceled` / `failed` / `processing` /
/// `queued` / `ready`) from Postgres-only signals. One source of
/// truth so the list page, the status-count aggregate, and every
/// ad-hoc caller stay aligned.
///
/// Priority (top row wins):
/// 1. Mutation is terminally failed / conflicted → `failed`.
///    The head itself is broken; the operator must see this.
/// 2. Latest `ingest_job` is `failed` → `failed`.
/// 3. Latest `ingest_job` is `leased` → `processing`. A worker is
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
/// 5. Latest `ingest_job` or mutation is `canceled` → `canceled` (no
///    readable, work was canceled before finishing).
/// 6. Latest `ingest_job` is `queued` → `queued` (new document
///    waiting for its first ingest; no readable yet).
/// 7. Mutation state is `accepted` / `running` → `processing`.
/// 8. Latest `ingest_job` is `completed` but no readable → `failed`
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
    pub parent_document_id: Option<Uuid>,
    pub parent_external_key: Option<String>,
    pub document_role: String,
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
    pub document_hint: Option<String>,
    pub storage_key: Option<String>,
    pub created_by_principal_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub enum UpdateRevisionDocumentHintOutcome {
    Updated(Box<ContentRevisionRow>),
    RevisionNotFound,
    KnowledgeProjectionNotFound,
}

#[derive(Debug, Clone)]
pub enum UpdateRevisionStorageKeyOutcome {
    Updated(Box<ContentRevisionRow>),
    RevisionNotFound,
    KnowledgeProjectionNotFound,
}

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
pub enum CreateContentRevisionOutcome {
    Created(Box<ContentRevisionRow>),
    DocumentNotFound,
    DocumentDeleted,
    ProjectionUnavailable,
}

#[derive(Debug, Clone)]
pub enum PromoteDocumentHeadOutcome {
    Promoted(ContentDocumentHeadRow),
    DocumentNotFound,
    RevisionNotFound,
    ReferenceIntegrityViolation,
}

#[derive(Debug, Clone)]
pub(crate) enum ValidatedDocumentHeadWriteOutcome {
    Updated(ContentDocumentHeadRow),
    DocumentNotFound,
    ReferenceIntegrityViolation,
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
    /// Declared structural parent key from the connector (the page a dependent
    /// rides with). Resolved to `parent_document_id` at admission when the
    /// parent already exists, otherwise left pending for the resolver.
    pub parent_external_key: Option<&'a str>,
    /// Resolved canonical parent document id, when the parent was already
    /// admitted at creation time.
    pub parent_document_id: Option<Uuid>,
    /// Typed role decided at admission: `primary`, `attachment`, or
    /// `attached_context`.
    pub document_role: &'a str,
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
    pub document_hint: Option<&'a str>,
    pub storage_key: Option<&'a str>,
    pub created_by_principal_id: Option<Uuid>,
}

/// Production append input. Revision identity and ancestry are deliberately
/// absent: the repository derives them while holding the document lock.
#[derive(Debug, Clone)]
pub struct NewContentRevisionProjection<'a> {
    pub document_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub content_source_kind: &'a str,
    pub checksum: &'a str,
    pub mime_type: &'a str,
    pub byte_size: i64,
    pub title: Option<&'a str>,
    pub language_code: Option<&'a str>,
    pub source_uri: Option<&'a str>,
    pub document_hint: Option<&'a str>,
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

/// Complete canonical + query-projection chunk payload. Owned values keep the
/// atomic replacement API free of parallel slices whose ordering could drift.
#[derive(Debug, Clone)]
pub struct NewContentChunkProjection {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub revision_id: Uuid,
    pub chunk_index: i32,
    pub start_offset: i32,
    pub end_offset: i32,
    pub token_count: Option<i32>,
    pub chunk_kind: Option<String>,
    pub content_text: String,
    pub normalized_text: String,
    pub text_checksum: String,
    pub support_block_ids: Vec<Uuid>,
    pub section_path: Vec<String>,
    pub heading_trail: Vec<String>,
    pub literal_digest: Option<String>,
    pub chunk_state: String,
    pub text_generation: Option<i64>,
    pub vector_generation: Option<i64>,
    pub quality_score: Option<f32>,
    pub window_text: Option<String>,
    pub occurred_at: Option<DateTime<Utc>>,
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
            deleted_at,
            parent_document_id,
            parent_external_key,
            document_role
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
            deleted_at,
            parent_document_id,
            parent_external_key,
            document_role
         from content_document
         where id = $1",
    )
    .bind(document_id)
    .fetch_optional(executor)
    .await
}

/// Loads and locks one canonical document row for a lifecycle transition.
///
/// Concurrent delete requests serialize on this row so only the transaction
/// that observes the non-deleted state creates the durable outbox event.
pub async fn lock_document_by_id_with_executor<'e, E>(
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
            deleted_at,
            parent_document_id,
            parent_external_key,
            document_role
         from content_document
         where id = $1
         for update",
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
            deleted_at,
            parent_document_id,
            parent_external_key,
            document_role
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

/// Lists the non-deleted child documents that resolve to `parent_document_id`
/// in `library_id`. Used by the delete-cascade lifecycle to act on a parent's
/// children. Multi-parent occurrence attribution is out of scope: a child has
/// exactly one canonical (first-resolved) `parent_document_id`.
pub async fn list_active_children_by_parent(
    postgres: &PgPool,
    library_id: Uuid,
    parent_document_id: Uuid,
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
            deleted_at,
            parent_document_id,
            parent_external_key,
            document_role
         from content_document
         where library_id = $1
           and parent_document_id = $2
           and document_state <> 'deleted'
         order by created_at desc, id desc",
    )
    .bind(library_id)
    .bind(parent_document_id)
    .fetch_all(postgres)
    .await
}

/// Detaches a child document from its resolved parent by clearing
/// `parent_document_id` while preserving the declared `parent_external_key`.
/// Used when a parent is deleted but the child (role `attachment`) stays alive
/// as a peer document; keeping the declared key lets the resolver re-attach if
/// the parent reappears.
pub async fn detach_document_parent(
    postgres: &PgPool,
    library_id: Uuid,
    document_id: Uuid,
) -> Result<(), sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let parent_locked = sqlx::query_scalar::<_, Uuid>(
        "select library.id
         from content_document as document
         join catalog_library as library on library.id = document.library_id
         where document.id = $1
           and library.id = $2
         for no key update of library",
    )
    .bind(document_id)
    .bind(library_id)
    .fetch_optional(&mut *transaction)
    .await?;
    if parent_locked.is_none() {
        transaction.rollback().await?;
        return Ok(());
    }
    sqlx::query(
        "update content_document
         set parent_document_id = null
         where id = $1 and library_id = $2",
    )
    .bind(document_id)
    .bind(library_id)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "update knowledge_document
         set parent_document_id = null,
             updated_at = now()
         where document_id = $1 and library_id = $2",
    )
    .bind(document_id)
    .bind(library_id)
    .execute(&mut *transaction)
    .await?;
    super::catalog_repository::touch_library_source_truth_version_with_executor(
        &mut *transaction,
        library_id,
    )
    .await?;
    transaction.commit().await?;
    Ok(())
}

pub async fn create_document(
    postgres: &PgPool,
    new_document: &NewContentDocument<'_>,
) -> Result<ContentDocumentRow, sqlx::Error> {
    create_document_with_executor(postgres, new_document).await
}

/// Creates the canonical document, its empty head, and the query-facing
/// document projection in one transaction.
///
/// `knowledge_document.file_name` is derived only from canonical metadata:
/// the readable/active revision storage key when one exists, otherwise the
/// document external key. A storage key generated by `ContentStorageService`
/// has a `<sha256>-<file-name>` basename; the digest prefix is stripped. This
/// makes rematerialization deterministic and prevents a stale projection from
/// becoming an accidental source of truth.
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
        || canonical_file_name_component(&document.external_key),
        canonical_file_name_component,
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
            active_revision_id: None,
            readable_revision_id: None,
            latest_mutation_id: None,
            latest_successful_attempt_id: None,
        },
    )
    .await?;
    let file_name =
        canonical_projection_file_name(&document.external_key, Some(&source_file_name), None);
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

fn canonical_projection_file_name(
    external_key: &str,
    source_file_name: Option<&str>,
    storage_key: Option<&str>,
) -> String {
    storage_key
        .and_then(canonical_file_name_from_storage_key)
        .or_else(|| source_file_name.map(canonical_file_name_component))
        .unwrap_or_else(|| canonical_file_name_component(external_key))
}

fn canonical_file_name_from_storage_key(storage_key: &str) -> Option<String> {
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
        .map(canonical_file_name_component)
}

pub(crate) fn canonical_file_name_component(value: &str) -> String {
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
            deleted_at,
            parent_document_id,
            parent_external_key,
            document_role
        )
        values ($1, $2, $3, $4, $5::content_document_state, $6, now(), null, $7, $8, $9)
        returning
            id,
            workspace_id,
            library_id,
            external_key,
            document_state::text as document_state,
            created_by_principal_id,
            created_at,
            deleted_at,
            parent_document_id,
            parent_external_key,
            document_role",
    )
    .bind(Uuid::now_v7())
    .bind(new_document.workspace_id)
    .bind(new_document.library_id)
    .bind(new_document.external_key)
    .bind(new_document.document_state)
    .bind(new_document.created_by_principal_id)
    .bind(new_document.parent_document_id)
    .bind(new_document.parent_external_key)
    .bind(new_document.document_role)
    .fetch_one(executor)
    .await
}

/// Materializes one knowledge document exclusively from its locked canonical
/// document and head state.
///
/// The library parent is locked before the canonical document, head, and
/// knowledge row. A delayed promotion therefore never trusts the head pointers
/// carried in its request: after it acquires the lock it observes the newest
/// committed canonical head and writes that state. The projection write, typed
/// role finalization, and durable generation fence commit atomically.
pub async fn materialize_knowledge_document_from_canonical_head(
    postgres: &PgPool,
    document_id: Uuid,
) -> Result<MaterializeKnowledgeDocumentOutcome, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let outcome = materialize_knowledge_document_from_canonical_head_with_transaction(
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

/// Transaction-scoped form used by canonical lifecycle operations so the
/// authoritative write and its query projection cannot commit independently.
pub async fn materialize_knowledge_document_from_canonical_head_with_transaction(
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

    let file_name = canonical_projection_file_name(
        &external_key,
        source_file_name.as_deref(),
        head_storage_key.as_deref(),
    );
    let is_raster_image = head_revision_id.is_some()
        && revision_is_raster_image(Some(&file_name), head_mime_type.as_deref());
    let document_role =
        derive_document_role(parent_document_id.is_some(), is_raster_image).to_string();

    let canonical_update = sqlx::query(
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
        changed: canonical_update.rows_affected() > 0 || projection_changed.is_some(),
    })
}

/// Promotes the canonical head and rematerializes its query projection in one
/// parent-first transaction. A stale caller payload cannot overwrite a newer
/// head because the materializer rereads the locked canonical row.
pub async fn promote_document_head_with_projection(
    postgres: &PgPool,
    new_head: &NewContentDocumentHead,
) -> Result<PromoteDocumentHeadOutcome, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let Some((workspace_id, library_id)) = sqlx::query_as::<_, (Uuid, Uuid)>(
        "select workspace_id, library_id from content_document where id = $1",
    )
    .bind(new_head.document_id)
    .fetch_optional(&mut *transaction)
    .await?
    else {
        transaction.rollback().await?;
        return Ok(PromoteDocumentHeadOutcome::DocumentNotFound);
    };
    if !super::catalog_repository::lock_library_for_lifecycle_event_with_executor(
        &mut *transaction,
        workspace_id,
        library_id,
    )
    .await?
    {
        transaction.rollback().await?;
        return Ok(PromoteDocumentHeadOutcome::DocumentNotFound);
    }
    if lock_document_by_id_with_executor(&mut *transaction, new_head.document_id).await?.is_none() {
        transaction.rollback().await?;
        return Ok(PromoteDocumentHeadOutcome::DocumentNotFound);
    }
    let previous_head =
        get_document_head_for_update_with_executor(&mut *transaction, new_head.document_id).await?;
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
    let materialized = materialize_knowledge_document_from_canonical_head_with_transaction(
        &mut transaction,
        new_head.document_id,
    )
    .await?;
    match materialized.outcome {
        MaterializeKnowledgeDocumentOutcome::Materialized => {
            let readable_changed = previous_head.as_ref().map_or_else(
                || head.readable_revision_id.is_some(),
                |previous| previous.readable_revision_id != head.readable_revision_id,
            );
            if readable_changed || materialized.changed {
                super::catalog_repository::touch_library_source_truth_version_with_executor(
                    &mut *transaction,
                    library_id,
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

pub async fn update_document_state(
    postgres: &PgPool,
    document_id: Uuid,
    document_state: &str,
    deleted_at: Option<DateTime<Utc>>,
) -> Result<Option<ContentDocumentRow>, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    // Read scope without a child lock, then acquire the library parent before
    // mutating the document. Catalog deletion uses the same parent-first order.
    let Some(document_scope) =
        get_document_by_id_with_executor(&mut *transaction, document_id).await?
    else {
        transaction.rollback().await?;
        return Ok(None);
    };
    let parent_locked = super::catalog_repository::lock_library_for_lifecycle_event_with_executor(
        &mut *transaction,
        document_scope.workspace_id,
        document_scope.library_id,
    )
    .await?;
    if !parent_locked {
        transaction.rollback().await?;
        return Ok(None);
    }
    let updated = update_document_state_with_executor(
        &mut *transaction,
        document_id,
        document_state,
        deleted_at,
    )
    .await?;
    if let Some(updated_document) = updated.as_ref() {
        // Keep the retrieval projection's visibility fence in the same
        // transaction. This path is used by unrecoverable-document cleanup,
        // where no later head-promotion step exists to repair a stale active
        // knowledge row.
        sqlx::query(
            "update knowledge_document
             set document_state = $2,
                 deleted_at = $3,
                 updated_at = now()
             where document_id = $1 and library_id = $4",
        )
        .bind(document_id)
        .bind(&updated_document.document_state)
        .bind(updated_document.deleted_at)
        .bind(updated_document.library_id)
        .execute(&mut *transaction)
        .await?;
        super::catalog_repository::touch_library_source_truth_version_with_executor(
            &mut *transaction,
            document_scope.library_id,
        )
        .await?;
    }
    transaction.commit().await?;
    Ok(updated)
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
            deleted_at,
            parent_document_id,
            parent_external_key,
            document_role",
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

/// Loads and locks one document-head row for a lifecycle transaction.
///
/// # Errors
/// Returns any `SQLx` error raised while reading or locking the row.
pub async fn get_document_head_for_update_with_executor<'e, E>(
    executor: E,
    document_id: Uuid,
) -> Result<Option<ContentDocumentHeadRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
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
         where document_id = $1
         for update",
    )
    .bind(document_id)
    .fetch_optional(executor)
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
    try_upsert_document_head_with_generation_policy(executor, new_head, true)
        .await?
        .ok_or(sqlx::Error::RowNotFound)
}

/// Writes a head inside a wider lifecycle transaction without independently
/// advancing the library generation. The caller must advance it exactly once
/// after every answer-visible canonical/projection write has succeeded.
pub async fn upsert_document_head_without_generation_with_executor<'e, E>(
    executor: E,
    new_head: &NewContentDocumentHead,
) -> Result<ContentDocumentHeadRow, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    try_upsert_document_head_with_generation_policy(executor, new_head, false)
        .await?
        .ok_or(sqlx::Error::RowNotFound)
}

/// Applies the structurally validated head write and reports why its guarded
/// input CTE produced no row.
///
/// Lifecycle callers must lock the target document before invoking this
/// function. The caller-owned transaction retains that lock while the guarded
/// write locks the library. Therefore an existing document with a rejected
/// revision, mutation, or attempt reference is a persistent integrity
/// violation, not a retryable concurrent-head conflict.
pub(crate) async fn upsert_document_head_without_generation_outcome(
    transaction: &mut Transaction<'_, Postgres>,
    new_head: &NewContentDocumentHead,
) -> Result<ValidatedDocumentHeadWriteOutcome, sqlx::Error> {
    if let Some(head) =
        try_upsert_document_head_with_generation_policy(&mut **transaction, new_head, false).await?
    {
        return Ok(ValidatedDocumentHeadWriteOutcome::Updated(head));
    }

    let document_exists = sqlx::query_scalar::<_, bool>(
        "select exists(select 1 from content_document where id = $1)",
    )
    .bind(new_head.document_id)
    .fetch_one(&mut **transaction)
    .await?;
    Ok(if document_exists {
        ValidatedDocumentHeadWriteOutcome::ReferenceIntegrityViolation
    } else {
        ValidatedDocumentHeadWriteOutcome::DocumentNotFound
    })
}

async fn try_upsert_document_head_with_generation_policy<'e, E>(
    executor: E,
    new_head: &NewContentDocumentHead,
    touch_generation: bool,
) -> Result<Option<ContentDocumentHeadRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    // Parent lock -> input validation -> head row -> conditional generation
    // update is one statement and one lock order. Foreign revision/mutation/
    // attempt ids therefore cannot be laundered through this denormalized head.
    // Only readable-pointer transitions invalidate answers; shell creation,
    // active work-in-progress pointers, operational ids, and exact no-ops do
    // not churn cache generations.
    sqlx::query_as::<_, ContentDocumentHeadRow>(
        "with locked_document as materialized (
            select
                document.id as document_id,
                document.workspace_id,
                document.library_id
            from content_document as document
            join catalog_library as library on library.id = document.library_id
            where document.id = $1
            for no key update of library
         ), validated_input as materialized (
            select document.*
            from locked_document as document
            where (
                $2::uuid is null
                or exists (
                    select 1
                    from content_revision as revision
                    where revision.id = $2
                      and revision.document_id = document.document_id
                      and revision.workspace_id = document.workspace_id
                      and revision.library_id = document.library_id
                )
            )
              and (
                $3::uuid is null
                or exists (
                    select 1
                    from content_revision as revision
                    where revision.id = $3
                      and revision.document_id = document.document_id
                      and revision.workspace_id = document.workspace_id
                      and revision.library_id = document.library_id
                )
            )
              and (
                $4::uuid is null
                or exists (
                    select 1
                    from content_mutation as mutation
                    where mutation.id = $4
                      and mutation.workspace_id = document.workspace_id
                      and mutation.library_id = document.library_id
                      and (
                        exists (
                            select 1
                            from content_mutation_item as item
                            where item.mutation_id = mutation.id
                              and (
                                item.document_id = document.document_id
                                or exists (
                                    select 1
                                    from content_revision as item_revision
                                    where item_revision.id in (
                                        item.base_revision_id,
                                        item.result_revision_id
                                    )
                                      and item_revision.document_id = document.document_id
                                      and item_revision.workspace_id = document.workspace_id
                                      and item_revision.library_id = document.library_id
                                )
                              )
                        )
                        or exists (
                            select 1
                            from ingest_job as job
                            where job.mutation_id = mutation.id
                              and job.workspace_id = document.workspace_id
                              and job.library_id = document.library_id
                              and (
                                job.knowledge_document_id = document.document_id
                                or exists (
                                    select 1
                                    from content_revision as job_revision
                                    where job_revision.id = job.knowledge_revision_id
                                      and job_revision.document_id = document.document_id
                                      and job_revision.workspace_id = document.workspace_id
                                      and job_revision.library_id = document.library_id
                                )
                              )
                        )
                        or (
                            $2::uuid is null
                            and $3::uuid is null
                            and not exists (
                                select 1
                                from content_mutation_item as item
                                where item.mutation_id = mutation.id
                                  and (
                                    item.document_id is not null
                                    or item.base_revision_id is not null
                                    or item.result_revision_id is not null
                                  )
                            )
                            and not exists (
                                select 1
                                from ingest_job as job
                                where job.mutation_id = mutation.id
                                  and (
                                    job.knowledge_document_id is not null
                                    or job.knowledge_revision_id is not null
                                  )
                            )
                        )
                      )
                )
            )
              and (
                $5::uuid is null
                -- Attempt rows are retention-pruned operational history; a
                -- pointer whose row is gone entirely is tolerated (and written
                -- back as null below), while an attempt that still exists but
                -- fails the ownership join stays a laundering violation.
                or not exists (
                    select 1
                    from ingest_attempt as absent_attempt
                    where absent_attempt.id = $5
                )
                or exists (
                    select 1
                    from ingest_attempt as attempt
                    join ingest_job as job on job.id = attempt.job_id
                    where attempt.id = $5
                      and job.workspace_id = document.workspace_id
                      and job.library_id = document.library_id
                      and (
                        job.knowledge_document_id = document.document_id
                        or exists (
                            select 1
                            from content_revision as job_revision
                            where job_revision.id = job.knowledge_revision_id
                              and job_revision.document_id = document.document_id
                              and job_revision.workspace_id = document.workspace_id
                              and job_revision.library_id = document.library_id
                        )
                        or exists (
                            select 1
                            from content_mutation_item as item
                            where item.id = job.mutation_item_id
                              and item.mutation_id = job.mutation_id
                              and item.document_id = document.document_id
                        )
                      )
                )
            )
         ), resolved_attempt_pointer as materialized (
            select
                input.document_id,
                case when exists (
                    select 1
                    from ingest_attempt as attempt
                    join ingest_job as job on job.id = attempt.job_id
                    where attempt.id = $5
                      and job.workspace_id = input.workspace_id
                      and job.library_id = input.library_id
                      and (
                        job.knowledge_document_id = input.document_id
                        or exists (
                            select 1
                            from content_revision as job_revision
                            where job_revision.id = job.knowledge_revision_id
                              and job_revision.document_id = input.document_id
                              and job_revision.workspace_id = input.workspace_id
                              and job_revision.library_id = input.library_id
                        )
                        or exists (
                            select 1
                            from content_mutation_item as item
                            where item.id = job.mutation_item_id
                              and item.mutation_id = job.mutation_id
                              and item.document_id = input.document_id
                        )
                      )
                ) then $5::uuid end as effective_attempt_id
            from validated_input as input
         ), previous_head as materialized (
            select head.document_id, head.readable_revision_id
            from content_document_head as head
            join validated_input as input on input.document_id = head.document_id
            for update of head
         ), answer_transition as materialized (
            select input.library_id
            from validated_input as input
            left join previous_head as previous on true
            where (
                previous.document_id is null
                and $3::uuid is not null
            )
               or (
                previous.document_id is not null
                and previous.readable_revision_id is distinct from $3::uuid
            )
         ), updated_head as (
         insert into content_document_head (
            document_id,
            active_revision_id,
            readable_revision_id,
            latest_mutation_id,
            latest_successful_attempt_id,
            head_updated_at
        )
        select $1, $2, $3, $4, pointer.effective_attempt_id, now()
        from validated_input
        join resolved_attempt_pointer as pointer
          on pointer.document_id = validated_input.document_id
        left join previous_head on true
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
            document_summary
         ), bumped_library as (
            update catalog_library as library
            set source_truth_version = greatest(
                    coalesce(library.source_truth_version, 0) + 1,
                    (extract(epoch from clock_timestamp()) * 1000000)::bigint
                )
            from answer_transition, updated_head
            where library.id = answer_transition.library_id
              and $6::boolean
            returning library.id
         )
         select
            head.document_id,
            head.active_revision_id,
            head.readable_revision_id,
            head.latest_mutation_id,
            head.latest_successful_attempt_id,
            head.head_updated_at,
            head.document_summary
         from updated_head as head
         left join bumped_library on true",
    )
    .bind(new_head.document_id)
    .bind(new_head.active_revision_id)
    .bind(new_head.readable_revision_id)
    .bind(new_head.latest_mutation_id)
    .bind(new_head.latest_successful_attempt_id)
    .bind(touch_generation)
    .fetch_optional(executor)
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

const FINGERPRINT_CACHE_CAPACITY: usize = 1_024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryReadableContentFingerprint {
    pub value: String,
    pub source_truth_version: i64,
    pub projection_is_current: bool,
}

#[derive(Debug, Clone)]
struct FingerprintCacheEntry {
    source_truth_version: i64,
    value: String,
    projection_is_current: bool,
    last_access: u64,
}

#[derive(Debug)]
struct FingerprintCache {
    capacity: usize,
    access_sequence: u64,
    entries: HashMap<Uuid, FingerprintCacheEntry>,
}

impl FingerprintCache {
    fn with_capacity(capacity: usize) -> Self {
        Self { capacity: capacity.max(1), access_sequence: 0, entries: HashMap::new() }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }

    const fn next_access_sequence(&mut self) -> u64 {
        self.access_sequence = self.access_sequence.saturating_add(1);
        self.access_sequence
    }

    fn get(&mut self, library_id: Uuid, source_truth_version: i64) -> Option<(String, bool)> {
        let last_access = self.next_access_sequence();
        let entry = self.entries.get_mut(&library_id)?;
        if entry.source_truth_version != source_truth_version {
            self.entries.remove(&library_id);
            return None;
        }
        entry.last_access = last_access;
        Some((entry.value.clone(), entry.projection_is_current))
    }

    fn insert(
        &mut self,
        library_id: Uuid,
        source_truth_version: i64,
        value: String,
        projection_is_current: bool,
    ) {
        let last_access = self.next_access_sequence();
        self.entries.insert(
            library_id,
            FingerprintCacheEntry {
                source_truth_version,
                value,
                projection_is_current,
                last_access,
            },
        );
        if self.entries.len() > self.capacity
            && let Some(least_recently_used) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_access)
                .map(|(library_id, _)| *library_id)
        {
            self.entries.remove(&least_recently_used);
        }
    }
}

fn fingerprint_cache() -> &'static Mutex<FingerprintCache> {
    static CACHE: OnceLock<Mutex<FingerprintCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(FingerprintCache::with_capacity(FINGERPRINT_CACHE_CAPACITY)))
}

fn fingerprint_singleflight_registry() -> &'static Mutex<HashMap<Uuid, Weak<AsyncMutex<()>>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<Uuid, Weak<AsyncMutex<()>>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn fingerprint_singleflight(library_id: Uuid) -> Arc<AsyncMutex<()>> {
    let mut registry = fingerprint_singleflight_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = registry.get(&library_id).and_then(Weak::upgrade) {
        return lock;
    }

    let lock = Arc::new(AsyncMutex::new(()));
    registry.insert(library_id, Arc::downgrade(&lock));
    lock
}

pub async fn get_library_readable_content_fingerprint(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<LibraryReadableContentFingerprint, sqlx::Error> {
    // Every lookup reads only the durable generation. Equal generations reuse
    // the bounded process LRU indefinitely; all canonical answer-visible
    // writes advance that generation atomically. A generation miss is
    // singleflighted per library so concurrent queries perform one O(chunks)
    // parity/fingerprint scan instead of stampeding Postgres.
    let observed_source_truth_version =
        super::catalog_repository::get_library_source_truth_version(postgres, library_id).await?;
    {
        let mut cache =
            fingerprint_cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((value, projection_is_current)) =
            cache.get(library_id, observed_source_truth_version)
        {
            return Ok(LibraryReadableContentFingerprint {
                value,
                source_truth_version: observed_source_truth_version,
                projection_is_current,
            });
        }
    }

    let singleflight = fingerprint_singleflight(library_id);
    let _singleflight_guard = singleflight.lock().await;

    // A preceding waiter may have filled the cache, and the generation may
    // have advanced while this request waited. Re-read both under the
    // per-library singleflight before doing the expensive scan.
    let current_source_truth_version =
        super::catalog_repository::get_library_source_truth_version(postgres, library_id).await?;
    {
        let mut cache =
            fingerprint_cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((value, projection_is_current)) =
            cache.get(library_id, current_source_truth_version)
        {
            return Ok(LibraryReadableContentFingerprint {
                value,
                source_truth_version: current_source_truth_version,
                projection_is_current,
            });
        }
    }

    let (value, source_truth_version, projection_is_current) =
        sqlx::query_as::<_, (String, i64, bool)>(
            "with library_source as materialized (
            select greatest(coalesce(source_truth_version, 1), 1) as source_truth_version
            from catalog_library
            where id = $1
        ),
        canonical_readable as materialized (
            select
                document.id as document_id,
                document.workspace_id,
                document.library_id,
                document.external_key,
                document.parent_document_id,
                document.document_role,
                head.readable_revision_id as revision_id
            from content_document as document
            join content_document_head as head
              on head.document_id = document.id
            where document.library_id = $1
              and document.document_state = 'active'
              and document.deleted_at is null
              and head.readable_revision_id is not null
        ),
        knowledge_readable as materialized (
            select
                document_id,
                workspace_id,
                library_id,
                external_key,
                parent_document_id,
                document_role,
                readable_revision_id as revision_id
            from knowledge_document
            where library_id = $1
              and document_state = 'active'
              and deleted_at is null
              and readable_revision_id is not null
        ),
        document_projection_parity as (
            select not exists (
                select 1
                from canonical_readable as canonical
                full join knowledge_readable as knowledge
                  on knowledge.document_id = canonical.document_id
                where canonical.document_id is null
                   or knowledge.document_id is null
                   or canonical.workspace_id is distinct from knowledge.workspace_id
                   or canonical.library_id is distinct from knowledge.library_id
                   or canonical.external_key is distinct from knowledge.external_key
                   or canonical.parent_document_id is distinct from knowledge.parent_document_id
                   or canonical.document_role is distinct from knowledge.document_role
                   or canonical.revision_id is distinct from knowledge.revision_id
            ) as is_current
        ),
        canonical_revisions as materialized (
            select
                revision.id as revision_id,
                revision.document_id,
                revision.workspace_id,
                revision.library_id,
                revision.revision_number::bigint as revision_number,
                revision.content_source_kind::text as revision_kind,
                revision.source_uri,
                revision.document_hint,
                revision.mime_type,
                revision.checksum,
                revision.title,
                revision.language_code,
                revision.byte_size
            from canonical_readable as document
            join content_revision as revision
              on revision.id = document.revision_id
        ),
        knowledge_revisions as materialized (
            select
                revision.revision_id,
                revision.document_id,
                revision.workspace_id,
                revision.library_id,
                revision.revision_number,
                revision.revision_kind,
                revision.source_uri,
                revision.document_hint,
                revision.mime_type,
                revision.checksum,
                revision.title,
                revision.byte_size,
                revision.normalized_text,
                revision.text_checksum,
                revision.image_checksum,
                revision.text_state,
                revision.vector_state,
                revision.graph_state
            from knowledge_readable as document
            join knowledge_revision as revision
              on revision.revision_id = document.revision_id
        ),
        revision_projection_parity as (
            select not exists (
                select 1
                from canonical_revisions as canonical
                full join knowledge_revisions as knowledge
                  on knowledge.revision_id = canonical.revision_id
                where canonical.revision_id is null
                   or knowledge.revision_id is null
                   or canonical.document_id is distinct from knowledge.document_id
                   or canonical.workspace_id is distinct from knowledge.workspace_id
                   or canonical.library_id is distinct from knowledge.library_id
                   or canonical.revision_number is distinct from knowledge.revision_number
                   or canonical.revision_kind is distinct from knowledge.revision_kind
                   or canonical.source_uri is distinct from knowledge.source_uri
                   or canonical.document_hint is distinct from knowledge.document_hint
                   or canonical.mime_type is distinct from knowledge.mime_type
                   or canonical.checksum is distinct from knowledge.checksum
                   or canonical.title is distinct from knowledge.title
                   or canonical.byte_size is distinct from knowledge.byte_size
            ) as is_current
        ),
        canonical_chunks as materialized (
            select
                chunk.id as chunk_id,
                chunk.revision_id,
                revision.document_id,
                revision.workspace_id,
                revision.library_id,
                chunk.chunk_index,
                chunk.start_offset as span_start,
                chunk.end_offset as span_end,
                chunk.token_count,
                chunk.normalized_text,
                chunk.text_checksum,
                chunk.occurred_at,
                chunk.occurred_until
            from canonical_revisions as revision
            join content_chunk as chunk
              on chunk.revision_id = revision.revision_id
            where chunk.raptor_level = 0
        ),
        knowledge_chunks as materialized (
            select
                chunk.chunk_id,
                chunk.revision_id,
                chunk.document_id,
                chunk.workspace_id,
                chunk.library_id,
                chunk.chunk_index,
                chunk.span_start,
                chunk.span_end,
                chunk.token_count,
                chunk.content_text,
                chunk.normalized_text,
                chunk.chunk_kind,
                chunk.support_block_ids,
                chunk.section_path,
                chunk.heading_trail,
                chunk.literal_digest,
                chunk.chunk_state,
                chunk.text_generation,
                chunk.vector_generation,
                chunk.quality_score,
                chunk.window_text,
                chunk.occurred_at,
                chunk.occurred_until
            from knowledge_revisions as revision
            join knowledge_chunk as chunk
              on chunk.revision_id = revision.revision_id
            where chunk.raptor_level is null
        ),
        chunk_projection_parity as (
            select not exists (
                select 1
                from canonical_chunks as canonical
                full join knowledge_chunks as knowledge
                  on knowledge.chunk_id = canonical.chunk_id
                where canonical.chunk_id is null
                   or knowledge.chunk_id is null
                   or canonical.revision_id is distinct from knowledge.revision_id
                   or canonical.document_id is distinct from knowledge.document_id
                   or canonical.workspace_id is distinct from knowledge.workspace_id
                   or canonical.library_id is distinct from knowledge.library_id
                   or canonical.chunk_index is distinct from knowledge.chunk_index
                   or canonical.span_start is distinct from knowledge.span_start
                   or canonical.span_end is distinct from knowledge.span_end
                   or canonical.token_count is distinct from knowledge.token_count
                   or canonical.normalized_text is distinct from knowledge.normalized_text
                   or replace(lower(canonical.text_checksum), 'sha256:', '')
                        is distinct from encode(
                            public.digest(knowledge.normalized_text, 'sha256'),
                            'hex'
                        )
                   or knowledge.chunk_state is distinct from 'ready'
                   or canonical.occurred_at is distinct from knowledge.occurred_at
                   or canonical.occurred_until is distinct from knowledge.occurred_until
            ) as is_current
        ),
        projection_parity as (
            select
                document.is_current
                and revision.is_current
                and chunk.is_current as is_current
            from document_projection_parity as document
            cross join revision_projection_parity as revision
            cross join chunk_projection_parity as chunk
        ),
        canonical_chunk_fingerprints as (
            select
                chunk.revision_id,
                count(*)::bigint as chunk_count,
                md5(string_agg(
                    md5(array_to_string(array[
                            chunk.chunk_id::text,
                            chunk.chunk_index::text,
                            chunk.span_start::text,
                            chunk.span_end::text,
                            coalesce(chunk.token_count::text, ''),
                            chunk.normalized_text,
                            chunk.text_checksum,
                            coalesce(chunk.occurred_at::text, ''),
                            coalesce(chunk.occurred_until::text, '')
                        ],
                        chr(31),
                        ''
                    )),
                    chr(30)
                    order by chunk.chunk_index, chunk.chunk_id
                )) as chunk_fingerprint
            from canonical_chunks as chunk
            group by chunk.revision_id
        ),
        knowledge_chunk_fingerprints as (
            select
                chunk.revision_id,
                md5(string_agg(
                    md5(array_to_string(array[
                            chunk.chunk_id::text,
                            chunk.chunk_index::text,
                            coalesce(chunk.chunk_kind, ''),
                            chunk.content_text,
                            chunk.normalized_text,
                            coalesce(chunk.span_start::text, ''),
                            coalesce(chunk.span_end::text, ''),
                            coalesce(chunk.token_count::text, ''),
                            coalesce(array_to_string(chunk.support_block_ids, chr(29)), ''),
                            coalesce(array_to_string(chunk.section_path, chr(29)), ''),
                            coalesce(array_to_string(chunk.heading_trail, chr(29)), ''),
                            coalesce(chunk.literal_digest, ''),
                            chunk.chunk_state,
                            coalesce(chunk.text_generation::text, ''),
                            coalesce(chunk.vector_generation::text, ''),
                            coalesce(chunk.quality_score::text, ''),
                            coalesce(chunk.window_text, ''),
                            coalesce(chunk.occurred_at::text, ''),
                            coalesce(chunk.occurred_until::text, '')
                        ],
                        chr(31),
                        ''
                    )),
                    chr(30)
                    order by chunk.chunk_index, chunk.chunk_id
                )) as chunk_fingerprint
            from knowledge_chunks as chunk
            group by chunk.revision_id
        ),
        document_fingerprints as (
            select
                document.document_id,
                array_to_string(
                    array[
                        document.document_id::text,
                        document.external_key,
                        coalesce(document.parent_document_id::text, ''),
                        document.document_role,
                        revision.revision_id::text,
                        revision.revision_number::text,
                        revision.revision_kind,
                        revision.checksum,
                        revision.mime_type,
                        revision.byte_size::text,
                        coalesce(revision.title, ''),
                        coalesce(revision.language_code, ''),
                        coalesce(revision.source_uri, ''),
                        coalesce(revision.document_hint, ''),
                        coalesce(canonical_chunks.chunk_count::text, '0'),
                        coalesce(canonical_chunks.chunk_fingerprint, ''),
                        coalesce(knowledge_revision.normalized_text, ''),
                        coalesce(knowledge_revision.text_checksum, ''),
                        coalesce(knowledge_revision.image_checksum, ''),
                        knowledge_revision.text_state,
                        knowledge_revision.vector_state,
                        knowledge_revision.graph_state,
                        coalesce(knowledge_chunks.chunk_fingerprint, '')
                    ],
                    chr(31),
                    ''
                ) as fingerprint_part
            from canonical_readable as document
            join canonical_revisions as revision
              on revision.revision_id = document.revision_id
            join knowledge_revisions as knowledge_revision
              on knowledge_revision.revision_id = document.revision_id
            left join canonical_chunk_fingerprints as canonical_chunks
              on canonical_chunks.revision_id = document.revision_id
            left join knowledge_chunk_fingerprints as knowledge_chunks
              on knowledge_chunks.revision_id = document.revision_id
        ), content_fingerprint as (
            select coalesce(
                md5(string_agg(
                    fingerprint_part,
                    chr(30)
                    order by document_id
                )),
                md5('empty')
            ) as value
            from document_fingerprints
        )
        select
            content_fingerprint.value,
            library_source.source_truth_version,
            projection_parity.is_current
        from content_fingerprint
        cross join library_source
        cross join projection_parity",
        )
        .bind(library_id)
        .fetch_one(postgres)
        .await?;

    {
        let mut cache =
            fingerprint_cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // Cache negative parity too. All waiters for one broken generation
        // must observe the same fail-closed result instead of serially repeating
        // the full scan. Projection repair is an answer-visible write and must
        // advance the durable generation before this entry can be replaced.
        cache.insert(library_id, source_truth_version, value.clone(), projection_is_current);
    }
    Ok(LibraryReadableContentFingerprint { value, source_truth_version, projection_is_current })
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
            document_hint,
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
    get_revision_by_id_with_executor(postgres, revision_id).await
}

pub async fn get_revision_by_id_with_executor<'e, E>(
    executor: E,
    revision_id: Uuid,
) -> Result<Option<ContentRevisionRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
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
            document_hint,
            storage_key,
            created_by_principal_id,
            created_at
         from content_revision
         where id = $1",
    )
    .bind(revision_id)
    .fetch_optional(executor)
    .await
}

pub async fn update_revision_storage_key(
    postgres: &PgPool,
    revision_id: Uuid,
    storage_key: Option<&str>,
) -> Result<UpdateRevisionStorageKeyOutcome, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let Some((document_id, workspace_id, library_id)) = sqlx::query_as::<_, (Uuid, Uuid, Uuid)>(
        "select document_id, workspace_id, library_id
         from content_revision
         where id = $1",
    )
    .bind(revision_id)
    .fetch_optional(&mut *transaction)
    .await?
    else {
        transaction.rollback().await?;
        return Ok(UpdateRevisionStorageKeyOutcome::RevisionNotFound);
    };
    if !super::catalog_repository::lock_library_for_lifecycle_event_with_executor(
        &mut *transaction,
        workspace_id,
        library_id,
    )
    .await?
    {
        transaction.rollback().await?;
        return Ok(UpdateRevisionStorageKeyOutcome::RevisionNotFound);
    }
    let updated = sqlx::query_as::<_, ContentRevisionRow>(
        "update content_revision
         set storage_key = $2
         where id = $1
           and document_id = $3
           and workspace_id = $4
           and library_id = $5
           and storage_key is distinct from $2
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
            document_hint,
            storage_key,
            created_by_principal_id,
            created_at",
    )
    .bind(revision_id)
    .bind(storage_key)
    .bind(document_id)
    .bind(workspace_id)
    .bind(library_id)
    .fetch_optional(&mut *transaction)
    .await?;
    let changed = updated.is_some();
    let revision = if let Some(updated) = updated {
        updated
    } else {
        get_revision_by_id_with_executor(&mut *transaction, revision_id)
            .await?
            .ok_or(sqlx::Error::RowNotFound)?
    };
    let projection_updated = sqlx::query(
        "update knowledge_revision
         set storage_ref = $2
         where revision_id = $1
           and document_id = $3
           and workspace_id = $4
           and library_id = $5",
    )
    .bind(revision_id)
    .bind(storage_key)
    .bind(document_id)
    .bind(workspace_id)
    .bind(library_id)
    .execute(&mut *transaction)
    .await?;
    if projection_updated.rows_affected() != 1 {
        transaction.rollback().await?;
        return Ok(UpdateRevisionStorageKeyOutcome::KnowledgeProjectionNotFound);
    }

    let materialized = materialize_knowledge_document_from_canonical_head_with_transaction(
        &mut transaction,
        document_id,
    )
    .await?;
    if materialized.outcome != MaterializeKnowledgeDocumentOutcome::Materialized {
        transaction.rollback().await?;
        return Ok(UpdateRevisionStorageKeyOutcome::KnowledgeProjectionNotFound);
    }
    if changed || materialized.changed {
        super::catalog_repository::touch_library_source_truth_version_with_executor(
            &mut *transaction,
            library_id,
        )
        .await?;
    }
    transaction.commit().await?;
    Ok(UpdateRevisionStorageKeyOutcome::Updated(Box::new(revision)))
}

pub async fn update_revision_document_hint(
    postgres: &PgPool,
    revision_id: Uuid,
    document_hint: Option<&str>,
) -> Result<UpdateRevisionDocumentHintOutcome, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let Some((document_id, workspace_id, library_id)) = sqlx::query_as::<_, (Uuid, Uuid, Uuid)>(
        "select document_id, workspace_id, library_id
             from content_revision
             where id = $1",
    )
    .bind(revision_id)
    .fetch_optional(&mut *transaction)
    .await?
    else {
        transaction.rollback().await?;
        return Ok(UpdateRevisionDocumentHintOutcome::RevisionNotFound);
    };

    // Parent-first locking matches every other answer-visible content
    // transition. Canonical metadata, the query projection, and the cache
    // generation either commit together or remain entirely unchanged.
    if !super::catalog_repository::lock_library_for_lifecycle_event_with_executor(
        &mut *transaction,
        workspace_id,
        library_id,
    )
    .await?
    {
        transaction.rollback().await?;
        return Ok(UpdateRevisionDocumentHintOutcome::RevisionNotFound);
    }

    let updated_revision = sqlx::query_as::<_, ContentRevisionRow>(
        "update content_revision
         set document_hint = $2
         where id = $1
           and document_id = $3
           and workspace_id = $4
           and library_id = $5
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
            document_hint,
            storage_key,
            created_by_principal_id,
            created_at",
    )
    .bind(revision_id)
    .bind(document_hint)
    .bind(document_id)
    .bind(workspace_id)
    .bind(library_id)
    .fetch_optional(&mut *transaction)
    .await?;
    let Some(updated_revision) = updated_revision else {
        transaction.rollback().await?;
        return Ok(UpdateRevisionDocumentHintOutcome::RevisionNotFound);
    };

    let knowledge_updated = sqlx::query(
        "update knowledge_revision
         set document_hint = $2
         where revision_id = $1
           and document_id = $3
           and workspace_id = $4
           and library_id = $5",
    )
    .bind(revision_id)
    .bind(document_hint)
    .bind(document_id)
    .bind(workspace_id)
    .bind(library_id)
    .execute(&mut *transaction)
    .await?;
    if knowledge_updated.rows_affected() != 1 {
        transaction.rollback().await?;
        return Ok(UpdateRevisionDocumentHintOutcome::KnowledgeProjectionNotFound);
    }

    super::catalog_repository::touch_library_source_truth_version_with_executor(
        &mut *transaction,
        library_id,
    )
    .await?;
    transaction.commit().await?;
    Ok(UpdateRevisionDocumentHintOutcome::Updated(Box::new(updated_revision)))
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
            document_hint,
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
            document_hint,
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
    create_revision_with_executor(postgres, new_revision).await
}

/// Appends the next canonical revision and its query projection atomically.
///
/// The document parent lock serializes revision numbering, so two concurrent
/// appenders cannot derive the same revision number from a stale pre-read.
pub async fn create_revision_with_projection(
    postgres: &PgPool,
    new_revision: &NewContentRevisionProjection<'_>,
) -> Result<CreateContentRevisionOutcome, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    if !super::catalog_repository::lock_library_for_lifecycle_event_with_executor(
        &mut *transaction,
        new_revision.workspace_id,
        new_revision.library_id,
    )
    .await?
    {
        transaction.rollback().await?;
        return Ok(CreateContentRevisionOutcome::DocumentNotFound);
    }
    let Some(document) =
        lock_document_by_id_with_executor(&mut *transaction, new_revision.document_id).await?
    else {
        transaction.rollback().await?;
        return Ok(CreateContentRevisionOutcome::DocumentNotFound);
    };
    if document.workspace_id != new_revision.workspace_id
        || document.library_id != new_revision.library_id
    {
        transaction.rollback().await?;
        return Ok(CreateContentRevisionOutcome::DocumentNotFound);
    }
    if document.document_state == "deleted" || document.deleted_at.is_some() {
        transaction.rollback().await?;
        return Ok(CreateContentRevisionOutcome::DocumentDeleted);
    }
    let materialized = materialize_knowledge_document_from_canonical_head_with_transaction(
        &mut transaction,
        document.id,
    )
    .await?;
    if materialized.outcome != MaterializeKnowledgeDocumentOutcome::Materialized {
        transaction.rollback().await?;
        return Ok(CreateContentRevisionOutcome::ProjectionUnavailable);
    }

    let latest = sqlx::query_as::<_, (Uuid, i32)>(
        "select id, revision_number
         from content_revision
         where document_id = $1
           and workspace_id = $2
           and library_id = $3
         order by revision_number desc, created_at desc
         limit 1",
    )
    .bind(document.id)
    .bind(document.workspace_id)
    .bind(document.library_id)
    .fetch_optional(&mut *transaction)
    .await?;
    let revision_number = latest.as_ref().map_or(1, |(_, number)| number.saturating_add(1));
    let derived_revision = NewContentRevision {
        document_id: document.id,
        workspace_id: document.workspace_id,
        library_id: document.library_id,
        revision_number,
        parent_revision_id: latest.map(|(id, _)| id),
        content_source_kind: new_revision.content_source_kind,
        checksum: new_revision.checksum,
        mime_type: new_revision.mime_type,
        byte_size: new_revision.byte_size,
        title: new_revision.title,
        language_code: new_revision.language_code,
        source_uri: new_revision.source_uri,
        document_hint: new_revision.document_hint,
        storage_key: new_revision.storage_key,
        created_by_principal_id: new_revision.created_by_principal_id,
    };
    let revision = create_revision_with_executor(&mut *transaction, &derived_revision).await?;
    sqlx::query(
        "insert into knowledge_revision (
            revision_id, workspace_id, library_id, document_id, revision_number,
            revision_state, revision_kind, storage_ref, source_uri, document_hint, mime_type,
            checksum, title, byte_size, normalized_text, text_checksum, image_checksum,
            text_state, vector_state, graph_state, text_readable_at, vector_ready_at,
            graph_ready_at, superseded_by_revision_id, created_at
         ) values (
            $1, $2, $3, $4, $5,
            'accepted', $6, $7, $8, $9, $10,
            $11, $12, $13, null, null, null,
            'accepted', 'accepted', 'accepted', null, null,
            null, null, $14
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
    .execute(&mut *transaction)
    .await?;

    if materialized.changed {
        super::catalog_repository::touch_library_source_truth_version_with_executor(
            &mut *transaction,
            document.library_id,
        )
        .await?;
    }

    transaction.commit().await?;
    Ok(CreateContentRevisionOutcome::Created(Box::new(revision)))
}

pub async fn create_revision_with_executor<'e, E>(
    executor: E,
    new_revision: &NewContentRevision<'_>,
) -> Result<ContentRevisionRow, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
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
            document_hint,
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
            $16,
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
            document_hint,
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
    .bind(new_revision.document_hint)
    .bind(new_revision.storage_key)
    .bind(new_revision.created_by_principal_id)
    .fetch_one(executor)
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

pub async fn count_chunks_by_revision(
    postgres: &PgPool,
    revision_id: Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("select count(*) from content_chunk where revision_id = $1")
        .bind(revision_id)
        .fetch_one(postgres)
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
        row.push_bind(canonical_content_chunk_id(new_chunk))
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

const CONTENT_CHUNK_ID_NAMESPACE: Uuid = Uuid::from_u128(0x6f44_2a36_0f5d_4f18_8f6c_f11d_f356_8f5a);

fn canonical_content_chunk_id(chunk: &NewContentChunk<'_>) -> Uuid {
    let name = format!("{}:{}:{}", chunk.revision_id, chunk.chunk_index, chunk.text_checksum);
    Uuid::new_v5(&CONTENT_CHUNK_ID_NAMESPACE, name.as_bytes())
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

/// Replaces canonical and query-facing chunks in one transaction.
///
/// Vector rows are dimension-sharded runtime artifacts and are intentionally
/// removed by the caller before this transaction; that operation is fallible
/// and propagated. The durable text rows below either both commit or both stay
/// unchanged, including when a projection FK/constraint rejects the batch.
pub async fn replace_chunks_with_projection(
    postgres: &PgPool,
    revision_id: Uuid,
    chunks: &[NewContentChunkProjection],
) -> Result<Vec<ContentChunkRow>, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let Some((document_id, workspace_id, library_id)) = sqlx::query_as::<_, (Uuid, Uuid, Uuid)>(
        "select document_id, workspace_id, library_id
         from content_revision
         where id = $1",
    )
    .bind(revision_id)
    .fetch_optional(&mut *transaction)
    .await?
    else {
        transaction.rollback().await?;
        return Err(sqlx::Error::RowNotFound);
    };
    if !super::catalog_repository::lock_library_for_lifecycle_event_with_executor(
        &mut *transaction,
        workspace_id,
        library_id,
    )
    .await?
    {
        transaction.rollback().await?;
        return Err(sqlx::Error::RowNotFound);
    }
    let locked_document = lock_document_by_id_with_executor(&mut *transaction, document_id).await?;
    if locked_document.as_ref().is_none_or(|document| {
        document.workspace_id != workspace_id || document.library_id != library_id
    }) {
        transaction.rollback().await?;
        return Err(sqlx::Error::RowNotFound);
    }
    if chunks.iter().any(|chunk| {
        chunk.revision_id != revision_id
            || chunk.document_id != document_id
            || chunk.workspace_id != workspace_id
            || chunk.library_id != library_id
            || chunk.start_offset < 0
            || chunk.end_offset < chunk.start_offset
    }) {
        transaction.rollback().await?;
        return Err(sqlx::Error::Protocol(
            "chunk projection payload does not match its canonical revision scope".to_string(),
        ));
    }

    sqlx::query("delete from knowledge_chunk where revision_id = $1")
        .bind(revision_id)
        .execute(&mut *transaction)
        .await?;
    sqlx::query("delete from content_chunk where revision_id = $1")
        .bind(revision_id)
        .execute(&mut *transaction)
        .await?;

    const POSTGRES_MAX_BIND_PARAMETERS: usize = 65_535;
    const CONTENT_CHUNK_BIND_COUNT: usize = 10;
    const KNOWLEDGE_CHUNK_BIND_COUNT: usize = 24;
    let mut created_chunks = Vec::with_capacity(chunks.len());
    for batch in chunks.chunks(POSTGRES_MAX_BIND_PARAMETERS / CONTENT_CHUNK_BIND_COUNT) {
        let mut builder = QueryBuilder::<Postgres>::new(
            "insert into content_chunk (
                id, revision_id, chunk_index, start_offset, end_offset, token_count,
                normalized_text, text_checksum, occurred_at, occurred_until
             ) ",
        );
        builder.push_values(batch, |mut row, chunk| {
            row.push_bind(canonical_content_chunk_projection_id(chunk))
                .push_bind(chunk.revision_id)
                .push_bind(chunk.chunk_index)
                .push_bind(chunk.start_offset)
                .push_bind(chunk.end_offset)
                .push_bind(chunk.token_count)
                .push_bind(&chunk.normalized_text)
                .push_bind(&chunk.text_checksum)
                .push_bind(chunk.occurred_at)
                .push_bind(chunk.occurred_until);
        });
        builder.push(
            " returning
                id, revision_id, chunk_index, start_offset, end_offset, token_count,
                normalized_text, text_checksum, occurred_at, occurred_until",
        );
        created_chunks.extend(
            builder.build_query_as::<ContentChunkRow>().fetch_all(&mut *transaction).await?,
        );
    }
    for batch in chunks.chunks(POSTGRES_MAX_BIND_PARAMETERS / KNOWLEDGE_CHUNK_BIND_COUNT) {
        let mut builder = QueryBuilder::<Postgres>::new(
            "insert into knowledge_chunk (
                chunk_id, workspace_id, library_id, document_id, revision_id, chunk_index,
                chunk_kind, content_text, normalized_text, span_start, span_end, token_count,
                support_block_ids, section_path, heading_trail, literal_digest, chunk_state,
                text_generation, vector_generation, quality_score, window_text, raptor_level,
                occurred_at, occurred_until
             ) ",
        );
        builder.push_values(batch, |mut row, chunk| {
            row.push_bind(canonical_content_chunk_projection_id(chunk))
                .push_bind(chunk.workspace_id)
                .push_bind(chunk.library_id)
                .push_bind(chunk.document_id)
                .push_bind(chunk.revision_id)
                .push_bind(chunk.chunk_index)
                .push_bind(&chunk.chunk_kind)
                .push_bind(&chunk.content_text)
                .push_bind(&chunk.normalized_text)
                .push_bind(Some(chunk.start_offset))
                .push_bind(Some(chunk.end_offset))
                .push_bind(chunk.token_count)
                .push_bind(&chunk.support_block_ids)
                .push_bind(&chunk.section_path)
                .push_bind(&chunk.heading_trail)
                .push_bind(&chunk.literal_digest)
                .push_bind(&chunk.chunk_state)
                .push_bind(chunk.text_generation)
                .push_bind(chunk.vector_generation)
                .push_bind(chunk.quality_score)
                .push_bind(&chunk.window_text)
                .push_bind(Option::<i32>::None)
                .push_bind(chunk.occurred_at)
                .push_bind(chunk.occurred_until);
        });
        builder.build().execute(&mut *transaction).await?;
    }

    super::catalog_repository::touch_library_source_truth_version_with_executor(
        &mut *transaction,
        library_id,
    )
    .await?;
    transaction.commit().await?;
    created_chunks.sort_by_key(|chunk| chunk.chunk_index);
    Ok(created_chunks)
}

fn canonical_content_chunk_projection_id(chunk: &NewContentChunkProjection) -> Uuid {
    let name = format!("{}:{}:{}", chunk.revision_id, chunk.chunk_index, chunk.text_checksum);
    Uuid::new_v5(&CONTENT_CHUNK_ID_NAMESPACE, name.as_bytes())
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

pub async fn acquire_content_library_storage_lock(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    acquire_content_library_storage_lock_in_tx(&mut transaction, library_id).await?;
    Ok(transaction)
}

pub async fn acquire_content_library_storage_lock_in_tx(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1::text, 0))")
        .bind(format!("content.library.storage:{library_id}"))
        .execute(&mut **transaction)
        .await?;
    Ok(())
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
    create_mutation_item_with_executor(postgres, new_item).await
}

/// Creates one mutation item on a caller-owned connection or transaction.
///
/// Admission paths use this executor form so an item cannot become visible
/// without its exact queue job and pending document-head ownership.
pub async fn create_mutation_item_with_executor<'e, E>(
    executor: E,
    new_item: &NewContentMutationItem<'_>,
) -> Result<ContentMutationItemRow, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
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
    .fetch_one(executor)
    .await
}

/// Atomically claims one legacy mutation item that has no document target.
///
/// The mutation id is part of the compare-and-set predicate so a caller cannot
/// claim an item discovered through a stale or mismatched mutation graph. A
/// concurrent claimant can update the row only while `document_id` is null;
/// after one claim commits every later claimant receives `None` and must
/// classify the now-bound row without rewriting its target.
pub async fn claim_unbound_mutation_item(
    postgres: &PgPool,
    mutation_id: Uuid,
    item_id: Uuid,
    document_id: Uuid,
    base_revision_id: Option<Uuid>,
    result_revision_id: Option<Uuid>,
    item_state: &str,
    message: Option<&str>,
) -> Result<Option<ContentMutationItemRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentMutationItemRow>(
        "update content_mutation_item
         set document_id = $3,
             base_revision_id = $4,
             result_revision_id = $5,
             item_state = $6::content_mutation_item_state,
             message = $7
         where id = $1
           and mutation_id = $2
           and document_id is null
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
    .bind(mutation_id)
    .bind(document_id)
    .bind(base_revision_id)
    .bind(result_revision_id)
    .bind(item_state)
    .bind(message)
    .fetch_optional(postgres)
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
/// readiness, `file_name` fallback, source access) without any per-document
/// round-trips. Knowledge-plane readiness signals (`knowledge_revision.text_state`
/// etc.) are merged in by the caller.
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
    pub revision_document_hint: Option<String>,
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
    pub attempt_failure_message: Option<String>,
    pub attempt_progress_percent: Option<i32>,

    // per-document billing rollup — summed across every execution
    // attributed to this document (ingest_attempt + graph_extraction_attempt).
    // Surfaced on the canonical list response so the frontend never has
    // to issue a library-wide `/billing/library-document-costs` fetch to
    // fill in the cost column.
    /// `None` is a fail-closed mixed-currency sentinel. The service rejects
    /// the complete page instead of presenting a dimensionally invalid sum.
    pub cost_total: Option<rust_decimal::Decimal>,
    pub cost_currency_code: Option<String>,
    /// Scope health is selected in the same Postgres statement snapshot as
    /// the per-document cost, preventing a clean-check/stale-total race.
    pub billing_rollup_dirty: bool,
    pub billing_rollup_terminal_error_code: Option<String>,
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
///   `pg_trgm` index. Case-insensitive.
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
    id_filter: &[Uuid],
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
    // derivation — that requires the knowledge revision state which isn't in
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
        "with billing_health as materialized (
            select
                exists (
                    select 1
                    from billing_execution_cost_rollup_state rollup_state
                    where rollup_state.library_id = $1
                      and rollup_state.applied_generation < rollup_state.dirty_generation
                ) as rollup_dirty,
                (
                    select rollup_state.terminal_error_code
                    from billing_execution_cost_rollup_state rollup_state
                    where rollup_state.library_id = $1
                      and rollup_state.terminal_error_code is not null
                    order by rollup_state.owning_execution_id
                    limit 1
                ) as terminal_error_code
        ), joined as (
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
                r.document_hint as revision_document_hint,
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
              and (cardinality($8::uuid[]) = 0 or d.id = any($8))
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
            p.revision_document_hint,
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
            a.failure_message as attempt_failure_message,
            a.progress_percent as attempt_progress_percent,
            c.cost_total,
            c.cost_currency_code,
            billing_health.rollup_dirty as billing_rollup_dirty,
            billing_health.terminal_error_code as billing_rollup_terminal_error_code
        from page p
        cross join billing_health
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
                case
                    when count(distinct bec.currency_code) <= 1
                    then coalesce(sum(bec.total_cost), 0)
                end as cost_total,
                case
                    when count(distinct bec.currency_code) <= 1
                    then coalesce(min(bec.currency_code), 'USD')
                end as cost_currency_code
            from billing_execution_cost bec
            where bec.library_id = p.library_id
              and bec.knowledge_document_id = p.id
        ) c on not billing_health.rollup_dirty
              and billing_health.terminal_error_code is null
        order by {page_order_sql}",
    );

    // Bind order: $1 library_id, $2 include_deleted, $3 search,
    //             $4 cursor_ts, $5 cursor_id, $6 fetch_limit,
    //             $7 status_filter, $8 id_filter.
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
    let mut query = sqlx::query_as::<_, ContentDocumentListRow>(sqlx::AssertSqlSafe(&*sql))
        .persistent(false)
        .bind(library_id)
        .bind(include_deleted)
        .bind(search_pattern);
    query = query.bind(cursor_ts);
    query = query.bind(cursor_id);
    query = query.bind(fetch_limit);
    query = query.bind(status_filter);
    query = query.bind(id_filter);

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
    sqlx::query_as::<_, DocumentListStatusCountsRow>(sqlx::AssertSqlSafe(&*sql))
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
        r"with candidate as (
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
                -- Partial metadata matches are calibrated against the
                -- lexical knowledge lane that merges with these rows via
                -- max(score): title_soft_raw is about 50, while useful
                -- body BM25 hits commonly land in the low hundreds. A
                -- single title token should not flood the result set, but
                -- a multi-token title match should beat ordinary body text.
                when lower(coalesce(r.title, '')) like any($3) then
                    70::double precision + 80::double precision * least(
                        4,
                        (
                            select count(*)::integer
                            from unnest($3::text[]) as pattern(value)
                            where lower(coalesce(r.title, '')) like pattern.value
                        )
                    )
                when lower(d.external_key) like any($3) then
                    65::double precision + 75::double precision * least(
                        4,
                        (
                            select count(*)::integer
                            from unnest($3::text[]) as pattern(value)
                            where lower(d.external_key) like pattern.value
                        )
                    )
                else 70::double precision
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
         limit $5",
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
    const fn is_empty(&self) -> bool {
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
    use std::sync::Arc;

    use uuid::Uuid;

    use super::{
        FingerprintCache, NewContentChunk, canonical_content_chunk_id, fingerprint_singleflight,
        metadata_search_terms,
    };

    #[test]
    fn fingerprint_cache_evicts_the_least_recently_used_library() {
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        let third = Uuid::now_v7();
        let mut cache = FingerprintCache::with_capacity(2);

        cache.insert(first, 1, "first".to_string(), true);
        cache.insert(second, 1, "second".to_string(), true);
        assert_eq!(cache.get(first, 1).map(|entry| entry.0), Some("first".to_string()));

        cache.insert(third, 1, "third".to_string(), true);

        assert_eq!(cache.len(), 2);
        assert!(cache.get(second, 1).is_none());
        assert_eq!(cache.get(first, 1).map(|entry| entry.0), Some("first".to_string()));
        assert_eq!(cache.get(third, 1).map(|entry| entry.0), Some("third".to_string()));
    }

    #[test]
    fn fingerprint_cache_keeps_a_value_until_the_durable_generation_changes() {
        let library_id = Uuid::now_v7();
        let mut cache = FingerprintCache::with_capacity(1);
        cache.insert(library_id, 41, "stable".to_string(), true);

        for _ in 0..10_000 {
            assert_eq!(cache.get(library_id, 41), Some(("stable".to_string(), true)));
        }
        assert!(cache.get(library_id, 42).is_none());
    }

    #[test]
    fn fingerprint_cache_coalesces_fail_closed_results_for_one_generation() {
        let library_id = Uuid::now_v7();
        let mut cache = FingerprintCache::with_capacity(1);
        cache.insert(library_id, 7, "divergent".to_string(), false);

        assert_eq!(cache.get(library_id, 7), Some(("divergent".to_string(), false)));
        assert!(cache.get(library_id, 8).is_none());
    }

    #[tokio::test]
    async fn fingerprint_singleflight_reuses_one_async_lock_per_library() {
        let library_id = Uuid::now_v7();
        let first = fingerprint_singleflight(library_id);
        let second = fingerprint_singleflight(library_id);

        assert!(Arc::ptr_eq(&first, &second));

        let first_guard = first.lock().await;
        let waiting =
            tokio::time::timeout(std::time::Duration::from_millis(1), second.lock()).await;
        assert!(waiting.is_err(), "same-library recomputes must serialize");
        drop(first_guard);
    }

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
        let terms = metadata_search_terms("\"Version 7.8.\" \"Alpha Suite Administrator Guide\"");
        assert!(terms.version.iter().any(|term| term == "7.8"));
    }

    #[test]
    fn metadata_search_terms_requires_context_for_two_part_numbers() {
        let terms = metadata_search_terms("1.2");
        assert!(terms.version.is_empty());
        let terms = metadata_search_terms("Alpha 1.2");
        assert_eq!(terms.version, vec!["1.2"]);
    }

    #[test]
    fn canonical_content_chunk_id_is_stable_for_same_revision_chunk_and_text() {
        let revision_id = Uuid::parse_str("019e1dd5-70d8-7f70-a7a0-7605bda658d9").unwrap();
        let chunk = NewContentChunk {
            revision_id,
            chunk_index: 7,
            start_offset: 12,
            end_offset: 48,
            token_count: Some(9),
            normalized_text: "Alpha Suite stores project settings.",
            text_checksum: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            occurred_at: None,
            occurred_until: None,
        };
        let same_identity = NewContentChunk { token_count: Some(11), ..chunk.clone() };

        assert_eq!(canonical_content_chunk_id(&chunk), canonical_content_chunk_id(&same_identity));
    }

    #[test]
    fn canonical_content_chunk_id_changes_when_content_identity_changes() {
        let revision_id = Uuid::parse_str("019e1dd5-70d8-7f70-a7a0-7605bda658d9").unwrap();
        let chunk = NewContentChunk {
            revision_id,
            chunk_index: 7,
            start_offset: 12,
            end_offset: 48,
            token_count: Some(9),
            normalized_text: "Alpha Suite stores project settings.",
            text_checksum: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            occurred_at: None,
            occurred_until: None,
        };
        let different_checksum = NewContentChunk {
            text_checksum: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            ..chunk.clone()
        };

        assert_ne!(
            canonical_content_chunk_id(&chunk),
            canonical_content_chunk_id(&different_checksum)
        );
    }
}
