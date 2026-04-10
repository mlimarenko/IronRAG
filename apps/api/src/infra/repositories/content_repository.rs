use chrono::{DateTime, Utc};
use sqlx::{Executor, FromRow, PgPool, Postgres, QueryBuilder, pool::PoolConnection};
use uuid::Uuid;

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
         order by created_at desc
         limit 1000",
    )
    .bind(library_id)
    .fetch_all(postgres)
    .await
}

pub async fn get_document_by_id(
    postgres: &PgPool,
    document_id: Uuid,
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
         where id = $1",
    )
    .bind(document_id)
    .fetch_optional(postgres)
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
    .fetch_one(postgres)
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
            text_checksum
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
            text_checksum
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
            text_checksum
        )
        values ($1, $2, $3, $4, $5, $6, $7, $8)
        returning
            id,
            revision_id,
            chunk_index,
            start_offset,
            end_offset,
            token_count,
            normalized_text,
            text_checksum",
    )
    .bind(Uuid::now_v7())
    .bind(new_chunk.revision_id)
    .bind(new_chunk.chunk_index)
    .bind(new_chunk.start_offset)
    .bind(new_chunk.end_offset)
    .bind(new_chunk.token_count)
    .bind(new_chunk.normalized_text)
    .bind(new_chunk.text_checksum)
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

    let mut builder = QueryBuilder::<Postgres>::new(
        "insert into content_chunk (
            id,
            revision_id,
            chunk_index,
            start_offset,
            end_offset,
            token_count,
            normalized_text,
            text_checksum
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
            .push_bind(new_chunk.text_checksum);
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
            text_checksum",
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
) -> Result<PoolConnection<Postgres>, sqlx::Error> {
    let mut connection = postgres.acquire().await?;
    sqlx::query("select pg_advisory_lock(hashtextextended($1::text, 0))")
        .bind(format!("content.mutation:{mutation_id}"))
        .execute(&mut *connection)
        .await?;
    Ok(connection)
}

pub async fn acquire_content_document_lock(
    postgres: &PgPool,
    document_id: Uuid,
) -> Result<PoolConnection<Postgres>, sqlx::Error> {
    let mut connection = postgres.acquire().await?;
    sqlx::query("select pg_advisory_lock(hashtextextended($1::text, 0))")
        .bind(format!("content.document:{document_id}"))
        .execute(&mut *connection)
        .await?;
    Ok(connection)
}

pub async fn release_content_mutation_lock(
    mut connection: PoolConnection<Postgres>,
    mutation_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("select pg_advisory_unlock(hashtextextended($1::text, 0))")
        .bind(format!("content.mutation:{mutation_id}"))
        .execute(&mut *connection)
        .await?;
    Ok(())
}

pub async fn release_content_document_lock(
    mut connection: PoolConnection<Postgres>,
    document_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("select pg_advisory_unlock(hashtextextended($1::text, 0))")
        .bind(format!("content.document:{document_id}"))
        .execute(&mut *connection)
        .await?;
    Ok(())
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
