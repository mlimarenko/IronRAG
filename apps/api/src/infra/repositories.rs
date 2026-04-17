#![allow(
    clippy::doc_markdown,
    clippy::missing_const_for_fn,
    clippy::missing_errors_doc,
    clippy::too_many_arguments,
    clippy::too_many_lines
)]

#[allow(
    clippy::bool_to_int_with_if,
    clippy::missing_errors_doc,
    clippy::option_if_let_else,
    clippy::too_many_arguments,
    clippy::too_many_lines
)]
pub mod ai_repository;
#[allow(clippy::missing_errors_doc)]
pub mod audit_repository;
#[allow(clippy::missing_errors_doc)]
pub mod billing_repository;
#[allow(clippy::missing_errors_doc)]
pub mod catalog_repository;
#[allow(clippy::missing_errors_doc)]
pub mod content_repository;
mod document_runtime_repository;
#[allow(clippy::missing_errors_doc)]
pub mod extract_repository;
#[allow(clippy::missing_errors_doc)]
pub mod iam_repository;
#[allow(clippy::missing_errors_doc, clippy::too_many_lines)]
pub mod ingest_repository;
#[allow(clippy::missing_errors_doc)]
pub mod ops_repository;
#[allow(clippy::missing_errors_doc)]
pub mod query_ir_cache_repository;
#[allow(clippy::missing_errors_doc)]
pub mod query_repository;
mod runtime_graph_repository;
mod runtime_graph_summary_repository;
pub mod runtime_provider_repository;
pub mod runtime_repository;
#[allow(clippy::missing_errors_doc)]
pub mod runtime_vector_repository;

pub use self::catalog_repository::{
    get_library_source_truth_version, touch_library_source_truth_version,
};
pub use document_runtime_repository::*;
pub use runtime_graph_repository::*;
pub use runtime_graph_summary_repository::*;
pub use runtime_provider_repository::*;
pub use runtime_vector_repository::*;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestionExecutionPayload {
    #[serde(alias = "project_id")]
    pub library_id: Uuid,
    #[serde(default)]
    pub upload_batch_id: Option<Uuid>,
    #[serde(default)]
    pub logical_document_id: Option<Uuid>,
    #[serde(default)]
    pub target_revision_id: Option<Uuid>,
    #[serde(default)]
    pub content_mutation_id: Option<Uuid>,
    #[serde(default)]
    pub stale_guard_revision_no: Option<i32>,
    #[serde(default)]
    pub attempt_kind: Option<String>,
    #[serde(default)]
    pub mutation_kind: Option<String>,
    pub source_id: Option<Uuid>,
    pub external_key: String,
    pub title: Option<String>,
    pub mime_type: Option<String>,
    pub text: Option<String>,
    pub file_kind: Option<String>,
    pub file_size_bytes: Option<u64>,
    pub adapter_status: Option<String>,
    pub extraction_error: Option<String>,
    #[serde(default)]
    pub extraction_kind: Option<String>,
    #[serde(default)]
    pub page_count: Option<u32>,
    #[serde(default)]
    pub extraction_warnings: Vec<String>,
    #[serde(default = "default_json_object")]
    pub source_map: serde_json::Value,
    #[serde(default)]
    pub extraction_provider_kind: Option<String>,
    #[serde(default)]
    pub extraction_model_name: Option<String>,
    #[serde(default)]
    pub extraction_version: Option<String>,
    pub ingest_mode: String,
    pub extra_metadata: serde_json::Value,
}

fn default_json_object() -> serde_json::Value {
    serde_json::json!({})
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphProgressCheckpointRow {
    pub ingestion_run_id: Uuid,
    pub attempt_no: i32,
    pub processed_chunks: i64,
    pub total_chunks: i64,
    pub progress_percent: Option<i32>,
    pub provider_call_count: i64,
    pub avg_call_elapsed_ms: Option<i64>,
    pub avg_chunk_elapsed_ms: Option<i64>,
    pub avg_chars_per_second: Option<f64>,
    pub avg_tokens_per_second: Option<f64>,
    pub last_provider_call_at: Option<DateTime<Utc>>,
    pub next_checkpoint_eta_ms: Option<i64>,
    pub pressure_kind: Option<String>,
    pub provider_failure_class: Option<String>,
    pub request_shape_key: Option<String>,
    pub request_size_bytes: Option<i64>,
    pub upstream_status: Option<String>,
    pub retry_outcome: Option<String>,
    pub computed_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct RuntimeGraphProgressCheckpointInput {
    pub ingestion_run_id: Uuid,
    pub attempt_no: i32,
    pub processed_chunks: i64,
    pub total_chunks: i64,
    pub progress_percent: Option<i32>,
    pub provider_call_count: i64,
    pub avg_call_elapsed_ms: Option<i64>,
    pub avg_chunk_elapsed_ms: Option<i64>,
    pub avg_chars_per_second: Option<f64>,
    pub avg_tokens_per_second: Option<f64>,
    pub last_provider_call_at: Option<DateTime<Utc>>,
    pub next_checkpoint_eta_ms: Option<i64>,
    pub pressure_kind: Option<String>,
    pub computed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphExtractionRecordRow {
    pub id: Uuid,
    pub runtime_execution_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub chunk_id: Uuid,
    pub provider_kind: String,
    pub model_name: String,
    pub extraction_version: String,
    pub prompt_hash: String,
    pub status: String,
    pub raw_output_json: serde_json::Value,
    pub normalized_output_json: serde_json::Value,
    pub glean_pass_count: i32,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphExtractionResumeStateRow {
    pub ingestion_run_id: Uuid,
    pub chunk_ordinal: i32,
    pub chunk_content_hash: String,
    pub status: String,
    pub last_attempt_no: i32,
    pub replay_count: i32,
    pub resume_hit_count: i32,
    pub downgrade_level: i32,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub prompt_hash: Option<String>,
    pub request_shape_key: Option<String>,
    pub request_size_bytes: Option<i64>,
    pub provider_failure_class: Option<String>,
    pub provider_failure_json: Option<serde_json::Value>,
    pub recovery_summary_json: serde_json::Value,
    pub raw_output_json: serde_json::Value,
    pub normalized_output_json: serde_json::Value,
    pub last_successful_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphExtractionResumeRollupRow {
    pub ingestion_run_id: Uuid,
    pub chunk_count: i64,
    pub ready_chunk_count: i64,
    pub failed_chunk_count: i64,
    pub replayed_chunk_count: i64,
    pub resume_hit_count: i64,
    pub resumed_chunk_count: i64,
    pub max_downgrade_level: i32,
}

#[derive(Debug, Clone)]
pub struct UpsertRuntimeGraphExtractionResumeStateInput {
    pub ingestion_run_id: Uuid,
    pub chunk_ordinal: i32,
    pub chunk_content_hash: String,
    pub status: String,
    pub last_attempt_no: i32,
    pub replay_count: i32,
    pub resume_hit_count: i32,
    pub downgrade_level: i32,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub prompt_hash: Option<String>,
    pub request_shape_key: Option<String>,
    pub request_size_bytes: Option<i64>,
    pub provider_failure_class: Option<String>,
    pub provider_failure_json: Option<serde_json::Value>,
    pub recovery_summary_json: serde_json::Value,
    pub raw_output_json: serde_json::Value,
    pub normalized_output_json: serde_json::Value,
    pub last_successful_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphExtractionRecoveryAttemptRow {
    pub id: Uuid,
    pub runtime_execution_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub revision_id: Option<Uuid>,
    pub ingestion_run_id: Option<Uuid>,
    pub attempt_no: i32,
    pub chunk_id: Option<Uuid>,
    pub recovery_kind: String,
    pub trigger_reason: String,
    pub status: String,
    pub raw_issue_summary: Option<String>,
    pub recovered_summary: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct CreateRuntimeGraphExtractionRecoveryAttemptInput {
    pub runtime_execution_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub revision_id: Option<Uuid>,
    pub ingestion_run_id: Option<Uuid>,
    pub attempt_no: i32,
    pub chunk_id: Option<Uuid>,
    pub recovery_kind: String,
    pub trigger_reason: String,
    pub status: String,
    pub raw_issue_summary: Option<String>,
    pub recovered_summary: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CreateRuntimeGraphExtractionRecordInput {
    pub id: Uuid,
    pub runtime_execution_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub chunk_id: Uuid,
    pub provider_kind: String,
    pub model_name: String,
    pub extraction_version: String,
    pub prompt_hash: String,
    pub status: String,
    pub raw_output_json: serde_json::Value,
    pub normalized_output_json: serde_json::Value,
    pub glean_pass_count: i32,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UpdateRuntimeGraphExtractionRecordInput {
    pub provider_kind: String,
    pub model_name: String,
    pub prompt_hash: String,
    pub status: String,
    pub raw_output_json: serde_json::Value,
    pub normalized_output_json: serde_json::Value,
    pub glean_pass_count: i32,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ContentMutationImpactScopeRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub mutation_id: Uuid,
    pub mutation_kind: String,
    pub source_revision_id: Option<Uuid>,
    pub target_revision_id: Option<Uuid>,
    pub scope_status: String,
    pub confidence_status: String,
    pub affected_node_ids_json: serde_json::Value,
    pub affected_relationship_ids_json: serde_json::Value,
    pub fallback_reason: Option<String>,
    pub detected_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct CreateContentMutationImpactScopeInput {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub mutation_id: Uuid,
    pub mutation_kind: String,
    pub source_revision_id: Option<Uuid>,
    pub target_revision_id: Option<Uuid>,
    pub scope_status: String,
    pub confidence_status: String,
    pub affected_node_ids_json: serde_json::Value,
    pub affected_relationship_ids_json: serde_json::Value,
    pub fallback_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphFilteredArtifactRow {
    pub id: Uuid,
    pub library_id: Uuid,
    pub ingestion_run_id: Option<Uuid>,
    pub revision_id: Option<Uuid>,
    pub target_kind: String,
    pub candidate_key: String,
    pub source_node_key: Option<String>,
    pub target_node_key: Option<String>,
    pub relation_type: Option<String>,
    pub filter_reason: String,
    pub summary: Option<String>,
    pub metadata_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphConvergenceCountersRow {
    pub queued_document_count: i64,
    pub processing_document_count: i64,
    pub ready_no_graph_count: i64,
    pub pending_update_count: i64,
    pub pending_delete_count: i64,
    pub filtered_artifact_count: i64,
    pub filtered_empty_relation_count: i64,
    pub filtered_degenerate_loop_count: i64,
    pub latest_failed_mutation_kind: Option<String>,
}

/// Deletes persisted query references that point at knowledge contributed by one document.
///
/// # Errors
/// Returns any `SQLx` error raised while deleting the persisted query references.
pub async fn delete_query_execution_references_by_document(
    pool: &PgPool,
    library_id: Uuid,
    document_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let chunk_result = sqlx::query(
        "delete from query_chunk_reference as reference
         using query_execution as execution
         where reference.execution_id = execution.id
           and execution.library_id = $1
           and exists (
               select 1
               from content_chunk
               join content_revision
                 on content_revision.id = content_chunk.revision_id
               where content_chunk.id = reference.chunk_id
                 and content_revision.document_id = $2
           )",
    )
    .bind(library_id)
    .bind(document_id)
    .execute(pool)
    .await?;
    Ok(chunk_result.rows_affected())
}

/// Deletes persisted query references that point at knowledge contributed by one content revision.
///
/// # Errors
/// Returns any `SQLx` error raised while deleting revision-scoped query references.
pub async fn delete_query_execution_references_by_content_revision(
    pool: &PgPool,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let chunk_result = sqlx::query(
        "delete from query_chunk_reference as reference
         using query_execution as execution
         where reference.execution_id = execution.id
           and execution.library_id = $1
           and exists (
               select 1
               from content_chunk
               join content_revision
                 on content_revision.id = content_chunk.revision_id
               where content_chunk.id = reference.chunk_id
                 and content_revision.document_id = $2
                 and content_chunk.revision_id = $3
           )",
    )
    .bind(library_id)
    .bind(document_id)
    .bind(revision_id)
    .execute(pool)
    .await?;
    Ok(chunk_result.rows_affected())
}

/// Persists one chunk-level graph extraction record.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the graph extraction record.
pub async fn create_runtime_graph_extraction_record(
    pool: &PgPool,
    input: &CreateRuntimeGraphExtractionRecordInput,
) -> Result<RuntimeGraphExtractionRecordRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphExtractionRecordRow>(
        "insert into runtime_graph_extraction (
            id, runtime_execution_id, library_id, document_id, chunk_id, provider_kind, model_name,
            extraction_version, prompt_hash, status, raw_output_json, normalized_output_json,
            glean_pass_count, error_message
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
         returning id, runtime_execution_id, library_id, document_id, chunk_id, provider_kind,
            model_name, extraction_version, prompt_hash, status, raw_output_json,
            normalized_output_json, glean_pass_count, error_message, created_at",
    )
    .bind(input.id)
    .bind(input.runtime_execution_id)
    .bind(input.library_id)
    .bind(input.document_id)
    .bind(input.chunk_id)
    .bind(&input.provider_kind)
    .bind(&input.model_name)
    .bind(&input.extraction_version)
    .bind(&input.prompt_hash)
    .bind(&input.status)
    .bind(input.raw_output_json.clone())
    .bind(input.normalized_output_json.clone())
    .bind(input.glean_pass_count)
    .bind(input.error_message.as_deref())
    .fetch_one(pool)
    .await
}

/// Updates one chunk-level graph extraction record.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the graph extraction record.
pub async fn update_runtime_graph_extraction_record(
    pool: &PgPool,
    id: Uuid,
    input: &UpdateRuntimeGraphExtractionRecordInput,
) -> Result<Option<RuntimeGraphExtractionRecordRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphExtractionRecordRow>(
        "update runtime_graph_extraction
         set provider_kind = $2,
             model_name = $3,
             prompt_hash = $4,
             status = $5,
             raw_output_json = $6,
             normalized_output_json = $7,
             glean_pass_count = $8,
             error_message = $9
         where id = $1
         returning id, runtime_execution_id, library_id, document_id, chunk_id, provider_kind,
            model_name, extraction_version, prompt_hash, status, raw_output_json,
            normalized_output_json, glean_pass_count, error_message, created_at",
    )
    .bind(id)
    .bind(&input.provider_kind)
    .bind(&input.model_name)
    .bind(&input.prompt_hash)
    .bind(&input.status)
    .bind(input.raw_output_json.clone())
    .bind(input.normalized_output_json.clone())
    .bind(input.glean_pass_count)
    .bind(input.error_message.as_deref())
    .fetch_optional(pool)
    .await
}

/// Upserts one bounded graph-progress checkpoint for the active extraction attempt.
///
/// # Errors
/// Returns any `SQLx` error raised while persisting the checkpoint row.
pub async fn upsert_runtime_graph_progress_checkpoint(
    pool: &PgPool,
    row: &RuntimeGraphProgressCheckpointInput,
) -> Result<RuntimeGraphProgressCheckpointRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphProgressCheckpointRow>(
        "insert into runtime_graph_progress_checkpoint (
            ingestion_run_id, attempt_no, processed_chunks, total_chunks, progress_percent,
            provider_call_count, avg_call_elapsed_ms, avg_chunk_elapsed_ms,
            avg_chars_per_second, avg_tokens_per_second, last_provider_call_at,
            next_checkpoint_eta_ms, pressure_kind, provider_failure_class,
            request_shape_key, request_size_bytes, upstream_status, retry_outcome, computed_at
         ) values (
            $1, $2, $3, $4, $5,
            $6, $7, $8,
            $9, $10, $11,
            $12, $13, $14,
            $15, $16, $17, $18, $19
         )
         on conflict (ingestion_run_id, attempt_no) do update
         set processed_chunks = excluded.processed_chunks,
             total_chunks = excluded.total_chunks,
             progress_percent = excluded.progress_percent,
             provider_call_count = excluded.provider_call_count,
             avg_call_elapsed_ms = excluded.avg_call_elapsed_ms,
             avg_chunk_elapsed_ms = excluded.avg_chunk_elapsed_ms,
             avg_chars_per_second = excluded.avg_chars_per_second,
             avg_tokens_per_second = excluded.avg_tokens_per_second,
             last_provider_call_at = excluded.last_provider_call_at,
             next_checkpoint_eta_ms = excluded.next_checkpoint_eta_ms,
             pressure_kind = excluded.pressure_kind,
             provider_failure_class = coalesce(
                 runtime_graph_progress_checkpoint.provider_failure_class,
                 excluded.provider_failure_class
             ),
             request_shape_key = coalesce(
                 runtime_graph_progress_checkpoint.request_shape_key,
                 excluded.request_shape_key
             ),
             request_size_bytes = coalesce(
                 runtime_graph_progress_checkpoint.request_size_bytes,
                 excluded.request_size_bytes
             ),
             upstream_status = coalesce(
                 runtime_graph_progress_checkpoint.upstream_status,
                 excluded.upstream_status
             ),
             retry_outcome = coalesce(
                 runtime_graph_progress_checkpoint.retry_outcome,
                 excluded.retry_outcome
             ),
             computed_at = excluded.computed_at
         returning ingestion_run_id, attempt_no, processed_chunks, total_chunks, progress_percent,
            provider_call_count, avg_call_elapsed_ms, avg_chunk_elapsed_ms,
            avg_chars_per_second, avg_tokens_per_second, last_provider_call_at,
            next_checkpoint_eta_ms, pressure_kind, provider_failure_class,
            request_shape_key, request_size_bytes, upstream_status, retry_outcome, computed_at",
    )
    .bind(row.ingestion_run_id)
    .bind(row.attempt_no)
    .bind(row.processed_chunks)
    .bind(row.total_chunks)
    .bind(row.progress_percent)
    .bind(row.provider_call_count)
    .bind(row.avg_call_elapsed_ms)
    .bind(row.avg_chunk_elapsed_ms)
    .bind(row.avg_chars_per_second)
    .bind(row.avg_tokens_per_second)
    .bind(row.last_provider_call_at)
    .bind(row.next_checkpoint_eta_ms)
    .bind(row.pressure_kind.as_deref())
    .bind(Option::<&str>::None)
    .bind(Option::<&str>::None)
    .bind(Option::<i64>::None)
    .bind(Option::<&str>::None)
    .bind(Option::<&str>::None)
    .bind(row.computed_at)
    .fetch_one(pool)
    .await
}

/// Loads the most recent graph-progress checkpoint for one runtime attempt.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the checkpoint row.
pub async fn get_runtime_graph_progress_checkpoint(
    pool: &PgPool,
    ingestion_run_id: Uuid,
    attempt_no: i32,
) -> Result<Option<RuntimeGraphProgressCheckpointRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphProgressCheckpointRow>(
        "select ingestion_run_id, attempt_no, processed_chunks, total_chunks, progress_percent,
            provider_call_count, avg_call_elapsed_ms, avg_chunk_elapsed_ms,
            avg_chars_per_second, avg_tokens_per_second, last_provider_call_at,
            next_checkpoint_eta_ms, pressure_kind, provider_failure_class,
            request_shape_key, request_size_bytes, upstream_status, retry_outcome, computed_at
         from runtime_graph_progress_checkpoint
         where ingestion_run_id = $1 and attempt_no = $2",
    )
    .bind(ingestion_run_id)
    .bind(attempt_no)
    .fetch_optional(pool)
    .await
}

/// Lists active graph-progress checkpoints for the current attempts in one library.
///
/// # Errors
/// Returns any `SQLx` error raised while querying checkpoint rows.
pub async fn list_active_runtime_graph_progress_checkpoints_by_library(
    pool: &PgPool,
    library_id: Uuid,
) -> Result<Vec<RuntimeGraphProgressCheckpointRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphProgressCheckpointRow>(
        "select checkpoint.ingestion_run_id, checkpoint.attempt_no, checkpoint.processed_chunks,
            checkpoint.total_chunks, checkpoint.progress_percent, checkpoint.provider_call_count,
            checkpoint.avg_call_elapsed_ms, checkpoint.avg_chunk_elapsed_ms,
            checkpoint.avg_chars_per_second, checkpoint.avg_tokens_per_second,
            checkpoint.last_provider_call_at, checkpoint.next_checkpoint_eta_ms,
            checkpoint.pressure_kind, checkpoint.provider_failure_class,
            checkpoint.request_shape_key, checkpoint.request_size_bytes,
            checkpoint.upstream_status, checkpoint.retry_outcome, checkpoint.computed_at
         from runtime_graph_progress_checkpoint as checkpoint
         join runtime_ingestion_run as run
           on run.id = checkpoint.ingestion_run_id
          and run.current_attempt_no = checkpoint.attempt_no
         where run.library_id = $1
           and run.status = 'processing'
           and run.current_stage = 'extracting_graph'
         order by checkpoint.avg_chunk_elapsed_ms desc nulls last,
            checkpoint.computed_at desc",
    )
    .bind(library_id)
    .fetch_all(pool)
    .await
}

/// Lists graph extraction records for one document.
///
/// # Errors
/// Returns any `SQLx` error raised while querying graph extraction records.
pub async fn list_runtime_graph_extraction_records_by_document(
    pool: &PgPool,
    document_id: Uuid,
) -> Result<Vec<RuntimeGraphExtractionRecordRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphExtractionRecordRow>(
        "select id, runtime_execution_id, library_id, document_id, chunk_id, provider_kind,
            model_name, extraction_version, prompt_hash, status, raw_output_json,
            normalized_output_json, glean_pass_count, error_message, created_at
         from runtime_graph_extraction
         where document_id = $1
         order by created_at asc, id asc",
    )
    .bind(document_id)
    .fetch_all(pool)
    .await
}

/// Loads one graph extraction record by id.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph extraction record.
pub async fn get_runtime_graph_extraction_record_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<RuntimeGraphExtractionRecordRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphExtractionRecordRow>(
        "select id, runtime_execution_id, library_id, document_id, chunk_id, provider_kind,
            model_name, extraction_version, prompt_hash, status, raw_output_json,
            normalized_output_json, glean_pass_count, error_message, created_at
         from runtime_graph_extraction
         where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Lists graph extraction records for one library.
///
/// # Errors
/// Returns any `SQLx` error raised while querying graph extraction records.
pub async fn list_runtime_graph_extraction_records_by_library(
    pool: &PgPool,
    library_id: Uuid,
) -> Result<Vec<RuntimeGraphExtractionRecordRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphExtractionRecordRow>(
        "select id, runtime_execution_id, library_id, document_id, chunk_id, provider_kind,
            model_name, extraction_version, prompt_hash, status, raw_output_json,
            normalized_output_json, glean_pass_count, error_message, created_at
         from runtime_graph_extraction
         where library_id = $1
         order by created_at asc, id asc",
    )
    .bind(library_id)
    .fetch_all(pool)
    .await
}

/// Lists graph-extraction resume-state rows for one runtime ingestion run.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the resume-state rows.
pub async fn list_runtime_graph_extraction_resume_states_by_run(
    pool: &PgPool,
    ingestion_run_id: Uuid,
) -> Result<Vec<RuntimeGraphExtractionResumeStateRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphExtractionResumeStateRow>(
        "select ingestion_run_id, chunk_ordinal, chunk_content_hash, status, last_attempt_no,
            replay_count, resume_hit_count, downgrade_level, provider_kind, model_name,
            prompt_hash, request_shape_key, request_size_bytes, provider_failure_class,
            provider_failure_json, recovery_summary_json, raw_output_json, normalized_output_json,
            last_successful_at, created_at, updated_at
         from runtime_graph_extraction_resume_state
         where ingestion_run_id = $1
         order by chunk_ordinal asc",
    )
    .bind(ingestion_run_id)
    .fetch_all(pool)
    .await
}

/// Upserts one graph-extraction resume-state row keyed by `(ingestion_run_id, chunk_ordinal)`.
///
/// # Errors
/// Returns any `SQLx` error raised while persisting the row.
pub async fn upsert_runtime_graph_extraction_resume_state(
    pool: &PgPool,
    input: &UpsertRuntimeGraphExtractionResumeStateInput,
) -> Result<RuntimeGraphExtractionResumeStateRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphExtractionResumeStateRow>(
        "insert into runtime_graph_extraction_resume_state (
            ingestion_run_id, chunk_ordinal, chunk_content_hash, status, last_attempt_no,
            replay_count, resume_hit_count, downgrade_level, provider_kind, model_name,
            prompt_hash, request_shape_key, request_size_bytes, provider_failure_class,
            provider_failure_json, recovery_summary_json, raw_output_json, normalized_output_json,
            last_successful_at
         ) values (
            $1, $2, $3, $4, $5,
            $6, $7, $8, $9, $10,
            $11, $12, $13, $14,
            $15, $16, $17, $18,
            $19
         )
         on conflict (ingestion_run_id, chunk_ordinal) do update
         set chunk_content_hash = excluded.chunk_content_hash,
             status = excluded.status,
             last_attempt_no = excluded.last_attempt_no,
             replay_count = excluded.replay_count,
             resume_hit_count = excluded.resume_hit_count,
             downgrade_level = excluded.downgrade_level,
             provider_kind = excluded.provider_kind,
             model_name = excluded.model_name,
             prompt_hash = excluded.prompt_hash,
             request_shape_key = excluded.request_shape_key,
             request_size_bytes = excluded.request_size_bytes,
             provider_failure_class = excluded.provider_failure_class,
             provider_failure_json = excluded.provider_failure_json,
             recovery_summary_json = excluded.recovery_summary_json,
             raw_output_json = excluded.raw_output_json,
             normalized_output_json = excluded.normalized_output_json,
             last_successful_at = excluded.last_successful_at,
             updated_at = now()
         returning ingestion_run_id, chunk_ordinal, chunk_content_hash, status, last_attempt_no,
            replay_count, resume_hit_count, downgrade_level, provider_kind, model_name,
            prompt_hash, request_shape_key, request_size_bytes, provider_failure_class,
            provider_failure_json, recovery_summary_json, raw_output_json, normalized_output_json,
            last_successful_at, created_at, updated_at",
    )
    .bind(input.ingestion_run_id)
    .bind(input.chunk_ordinal)
    .bind(&input.chunk_content_hash)
    .bind(&input.status)
    .bind(input.last_attempt_no)
    .bind(input.replay_count)
    .bind(input.resume_hit_count)
    .bind(input.downgrade_level)
    .bind(input.provider_kind.as_deref())
    .bind(input.model_name.as_deref())
    .bind(input.prompt_hash.as_deref())
    .bind(input.request_shape_key.as_deref())
    .bind(input.request_size_bytes)
    .bind(input.provider_failure_class.as_deref())
    .bind(input.provider_failure_json.clone())
    .bind(input.recovery_summary_json.clone())
    .bind(input.raw_output_json.clone())
    .bind(input.normalized_output_json.clone())
    .bind(input.last_successful_at)
    .fetch_one(pool)
    .await
}

/// Increments the resume-hit counter for one persisted graph-extraction resume row.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the row.
pub async fn increment_runtime_graph_extraction_resume_hit(
    pool: &PgPool,
    ingestion_run_id: Uuid,
    chunk_ordinal: i32,
) -> Result<RuntimeGraphExtractionResumeStateRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphExtractionResumeStateRow>(
        "update runtime_graph_extraction_resume_state
         set resume_hit_count = resume_hit_count + 1,
             updated_at = now()
         where ingestion_run_id = $1
           and chunk_ordinal = $2
         returning ingestion_run_id, chunk_ordinal, chunk_content_hash, status, last_attempt_no,
            replay_count, resume_hit_count, downgrade_level, provider_kind, model_name,
            prompt_hash, request_shape_key, request_size_bytes, provider_failure_class,
            provider_failure_json, recovery_summary_json, raw_output_json, normalized_output_json,
            last_successful_at, created_at, updated_at",
    )
    .bind(ingestion_run_id)
    .bind(chunk_ordinal)
    .fetch_one(pool)
    .await
}

/// Loads one aggregated graph-extraction resume rollup for a single ingestion run.
///
/// # Errors
/// Returns any `SQLx` error raised while loading the aggregated resume rollup.
pub async fn load_runtime_graph_extraction_resume_rollup_by_run(
    pool: &PgPool,
    ingestion_run_id: Uuid,
) -> Result<Option<RuntimeGraphExtractionResumeRollupRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphExtractionResumeRollupRow>(
        "select ingestion_run_id,
            count(*)::bigint as chunk_count,
            count(*) filter (where status = 'ready')::bigint as ready_chunk_count,
            count(*) filter (where status = 'failed')::bigint as failed_chunk_count,
            coalesce(sum(greatest(replay_count, 0)), 0)::bigint as replayed_chunk_count,
            coalesce(sum(greatest(resume_hit_count, 0)), 0)::bigint as resume_hit_count,
            count(*) filter (where resume_hit_count > 0)::bigint as resumed_chunk_count,
            coalesce(max(greatest(downgrade_level, 0)), 0)::int as max_downgrade_level
         from runtime_graph_extraction_resume_state
         where ingestion_run_id = $1
         group by ingestion_run_id",
    )
    .bind(ingestion_run_id)
    .fetch_optional(pool)
    .await
}

/// Lists aggregated graph-extraction resume rollups for active runs in one library.
///
/// # Errors
/// Returns any `SQLx` error raised while loading the aggregated resume rollups.
pub async fn list_active_runtime_graph_extraction_resume_rollups_by_library(
    pool: &PgPool,
    library_id: Uuid,
) -> Result<Vec<RuntimeGraphExtractionResumeRollupRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphExtractionResumeRollupRow>(
        "select resume.ingestion_run_id,
            count(*)::bigint as chunk_count,
            count(*) filter (where resume.status = 'ready')::bigint as ready_chunk_count,
            count(*) filter (where resume.status = 'failed')::bigint as failed_chunk_count,
            coalesce(sum(greatest(resume.replay_count, 0)), 0)::bigint as replayed_chunk_count,
            coalesce(sum(greatest(resume.resume_hit_count, 0)), 0)::bigint as resume_hit_count,
            count(*) filter (where resume.resume_hit_count > 0)::bigint as resumed_chunk_count,
            coalesce(max(greatest(resume.downgrade_level, 0)), 0)::int as max_downgrade_level
         from runtime_graph_extraction_resume_state resume
         join runtime_ingestion_run run
           on run.id = resume.ingestion_run_id
         where run.library_id = $1
           and run.status = 'processing'
           and run.current_stage = 'extracting_graph'
         group by resume.ingestion_run_id",
    )
    .bind(library_id)
    .fetch_all(pool)
    .await
}

/// Creates one extraction-recovery attempt row.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the recovery attempt row.
pub async fn create_runtime_graph_extraction_recovery_attempt(
    pool: &PgPool,
    input: &CreateRuntimeGraphExtractionRecoveryAttemptInput,
) -> Result<RuntimeGraphExtractionRecoveryAttemptRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphExtractionRecoveryAttemptRow>(
        "insert into runtime_graph_extraction_recovery_attempt (
            id, runtime_execution_id, workspace_id, library_id, document_id, revision_id,
            ingestion_run_id, attempt_no, chunk_id, recovery_kind, trigger_reason, status,
            raw_issue_summary, recovered_summary
         ) values (
            $1, $2, $3, $4, $5, $6, $7,
            $8, $9, $10, $11, $12, $13,
            $14
         )
         returning id, runtime_execution_id, workspace_id, library_id, document_id, revision_id,
            ingestion_run_id, attempt_no, chunk_id, recovery_kind, trigger_reason, status,
            raw_issue_summary, recovered_summary, started_at, finished_at, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(input.runtime_execution_id)
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.document_id)
    .bind(input.revision_id)
    .bind(input.ingestion_run_id)
    .bind(input.attempt_no)
    .bind(input.chunk_id)
    .bind(&input.recovery_kind)
    .bind(&input.trigger_reason)
    .bind(&input.status)
    .bind(input.raw_issue_summary.as_deref())
    .bind(input.recovered_summary.as_deref())
    .fetch_one(pool)
    .await
}

/// Updates the terminal status of one extraction-recovery attempt row.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the recovery attempt row.
pub async fn update_runtime_graph_extraction_recovery_attempt_status(
    pool: &PgPool,
    id: Uuid,
    status: &str,
    recovered_summary: Option<&str>,
) -> Result<Option<RuntimeGraphExtractionRecoveryAttemptRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphExtractionRecoveryAttemptRow>(
        "update runtime_graph_extraction_recovery_attempt
         set status = $2,
             recovered_summary = coalesce($3, recovered_summary),
             finished_at = case
                when $2 in ('recovered', 'partial', 'failed', 'skipped') then now()
                else finished_at
             end,
             updated_at = now()
         where id = $1
         returning id, runtime_execution_id, workspace_id, library_id, document_id, revision_id,
            ingestion_run_id, attempt_no, chunk_id, recovery_kind, trigger_reason, status,
            raw_issue_summary, recovered_summary, started_at, finished_at, created_at, updated_at",
    )
    .bind(id)
    .bind(status)
    .bind(recovered_summary)
    .fetch_optional(pool)
    .await
}

/// Lists extraction-recovery attempts for one runtime ingestion run and attempt number.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the recovery attempt rows.
pub async fn list_runtime_graph_extraction_recovery_attempts_by_run(
    pool: &PgPool,
    ingestion_run_id: Uuid,
    attempt_no: i32,
) -> Result<Vec<RuntimeGraphExtractionRecoveryAttemptRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphExtractionRecoveryAttemptRow>(
        "select id, runtime_execution_id, workspace_id, library_id, document_id, revision_id,
            ingestion_run_id, attempt_no, chunk_id, recovery_kind, trigger_reason, status,
            raw_issue_summary, recovered_summary, started_at, finished_at, created_at, updated_at
         from runtime_graph_extraction_recovery_attempt
         where ingestion_run_id = $1
           and attempt_no = $2
         order by started_at asc, created_at asc",
    )
    .bind(ingestion_run_id)
    .bind(attempt_no)
    .fetch_all(pool)
    .await
}

/// Lists extraction-recovery attempts for one document.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the recovery attempt rows.
pub async fn list_runtime_graph_extraction_recovery_attempts_by_document(
    pool: &PgPool,
    document_id: Uuid,
) -> Result<Vec<RuntimeGraphExtractionRecoveryAttemptRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphExtractionRecoveryAttemptRow>(
        "select id, runtime_execution_id, workspace_id, library_id, document_id, revision_id,
            ingestion_run_id, attempt_no, chunk_id, recovery_kind, trigger_reason, status,
            raw_issue_summary, recovered_summary, started_at, finished_at, created_at, updated_at
         from runtime_graph_extraction_recovery_attempt
         where document_id = $1
         order by started_at desc, created_at desc",
    )
    .bind(document_id)
    .fetch_all(pool)
    .await
}

/// Creates one mutation impact-scope row for a document mutation workflow.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the impact-scope row.
pub async fn create_content_mutation_impact_scope(
    pool: &PgPool,
    input: &CreateContentMutationImpactScopeInput,
) -> Result<ContentMutationImpactScopeRow, sqlx::Error> {
    sqlx::query_as::<_, ContentMutationImpactScopeRow>(
        "insert into content_mutation_impact_scope (
            id, workspace_id, library_id, document_id, mutation_id, mutation_kind,
            source_revision_id, target_revision_id, scope_status, confidence_status,
            affected_node_ids_json, affected_relationship_ids_json, fallback_reason
         ) values (
            $1, $2, $3, $4, $5, $6,
            $7, $8, $9, $10,
            $11, $12, $13
         )
         returning id, workspace_id, library_id, document_id, mutation_id, mutation_kind,
            source_revision_id, target_revision_id, scope_status, confidence_status,
            affected_node_ids_json, affected_relationship_ids_json, fallback_reason, detected_at,
            completed_at, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(input.workspace_id)
    .bind(input.library_id)
    .bind(input.document_id)
    .bind(input.mutation_id)
    .bind(&input.mutation_kind)
    .bind(input.source_revision_id)
    .bind(input.target_revision_id)
    .bind(&input.scope_status)
    .bind(&input.confidence_status)
    .bind(input.affected_node_ids_json.clone())
    .bind(input.affected_relationship_ids_json.clone())
    .bind(input.fallback_reason.as_deref())
    .fetch_one(pool)
    .await
}

/// Updates an existing mutation impact-scope row while the workflow is still active.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the impact-scope row.
pub async fn update_content_mutation_impact_scope(
    pool: &PgPool,
    mutation_id: Uuid,
    scope_status: &str,
    confidence_status: &str,
    affected_node_ids_json: serde_json::Value,
    affected_relationship_ids_json: serde_json::Value,
    fallback_reason: Option<&str>,
) -> Result<Option<ContentMutationImpactScopeRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentMutationImpactScopeRow>(
        "update content_mutation_impact_scope
         set scope_status = $2,
             confidence_status = $3,
             affected_node_ids_json = $4,
             affected_relationship_ids_json = $5,
             fallback_reason = $6,
             updated_at = now()
         where mutation_id = $1
         returning id, workspace_id, library_id, document_id, mutation_id, mutation_kind,
            source_revision_id, target_revision_id, scope_status, confidence_status,
            affected_node_ids_json, affected_relationship_ids_json, fallback_reason, detected_at,
            completed_at, created_at, updated_at",
    )
    .bind(mutation_id)
    .bind(scope_status)
    .bind(confidence_status)
    .bind(affected_node_ids_json)
    .bind(affected_relationship_ids_json)
    .bind(fallback_reason)
    .fetch_optional(pool)
    .await
}

/// Completes one mutation impact-scope row for a workflow.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the impact-scope row.
pub async fn complete_content_mutation_impact_scope(
    pool: &PgPool,
    mutation_id: Uuid,
    scope_status: &str,
    fallback_reason: Option<&str>,
) -> Result<Option<ContentMutationImpactScopeRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentMutationImpactScopeRow>(
        "update content_mutation_impact_scope
         set scope_status = $2,
             fallback_reason = coalesce($3, fallback_reason),
             completed_at = now(),
             updated_at = now()
         where mutation_id = $1
         returning id, workspace_id, library_id, document_id, mutation_id, mutation_kind,
            source_revision_id, target_revision_id, scope_status, confidence_status,
            affected_node_ids_json, affected_relationship_ids_json, fallback_reason, detected_at,
            completed_at, created_at, updated_at",
    )
    .bind(mutation_id)
    .bind(scope_status)
    .bind(fallback_reason)
    .fetch_optional(pool)
    .await
}

/// Loads the impact-scope row for one mutation workflow.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the impact-scope row.
pub async fn get_content_mutation_impact_scope_by_mutation_id(
    pool: &PgPool,
    mutation_id: Uuid,
) -> Result<Option<ContentMutationImpactScopeRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentMutationImpactScopeRow>(
        "select id, workspace_id, library_id, document_id, mutation_id, mutation_kind,
            source_revision_id, target_revision_id, scope_status, confidence_status,
            affected_node_ids_json, affected_relationship_ids_json, fallback_reason, detected_at,
            completed_at, created_at, updated_at
         from content_mutation_impact_scope
         where mutation_id = $1",
    )
    .bind(mutation_id)
    .fetch_optional(pool)
    .await
}

/// Loads the active impact-scope row for one logical document.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the impact-scope row.
pub async fn get_active_content_mutation_impact_scope_by_document_id(
    pool: &PgPool,
    document_id: Uuid,
) -> Result<Option<ContentMutationImpactScopeRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentMutationImpactScopeRow>(
        "select id, workspace_id, library_id, document_id, mutation_id, mutation_kind,
            source_revision_id, target_revision_id, scope_status, confidence_status,
            affected_node_ids_json, affected_relationship_ids_json, fallback_reason, detected_at,
            completed_at, created_at, updated_at
         from content_mutation_impact_scope
         where document_id = $1
           and completed_at is null
         order by updated_at desc, detected_at desc, created_at desc
         limit 1",
    )
    .bind(document_id)
    .fetch_optional(pool)
    .await
}

/// Lists active mutation impact-scope rows for one project.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the impact-scope rows.
pub async fn list_active_content_mutation_impact_scopes_by_library(
    pool: &PgPool,
    library_id: Uuid,
) -> Result<Vec<ContentMutationImpactScopeRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentMutationImpactScopeRow>(
        "select id, workspace_id, library_id, document_id, mutation_id, mutation_kind,
            source_revision_id, target_revision_id, scope_status, confidence_status,
            affected_node_ids_json, affected_relationship_ids_json, fallback_reason, detected_at,
            completed_at, created_at, updated_at
         from content_mutation_impact_scope
         where library_id = $1
           and completed_at is null
         order by updated_at desc, detected_at desc, created_at desc",
    )
    .bind(library_id)
    .fetch_all(pool)
    .await
}
