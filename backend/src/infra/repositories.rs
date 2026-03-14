use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct WorkspaceRow {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ProjectRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ProviderAccountRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub provider_kind: String,
    pub label: String,
    pub api_base_url: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ModelProfileRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub provider_account_id: Uuid,
    pub profile_kind: String,
    pub model_name: String,
    pub temperature: Option<f64>,
    pub max_output_tokens: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Database repository helper: `list_workspaces`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn list_workspaces(pool: &PgPool) -> Result<Vec<WorkspaceRow>, sqlx::Error> {
    sqlx::query_as::<_, WorkspaceRow>(
        "select id, slug, name, status, created_at, updated_at from workspace order by created_at desc",
    )
    .fetch_all(pool)
    .await
}

/// Database repository helper: `create_workspace`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn create_workspace(
    pool: &PgPool,
    slug: &str,
    name: &str,
) -> Result<WorkspaceRow, sqlx::Error> {
    sqlx::query_as::<_, WorkspaceRow>(
        "insert into workspace (id, slug, name) values ($1, $2, $3)
         returning id, slug, name, status, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(slug)
    .bind(name)
    .fetch_one(pool)
    .await
}

/// Database repository helper: `list_projects`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn list_projects(
    pool: &PgPool,
    workspace_id: Option<Uuid>,
) -> Result<Vec<ProjectRow>, sqlx::Error> {
    match workspace_id {
        Some(workspace_id) => {
            sqlx::query_as::<_, ProjectRow>(
                "select id, workspace_id, slug, name, description, created_at, updated_at
                 from project where workspace_id = $1 order by created_at desc",
            )
            .bind(workspace_id)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, ProjectRow>(
                "select id, workspace_id, slug, name, description, created_at, updated_at
                 from project order by created_at desc",
            )
            .fetch_all(pool)
            .await
        }
    }
}

/// Database repository helper: `create_project`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn create_project(
    pool: &PgPool,
    workspace_id: Uuid,
    slug: &str,
    name: &str,
    description: Option<&str>,
) -> Result<ProjectRow, sqlx::Error> {
    sqlx::query_as::<_, ProjectRow>(
        "insert into project (id, workspace_id, slug, name, description) values ($1, $2, $3, $4, $5)
         returning id, workspace_id, slug, name, description, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(workspace_id)
    .bind(slug)
    .bind(name)
    .bind(description)
    .fetch_one(pool)
    .await
}

/// Database repository helper: `list_provider_accounts`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn list_provider_accounts(
    pool: &PgPool,
    workspace_id: Option<Uuid>,
) -> Result<Vec<ProviderAccountRow>, sqlx::Error> {
    match workspace_id {
        Some(workspace_id) => {
            sqlx::query_as::<_, ProviderAccountRow>(
                "select id, workspace_id, provider_kind, label, api_base_url, status, created_at, updated_at
                 from provider_account where workspace_id = $1 order by created_at desc",
            )
            .bind(workspace_id)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, ProviderAccountRow>(
                "select id, workspace_id, provider_kind, label, api_base_url, status, created_at, updated_at
                 from provider_account order by created_at desc",
            )
            .fetch_all(pool)
            .await
        }
    }
}

/// Database repository helper: `create_provider_account`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn create_provider_account(
    pool: &PgPool,
    workspace_id: Uuid,
    provider_kind: &str,
    label: &str,
    api_base_url: Option<&str>,
) -> Result<ProviderAccountRow, sqlx::Error> {
    sqlx::query_as::<_, ProviderAccountRow>(
        "insert into provider_account (id, workspace_id, provider_kind, label, api_base_url)
         values ($1, $2, $3, $4, $5)
         returning id, workspace_id, provider_kind, label, api_base_url, status, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(workspace_id)
    .bind(provider_kind)
    .bind(label)
    .bind(api_base_url)
    .fetch_one(pool)
    .await
}

/// Database repository helper: `list_model_profiles`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn list_model_profiles(
    pool: &PgPool,
    workspace_id: Option<Uuid>,
) -> Result<Vec<ModelProfileRow>, sqlx::Error> {
    match workspace_id {
        Some(workspace_id) => {
            sqlx::query_as::<_, ModelProfileRow>(
                "select id, workspace_id, provider_account_id, profile_kind, model_name, temperature, max_output_tokens, created_at, updated_at
                 from model_profile where workspace_id = $1 order by created_at desc",
            )
            .bind(workspace_id)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, ModelProfileRow>(
                "select id, workspace_id, provider_account_id, profile_kind, model_name, temperature, max_output_tokens, created_at, updated_at
                 from model_profile order by created_at desc",
            )
            .fetch_all(pool)
            .await
        }
    }
}

/// Database repository helper: `create_model_profile`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn create_model_profile(
    pool: &PgPool,
    workspace_id: Uuid,
    provider_account_id: Uuid,
    profile_kind: &str,
    model_name: &str,
    temperature: Option<f64>,
    max_output_tokens: Option<i32>,
) -> Result<ModelProfileRow, sqlx::Error> {
    sqlx::query_as::<_, ModelProfileRow>(
        "insert into model_profile (id, workspace_id, provider_account_id, profile_kind, model_name, temperature, max_output_tokens)
         values ($1, $2, $3, $4, $5, $6, $7)
         returning id, workspace_id, provider_account_id, profile_kind, model_name, temperature, max_output_tokens, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(workspace_id)
    .bind(provider_account_id)
    .bind(profile_kind)
    .bind(model_name)
    .bind(temperature)
    .bind(max_output_tokens)
    .fetch_one(pool)
    .await
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct SourceRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub source_kind: String,
    pub label: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct IngestionJobRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub source_id: Option<Uuid>,
    pub trigger_kind: String,
    pub status: String,
    pub stage: String,
    pub requested_by: Option<String>,
    pub error_message: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub idempotency_key: Option<String>,
    pub parent_job_id: Option<Uuid>,
    pub attempt_count: i32,
    pub worker_id: Option<String>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub heartbeat_at: Option<DateTime<Utc>>,
    pub payload_json: serde_json::Value,
    pub result_json: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestionExecutionPayload {
    pub project_id: Uuid,
    pub source_id: Option<Uuid>,
    pub external_key: String,
    pub title: Option<String>,
    pub mime_type: Option<String>,
    pub text: String,
    pub ingest_mode: String,
    pub extra_metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct IngestionJobAttemptRow {
    pub id: Uuid,
    pub job_id: Uuid,
    pub attempt_no: i32,
    pub worker_id: Option<String>,
    pub status: String,
    pub stage: String,
    pub error_message: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Database repository helper: `list_sources`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn list_sources(
    pool: &PgPool,
    project_id: Option<Uuid>,
) -> Result<Vec<SourceRow>, sqlx::Error> {
    match project_id {
        Some(project_id) => {
            sqlx::query_as::<_, SourceRow>(
                "select id, project_id, source_kind, label, status, created_at, updated_at
                 from source where project_id = $1 order by created_at desc",
            )
            .bind(project_id)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, SourceRow>(
                "select id, project_id, source_kind, label, status, created_at, updated_at
                 from source order by created_at desc",
            )
            .fetch_all(pool)
            .await
        }
    }
}

/// Database repository helper: `create_source`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn create_source(
    pool: &PgPool,
    project_id: Uuid,
    source_kind: &str,
    label: &str,
) -> Result<SourceRow, sqlx::Error> {
    sqlx::query_as::<_, SourceRow>(
        "insert into source (id, project_id, source_kind, label) values ($1, $2, $3, $4)
         returning id, project_id, source_kind, label, status, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(source_kind)
    .bind(label)
    .fetch_one(pool)
    .await
}

/// Database repository helper: `list_ingestion_jobs`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn list_ingestion_jobs(
    pool: &PgPool,
    project_id: Option<Uuid>,
) -> Result<Vec<IngestionJobRow>, sqlx::Error> {
    match project_id {
        Some(project_id) => {
            sqlx::query_as::<_, IngestionJobRow>(
                "select id, project_id, source_id, trigger_kind, status, stage, requested_by, error_message, started_at, finished_at, created_at, updated_at, idempotency_key, parent_job_id, attempt_count, worker_id, lease_expires_at, heartbeat_at, payload_json, result_json
                 from ingestion_job where project_id = $1 order by created_at desc",
            )
            .bind(project_id)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, IngestionJobRow>(
                "select id, project_id, source_id, trigger_kind, status, stage, requested_by, error_message, started_at, finished_at, created_at, updated_at, idempotency_key, parent_job_id, attempt_count, worker_id, lease_expires_at, heartbeat_at, payload_json, result_json
                 from ingestion_job order by created_at desc",
            )
            .fetch_all(pool)
            .await
        }
    }
}

/// Database repository helper: `create_ingestion_job`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn create_ingestion_job(
    pool: &PgPool,
    project_id: Uuid,
    source_id: Option<Uuid>,
    trigger_kind: &str,
    requested_by: Option<&str>,
    parent_job_id: Option<Uuid>,
    idempotency_key: Option<&str>,
    payload_json: serde_json::Value,
) -> Result<IngestionJobRow, sqlx::Error> {
    sqlx::query_as::<_, IngestionJobRow>(
        "insert into ingestion_job (id, project_id, source_id, trigger_kind, status, stage, requested_by, parent_job_id, idempotency_key, payload_json)
         values ($1, $2, $3, $4, 'queued', 'created', $5, $6, $7, $8)
         returning id, project_id, source_id, trigger_kind, status, stage, requested_by, error_message, started_at, finished_at, created_at, updated_at, idempotency_key, parent_job_id, attempt_count, worker_id, lease_expires_at, heartbeat_at, payload_json, result_json",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(source_id)
    .bind(trigger_kind)
    .bind(requested_by)
    .bind(parent_job_id)
    .bind(idempotency_key)
    .bind(payload_json)
    .fetch_one(pool)
    .await
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct DocumentRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub source_id: Option<Uuid>,
    pub external_key: String,
    pub title: Option<String>,
    pub mime_type: Option<String>,
    pub checksum: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Database repository helper: `list_documents`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn list_documents(
    pool: &PgPool,
    project_id: Option<Uuid>,
) -> Result<Vec<DocumentRow>, sqlx::Error> {
    match project_id {
        Some(project_id) => {
            sqlx::query_as::<_, DocumentRow>(
                "select id, project_id, source_id, external_key, title, mime_type, checksum, created_at, updated_at
                 from document where project_id = $1 order by created_at desc",
            )
            .bind(project_id)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, DocumentRow>(
                "select id, project_id, source_id, external_key, title, mime_type, checksum, created_at, updated_at
                 from document order by created_at desc",
            )
            .fetch_all(pool)
            .await
        }
    }
}

/// Database repository helper: `create_document`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn create_document(
    pool: &PgPool,
    project_id: Uuid,
    source_id: Option<Uuid>,
    external_key: &str,
    title: Option<&str>,
    mime_type: Option<&str>,
    checksum: Option<&str>,
) -> Result<DocumentRow, sqlx::Error> {
    sqlx::query_as::<_, DocumentRow>(
        "insert into document (id, project_id, source_id, external_key, title, mime_type, checksum)
         values ($1, $2, $3, $4, $5, $6, $7)
         returning id, project_id, source_id, external_key, title, mime_type, checksum, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(source_id)
    .bind(external_key)
    .bind(title)
    .bind(mime_type)
    .bind(checksum)
    .fetch_one(pool)
    .await
}

/// Database repository helper: `get_document_by_id`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn get_document_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<DocumentRow>, sqlx::Error> {
    sqlx::query_as::<_, DocumentRow>(
        "select id, project_id, source_id, external_key, title, mime_type, checksum, created_at, updated_at
         from document where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RetrievalRunRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub query_text: String,
    pub model_profile_id: Option<Uuid>,
    pub top_k: i32,
    pub response_text: Option<String>,
    pub debug_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

/// Database repository helper: `list_retrieval_runs`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn list_retrieval_runs(
    pool: &PgPool,
    project_id: Option<Uuid>,
) -> Result<Vec<RetrievalRunRow>, sqlx::Error> {
    match project_id {
        Some(project_id) => {
            sqlx::query_as::<_, RetrievalRunRow>(
                "select id, project_id, query_text, model_profile_id, top_k, response_text, debug_json, created_at
                 from retrieval_run where project_id = $1 order by created_at desc",
            )
            .bind(project_id)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, RetrievalRunRow>(
                "select id, project_id, query_text, model_profile_id, top_k, response_text, debug_json, created_at
                 from retrieval_run order by created_at desc",
            )
            .fetch_all(pool)
            .await
        }
    }
}

/// Database repository helper: `create_retrieval_run`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn create_retrieval_run(
    pool: &PgPool,
    project_id: Uuid,
    query_text: &str,
    model_profile_id: Option<Uuid>,
    top_k: i32,
    response_text: Option<&str>,
    debug_json: serde_json::Value,
) -> Result<RetrievalRunRow, sqlx::Error> {
    sqlx::query_as::<_, RetrievalRunRow>(
        "insert into retrieval_run (id, project_id, query_text, model_profile_id, top_k, response_text, debug_json)
         values ($1, $2, $3, $4, $5, $6, $7)
         returning id, project_id, query_text, model_profile_id, top_k, response_text, debug_json, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(project_id)
    .bind(query_text)
    .bind(model_profile_id)
    .bind(top_k)
    .bind(response_text)
    .bind(debug_json)
    .fetch_one(pool)
    .await
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ChunkRow {
    pub id: Uuid,
    pub document_id: Uuid,
    pub project_id: Uuid,
    pub ordinal: i32,
    pub content: String,
    pub token_count: Option<i32>,
    pub metadata_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

/// Database repository helper: `create_chunk`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn create_chunk(
    pool: &PgPool,
    document_id: Uuid,
    project_id: Uuid,
    ordinal: i32,
    content: &str,
    token_count: Option<i32>,
    metadata_json: serde_json::Value,
) -> Result<ChunkRow, sqlx::Error> {
    sqlx::query_as::<_, ChunkRow>(
        "insert into chunk (id, document_id, project_id, ordinal, content, token_count, metadata_json)
         values ($1, $2, $3, $4, $5, $6, $7)
         returning id, document_id, project_id, ordinal, content, token_count, metadata_json, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(document_id)
    .bind(project_id)
    .bind(ordinal)
    .bind(content)
    .bind(token_count)
    .bind(metadata_json)
    .fetch_one(pool)
    .await
}

/// Database repository helper: `list_chunks_by_document`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn list_chunks_by_document(
    pool: &PgPool,
    document_id: Uuid,
) -> Result<Vec<ChunkRow>, sqlx::Error> {
    sqlx::query_as::<_, ChunkRow>(
        "select id, document_id, project_id, ordinal, content, token_count, metadata_json, created_at
         from chunk where document_id = $1 order by ordinal asc",
    )
    .bind(document_id)
    .fetch_all(pool)
    .await
}

/// Database repository helper: `search_chunks_by_project`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn search_chunks_by_project(
    pool: &PgPool,
    project_id: Uuid,
    query_text: &str,
    top_k: i32,
) -> Result<Vec<ChunkRow>, sqlx::Error> {
    let pattern = format!("%{query_text}%");
    sqlx::query_as::<_, ChunkRow>(
        "select id, document_id, project_id, ordinal, content, token_count, metadata_json, created_at
         from chunk
         where project_id = $1 and content ilike $2
         order by ordinal asc
         limit $3",
    )
    .bind(project_id)
    .bind(pattern)
    .bind(top_k)
    .fetch_all(pool)
    .await
}

/// Database repository helper: `list_chunks_by_project`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn list_chunks_by_project(
    pool: &PgPool,
    project_id: Uuid,
    limit: i64,
) -> Result<Vec<ChunkRow>, sqlx::Error> {
    sqlx::query_as::<_, ChunkRow>(
        "select id, document_id, project_id, ordinal, content, token_count, metadata_json, created_at
         from chunk where project_id = $1 order by created_at desc limit $2",
    )
    .bind(project_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ChunkEmbeddingRow {
    pub chunk_id: Uuid,
    pub project_id: Uuid,
    pub provider_kind: String,
    pub model_name: String,
    pub dimensions: i32,
    pub embedding_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Database repository helper: `upsert_chunk_embedding`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn upsert_chunk_embedding(
    pool: &PgPool,
    chunk_id: Uuid,
    project_id: Uuid,
    provider_kind: &str,
    model_name: &str,
    dimensions: i32,
    embedding_json: serde_json::Value,
) -> Result<ChunkEmbeddingRow, sqlx::Error> {
    sqlx::query_as::<_, ChunkEmbeddingRow>(
        "insert into chunk_embedding (chunk_id, project_id, provider_kind, model_name, dimensions, embedding_json)
         values ($1, $2, $3, $4, $5, $6)
         on conflict (chunk_id) do update set
           provider_kind = excluded.provider_kind,
           model_name = excluded.model_name,
           dimensions = excluded.dimensions,
           embedding_json = excluded.embedding_json,
           updated_at = now()
         returning chunk_id, project_id, provider_kind, model_name, dimensions, embedding_json, created_at, updated_at",
    )
    .bind(chunk_id)
    .bind(project_id)
    .bind(provider_kind)
    .bind(model_name)
    .bind(dimensions)
    .bind(embedding_json)
    .fetch_one(pool)
    .await
}

/// Database repository helper: `list_chunk_embeddings_by_project`.
///
/// # Errors
/// Returns any `SQLx` error raised while executing the underlying database query.
pub async fn list_chunk_embeddings_by_project(
    pool: &PgPool,
    project_id: Uuid,
    limit: i64,
) -> Result<Vec<ChunkEmbeddingRow>, sqlx::Error> {
    sqlx::query_as::<_, ChunkEmbeddingRow>(
        "select chunk_id, project_id, provider_kind, model_name, dimensions, embedding_json, created_at, updated_at
         from chunk_embedding where project_id = $1 order by updated_at desc limit $2",
    )
    .bind(project_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ApiTokenRow {
    pub id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub token_kind: String,
    pub label: String,
    pub token_hash: String,
    pub scope_json: serde_json::Value,
    pub status: String,
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Finds an active API token row by its hashed token value.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the `api_token` row.
pub async fn find_api_token_by_hash(
    pool: &PgPool,
    token_hash: &str,
) -> Result<Option<ApiTokenRow>, sqlx::Error> {
    sqlx::query_as::<_, ApiTokenRow>(
        "select id, workspace_id, token_kind, label, token_hash, scope_json, status, last_used_at, created_at, updated_at
         from api_token where token_hash = $1 and status = 'active'",
    )
    .bind(token_hash)
    .fetch_optional(pool)
    .await
}

/// Updates the last-used timestamp for an API token.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the `api_token` row.
pub async fn touch_api_token_last_used(pool: &PgPool, token_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("update api_token set last_used_at = now(), updated_at = now() where id = $1")
        .bind(token_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Creates a new API token row.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the `api_token` row.
pub async fn create_api_token(
    pool: &PgPool,
    workspace_id: Option<Uuid>,
    token_kind: &str,
    label: &str,
    token_hash: &str,
    scope_json: serde_json::Value,
) -> Result<ApiTokenRow, sqlx::Error> {
    sqlx::query_as::<_, ApiTokenRow>(
        "insert into api_token (id, workspace_id, token_kind, label, token_hash, scope_json)
         values ($1, $2, $3, $4, $5, $6)
         returning id, workspace_id, token_kind, label, token_hash, scope_json, status, last_used_at, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(workspace_id)
    .bind(token_kind)
    .bind(label)
    .bind(token_hash)
    .bind(scope_json)
    .fetch_one(pool)
    .await
}

/// Loads an API token by primary key.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the `api_token` row.
pub async fn get_api_token_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<ApiTokenRow>, sqlx::Error> {
    sqlx::query_as::<_, ApiTokenRow>(
        "select id, workspace_id, token_kind, label, token_hash, scope_json, status, last_used_at, created_at, updated_at
         from api_token where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Lists API tokens, optionally filtered by workspace.
///
/// # Errors
/// Returns any `SQLx` error raised while querying `api_token` rows.
pub async fn list_api_tokens(
    pool: &PgPool,
    workspace_id: Option<Uuid>,
) -> Result<Vec<ApiTokenRow>, sqlx::Error> {
    match workspace_id {
        Some(workspace_id) => {
            sqlx::query_as::<_, ApiTokenRow>(
                "select id, workspace_id, token_kind, label, token_hash, scope_json, status, last_used_at, created_at, updated_at
                 from api_token where workspace_id = $1 order by created_at desc",
            )
            .bind(workspace_id)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, ApiTokenRow>(
                "select id, workspace_id, token_kind, label, token_hash, scope_json, status, last_used_at, created_at, updated_at
                 from api_token order by created_at desc",
            )
            .fetch_all(pool)
            .await
        }
    }
}

/// Loads a model profile by primary key.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the `model_profile` row.
pub async fn get_model_profile_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<ModelProfileRow>, sqlx::Error> {
    sqlx::query_as::<_, ModelProfileRow>(
        "select id, workspace_id, provider_account_id, profile_kind, model_name, temperature, max_output_tokens, created_at, updated_at
         from model_profile where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Loads a provider account by primary key.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the `provider_account` row.
pub async fn get_provider_account_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<ProviderAccountRow>, sqlx::Error> {
    sqlx::query_as::<_, ProviderAccountRow>(
        "select id, workspace_id, provider_kind, label, api_base_url, status, created_at, updated_at
         from provider_account where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct UsageEventRow {
    pub id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub provider_account_id: Option<Uuid>,
    pub model_profile_id: Option<Uuid>,
    pub usage_kind: String,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub total_tokens: Option<i32>,
    pub raw_usage_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct NewUsageEvent {
    pub workspace_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub provider_account_id: Option<Uuid>,
    pub model_profile_id: Option<Uuid>,
    pub usage_kind: String,
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    pub total_tokens: Option<i32>,
    pub raw_usage_json: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct CostLedgerRow {
    pub id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub usage_event_id: Uuid,
    pub provider_kind: String,
    pub model_name: String,
    pub currency: String,
    pub estimated_cost: rust_decimal::Decimal,
    pub pricing_snapshot_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

/// Creates a persisted usage event row for token/cost accounting.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the `usage_event` row.
pub async fn create_usage_event(
    pool: &PgPool,
    new_event: &NewUsageEvent,
) -> Result<UsageEventRow, sqlx::Error> {
    sqlx::query_as::<_, UsageEventRow>(
        "insert into usage_event (id, workspace_id, project_id, provider_account_id, model_profile_id, usage_kind, prompt_tokens, completion_tokens, total_tokens, raw_usage_json)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         returning id, workspace_id, project_id, provider_account_id, model_profile_id, usage_kind, prompt_tokens, completion_tokens, total_tokens, raw_usage_json, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(new_event.workspace_id)
    .bind(new_event.project_id)
    .bind(new_event.provider_account_id)
    .bind(new_event.model_profile_id)
    .bind(&new_event.usage_kind)
    .bind(new_event.prompt_tokens)
    .bind(new_event.completion_tokens)
    .bind(new_event.total_tokens)
    .bind(new_event.raw_usage_json.clone())
    .fetch_one(pool)
    .await
}

/// Creates a persisted cost ledger row linked to a usage event.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the `cost_ledger` row.
pub async fn create_cost_ledger(
    pool: &PgPool,
    workspace_id: Option<Uuid>,
    project_id: Option<Uuid>,
    usage_event_id: Uuid,
    provider_kind: &str,
    model_name: &str,
    estimated_cost: rust_decimal::Decimal,
    pricing_snapshot_json: serde_json::Value,
) -> Result<CostLedgerRow, sqlx::Error> {
    sqlx::query_as::<_, CostLedgerRow>(
        "insert into cost_ledger (id, workspace_id, project_id, usage_event_id, provider_kind, model_name, estimated_cost, pricing_snapshot_json)
         values ($1, $2, $3, $4, $5, $6, $7, $8)
         returning id, workspace_id, project_id, usage_event_id, provider_kind, model_name, currency, estimated_cost, pricing_snapshot_json, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(workspace_id)
    .bind(project_id)
    .bind(usage_event_id)
    .bind(provider_kind)
    .bind(model_name)
    .bind(estimated_cost)
    .bind(pricing_snapshot_json)
    .fetch_one(pool)
    .await
}

/// Loads a project by primary key.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the `project` row.
pub async fn get_project_by_id(pool: &PgPool, id: Uuid) -> Result<Option<ProjectRow>, sqlx::Error> {
    sqlx::query_as::<_, ProjectRow>(
        "select id, workspace_id, slug, name, description, created_at, updated_at from project where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Loads a workspace by primary key.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the `workspace` row.
pub async fn get_workspace_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<WorkspaceRow>, sqlx::Error> {
    sqlx::query_as::<_, WorkspaceRow>(
        "select id, slug, name, status, created_at, updated_at from workspace where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Loads an ingestion job by primary key.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the `ingestion_job` row.
pub async fn get_ingestion_job_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<IngestionJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestionJobRow>(
        "select id, project_id, source_id, trigger_kind, status, stage, requested_by, error_message, started_at, finished_at, created_at, updated_at, idempotency_key, parent_job_id, attempt_count, worker_id, lease_expires_at, heartbeat_at, payload_json, result_json
         from ingestion_job where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Loads a retrieval run by primary key.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the `retrieval_run` row.
pub fn parse_ingestion_execution_payload(
    row: &IngestionJobRow,
) -> Result<IngestionExecutionPayload, serde_json::Error> {
    serde_json::from_value(row.payload_json.clone())
}

pub async fn record_ingestion_job_attempt_claim(
    pool: &PgPool,
    job_id: Uuid,
    attempt_no: i32,
    worker_id: &str,
    stage: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into ingestion_job_attempt (id, job_id, attempt_no, worker_id, status, stage)
         values ($1, $2, $3, $4, 'running', $5)
         on conflict (job_id, attempt_no) do update
         set worker_id = excluded.worker_id,
             status = excluded.status,
             stage = excluded.stage,
             error_message = null,
             finished_at = null",
    )
    .bind(Uuid::now_v7())
    .bind(job_id)
    .bind(attempt_no)
    .bind(worker_id)
    .bind(stage)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn mark_ingestion_job_attempt_stage(
    pool: &PgPool,
    job_id: Uuid,
    attempt_no: i32,
    worker_id: &str,
    status: &str,
    stage: &str,
    error_message: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update ingestion_job_attempt
         set worker_id = $4,
             status = $5,
             stage = $6,
             error_message = $7
         where job_id = $1 and attempt_no = $2 and (worker_id = $3 or worker_id is null)",
    )
    .bind(job_id)
    .bind(attempt_no)
    .bind(worker_id)
    .bind(worker_id)
    .bind(status)
    .bind(stage)
    .bind(error_message)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn complete_ingestion_job_attempt(
    pool: &PgPool,
    job_id: Uuid,
    attempt_no: i32,
    worker_id: &str,
    stage: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update ingestion_job_attempt
         set worker_id = $4,
             status = 'completed',
             stage = $5,
             error_message = null,
             finished_at = now()
         where job_id = $1 and attempt_no = $2 and (worker_id = $3 or worker_id is null)",
    )
    .bind(job_id)
    .bind(attempt_no)
    .bind(worker_id)
    .bind(worker_id)
    .bind(stage)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn fail_ingestion_job_attempt(
    pool: &PgPool,
    job_id: Uuid,
    attempt_no: i32,
    worker_id: &str,
    stage: &str,
    error_message: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update ingestion_job_attempt
         set worker_id = $4,
             status = 'retryable_failed',
             stage = $5,
             error_message = $6,
             finished_at = now()
         where job_id = $1 and attempt_no = $2 and (worker_id = $3 or worker_id is null)",
    )
    .bind(job_id)
    .bind(attempt_no)
    .bind(worker_id)
    .bind(worker_id)
    .bind(stage)
    .bind(error_message)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn recover_expired_ingestion_job_leases(
    pool: &PgPool,
) -> Result<Vec<IngestionJobRow>, sqlx::Error> {
    sqlx::query_as::<_, IngestionJobRow>(
        "update ingestion_job
         set status = 'queued',
             stage = 'requeued_after_lease_expiry',
             worker_id = null,
             lease_expires_at = null,
             error_message = null,
             updated_at = now()
         where status = 'running'
           and lease_expires_at is not null
           and lease_expires_at < now()
         returning id, project_id, source_id, trigger_kind, status, stage, requested_by, error_message, started_at, finished_at, created_at, updated_at, idempotency_key, parent_job_id, attempt_count, worker_id, lease_expires_at, heartbeat_at, payload_json, result_json",
    )
    .fetch_all(pool)
    .await
}

pub async fn claim_next_ingestion_job(
    pool: &PgPool,
    worker_id: &str,
    lease_duration: chrono::Duration,
) -> Result<Option<IngestionJobRow>, sqlx::Error> {
    let lease_expires_at = Utc::now() + lease_duration;
    let claimed = sqlx::query_as::<_, IngestionJobRow>(
        "with candidate as (
            select id
            from ingestion_job
            where status = 'queued'
              and (lease_expires_at is null or lease_expires_at < now())
            order by created_at asc
            limit 1
            for update skip locked
         )
         update ingestion_job as job
         set status = 'running',
             stage = case
                 when job.attempt_count = 0 then 'claimed'
                 else 'reclaimed_after_lease_expiry'
             end,
             started_at = coalesce(job.started_at, now()),
             finished_at = null,
             updated_at = now(),
             attempt_count = job.attempt_count + 1,
             worker_id = $1,
             lease_expires_at = $2,
             heartbeat_at = now()
         from candidate
         where job.id = candidate.id
         returning job.id, job.project_id, job.source_id, job.trigger_kind, job.status, job.stage, job.requested_by, job.error_message, job.started_at, job.finished_at, job.created_at, job.updated_at, job.idempotency_key, job.parent_job_id, job.attempt_count, job.worker_id, job.lease_expires_at, job.heartbeat_at, job.payload_json, job.result_json",
    )
    .bind(worker_id)
    .bind(lease_expires_at)
    .fetch_optional(pool)
    .await?;

    if let Some(job) = &claimed {
        record_ingestion_job_attempt_claim(pool, job.id, job.attempt_count, worker_id, &job.stage)
            .await?;
    }

    Ok(claimed)
}

pub async fn mark_ingestion_job_stage(
    pool: &PgPool,
    job_id: Uuid,
    worker_id: &str,
    status: &str,
    stage: &str,
    error_message: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update ingestion_job
         set status = $2,
             stage = $3,
             error_message = $4,
             worker_id = $5,
             heartbeat_at = now(),
             updated_at = now()
         where id = $1",
    )
    .bind(job_id)
    .bind(status)
    .bind(stage)
    .bind(error_message)
    .bind(worker_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn complete_ingestion_job(
    pool: &PgPool,
    job_id: Uuid,
    worker_id: &str,
    result_json: serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update ingestion_job
         set status = 'completed',
             stage = 'completed',
             worker_id = $2,
             error_message = null,
             finished_at = now(),
             heartbeat_at = now(),
             lease_expires_at = null,
             result_json = $3,
             updated_at = now()
         where id = $1",
    )
    .bind(job_id)
    .bind(worker_id)
    .bind(result_json)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn fail_ingestion_job(
    pool: &PgPool,
    job_id: Uuid,
    worker_id: &str,
    error_message: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update ingestion_job
         set status = 'retryable_failed',
             stage = 'failed',
             worker_id = $2,
             error_message = $3,
             finished_at = now(),
             heartbeat_at = now(),
             lease_expires_at = null,
             updated_at = now()
         where id = $1",
    )
    .bind(job_id)
    .bind(worker_id)
    .bind(error_message)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_retrieval_run_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<RetrievalRunRow>, sqlx::Error> {
    sqlx::query_as::<_, RetrievalRunRow>(
        "select id, project_id, query_text, model_profile_id, top_k, response_text, debug_json, created_at
         from retrieval_run where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Lists usage events, optionally filtered by project.
///
/// # Errors
/// Returns any `SQLx` error raised while querying `usage_event` rows.
pub async fn list_usage_events(
    pool: &PgPool,
    project_id: Option<Uuid>,
) -> Result<Vec<UsageEventRow>, sqlx::Error> {
    match project_id {
        Some(project_id) => {
            sqlx::query_as::<_, UsageEventRow>(
                "select id, workspace_id, project_id, provider_account_id, model_profile_id, usage_kind, prompt_tokens, completion_tokens, total_tokens, raw_usage_json, created_at
                 from usage_event where project_id = $1 order by created_at desc",
            )
            .bind(project_id)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, UsageEventRow>(
                "select id, workspace_id, project_id, provider_account_id, model_profile_id, usage_kind, prompt_tokens, completion_tokens, total_tokens, raw_usage_json, created_at
                 from usage_event order by created_at desc",
            )
            .fetch_all(pool)
            .await
        }
    }
}

/// Lists cost ledger rows, optionally filtered by project.
///
/// # Errors
/// Returns any `SQLx` error raised while querying `cost_ledger` rows.
pub async fn list_cost_ledger(
    pool: &PgPool,
    project_id: Option<Uuid>,
) -> Result<Vec<CostLedgerRow>, sqlx::Error> {
    match project_id {
        Some(project_id) => {
            sqlx::query_as::<_, CostLedgerRow>(
                "select id, workspace_id, project_id, usage_event_id, provider_kind, model_name, currency, estimated_cost, pricing_snapshot_json, created_at
                 from cost_ledger where project_id = $1 order by created_at desc",
            )
            .bind(project_id)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, CostLedgerRow>(
                "select id, workspace_id, project_id, usage_event_id, provider_kind, model_name, currency, estimated_cost, pricing_snapshot_json, created_at
                 from cost_ledger order by created_at desc",
            )
            .fetch_all(pool)
            .await
        }
    }
}

/// Loads a usage event by primary key.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the `usage_event` row.
pub async fn get_usage_event_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<UsageEventRow>, sqlx::Error> {
    sqlx::query_as::<_, UsageEventRow>(
        "select id, workspace_id, project_id, provider_account_id, model_profile_id, usage_kind, prompt_tokens, completion_tokens, total_tokens, raw_usage_json, created_at
         from usage_event where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Loads a cost ledger row by primary key.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the `cost_ledger` row.
pub async fn get_cost_ledger_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<CostLedgerRow>, sqlx::Error> {
    sqlx::query_as::<_, CostLedgerRow>(
        "select id, workspace_id, project_id, usage_event_id, provider_kind, model_name, currency, estimated_cost, pricing_snapshot_json, created_at
         from cost_ledger where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct UsageCostTotalsRow {
    pub usage_events: i64,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub estimated_cost: rust_decimal::Decimal,
}

/// Aggregates usage and estimated cost totals, optionally for one project.
///
/// # Errors
/// Returns any `SQLx` error raised while aggregating usage and cost totals.
pub async fn get_usage_cost_totals(
    pool: &PgPool,
    project_id: Option<Uuid>,
) -> Result<UsageCostTotalsRow, sqlx::Error> {
    match project_id {
        Some(project_id) => {
            sqlx::query_as::<_, UsageCostTotalsRow>(
                "select
                    count(distinct ue.id) as usage_events,
                    sum(ue.prompt_tokens)::bigint as prompt_tokens,
                    sum(ue.completion_tokens)::bigint as completion_tokens,
                    sum(ue.total_tokens)::bigint as total_tokens,
                    coalesce(sum(cl.estimated_cost), 0) as estimated_cost
                 from usage_event ue
                 left join cost_ledger cl on cl.usage_event_id = ue.id
                 where ue.project_id = $1",
            )
            .bind(project_id)
            .fetch_one(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, UsageCostTotalsRow>(
                "select
                    count(distinct ue.id) as usage_events,
                    sum(ue.prompt_tokens)::bigint as prompt_tokens,
                    sum(ue.completion_tokens)::bigint as completion_tokens,
                    sum(ue.total_tokens)::bigint as total_tokens,
                    coalesce(sum(cl.estimated_cost), 0) as estimated_cost
                 from usage_event ue
                 left join cost_ledger cl on cl.usage_event_id = ue.id",
            )
            .fetch_one(pool)
            .await
        }
    }
}

/// Aggregates usage and estimated cost totals for one workspace.
///
/// # Errors
/// Returns any `SQLx` error raised while aggregating usage and cost totals.
pub async fn get_workspace_usage_cost_totals(
    pool: &PgPool,
    workspace_id: Uuid,
) -> Result<UsageCostTotalsRow, sqlx::Error> {
    sqlx::query_as::<_, UsageCostTotalsRow>(
        "select
            count(distinct ue.id) as usage_events,
            sum(ue.prompt_tokens)::bigint as prompt_tokens,
            sum(ue.completion_tokens)::bigint as completion_tokens,
            sum(ue.total_tokens)::bigint as total_tokens,
            coalesce(sum(cl.estimated_cost), 0) as estimated_cost
         from usage_event ue
         left join cost_ledger cl on cl.usage_event_id = ue.id
         where ue.workspace_id = $1",
    )
    .bind(workspace_id)
    .fetch_one(pool)
    .await
}
