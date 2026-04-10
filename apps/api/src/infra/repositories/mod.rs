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
#[allow(clippy::missing_errors_doc)]
pub mod extract_repository;
#[allow(clippy::missing_errors_doc)]
pub mod iam_repository;
#[allow(clippy::missing_errors_doc, clippy::too_many_lines)]
pub mod ingest_repository;
#[allow(clippy::missing_errors_doc)]
pub mod ops_repository;
#[allow(clippy::missing_errors_doc)]
pub mod query_repository;
mod runtime_graph_repository;
mod runtime_graph_summary_repository;
pub mod runtime_repository;

pub use runtime_graph_repository::*;
pub use runtime_graph_summary_repository::*;

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::domains::{
    billing::{
        PricingBillingUnit, PricingCapability, RuntimeStageBillingPolicy,
        decorate_payload_with_stage_ownership, runtime_stage_billing_policy,
        stage_native_ownership,
    },
    runtime_ingestion::RuntimeQueueWaitingReason,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestionExecutionPayload {
    #[serde(alias = "project_id")]
    pub library_id: Uuid,
    #[serde(default)]
    pub runtime_ingestion_run_id: Option<Uuid>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentRow {
    pub id: Uuid,
    pub library_id: Uuid,
    pub source_id: Option<Uuid>,
    pub external_key: String,
    pub title: Option<String>,
    pub mime_type: Option<String>,
    pub checksum: Option<String>,
    pub active_revision_id: Option<Uuid>,
    pub document_state: String,
    pub mutation_kind: Option<String>,
    pub mutation_status: Option<String>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkRow {
    pub id: Uuid,
    pub document_id: Uuid,
    pub library_id: Uuid,
    pub ordinal: i32,
    pub content: String,
    pub token_count: Option<i32>,
    pub metadata_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

/// Advances one library's dedicated source-truth version and returns the new value.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the `catalog_library` row.
pub async fn touch_library_source_truth_version(
    pool: &PgPool,
    library_id: Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "update catalog_library
         set source_truth_version = greatest(
                coalesce(source_truth_version, 0) + 1,
                (extract(epoch from clock_timestamp()) * 1000000)::bigint
             )
         where id = $1
         returning source_truth_version",
    )
    .bind(library_id)
    .fetch_one(pool)
    .await
    .map(|version| version.max(1))
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct VisibleLibraryWithCountsRow {
    pub library_id: Uuid,
    pub workspace_id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub document_count: i64,
    pub readable_document_count: i64,
    pub processing_document_count: i64,
    pub failed_document_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct DocumentMemorySearchHitRow {
    pub document_id: Uuid,
    pub library_id: Uuid,
    pub workspace_id: Uuid,
    pub document_title: Option<String>,
    pub external_key: String,
    pub latest_revision_id: Option<Uuid>,
    pub chunk_match_count: i64,
    pub excerpt: Option<String>,
    pub excerpt_start_offset: Option<i64>,
    pub excerpt_end_offset: Option<i64>,
    pub readability_state: String,
    pub status_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct LatestReadableRuntimeDocumentStateRow {
    pub document_id: Uuid,
    pub library_id: Uuid,
    pub workspace_id: Uuid,
    pub latest_revision_id: Option<Uuid>,
    pub ingestion_run_id: Option<Uuid>,
    pub runtime_status: Option<String>,
    pub readability_state: String,
    pub status_reason: Option<String>,
    pub content_text: Option<String>,
    pub content_char_count: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeDocumentReadSliceRow {
    pub document_id: Uuid,
    pub library_id: Uuid,
    pub workspace_id: Uuid,
    pub latest_revision_id: Option<Uuid>,
    pub ingestion_run_id: Uuid,
    pub content: String,
    pub slice_start_offset: i64,
    pub slice_end_offset: i64,
    pub total_content_length: i64,
}

/// Lists libraries in one workspace with document readiness counters.
///
/// # Errors
/// Returns any `SQLx` error raised while querying aggregated library counts.
pub async fn list_visible_libraries_with_counts(
    pool: &PgPool,
    workspace_id: Uuid,
) -> Result<Vec<VisibleLibraryWithCountsRow>, sqlx::Error> {
    sqlx::query_as::<_, VisibleLibraryWithCountsRow>(
        "select l.id as library_id,
                l.workspace_id,
                l.slug,
                l.display_name as name,
                l.description,
                count(d.id)::bigint as document_count,
                count(d.id) filter (
                    where latest_run.id is not null
                      and extracted.content_text is not null
                      and btrim(extracted.content_text) <> ''
                )::bigint as readable_document_count,
                count(d.id) filter (
                    where latest_run.id is not null
                      and latest_run.status <> 'failed'
                      and (
                            extracted.content_text is null
                            or btrim(extracted.content_text) = ''
                          )
                )::bigint as processing_document_count,
                count(d.id) filter (
                    where latest_run.status = 'failed'
                      and (
                            extracted.content_text is null
                            or btrim(extracted.content_text) = ''
                          )
                )::bigint as failed_document_count
         from catalog_library l
         left join content_document d
           on d.library_id = l.id
          and d.document_state = 'active'
          and d.deleted_at is null
         left join lateral (
            select rir.id, rir.status
            from runtime_ingestion_run rir
            where rir.library_id = l.id
              and rir.document_id = d.id
            order by rir.created_at desc
            limit 1
         ) latest_run on true
         left join runtime_extracted_content extracted
           on extracted.ingestion_run_id = latest_run.id
         where l.workspace_id = $1
           and l.lifecycle_state = 'active'
         group by l.id, l.workspace_id, l.slug, l.display_name, l.description, l.created_at
         order by l.created_at asc, l.display_name asc",
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await
}

/// Searches document memory across one or more library scopes and aggregates chunk matches.
///
/// # Errors
/// Returns any `SQLx` error raised while querying document-level memory hits.
pub async fn search_document_memory_by_library_scope(
    pool: &PgPool,
    library_ids: &[Uuid],
    query_text: &str,
    limit: i64,
) -> Result<Vec<DocumentMemorySearchHitRow>, sqlx::Error> {
    if library_ids.is_empty() {
        return Ok(Vec::new());
    }

    let pattern = format!("%{query_text}%");
    sqlx::query_as::<_, DocumentMemorySearchHitRow>(
        "with latest_run as (
            select distinct on (rir.document_id)
                   rir.document_id,
                   rir.id,
                   rir.revision_id,
                   rir.status,
                   rir.latest_error_message
            from runtime_ingestion_run rir
            where rir.library_id = any($1)
              and rir.document_id is not null
            order by rir.document_id, rir.created_at desc
         ),
         latest_state as (
            select d.id as document_id,
                   d.library_id,
                   d.workspace_id,
                   revision.title as document_title,
                   d.external_key,
                   coalesce(lr.revision_id, head.readable_revision_id, head.active_revision_id) as latest_revision_id,
                   lr.id as ingestion_run_id,
                   lr.status,
                   lr.latest_error_message,
                   nullif(btrim(extracted.content_text), '') as content_text
            from content_document d
            left join content_document_head head
              on head.document_id = d.id
            left join content_revision revision
              on revision.id = coalesce(head.readable_revision_id, head.active_revision_id)
            left join latest_run lr
              on lr.document_id = d.id
            left join runtime_extracted_content extracted
              on extracted.ingestion_run_id = lr.id
            where d.library_id = any($1)
              and d.document_state = 'active'
              and d.deleted_at is null
         ),
         readable_matches as (
            select ls.document_id,
                   ls.library_id,
                   greatest(
                       (
                           char_length(lower(ls.content_text))
                           - char_length(replace(lower(ls.content_text), lower($3), ''))
                       ) / greatest(char_length($3), 1),
                       1
                   )::bigint as match_count,
                   nullif(strpos(lower(ls.content_text), lower($3)), 0) as match_pos
            from latest_state ls
            where ls.content_text is not null
              and ls.content_text ilike $2
         ),
         fallback_chunk_matches as (
            select state.document_id,
                   state.library_id,
                   count(*)::bigint as match_count,
                   (array_agg(c.normalized_text order by c.chunk_index asc))[1] as first_chunk_excerpt
            from latest_state state
            join content_revision revision
              on revision.id = state.latest_revision_id
             and revision.document_id = state.document_id
            join content_chunk c
              on c.revision_id = revision.id
            where state.library_id = any($1)
              and c.normalized_text ilike $2
              and state.content_text is null
            group by state.document_id, state.library_id
         ),
         matched_documents as (
            select rm.document_id,
                   rm.library_id,
                   rm.match_count,
                   rm.match_pos,
                   null::text as first_chunk_excerpt
            from readable_matches rm
            union all
            select cm.document_id,
                   cm.library_id,
                   cm.match_count,
                   null::integer as match_pos,
                   cm.first_chunk_excerpt
            from fallback_chunk_matches cm
         )
         select ls.document_id,
                ls.library_id,
                ls.workspace_id,
                ls.document_title,
                ls.external_key,
                ls.latest_revision_id,
                md.match_count as chunk_match_count,
                case
                    when ls.content_text is not null
                    then substring(
                        ls.content_text
                        from greatest(coalesce(md.match_pos, 1), 1)
                        for 320
                    )
                    else md.first_chunk_excerpt
                end as excerpt,
                case
                    when ls.content_text is not null
                     and md.match_pos is not null
                    then (md.match_pos - 1)::bigint
                    else null
                end as excerpt_start_offset,
                case
                    when ls.content_text is not null
                     and md.match_pos is not null
                    then (
                        md.match_pos
                        - 1
                        + char_length($3)
                    )::bigint
                    else null
                end as excerpt_end_offset,
                case
                    when ls.content_text is not null then 'readable'
                    when ls.status = 'failed' then 'failed'
                    when ls.ingestion_run_id is not null then 'processing'
                    else 'unavailable'
                end as readability_state,
                case
                    when ls.ingestion_run_id is null then 'document has no runtime ingestion state yet'
                    when ls.content_text is not null then null
                    when ls.status = 'failed' then coalesce(ls.latest_error_message, 'document ingestion failed')
                    when ls.status in ('ready', 'ready_no_graph')
                    then 'document finished without normalized extracted text'
                    else 'document is still being processed'
                end as status_reason
         from matched_documents md
         join latest_state ls
           on ls.document_id = md.document_id
          and ls.library_id = md.library_id
         order by md.match_count desc, ls.document_id desc
         limit $4",
    )
    .bind(library_ids)
    .bind(pattern)
    .bind(query_text)
    .bind(limit.max(1))
    .fetch_all(pool)
    .await
}

/// Resolves the latest readable state projection for one logical document.
///
/// # Errors
/// Returns any `SQLx` error raised while querying document runtime state.
pub async fn get_latest_readable_runtime_document_state(
    pool: &PgPool,
    document_id: Uuid,
) -> Result<Option<LatestReadableRuntimeDocumentStateRow>, sqlx::Error> {
    sqlx::query_as::<_, LatestReadableRuntimeDocumentStateRow>(
        "select d.id as document_id,
                d.library_id,
                d.workspace_id,
                coalesce(latest_run.revision_id, head.readable_revision_id, head.active_revision_id) as latest_revision_id,
                latest_run.id as ingestion_run_id,
                latest_run.status as runtime_status,
                case
                    when extracted.content_text is not null
                     and btrim(extracted.content_text) <> ''
                    then 'readable'
                    when latest_run.status = 'failed' then 'failed'
                    when latest_run.id is not null then 'processing'
                    else 'unavailable'
                end as readability_state,
                case
                    when latest_run.id is null then 'document has no runtime ingestion state yet'
                    when extracted.content_text is not null
                     and btrim(extracted.content_text) <> ''
                    then null
                    when latest_run.status = 'failed' then coalesce(latest_run.latest_error_message, 'document ingestion failed')
                    when latest_run.status in ('ready', 'ready_no_graph')
                     and (extracted.content_text is null or btrim(extracted.content_text) = '')
                    then 'document finished without normalized extracted text'
                    else 'document is still being processed'
                end as status_reason,
                nullif(btrim(extracted.content_text), '') as content_text,
                extracted.char_count as content_char_count
         from content_document d
         left join content_document_head head
           on head.document_id = d.id
         left join lateral (
            select rir.id, rir.revision_id, rir.status, rir.latest_error_message
            from runtime_ingestion_run rir
            where rir.library_id = d.library_id
              and rir.document_id = d.id
            order by rir.created_at desc
            limit 1
         ) latest_run on true
         left join runtime_extracted_content extracted
           on extracted.ingestion_run_id = latest_run.id
         where d.id = $1
           and d.document_state = 'active'
           and d.deleted_at is null",
    )
    .bind(document_id)
    .fetch_optional(pool)
    .await
}

/// Loads one normalized read window from the latest readable text for a document.
///
/// # Errors
/// Returns any `SQLx` error raised while querying document runtime state.
pub async fn load_runtime_document_read_slice(
    pool: &PgPool,
    document_id: Uuid,
    start_offset: usize,
    requested_length: usize,
) -> Result<Option<RuntimeDocumentReadSliceRow>, sqlx::Error> {
    let Some(state) = get_latest_readable_runtime_document_state(pool, document_id).await? else {
        return Ok(None);
    };
    if state.readability_state != "readable" {
        return Ok(None);
    }

    let Some(ingestion_run_id) = state.ingestion_run_id else {
        return Ok(None);
    };
    let Some(content_text) = state.content_text else {
        return Ok(None);
    };

    let total_content_length = content_text.chars().count();
    let bounded_start = start_offset.min(total_content_length);
    let bounded_length = requested_length.max(1);
    let slice_content = slice_text_by_chars(&content_text, bounded_start, bounded_length);
    let slice_end_offset = bounded_start.saturating_add(slice_content.chars().count());

    Ok(Some(RuntimeDocumentReadSliceRow {
        document_id: state.document_id,
        library_id: state.library_id,
        workspace_id: state.workspace_id,
        latest_revision_id: state.latest_revision_id,
        ingestion_run_id,
        content: slice_content,
        slice_start_offset: i64::try_from(bounded_start).unwrap_or(i64::MAX),
        slice_end_offset: i64::try_from(slice_end_offset).unwrap_or(i64::MAX),
        total_content_length: i64::try_from(total_content_length).unwrap_or(i64::MAX),
    }))
}

fn slice_text_by_chars(content: &str, start_offset: usize, requested_length: usize) -> String {
    content.chars().skip(start_offset).take(requested_length).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeIngestionRunRow {
    pub id: Uuid,
    pub library_id: Uuid,
    pub document_id: Option<Uuid>,
    pub revision_id: Option<Uuid>,
    pub upload_batch_id: Option<Uuid>,
    pub track_id: String,
    pub file_name: String,
    pub file_type: String,
    pub mime_type: Option<String>,
    pub file_size_bytes: Option<i64>,
    pub status: String,
    pub current_stage: String,
    pub progress_percent: Option<i32>,
    pub activity_status: String,
    pub last_activity_at: Option<DateTime<Utc>>,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    pub provider_profile_snapshot_json: serde_json::Value,
    pub latest_error_message: Option<String>,
    pub current_attempt_no: i32,
    pub attempt_kind: String,
    pub queue_started_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub queue_elapsed_ms: Option<i64>,
    pub total_elapsed_ms: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeIngestionStageEventRow {
    pub id: Uuid,
    pub ingestion_run_id: Uuid,
    pub attempt_no: i32,
    pub stage: String,
    pub status: String,
    pub message: Option<String>,
    pub metadata_json: serde_json::Value,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub elapsed_ms: Option<i64>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct AttemptStageAccountingRow {
    pub id: Uuid,
    pub ingestion_run_id: Uuid,
    pub stage_event_id: Uuid,
    pub stage: String,
    pub accounting_scope: String,
    pub call_sequence_no: i32,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub capability: String,
    pub billing_unit: String,
    pub pricing_catalog_entry_id: Option<Uuid>,
    pub pricing_status: String,
    pub estimated_cost: Option<Decimal>,
    pub currency: Option<String>,
    pub token_usage_json: serde_json::Value,
    pub pricing_snapshot_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct AttemptStageCostSummaryRow {
    pub ingestion_run_id: Uuid,
    pub total_estimated_cost: Option<Decimal>,
    pub settled_estimated_cost: Option<Decimal>,
    pub in_flight_estimated_cost: Option<Decimal>,
    pub currency: Option<String>,
    pub priced_stage_count: i32,
    pub unpriced_stage_count: i32,
    pub in_flight_stage_count: i32,
    pub missing_stage_count: i32,
    pub accounting_status: String,
    pub computed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeCollectionResolvedStageAccountingRow {
    pub ingestion_run_id: Uuid,
    pub file_type: String,
    pub stage: String,
    pub accounting_scope: String,
    pub pricing_status: String,
    pub estimated_cost: Option<Decimal>,
    pub currency: Option<String>,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeCollectionProgressRollupRow {
    pub accepted_count: i64,
    pub content_extracted_count: i64,
    pub chunked_count: i64,
    pub embedded_count: i64,
    pub extracting_graph_count: i64,
    pub graph_ready_count: i64,
    pub ready_count: i64,
    pub failed_count: i64,
    pub queue_backlog_count: i64,
    pub processing_backlog_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeCollectionStageRollupRow {
    pub stage: String,
    pub active_count: i64,
    pub completed_count: i64,
    pub failed_count: i64,
    pub avg_elapsed_ms: Option<i64>,
    pub max_elapsed_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeCollectionFormatRollupRow {
    pub file_type: String,
    pub document_count: i64,
    pub queued_count: i64,
    pub processing_count: i64,
    pub ready_count: i64,
    pub ready_no_graph_count: i64,
    pub failed_count: i64,
    pub content_extracted_count: i64,
    pub chunked_count: i64,
    pub embedded_count: i64,
    pub extracting_graph_count: i64,
    pub graph_ready_count: i64,
    pub avg_queue_elapsed_ms: Option<i64>,
    pub max_queue_elapsed_ms: Option<i64>,
    pub avg_total_elapsed_ms: Option<i64>,
    pub max_total_elapsed_ms: Option<i64>,
    pub bottleneck_stage: Option<String>,
    pub bottleneck_avg_elapsed_ms: Option<i64>,
    pub bottleneck_max_elapsed_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeProviderFailureSnapshotRow {
    pub ingestion_run_id: Uuid,
    pub attempt_no: i32,
    pub provider_failure_class: Option<String>,
    pub request_shape_key: Option<String>,
    pub request_size_bytes: Option<i64>,
    pub upstream_status: Option<String>,
    pub retry_outcome: Option<String>,
    pub computed_at: DateTime<Utc>,
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
pub struct RuntimeExtractedContentRow {
    pub id: Uuid,
    pub ingestion_run_id: Uuid,
    pub document_id: Option<Uuid>,
    pub extraction_kind: String,
    pub content_text: Option<String>,
    pub page_count: Option<i32>,
    pub char_count: Option<i32>,
    pub extraction_warnings_json: serde_json::Value,
    pub source_map_json: serde_json::Value,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub extraction_version: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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
pub struct RuntimeDocumentContributionSummaryRow {
    pub document_id: Uuid,
    pub revision_id: Option<Uuid>,
    pub ingestion_run_id: Option<Uuid>,
    pub latest_attempt_no: i32,
    pub chunk_count: Option<i32>,
    pub admitted_graph_node_count: i32,
    pub admitted_graph_edge_count: i32,
    pub filtered_graph_edge_count: i32,
    pub filtered_artifact_count: i32,
    pub computed_at: DateTime<Utc>,
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

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeProviderProfileRow {
    pub library_id: Uuid,
    pub indexing_provider_kind: String,
    pub indexing_model_name: String,
    pub embedding_provider_kind: String,
    pub embedding_model_name: String,
    pub answer_provider_kind: String,
    pub answer_model_name: String,
    pub vision_provider_kind: String,
    pub vision_model_name: String,
    pub last_validated_at: Option<DateTime<Utc>>,
    pub last_validation_status: Option<String>,
    pub last_validation_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeProviderValidationLogRow {
    pub id: Uuid,
    pub library_id: Option<Uuid>,
    pub provider_kind: String,
    pub model_name: String,
    pub capability: String,
    pub status: String,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ModelPricingCatalogEntryRow {
    pub id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub provider_kind: String,
    pub model_name: String,
    pub capability: String,
    pub billing_unit: String,
    pub input_price: Option<Decimal>,
    pub output_price: Option<Decimal>,
    pub currency: String,
    pub status: String,
    pub source_kind: String,
    pub note: Option<String>,
    pub effective_from: DateTime<Utc>,
    pub effective_to: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ModelPricingResolutionRow {
    pub pricing_catalog_entry_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub provider_kind: String,
    pub model_name: String,
    pub capability: String,
    pub billing_unit: String,
    pub input_price: Option<Decimal>,
    pub output_price: Option<Decimal>,
    pub currency: String,
    pub status: String,
    pub source_kind: String,
    pub effective_from: DateTime<Utc>,
    pub effective_to: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewAttemptStageAccounting {
    pub ingestion_run_id: Uuid,
    pub stage_event_id: Uuid,
    pub stage: String,
    pub accounting_scope: String,
    pub call_sequence_no: i32,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub capability: String,
    pub billing_unit: String,
    pub pricing_catalog_entry_id: Option<Uuid>,
    pub pricing_status: String,
    pub estimated_cost: Option<Decimal>,
    pub currency: Option<String>,
    pub token_usage_json: serde_json::Value,
    pub pricing_snapshot_json: serde_json::Value,
}

fn sanitize_new_attempt_stage_accounting(
    new_row: &NewAttemptStageAccounting,
) -> Result<NewAttemptStageAccounting, sqlx::Error> {
    let ownership =
        stage_native_ownership(new_row.ingestion_run_id, new_row.stage_event_id, &new_row.stage);
    let mut normalized = new_row.clone();
    if normalized.accounting_scope.trim().is_empty() {
        normalized.accounting_scope = "stage_rollup".to_string();
    }
    match normalized.accounting_scope.as_str() {
        "stage_rollup" => normalized.call_sequence_no = 0,
        "provider_call" => {
            if normalized.call_sequence_no <= 0 {
                return Err(sqlx::Error::Protocol(format!(
                    "provider_call accounting for stage {} must use positive call_sequence_no",
                    normalized.stage
                )));
            }
        }
        other => {
            return Err(sqlx::Error::Protocol(format!(
                "unsupported accounting_scope {} for stage {}",
                other, normalized.stage
            )));
        }
    }
    normalized.token_usage_json =
        decorate_payload_with_stage_ownership(normalized.token_usage_json, &ownership);
    normalized.pricing_snapshot_json =
        decorate_payload_with_stage_ownership(normalized.pricing_snapshot_json, &ownership);

    match runtime_stage_billing_policy(&normalized.stage) {
        RuntimeStageBillingPolicy::Billable { capability, billing_unit } => {
            let expected_capability = pricing_capability_label(&capability);
            let expected_billing_unit = pricing_billing_unit_label(&billing_unit);
            if normalized.capability != expected_capability
                || normalized.billing_unit != expected_billing_unit
            {
                return Err(sqlx::Error::Protocol(format!(
                    "stage accounting ownership mismatch for {}: expected {} / {}, got {} / {}",
                    normalized.stage,
                    expected_capability,
                    expected_billing_unit,
                    normalized.capability,
                    normalized.billing_unit,
                )));
            }
        }
        RuntimeStageBillingPolicy::NonBillable => {
            if normalized.pricing_status.eq_ignore_ascii_case("priced")
                || normalized.estimated_cost.is_some()
                || normalized.pricing_catalog_entry_id.is_some()
            {
                return Err(sqlx::Error::Protocol(format!(
                    "non-billable stage {} cannot persist priced accounting artifacts",
                    normalized.stage
                )));
            }
        }
    }

    Ok(normalized)
}

fn pricing_capability_label(value: &PricingCapability) -> &'static str {
    match value {
        PricingCapability::Indexing => "indexing",
        PricingCapability::Embedding => "embedding",
        PricingCapability::Answer => "answer",
        PricingCapability::Vision => "vision",
        PricingCapability::GraphExtract => "graph_extract",
    }
}

fn pricing_billing_unit_label(value: &PricingBillingUnit) -> &'static str {
    match value {
        PricingBillingUnit::Per1MInputTokens => "per_1m_input_tokens",
        PricingBillingUnit::Per1MCachedInputTokens => "per_1m_cached_input_tokens",
        PricingBillingUnit::Per1MOutputTokens => "per_1m_output_tokens",
        PricingBillingUnit::Per1MTokens => "per_1m_tokens",
        PricingBillingUnit::FixedPerCall => "fixed_per_call",
    }
}

#[derive(Debug, Clone)]
pub struct NewModelPricingCatalogEntry {
    pub workspace_id: Option<Uuid>,
    pub provider_kind: String,
    pub model_name: String,
    pub capability: String,
    pub billing_unit: String,
    pub input_price: Option<Decimal>,
    pub output_price: Option<Decimal>,
    pub currency: String,
    pub source_kind: String,
    pub note: Option<String>,
    pub effective_from: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct UpdateModelPricingCatalogEntry {
    pub workspace_id: Option<Uuid>,
    pub provider_kind: String,
    pub model_name: String,
    pub capability: String,
    pub billing_unit: String,
    pub input_price: Option<Decimal>,
    pub output_price: Option<Decimal>,
    pub currency: String,
    pub note: Option<String>,
    pub effective_from: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeVectorTargetRow {
    pub id: Uuid,
    pub library_id: Uuid,
    pub target_kind: String,
    pub target_id: Uuid,
    pub provider_kind: String,
    pub model_name: String,
    pub dimensions: Option<i32>,
    pub embedding_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct RuntimeVectorTargetUpsertInput {
    pub library_id: Uuid,
    pub target_kind: String,
    pub target_id: Uuid,
    pub provider_kind: String,
    pub model_name: String,
    pub dimensions: Option<i32>,
    pub embedding_json: serde_json::Value,
}

#[must_use]
pub fn runtime_queue_waiting_reason_key(value: &RuntimeQueueWaitingReason) -> &'static str {
    match value {
        RuntimeQueueWaitingReason::OrdinaryBacklog => "ordinary_backlog",
        RuntimeQueueWaitingReason::IsolatedCapacityWait => "isolated_capacity_wait",
        RuntimeQueueWaitingReason::Blocked => "blocked",
        RuntimeQueueWaitingReason::Degraded => "degraded",
    }
}

#[must_use]
pub fn parse_runtime_queue_waiting_reason(
    value: Option<&str>,
) -> Option<RuntimeQueueWaitingReason> {
    match value {
        Some("ordinary_backlog") => Some(RuntimeQueueWaitingReason::OrdinaryBacklog),
        Some("isolated_capacity_wait") => Some(RuntimeQueueWaitingReason::IsolatedCapacityWait),
        Some("blocked") => Some(RuntimeQueueWaitingReason::Blocked),
        Some("degraded") => Some(RuntimeQueueWaitingReason::Degraded),
        _ => None,
    }
}

/// Creates a runtime ingestion run.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the runtime ingestion run.
pub async fn create_runtime_ingestion_run(
    pool: &PgPool,
    library_id: Uuid,
    document_id: Option<Uuid>,
    revision_id: Option<Uuid>,
    upload_batch_id: Option<Uuid>,
    track_id: &str,
    file_name: &str,
    file_type: &str,
    mime_type: Option<&str>,
    file_size_bytes: Option<i64>,
    status: &str,
    current_stage: &str,
    attempt_kind: &str,
    provider_profile_snapshot_json: serde_json::Value,
) -> Result<RuntimeIngestionRunRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "insert into runtime_ingestion_run (
            id, library_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, attempt_kind, provider_profile_snapshot_json
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
         returning id, library_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type,
            mime_type, file_size_bytes, status, current_stage, progress_percent,
            activity_status, last_activity_at, last_heartbeat_at,
            provider_profile_snapshot_json, latest_error_message, current_attempt_no, attempt_kind,
            queue_started_at, started_at, finished_at, queue_elapsed_ms, total_elapsed_ms, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(library_id)
    .bind(document_id)
    .bind(revision_id)
    .bind(upload_batch_id)
    .bind(track_id)
    .bind(file_name)
    .bind(file_type)
    .bind(mime_type)
    .bind(file_size_bytes)
    .bind(status)
    .bind(current_stage)
    .bind(attempt_kind)
    .bind(provider_profile_snapshot_json)
    .fetch_one(pool)
    .await
}

/// Lists runtime ingestion runs for one library.
///
/// # Errors
/// Returns any `SQLx` error raised while querying runtime ingestion runs.
pub async fn list_runtime_ingestion_runs_by_library(
    pool: &PgPool,
    library_id: Uuid,
) -> Result<Vec<RuntimeIngestionRunRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "select id, library_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status,
            last_activity_at, last_heartbeat_at, provider_profile_snapshot_json,
            latest_error_message, current_attempt_no, attempt_kind, queue_started_at, started_at, finished_at,
            queue_elapsed_ms, total_elapsed_ms, created_at, updated_at
         from runtime_ingestion_run
         where library_id = $1
           and (
                document_id is null
                or not exists (
                    select 1
                    from content_document
                    where content_document.id = runtime_ingestion_run.document_id
                      and content_document.deleted_at is not null
                )
           )
         order by created_at desc",
    )
    .bind(library_id)
    .fetch_all(pool)
    .await
}

/// Loads one runtime ingestion run by id.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the runtime ingestion run.
pub async fn get_runtime_ingestion_run_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<RuntimeIngestionRunRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "select id, library_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status,
            last_activity_at, last_heartbeat_at, provider_profile_snapshot_json,
            latest_error_message, current_attempt_no, attempt_kind, queue_started_at, started_at, finished_at,
            queue_elapsed_ms, total_elapsed_ms, created_at, updated_at
         from runtime_ingestion_run
         where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Loads one runtime ingestion run by track id.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the runtime ingestion run.
pub async fn get_runtime_ingestion_run_by_track_id(
    pool: &PgPool,
    track_id: &str,
) -> Result<Option<RuntimeIngestionRunRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "select id, library_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status,
            last_activity_at, last_heartbeat_at, provider_profile_snapshot_json,
            latest_error_message, current_attempt_no, attempt_kind, queue_started_at, started_at, finished_at,
            queue_elapsed_ms, total_elapsed_ms, created_at, updated_at
         from runtime_ingestion_run
         where track_id = $1",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
}

/// Deletes one runtime ingestion run by id.
///
/// # Errors
/// Returns any `SQLx` error raised while deleting the runtime ingestion run.
pub async fn delete_runtime_ingestion_run_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("delete from runtime_ingestion_run where id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Appends a runtime ingestion stage event.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the stage event.
pub async fn append_runtime_stage_event(
    pool: &PgPool,
    ingestion_run_id: Uuid,
    attempt_no: i32,
    stage: &str,
    status: &str,
    message: Option<&str>,
    metadata_json: serde_json::Value,
) -> Result<RuntimeIngestionStageEventRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionStageEventRow>(
        "insert into runtime_ingestion_stage_event (
            id, ingestion_run_id, attempt_no, stage, status, message, metadata_json,
            provider_kind, model_name, started_at, finished_at, elapsed_ms
         ) values (
            $1, $2, $3, $4, $5, $6, $7,
            nullif($7 ->> 'provider_kind', ''),
            nullif($7 ->> 'model_name', ''),
            coalesce(($7 ->> 'started_at')::timestamptz, now()),
            ($7 ->> 'finished_at')::timestamptz,
            coalesce(
                ($7 ->> 'elapsed_ms')::bigint,
                case
                    when ($7 ->> 'started_at')::timestamptz is not null
                     and ($7 ->> 'finished_at')::timestamptz is not null
                        then greatest(
                            0,
                            floor(
                                extract(
                                    epoch from (
                                        ($7 ->> 'finished_at')::timestamptz
                                        - ($7 ->> 'started_at')::timestamptz
                                    )
                                ) * 1000
                            )::bigint
                        )
                    else null
                end
            )
         )
         returning id, ingestion_run_id, attempt_no, stage, status, message, metadata_json,
            provider_kind, model_name, started_at, finished_at, elapsed_ms, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(ingestion_run_id)
    .bind(attempt_no)
    .bind(stage)
    .bind(status)
    .bind(message)
    .bind(metadata_json)
    .fetch_one(pool)
    .await
}

/// Lists runtime stage events for one ingestion run.
///
/// # Errors
/// Returns any `SQLx` error raised while querying stage events.
pub async fn list_runtime_stage_events_by_run(
    pool: &PgPool,
    ingestion_run_id: Uuid,
) -> Result<Vec<RuntimeIngestionStageEventRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionStageEventRow>(
        "select id, ingestion_run_id, attempt_no, stage, status, message, metadata_json,
            provider_kind, model_name, started_at, finished_at, elapsed_ms, created_at
         from runtime_ingestion_stage_event
         where ingestion_run_id = $1
         order by created_at asc, id asc",
    )
    .bind(ingestion_run_id)
    .fetch_all(pool)
    .await
}

/// Updates the status, stage, and progress for a runtime ingestion run.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the runtime ingestion run.
pub async fn update_runtime_ingestion_run_status(
    pool: &PgPool,
    id: Uuid,
    status: &str,
    current_stage: &str,
    progress_percent: Option<i32>,
    latest_error_message: Option<&str>,
) -> Result<RuntimeIngestionRunRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "update runtime_ingestion_run
         set status = $2,
             current_stage = $3,
             progress_percent = $4,
             latest_error_message = $5,
             activity_status = case
                 when $2 in ('ready', 'ready_no_graph') then 'ready'
                 when $2 = 'failed' then 'failed'
                 when $2 = 'processing' then 'active'
                 else 'queued'
             end,
             last_activity_at = now(),
             last_heartbeat_at = case when $2 = 'processing' then now() else last_heartbeat_at end,
             updated_at = now(),
             started_at = coalesce(started_at, now()),
             queue_elapsed_ms = case
                 when started_at is null then queue_elapsed_ms
                 else coalesce(queue_elapsed_ms, greatest(0, floor(extract(epoch from (started_at - queue_started_at)) * 1000)::bigint))
             end,
             finished_at = case when $2 in ('ready', 'ready_no_graph', 'failed') then now() else finished_at end,
             total_elapsed_ms = case
                 when $2 in ('ready', 'ready_no_graph', 'failed')
                     then greatest(0, floor(extract(epoch from (now() - queue_started_at)) * 1000)::bigint)
                 else total_elapsed_ms
             end
         where id = $1
         returning id, library_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status,
            last_activity_at, last_heartbeat_at, provider_profile_snapshot_json,
            latest_error_message, current_attempt_no, attempt_kind, queue_started_at, started_at, finished_at,
            queue_elapsed_ms, total_elapsed_ms, created_at, updated_at",
    )
    .bind(id)
    .bind(status)
    .bind(current_stage)
    .bind(progress_percent)
    .bind(latest_error_message)
    .fetch_one(pool)
    .await
}

/// Updates a processing-stage transition in one write so long-running workers do not churn the
/// runtime row with separate status and activity updates.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the runtime ingestion run.
pub async fn update_runtime_ingestion_run_processing_stage(
    pool: &PgPool,
    id: Uuid,
    current_stage: &str,
    progress_percent: Option<i32>,
    last_activity_at: DateTime<Utc>,
    latest_error_message: Option<&str>,
) -> Result<RuntimeIngestionRunRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "update runtime_ingestion_run
         set status = 'processing',
             current_stage = $2,
             progress_percent = $3,
             activity_status = 'active',
             last_activity_at = $4,
             last_heartbeat_at = coalesce(last_heartbeat_at, $4),
             latest_error_message = $5,
             updated_at = now(),
             started_at = coalesce(started_at, $4),
             queue_elapsed_ms = coalesce(
                 queue_elapsed_ms,
                 greatest(0, floor(extract(epoch from (coalesce(started_at, $4) - queue_started_at)) * 1000)::bigint)
             )
         where id = $1
         returning id, library_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status,
            last_activity_at, last_heartbeat_at, provider_profile_snapshot_json,
            latest_error_message, current_attempt_no, attempt_kind, queue_started_at, started_at, finished_at,
            queue_elapsed_ms, total_elapsed_ms, created_at, updated_at",
    )
    .bind(id)
    .bind(current_stage)
    .bind(progress_percent)
    .bind(last_activity_at)
    .bind(latest_error_message)
    .fetch_one(pool)
    .await
}

/// Advances a processing-stage progress checkpoint only when the visible progress marker or
/// activity heartbeat meaningfully changes.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the runtime ingestion run.
pub async fn update_runtime_ingestion_run_processing_stage_checkpoint(
    pool: &PgPool,
    id: Uuid,
    current_stage: &str,
    progress_percent: i32,
    last_activity_at: DateTime<Utc>,
) -> Result<Option<RuntimeIngestionRunRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "update runtime_ingestion_run
         set status = 'processing',
             current_stage = $2,
             progress_percent = greatest(coalesce(progress_percent, $3), $3),
             activity_status = 'active',
             last_activity_at = $4,
             last_heartbeat_at = coalesce(last_heartbeat_at, $4),
             updated_at = now()
         where id = $1
           and status = 'processing'
           and current_stage = $2
           and (
                coalesce(progress_percent, -1) < $3
                or last_activity_at is null
                or last_activity_at <= ($4 - interval '30 seconds')
           )
         returning id, library_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status,
            last_activity_at, last_heartbeat_at, provider_profile_snapshot_json,
            latest_error_message, current_attempt_no, attempt_kind, queue_started_at, started_at, finished_at,
            queue_elapsed_ms, total_elapsed_ms, created_at, updated_at",
    )
    .bind(id)
    .bind(current_stage)
    .bind(progress_percent)
    .bind(last_activity_at)
    .fetch_optional(pool)
    .await
}

/// Marks a runtime ingestion run as claimed by a worker without implying that stage execution has
/// already produced visible processing activity.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the runtime ingestion run.
pub async fn mark_runtime_ingestion_run_claimed(
    pool: &PgPool,
    id: Uuid,
    claimed_at: DateTime<Utc>,
) -> Result<RuntimeIngestionRunRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "update runtime_ingestion_run
         set started_at = coalesce(started_at, $2),
             queue_elapsed_ms = coalesce(
                 queue_elapsed_ms,
                 greatest(0, floor(extract(epoch from (coalesce(started_at, $2) - queue_started_at)) * 1000)::bigint)
             ),
             last_heartbeat_at = coalesce(last_heartbeat_at, $2),
             updated_at = now()
         where id = $1
         returning id, library_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status,
            last_activity_at, last_heartbeat_at, provider_profile_snapshot_json,
            latest_error_message, current_attempt_no, attempt_kind, queue_started_at, started_at, finished_at,
            queue_elapsed_ms, total_elapsed_ms, created_at, updated_at",
    )
    .bind(id)
    .bind(claimed_at)
    .fetch_one(pool)
    .await
}

/// Resets an existing runtime ingestion run back to the accepted queue state for a new attempt.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the runtime ingestion run.
pub async fn requeue_runtime_ingestion_run(
    pool: &PgPool,
    id: Uuid,
    provider_profile_snapshot_json: serde_json::Value,
) -> Result<RuntimeIngestionRunRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "update runtime_ingestion_run
         set status = 'queued',
             current_stage = 'accepted',
             progress_percent = null,
             activity_status = 'queued',
             last_activity_at = now(),
             last_heartbeat_at = null,
             provider_profile_snapshot_json = $2,
             latest_error_message = null,
             current_attempt_no = current_attempt_no + 1,
             attempt_kind = 'reprocess',
             queue_started_at = now(),
             queue_elapsed_ms = null,
             total_elapsed_ms = null,
             started_at = null,
             finished_at = null,
             updated_at = now()
         where id = $1
         returning id, library_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status,
            last_activity_at, last_heartbeat_at, provider_profile_snapshot_json,
            latest_error_message, current_attempt_no, attempt_kind, queue_started_at, started_at, finished_at,
            queue_elapsed_ms, total_elapsed_ms, created_at, updated_at",
    )
    .bind(id)
    .bind(provider_profile_snapshot_json)
    .fetch_one(pool)
    .await
}

/// Resets a runtime ingestion run for a new revision-aware attempt.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the runtime ingestion run.
pub async fn prepare_runtime_ingestion_run_for_attempt(
    pool: &PgPool,
    id: Uuid,
    revision_id: Option<Uuid>,
    provider_profile_snapshot_json: serde_json::Value,
    attempt_kind: &str,
    file_name: &str,
    file_type: &str,
    mime_type: Option<&str>,
    file_size_bytes: Option<i64>,
) -> Result<RuntimeIngestionRunRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "update runtime_ingestion_run
         set revision_id = $2,
             status = 'queued',
             current_stage = 'accepted',
             progress_percent = null,
             activity_status = 'queued',
             last_activity_at = now(),
             last_heartbeat_at = null,
             provider_profile_snapshot_json = $3,
             latest_error_message = null,
             current_attempt_no = current_attempt_no + 1,
             attempt_kind = $4,
             file_name = $5,
             file_type = $6,
             mime_type = $7,
             file_size_bytes = $8,
             queue_started_at = now(),
             queue_elapsed_ms = null,
             total_elapsed_ms = null,
             started_at = null,
             finished_at = null,
             updated_at = now()
         where id = $1
         returning id, library_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status,
            last_activity_at, last_heartbeat_at, provider_profile_snapshot_json,
            latest_error_message, current_attempt_no, attempt_kind, queue_started_at, started_at, finished_at,
            queue_elapsed_ms, total_elapsed_ms, created_at, updated_at",
    )
    .bind(id)
    .bind(revision_id)
    .bind(provider_profile_snapshot_json)
    .bind(attempt_kind)
    .bind(file_name)
    .bind(file_type)
    .bind(mime_type)
    .bind(file_size_bytes)
    .fetch_one(pool)
    .await
}

/// Attaches the persisted document id to a runtime ingestion run.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the runtime ingestion run.
pub async fn attach_runtime_ingestion_run_document(
    pool: &PgPool,
    id: Uuid,
    document_id: Uuid,
    revision_id: Option<Uuid>,
) -> Result<RuntimeIngestionRunRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "update runtime_ingestion_run
         set document_id = $2,
             revision_id = $3,
             updated_at = now()
         where id = $1
         returning id, library_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status,
            last_activity_at, last_heartbeat_at, provider_profile_snapshot_json,
            latest_error_message, current_attempt_no, attempt_kind, queue_started_at, started_at, finished_at,
            queue_elapsed_ms, total_elapsed_ms, created_at, updated_at",
    )
    .bind(id)
    .bind(document_id)
    .bind(revision_id)
    .fetch_one(pool)
    .await
}

/// Updates activity timestamps and the visible activity state for the active runtime ingestion run.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the runtime ingestion run.
pub async fn update_runtime_ingestion_run_activity(
    pool: &PgPool,
    id: Uuid,
    activity_status: &str,
    last_activity_at: DateTime<Utc>,
    last_heartbeat_at: Option<DateTime<Utc>>,
) -> Result<RuntimeIngestionRunRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "update runtime_ingestion_run
         set activity_status = $2,
             last_activity_at = $3,
             last_heartbeat_at = coalesce($4, last_heartbeat_at),
             updated_at = now()
         where id = $1
         returning id, library_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status, last_activity_at,
            last_heartbeat_at, provider_profile_snapshot_json, latest_error_message, current_attempt_no,
            attempt_kind, queue_started_at, started_at, finished_at, queue_elapsed_ms, total_elapsed_ms,
            created_at, updated_at",
    )
    .bind(id)
    .bind(activity_status)
    .bind(last_activity_at)
    .bind(last_heartbeat_at)
    .fetch_one(pool)
    .await
}

/// Updates activity state alongside the visible stage transition for the active runtime ingestion run.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the runtime ingestion run.
pub async fn update_runtime_ingestion_run_stage_activity(
    pool: &PgPool,
    id: Uuid,
    current_stage: &str,
    progress_percent: Option<i32>,
    activity_status: &str,
    last_activity_at: DateTime<Utc>,
    latest_error_message: Option<&str>,
) -> Result<RuntimeIngestionRunRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "update runtime_ingestion_run
         set current_stage = $2,
             progress_percent = $3,
             activity_status = $4,
             last_activity_at = $5,
             latest_error_message = $6,
             updated_at = now()
         where id = $1
         returning id, library_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status, last_activity_at,
            last_heartbeat_at, provider_profile_snapshot_json, latest_error_message, current_attempt_no,
            attempt_kind, queue_started_at, started_at, finished_at, queue_elapsed_ms, total_elapsed_ms,
            created_at, updated_at",
    )
    .bind(id)
    .bind(current_stage)
    .bind(progress_percent)
    .bind(activity_status)
    .bind(last_activity_at)
    .bind(latest_error_message)
    .fetch_one(pool)
    .await
}

/// Updates a queued runtime ingestion run without stamping synthetic visible activity.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the runtime ingestion run.
pub async fn update_runtime_ingestion_run_queued_stage(
    pool: &PgPool,
    id: Uuid,
    current_stage: &str,
    progress_percent: Option<i32>,
    activity_status: &str,
    latest_error_message: Option<&str>,
) -> Result<RuntimeIngestionRunRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "update runtime_ingestion_run
         set current_stage = $2,
             progress_percent = $3,
             activity_status = $4,
             latest_error_message = $5,
             updated_at = now()
         where id = $1
         returning id, library_id, document_id, revision_id, upload_batch_id, track_id, file_name, file_type, mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status, last_activity_at,
            last_heartbeat_at, provider_profile_snapshot_json, latest_error_message, current_attempt_no,
            attempt_kind, queue_started_at, started_at, finished_at, queue_elapsed_ms, total_elapsed_ms,
            created_at, updated_at",
    )
    .bind(id)
    .bind(current_stage)
    .bind(progress_percent)
    .bind(activity_status)
    .bind(latest_error_message)
    .fetch_one(pool)
    .await
}

/// Updates the worker heartbeat snapshot for the active runtime ingestion run.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the runtime ingestion run.
pub async fn update_runtime_ingestion_run_heartbeat(
    pool: &PgPool,
    id: Uuid,
    last_heartbeat_at: DateTime<Utc>,
    activity_status: &str,
) -> Result<Option<RuntimeIngestionRunRow>, sqlx::Error> {
    update_runtime_ingestion_run_heartbeat_with_interval(
        pool,
        id,
        last_heartbeat_at,
        activity_status,
        1,
    )
    .await
}

/// Updates the worker heartbeat snapshot for the active runtime ingestion run behind a bounded
/// write interval.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the runtime ingestion run.
pub async fn update_runtime_ingestion_run_heartbeat_with_interval(
    pool: &PgPool,
    id: Uuid,
    last_heartbeat_at: DateTime<Utc>,
    activity_status: &str,
    min_write_interval_seconds: i64,
) -> Result<Option<RuntimeIngestionRunRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeIngestionRunRow>(
        "with candidate as (
            select id
            from runtime_ingestion_run
            where id = $1
              and (
                    last_heartbeat_at is null
                    or last_heartbeat_at <= ($2 - ($4 * interval '1 second'))
                    or activity_status <> $3
               )
            for update skip locked
         )
         update runtime_ingestion_run as run
         set activity_status = $3,
             last_activity_at = greatest(coalesce(last_activity_at, $2), $2),
             last_heartbeat_at = $2
         from candidate
         where run.id = candidate.id
         returning run.id, run.library_id, run.document_id, run.revision_id, run.upload_batch_id, run.track_id, run.file_name, run.file_type, run.mime_type,
            file_size_bytes, status, current_stage, progress_percent, activity_status, last_activity_at,
            last_heartbeat_at, provider_profile_snapshot_json, latest_error_message, current_attempt_no,
            attempt_kind, queue_started_at, started_at, finished_at, queue_elapsed_ms, total_elapsed_ms,
            created_at, updated_at",
    )
    .bind(id)
    .bind(last_heartbeat_at)
    .bind(activity_status)
    .bind(min_write_interval_seconds.max(1))
    .fetch_optional(pool)
    .await
}

/// Upserts the full contribution summary for the latest active content revision.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting or updating the contribution summary row.
pub async fn upsert_runtime_document_contribution_summary(
    pool: &PgPool,
    document_id: Uuid,
    revision_id: Option<Uuid>,
    ingestion_run_id: Option<Uuid>,
    latest_attempt_no: i32,
    chunk_count: Option<i32>,
    admitted_graph_node_count: i32,
    admitted_graph_edge_count: i32,
    filtered_graph_edge_count: i32,
    filtered_artifact_count: i32,
) -> Result<RuntimeDocumentContributionSummaryRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeDocumentContributionSummaryRow>(
        "insert into runtime_document_contribution_summary (
            document_id, revision_id, ingestion_run_id, latest_attempt_no, chunk_count,
            admitted_graph_node_count, admitted_graph_edge_count, filtered_graph_edge_count,
            filtered_artifact_count, computed_at
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, now())
         on conflict (document_id) do update
         set revision_id = excluded.revision_id,
             ingestion_run_id = excluded.ingestion_run_id,
             latest_attempt_no = excluded.latest_attempt_no,
             chunk_count = excluded.chunk_count,
             admitted_graph_node_count = excluded.admitted_graph_node_count,
             admitted_graph_edge_count = excluded.admitted_graph_edge_count,
             filtered_graph_edge_count = excluded.filtered_graph_edge_count,
             filtered_artifact_count = excluded.filtered_artifact_count,
             computed_at = excluded.computed_at
         returning document_id, revision_id, ingestion_run_id, latest_attempt_no, chunk_count,
            admitted_graph_node_count, admitted_graph_edge_count, filtered_graph_edge_count,
            filtered_artifact_count, computed_at",
    )
    .bind(document_id)
    .bind(revision_id)
    .bind(ingestion_run_id)
    .bind(latest_attempt_no)
    .bind(chunk_count)
    .bind(admitted_graph_node_count)
    .bind(admitted_graph_edge_count)
    .bind(filtered_graph_edge_count)
    .bind(filtered_artifact_count)
    .fetch_one(pool)
    .await
}

/// Upserts just the persisted chunk count for the latest active revision summary.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting or updating the contribution summary row.
pub async fn upsert_runtime_document_chunk_count(
    pool: &PgPool,
    document_id: Uuid,
    revision_id: Option<Uuid>,
    ingestion_run_id: Option<Uuid>,
    latest_attempt_no: i32,
    chunk_count: Option<i32>,
) -> Result<RuntimeDocumentContributionSummaryRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeDocumentContributionSummaryRow>(
        "insert into runtime_document_contribution_summary (
            document_id, revision_id, ingestion_run_id, latest_attempt_no, chunk_count, computed_at
         ) values ($1, $2, $3, $4, $5, now())
         on conflict (document_id) do update
         set revision_id = excluded.revision_id,
             ingestion_run_id = excluded.ingestion_run_id,
             latest_attempt_no = excluded.latest_attempt_no,
             chunk_count = excluded.chunk_count,
             computed_at = excluded.computed_at
         returning document_id, revision_id, ingestion_run_id, latest_attempt_no, chunk_count,
            admitted_graph_node_count, admitted_graph_edge_count, filtered_graph_edge_count,
            filtered_artifact_count, computed_at",
    )
    .bind(document_id)
    .bind(revision_id)
    .bind(ingestion_run_id)
    .bind(latest_attempt_no)
    .bind(chunk_count)
    .fetch_one(pool)
    .await
}

/// Upserts admitted and filtered graph contribution counts for the latest active revision summary.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting or updating the contribution summary row.
pub async fn upsert_runtime_document_graph_contribution_counts(
    pool: &PgPool,
    document_id: Uuid,
    revision_id: Option<Uuid>,
    ingestion_run_id: Option<Uuid>,
    latest_attempt_no: i32,
    admitted_graph_node_count: i32,
    admitted_graph_edge_count: i32,
    filtered_graph_edge_count: i32,
    filtered_artifact_count: i32,
) -> Result<RuntimeDocumentContributionSummaryRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeDocumentContributionSummaryRow>(
        "insert into runtime_document_contribution_summary (
            document_id, revision_id, ingestion_run_id, latest_attempt_no,
            admitted_graph_node_count, admitted_graph_edge_count, filtered_graph_edge_count,
            filtered_artifact_count, computed_at
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, now())
         on conflict (document_id) do update
         set revision_id = excluded.revision_id,
             ingestion_run_id = excluded.ingestion_run_id,
             latest_attempt_no = excluded.latest_attempt_no,
             admitted_graph_node_count = excluded.admitted_graph_node_count,
             admitted_graph_edge_count = excluded.admitted_graph_edge_count,
             filtered_graph_edge_count = excluded.filtered_graph_edge_count,
             filtered_artifact_count = excluded.filtered_artifact_count,
             computed_at = excluded.computed_at
         returning document_id, revision_id, ingestion_run_id, latest_attempt_no, chunk_count,
            admitted_graph_node_count, admitted_graph_edge_count, filtered_graph_edge_count,
            filtered_artifact_count, computed_at",
    )
    .bind(document_id)
    .bind(revision_id)
    .bind(ingestion_run_id)
    .bind(latest_attempt_no)
    .bind(admitted_graph_node_count)
    .bind(admitted_graph_edge_count)
    .bind(filtered_graph_edge_count)
    .bind(filtered_artifact_count)
    .fetch_one(pool)
    .await
}

/// Loads the latest contribution summary for one logical document.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the contribution summary row.
pub async fn get_runtime_document_contribution_summary_by_document_id(
    pool: &PgPool,
    document_id: Uuid,
) -> Result<Option<RuntimeDocumentContributionSummaryRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeDocumentContributionSummaryRow>(
        "select document_id, revision_id, ingestion_run_id, latest_attempt_no, chunk_count,
            admitted_graph_node_count, admitted_graph_edge_count, filtered_graph_edge_count,
            filtered_artifact_count, computed_at
         from runtime_document_contribution_summary
         where document_id = $1",
    )
    .bind(document_id)
    .fetch_optional(pool)
    .await
}

/// Deletes the cached contribution summary for one logical document.
///
/// # Errors
/// Returns any `SQLx` error raised while deleting the contribution summary row.
pub async fn delete_runtime_document_contribution_summary_by_document_id(
    pool: &PgPool,
    document_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let result =
        sqlx::query("delete from runtime_document_contribution_summary where document_id = $1")
            .bind(document_id)
            .execute(pool)
            .await?;
    Ok(result.rows_affected())
}

/// Upserts extracted-content metadata for one runtime ingestion run.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting or updating extracted content.
pub async fn upsert_runtime_extracted_content(
    pool: &PgPool,
    ingestion_run_id: Uuid,
    document_id: Option<Uuid>,
    extraction_kind: &str,
    content_text: Option<&str>,
    page_count: Option<i32>,
    char_count: Option<i32>,
    extraction_warnings_json: serde_json::Value,
    source_map_json: serde_json::Value,
    provider_kind: Option<&str>,
    model_name: Option<&str>,
    extraction_version: Option<&str>,
) -> Result<RuntimeExtractedContentRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeExtractedContentRow>(
        "insert into runtime_extracted_content (
            id, ingestion_run_id, document_id, extraction_kind, content_text, page_count, char_count,
            extraction_warnings_json, source_map_json, provider_kind, model_name, extraction_version
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
         on conflict (ingestion_run_id) do update
         set document_id = excluded.document_id,
             extraction_kind = excluded.extraction_kind,
             content_text = excluded.content_text,
             page_count = excluded.page_count,
             char_count = excluded.char_count,
             extraction_warnings_json = excluded.extraction_warnings_json,
             source_map_json = excluded.source_map_json,
             provider_kind = excluded.provider_kind,
             model_name = excluded.model_name,
             extraction_version = excluded.extraction_version,
             updated_at = now()
         returning id, ingestion_run_id, document_id, extraction_kind, content_text, page_count,
            char_count, extraction_warnings_json, source_map_json, provider_kind, model_name,
            extraction_version, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(ingestion_run_id)
    .bind(document_id)
    .bind(extraction_kind)
    .bind(content_text)
    .bind(page_count)
    .bind(char_count)
    .bind(extraction_warnings_json)
    .bind(source_map_json)
    .bind(provider_kind)
    .bind(model_name)
    .bind(extraction_version)
    .fetch_one(pool)
    .await
}

/// Loads extracted content for one runtime ingestion run.
///
/// # Errors
/// Returns any `SQLx` error raised while querying extracted-content metadata.
pub async fn get_runtime_extracted_content_by_run(
    pool: &PgPool,
    ingestion_run_id: Uuid,
) -> Result<Option<RuntimeExtractedContentRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeExtractedContentRow>(
        "select id, ingestion_run_id, document_id, extraction_kind, content_text, page_count,
            char_count, extraction_warnings_json, source_map_json, provider_kind, model_name,
            extraction_version, created_at, updated_at
         from runtime_extracted_content
         where ingestion_run_id = $1",
    )
    .bind(ingestion_run_id)
    .fetch_optional(pool)
    .await
}

/// Creates one stage-accounting row for a runtime ingestion attempt.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the stage-accounting row.
pub async fn create_attempt_stage_accounting(
    pool: &PgPool,
    new_row: &NewAttemptStageAccounting,
) -> Result<AttemptStageAccountingRow, sqlx::Error> {
    let normalized = sanitize_new_attempt_stage_accounting(new_row)?;
    sqlx::query_as::<_, AttemptStageAccountingRow>(
        "insert into runtime_attempt_stage_accounting (
            id, ingestion_run_id, stage_event_id, stage, workspace_id, library_id, provider_kind,
            model_name, capability, billing_unit, accounting_scope, call_sequence_no,
            pricing_catalog_entry_id, pricing_status, estimated_cost, currency, token_usage_json,
            pricing_snapshot_json
         ) values (
            $1, $2, $3, $4, $5, $6, $7,
            $8, $9, $10, $11, $12,
            $13, $14, $15, $16, $17,
            $18, $19
         )
         on conflict (stage_event_id, accounting_scope, call_sequence_no) do update
         set ingestion_run_id = excluded.ingestion_run_id,
             stage = excluded.stage,
             workspace_id = excluded.workspace_id,
             library_id = excluded.library_id,
             provider_kind = excluded.provider_kind,
             model_name = excluded.model_name,
             capability = excluded.capability,
             billing_unit = excluded.billing_unit,
             accounting_scope = excluded.accounting_scope,
             call_sequence_no = excluded.call_sequence_no,
             pricing_catalog_entry_id = excluded.pricing_catalog_entry_id,
             pricing_status = excluded.pricing_status,
             estimated_cost = excluded.estimated_cost,
             currency = excluded.currency,
             token_usage_json = excluded.token_usage_json,
             pricing_snapshot_json = excluded.pricing_snapshot_json
         returning id, ingestion_run_id, stage_event_id, stage, accounting_scope, call_sequence_no, workspace_id, library_id,
            provider_kind, model_name, capability, billing_unit, pricing_catalog_entry_id,
            pricing_status, estimated_cost, currency, token_usage_json, pricing_snapshot_json,
            created_at",
    )
    .bind(Uuid::now_v7())
    .bind(normalized.ingestion_run_id)
    .bind(normalized.stage_event_id)
    .bind(&normalized.stage)
    .bind(normalized.workspace_id)
    .bind(normalized.library_id)
    .bind(normalized.provider_kind.as_deref())
    .bind(normalized.model_name.as_deref())
    .bind(&normalized.capability)
    .bind(&normalized.billing_unit)
    .bind(&normalized.accounting_scope)
    .bind(normalized.call_sequence_no)
    .bind(normalized.pricing_catalog_entry_id)
    .bind(&normalized.pricing_status)
    .bind(normalized.estimated_cost)
    .bind(normalized.currency.as_deref())
    .bind(normalized.token_usage_json.clone())
    .bind(normalized.pricing_snapshot_json.clone())
    .fetch_one(pool)
    .await
}

/// Lists stage-accounting rows for one runtime ingestion run.
///
/// # Errors
/// Returns any `SQLx` error raised while loading stage-accounting rows.
pub async fn list_attempt_stage_accounting_by_run(
    pool: &PgPool,
    ingestion_run_id: Uuid,
) -> Result<Vec<AttemptStageAccountingRow>, sqlx::Error> {
    sqlx::query_as::<_, AttemptStageAccountingRow>(
        "select id, ingestion_run_id, stage_event_id, stage, accounting_scope, call_sequence_no, workspace_id, library_id, provider_kind,
            model_name, capability, billing_unit, pricing_catalog_entry_id, pricing_status,
            estimated_cost, currency, token_usage_json, pricing_snapshot_json, created_at
         from runtime_attempt_stage_accounting
         where ingestion_run_id = $1
         order by created_at asc, accounting_scope asc, call_sequence_no asc, id asc",
    )
    .bind(ingestion_run_id)
    .fetch_all(pool)
    .await
}

/// Loads one stage-accounting row by its canonical provider/stage key.
///
/// # Errors
/// Returns any `SQLx` error raised while loading the stage-accounting row.
pub async fn get_attempt_stage_accounting_by_scope(
    pool: &PgPool,
    stage_event_id: Uuid,
    accounting_scope: &str,
    call_sequence_no: i32,
) -> Result<Option<AttemptStageAccountingRow>, sqlx::Error> {
    sqlx::query_as::<_, AttemptStageAccountingRow>(
        "select id, ingestion_run_id, stage_event_id, stage, accounting_scope, call_sequence_no, workspace_id, library_id, provider_kind,
            model_name, capability, billing_unit, pricing_catalog_entry_id, pricing_status,
            estimated_cost, currency, token_usage_json, pricing_snapshot_json, created_at
         from runtime_attempt_stage_accounting
         where stage_event_id = $1
           and accounting_scope = $2
           and call_sequence_no = $3",
    )
    .bind(stage_event_id)
    .bind(accounting_scope)
    .bind(call_sequence_no)
    .fetch_optional(pool)
    .await
}

/// Recomputes and persists one attempt cost summary.
///
/// # Errors
/// Returns any `SQLx` error raised while refreshing the attempt cost summary.
pub async fn refresh_attempt_stage_cost_summary(
    pool: &PgPool,
    ingestion_run_id: Uuid,
) -> Result<AttemptStageCostSummaryRow, sqlx::Error> {
    sqlx::query_as::<_, AttemptStageCostSummaryRow>(
        "insert into runtime_attempt_cost_summary (
            ingestion_run_id, total_estimated_cost, settled_estimated_cost, in_flight_estimated_cost,
            currency, priced_stage_count, unpriced_stage_count, in_flight_stage_count,
            missing_stage_count, accounting_status, computed_at
         )
         with current_attempt as (
            select id as ingestion_run_id, current_attempt_no
            from runtime_ingestion_run
            where id = $1
         ),
         billable_stages as (
            select distinct stage_event.ingestion_run_id, stage_event.stage
            from runtime_ingestion_stage_event as stage_event
            join current_attempt
              on current_attempt.ingestion_run_id = stage_event.ingestion_run_id
             and current_attempt.current_attempt_no = stage_event.attempt_no
            where stage_event.stage in ('extracting_content', 'embedding_chunks', 'extracting_graph')
         ),
         stage_rollups as (
            select
                accounting.ingestion_run_id,
                accounting.stage,
                max(accounting.estimated_cost) as estimated_cost,
                max(accounting.currency) as currency,
                (array_agg(accounting.pricing_status order by accounting.created_at desc))[1] as pricing_status
            from runtime_attempt_stage_accounting as accounting
            join runtime_ingestion_stage_event as stage_event
              on stage_event.id = accounting.stage_event_id
            join current_attempt
              on current_attempt.ingestion_run_id = accounting.ingestion_run_id
             and current_attempt.current_attempt_no = stage_event.attempt_no
            where accounting.ingestion_run_id = $1
              and accounting.accounting_scope = 'stage_rollup'
              and accounting.stage in ('extracting_content', 'embedding_chunks', 'extracting_graph')
            group by accounting.ingestion_run_id, accounting.stage
         ),
         provider_calls as (
            select
                accounting.ingestion_run_id,
                accounting.stage,
                sum(accounting.estimated_cost) as estimated_cost,
                max(accounting.currency) as currency,
                count(*) filter (where accounting.pricing_status = 'priced')::integer as priced_call_count,
                count(*) filter (where accounting.pricing_status <> 'priced')::integer as unpriced_call_count
            from runtime_attempt_stage_accounting as accounting
            join runtime_ingestion_stage_event as stage_event
              on stage_event.id = accounting.stage_event_id
            join current_attempt
              on current_attempt.ingestion_run_id = accounting.ingestion_run_id
             and current_attempt.current_attempt_no = stage_event.attempt_no
            where accounting.ingestion_run_id = $1
              and accounting.accounting_scope = 'provider_call'
              and accounting.stage in ('extracting_content', 'embedding_chunks', 'extracting_graph')
            group by accounting.ingestion_run_id, accounting.stage
         ),
         resolved_stage_accounting as (
            select
                billable_stages.ingestion_run_id,
                billable_stages.stage,
                case
                    when stage_rollups.stage is not null then 'stage_rollup'
                    when provider_calls.stage is not null then 'provider_call'
                    else 'missing'
                end as accounting_scope,
                coalesce(stage_rollups.estimated_cost, provider_calls.estimated_cost) as estimated_cost,
                coalesce(stage_rollups.currency, provider_calls.currency) as currency,
                case
                    when stage_rollups.stage is not null then stage_rollups.pricing_status
                    when provider_calls.stage is not null
                     and provider_calls.priced_call_count > 0
                     and provider_calls.unpriced_call_count = 0 then 'priced'
                    when provider_calls.stage is not null
                     and provider_calls.priced_call_count > 0 then 'partial'
                    when provider_calls.stage is not null then 'unpriced'
                    else 'unpriced'
                end as pricing_status
            from billable_stages
            left join stage_rollups
              on stage_rollups.ingestion_run_id = billable_stages.ingestion_run_id
             and stage_rollups.stage = billable_stages.stage
            left join provider_calls
              on provider_calls.ingestion_run_id = billable_stages.ingestion_run_id
             and provider_calls.stage = billable_stages.stage
         )
         select
            $1,
            sum(resolved_stage_accounting.estimated_cost) as total_estimated_cost,
            sum(resolved_stage_accounting.estimated_cost) filter (where resolved_stage_accounting.accounting_scope = 'stage_rollup') as settled_estimated_cost,
            sum(resolved_stage_accounting.estimated_cost) filter (where resolved_stage_accounting.accounting_scope = 'provider_call') as in_flight_estimated_cost,
            max(resolved_stage_accounting.currency) as currency,
            count(*) filter (
                where resolved_stage_accounting.accounting_scope = 'stage_rollup'
                  and resolved_stage_accounting.pricing_status = 'priced'
            )::integer as priced_stage_count,
            count(*) filter (
                where resolved_stage_accounting.accounting_scope = 'stage_rollup'
                  and resolved_stage_accounting.pricing_status <> 'priced'
            )::integer as unpriced_stage_count,
            count(*) filter (where resolved_stage_accounting.accounting_scope = 'provider_call')::integer as in_flight_stage_count,
            count(*) filter (where resolved_stage_accounting.accounting_scope = 'missing')::integer as missing_stage_count,
            case
                when count(*) filter (where resolved_stage_accounting.accounting_scope = 'provider_call') > 0
                    then 'in_flight_unsettled'
                when count(*) filter (
                    where resolved_stage_accounting.accounting_scope = 'stage_rollup'
                      and resolved_stage_accounting.pricing_status = 'priced'
                ) > 0
                 and count(*) filter (
                    where resolved_stage_accounting.accounting_scope <> 'stage_rollup'
                       or resolved_stage_accounting.pricing_status <> 'priced'
                ) = 0 then 'priced'
                when count(*) filter (where resolved_stage_accounting.accounting_scope = 'stage_rollup') > 0
                    then 'partial'
                else 'unpriced'
            end as accounting_status,
            now()
         from resolved_stage_accounting
         on conflict (ingestion_run_id) do update
         set total_estimated_cost = excluded.total_estimated_cost,
             settled_estimated_cost = excluded.settled_estimated_cost,
             in_flight_estimated_cost = excluded.in_flight_estimated_cost,
             currency = excluded.currency,
             priced_stage_count = excluded.priced_stage_count,
             unpriced_stage_count = excluded.unpriced_stage_count,
             in_flight_stage_count = excluded.in_flight_stage_count,
             missing_stage_count = excluded.missing_stage_count,
             accounting_status = excluded.accounting_status,
             computed_at = excluded.computed_at
         returning ingestion_run_id, total_estimated_cost, settled_estimated_cost, in_flight_estimated_cost, currency, priced_stage_count,
            unpriced_stage_count, in_flight_stage_count, missing_stage_count, accounting_status, computed_at",
    )
    .bind(ingestion_run_id)
    .fetch_one(pool)
    .await
}

/// Loads the persisted latest-attempt cost summary for one runtime ingestion run.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the attempt summary row.
pub async fn get_attempt_stage_cost_summary_by_run(
    pool: &PgPool,
    ingestion_run_id: Uuid,
) -> Result<Option<AttemptStageCostSummaryRow>, sqlx::Error> {
    sqlx::query_as::<_, AttemptStageCostSummaryRow>(
        "select ingestion_run_id, total_estimated_cost, settled_estimated_cost, in_flight_estimated_cost, currency, priced_stage_count,
            unpriced_stage_count, in_flight_stage_count, missing_stage_count, accounting_status, computed_at
         from runtime_attempt_cost_summary
         where ingestion_run_id = $1",
    )
    .bind(ingestion_run_id)
    .fetch_optional(pool)
    .await
}

/// Lists resolved current-attempt billable accounting rows for one library.
///
/// This returns at most one logical row per ingestion run and billable stage, preferring a
/// settled `stage_rollup` when present and otherwise aggregating in-flight `provider_call` rows.
///
/// # Errors
/// Returns any `SQLx` error raised while loading the collection accounting rows.
pub async fn list_runtime_collection_resolved_stage_accounting(
    pool: &PgPool,
    library_id: Uuid,
) -> Result<Vec<RuntimeCollectionResolvedStageAccountingRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeCollectionResolvedStageAccountingRow>(
        "with current_runs as (
            select id as ingestion_run_id, file_type, current_attempt_no
            from runtime_ingestion_run
            where library_id = $1
         ),
         billable_stages as (
            select distinct current_runs.ingestion_run_id, current_runs.file_type, stage_event.stage
            from runtime_ingestion_stage_event as stage_event
            join current_runs
              on current_runs.ingestion_run_id = stage_event.ingestion_run_id
             and current_runs.current_attempt_no = stage_event.attempt_no
            where stage_event.stage in ('extracting_content', 'embedding_chunks', 'extracting_graph')
         ),
         stage_rollups as (
            select
                accounting.ingestion_run_id,
                current_runs.file_type,
                accounting.stage,
                max(accounting.estimated_cost) as estimated_cost,
                max(accounting.currency) as currency,
                (array_agg(accounting.pricing_status order by accounting.created_at desc))[1] as pricing_status,
                max(coalesce(nullif(accounting.token_usage_json ->> 'prompt_tokens', '')::bigint, 0))::bigint as prompt_tokens,
                max(coalesce(nullif(accounting.token_usage_json ->> 'completion_tokens', '')::bigint, 0))::bigint as completion_tokens,
                max(coalesce(nullif(accounting.token_usage_json ->> 'total_tokens', '')::bigint, 0))::bigint as total_tokens
            from runtime_attempt_stage_accounting as accounting
            join runtime_ingestion_stage_event as stage_event
              on stage_event.id = accounting.stage_event_id
            join current_runs
              on current_runs.ingestion_run_id = accounting.ingestion_run_id
             and current_runs.current_attempt_no = stage_event.attempt_no
            where accounting.accounting_scope = 'stage_rollup'
              and accounting.stage in ('extracting_content', 'embedding_chunks', 'extracting_graph')
            group by accounting.ingestion_run_id, current_runs.file_type, accounting.stage
         ),
         provider_calls as (
            select
                accounting.ingestion_run_id,
                current_runs.file_type,
                accounting.stage,
                sum(accounting.estimated_cost) as estimated_cost,
                max(accounting.currency) as currency,
                count(*) filter (where accounting.pricing_status = 'priced')::integer as priced_call_count,
                count(*) filter (where accounting.pricing_status <> 'priced')::integer as unpriced_call_count,
                sum(coalesce(nullif(accounting.token_usage_json ->> 'prompt_tokens', '')::bigint, 0))::bigint as prompt_tokens,
                sum(coalesce(nullif(accounting.token_usage_json ->> 'completion_tokens', '')::bigint, 0))::bigint as completion_tokens,
                sum(coalesce(nullif(accounting.token_usage_json ->> 'total_tokens', '')::bigint, 0))::bigint as total_tokens
            from runtime_attempt_stage_accounting as accounting
            join runtime_ingestion_stage_event as stage_event
              on stage_event.id = accounting.stage_event_id
            join current_runs
              on current_runs.ingestion_run_id = accounting.ingestion_run_id
             and current_runs.current_attempt_no = stage_event.attempt_no
            where accounting.accounting_scope = 'provider_call'
              and accounting.stage in ('extracting_content', 'embedding_chunks', 'extracting_graph')
            group by accounting.ingestion_run_id, current_runs.file_type, accounting.stage
         )
         select
            billable_stages.ingestion_run_id,
            billable_stages.file_type,
            billable_stages.stage,
            case
                when stage_rollups.stage is not null then 'stage_rollup'
                when provider_calls.stage is not null then 'provider_call'
                else 'missing'
            end as accounting_scope,
            case
                when stage_rollups.stage is not null then stage_rollups.pricing_status
                when provider_calls.stage is not null
                 and provider_calls.priced_call_count > 0
                 and provider_calls.unpriced_call_count = 0 then 'priced'
                when provider_calls.stage is not null
                 and provider_calls.priced_call_count > 0 then 'partial'
                when provider_calls.stage is not null then 'unpriced'
                else 'unpriced'
            end as pricing_status,
            coalesce(stage_rollups.estimated_cost, provider_calls.estimated_cost) as estimated_cost,
            coalesce(stage_rollups.currency, provider_calls.currency) as currency,
            coalesce(stage_rollups.prompt_tokens, provider_calls.prompt_tokens, 0)::bigint as prompt_tokens,
            coalesce(stage_rollups.completion_tokens, provider_calls.completion_tokens, 0)::bigint as completion_tokens,
            coalesce(stage_rollups.total_tokens, provider_calls.total_tokens, 0)::bigint as total_tokens
         from billable_stages
         left join stage_rollups
           on stage_rollups.ingestion_run_id = billable_stages.ingestion_run_id
          and stage_rollups.file_type = billable_stages.file_type
          and stage_rollups.stage = billable_stages.stage
         left join provider_calls
           on provider_calls.ingestion_run_id = billable_stages.ingestion_run_id
          and provider_calls.file_type = billable_stages.file_type
          and provider_calls.stage = billable_stages.stage
         order by billable_stages.file_type asc, billable_stages.ingestion_run_id asc, billable_stages.stage asc",
    )
    .bind(library_id)
    .fetch_all(pool)
    .await
}

/// Loads milestone and backlog counters for one library's current runtime collection state.
///
/// # Errors
/// Returns any `SQLx` error raised while loading the collection progress rollup.
pub async fn load_runtime_collection_progress_rollup(
    pool: &PgPool,
    library_id: Uuid,
) -> Result<RuntimeCollectionProgressRollupRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeCollectionProgressRollupRow>(
        "with current_runs as (
            select
                run.id as ingestion_run_id,
                run.document_id,
                run.status,
                run.current_stage,
                run.current_attempt_no
            from runtime_ingestion_run as run
            where run.library_id = $1
         ),
         extracted as (
            select distinct extraction.ingestion_run_id
            from runtime_extracted_content as extraction
            join current_runs
              on current_runs.ingestion_run_id = extraction.ingestion_run_id
         ),
         latest_stage_status as (
            select ingestion_run_id, stage, status
            from (
                select
                    stage_event.ingestion_run_id,
                    stage_event.stage,
                    stage_event.status,
                    row_number() over (
                        partition by stage_event.ingestion_run_id, stage_event.stage
                        order by stage_event.created_at desc, stage_event.id desc
                    ) as status_rank
                from runtime_ingestion_stage_event as stage_event
                join current_runs
                  on current_runs.ingestion_run_id = stage_event.ingestion_run_id
                 and current_runs.current_attempt_no = stage_event.attempt_no
            ) as ranked_stage_status
            where status_rank = 1
         ),
         stage_flags as (
            select
                latest_stage_status.ingestion_run_id,
                bool_or(
                    latest_stage_status.stage = 'extracting_content'
                    and latest_stage_status.status = 'completed'
                ) as content_extracted_complete,
                bool_or(
                    latest_stage_status.stage = 'chunking'
                    and latest_stage_status.status = 'completed'
                ) as chunking_complete,
                bool_or(
                    latest_stage_status.stage = 'embedding_chunks'
                    and latest_stage_status.status = 'completed'
                ) as embedding_complete,
                bool_or(
                    latest_stage_status.stage = 'extracting_graph'
                    and latest_stage_status.status = 'completed'
                ) as graph_ready_complete
            from latest_stage_status
            group by latest_stage_status.ingestion_run_id
         ),
         contribution as (
            select
                current_runs.ingestion_run_id,
                summary.chunk_count
            from current_runs
            left join runtime_document_contribution_summary as summary
              on summary.document_id = current_runs.document_id
         )
         select
            count(*)::bigint as accepted_count,
            count(*) filter (
                where extracted.ingestion_run_id is not null
                   or coalesce(stage_flags.content_extracted_complete, false)
            )::bigint as content_extracted_count,
            count(*) filter (
                where coalesce(contribution.chunk_count, 0) > 0
                   or coalesce(stage_flags.chunking_complete, false)
            )::bigint as chunked_count,
            count(*) filter (
                where coalesce(stage_flags.embedding_complete, false)
                   or current_runs.current_stage in (
                        'extracting_graph',
                        'merging_graph',
                        'projecting_graph',
                        'finalizing'
                   )
                   or current_runs.status in ('ready', 'ready_no_graph')
            )::bigint as embedded_count,
            count(*) filter (
                where current_runs.status = 'processing'
                  and current_runs.current_stage = 'extracting_graph'
            )::bigint as extracting_graph_count,
            count(*) filter (
                where coalesce(stage_flags.graph_ready_complete, false)
                   or current_runs.current_stage in (
                        'merging_graph',
                        'projecting_graph',
                        'finalizing'
                   )
                   or current_runs.status in ('ready', 'ready_no_graph')
            )::bigint as graph_ready_count,
            count(*) filter (where current_runs.status = 'ready')::bigint as ready_count,
            count(*) filter (where current_runs.status = 'failed')::bigint as failed_count,
            count(*) filter (where current_runs.status = 'queued')::bigint as queue_backlog_count,
            count(*) filter (where current_runs.status = 'processing')::bigint as processing_backlog_count
         from current_runs
         left join extracted
           on extracted.ingestion_run_id = current_runs.ingestion_run_id
         left join stage_flags
           on stage_flags.ingestion_run_id = current_runs.ingestion_run_id
         left join contribution
           on contribution.ingestion_run_id = current_runs.ingestion_run_id",
    )
    .bind(library_id)
    .fetch_one(pool)
    .await
}

/// Lists elapsed-time and status rollups for current-attempt stage events in one library.
///
/// # Errors
/// Returns any `SQLx` error raised while loading the collection stage rollups.
pub async fn list_runtime_collection_stage_rollups(
    pool: &PgPool,
    library_id: Uuid,
) -> Result<Vec<RuntimeCollectionStageRollupRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeCollectionStageRollupRow>(
        "with current_runs as (
            select id as ingestion_run_id, current_attempt_no
            from runtime_ingestion_run
            where library_id = $1
         ),
         latest_stage_status as (
            select stage, status, elapsed_ms
            from (
                select
                    stage_event.ingestion_run_id,
                    stage_event.stage,
                    stage_event.status,
                    stage_event.elapsed_ms,
                    row_number() over (
                        partition by stage_event.ingestion_run_id, stage_event.stage
                        order by stage_event.created_at desc, stage_event.id desc
                    ) as status_rank
                from runtime_ingestion_stage_event as stage_event
                join current_runs
                  on current_runs.ingestion_run_id = stage_event.ingestion_run_id
                 and current_runs.current_attempt_no = stage_event.attempt_no
            ) as ranked_stage_status
            where status_rank = 1
         )
         select
            latest_stage_status.stage,
            count(*) filter (where latest_stage_status.status = 'started')::bigint as active_count,
            count(*) filter (where latest_stage_status.status = 'completed')::bigint as completed_count,
            count(*) filter (where latest_stage_status.status = 'failed')::bigint as failed_count,
            (
                avg(latest_stage_status.elapsed_ms) filter (
                where latest_stage_status.status in ('completed', 'failed')
                  and latest_stage_status.elapsed_ms is not null
                )
            )::bigint as avg_elapsed_ms,
            max(latest_stage_status.elapsed_ms) filter (
                where latest_stage_status.status in ('completed', 'failed')
            ) as max_elapsed_ms
         from latest_stage_status
         group by latest_stage_status.stage
         order by latest_stage_status.stage asc",
    )
    .bind(library_id)
    .fetch_all(pool)
    .await
}

/// Lists per-format progress, backlog, and elapsed-time rollups for one library's current runs.
///
/// # Errors
/// Returns any `SQLx` error raised while loading the collection format diagnostics.
pub async fn list_runtime_collection_format_rollups(
    pool: &PgPool,
    library_id: Uuid,
) -> Result<Vec<RuntimeCollectionFormatRollupRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeCollectionFormatRollupRow>(
        "with current_runs as (
            select
                run.id as ingestion_run_id,
                run.document_id,
                run.file_type,
                run.status,
                run.current_stage,
                run.current_attempt_no,
                run.queue_elapsed_ms,
                run.total_elapsed_ms
            from runtime_ingestion_run as run
            where run.library_id = $1
         ),
         extracted as (
            select distinct extraction.ingestion_run_id
            from runtime_extracted_content as extraction
            join current_runs
              on current_runs.ingestion_run_id = extraction.ingestion_run_id
         ),
         latest_stage_status as (
            select ingestion_run_id, file_type, stage, status, elapsed_ms
            from (
                select
                    stage_event.ingestion_run_id,
                    current_runs.file_type,
                    stage_event.stage,
                    stage_event.status,
                    stage_event.elapsed_ms,
                    row_number() over (
                        partition by stage_event.ingestion_run_id, stage_event.stage
                        order by stage_event.created_at desc, stage_event.id desc
                    ) as status_rank
                from runtime_ingestion_stage_event as stage_event
                join current_runs
                  on current_runs.ingestion_run_id = stage_event.ingestion_run_id
                 and current_runs.current_attempt_no = stage_event.attempt_no
            ) as ranked_stage_status
            where status_rank = 1
         ),
         stage_flags as (
            select
                latest_stage_status.ingestion_run_id,
                bool_or(
                    latest_stage_status.stage = 'extracting_content'
                    and latest_stage_status.status = 'completed'
                ) as content_extracted_complete,
                bool_or(
                    latest_stage_status.stage = 'chunking'
                    and latest_stage_status.status = 'completed'
                ) as chunking_complete,
                bool_or(
                    latest_stage_status.stage = 'embedding_chunks'
                    and latest_stage_status.status = 'completed'
                ) as embedding_complete,
                bool_or(
                    latest_stage_status.stage = 'extracting_graph'
                    and latest_stage_status.status = 'completed'
                ) as graph_ready_complete
            from latest_stage_status
            group by latest_stage_status.ingestion_run_id
         ),
         contribution as (
            select
                current_runs.ingestion_run_id,
                summary.chunk_count
            from current_runs
            left join runtime_document_contribution_summary as summary
              on summary.document_id = current_runs.document_id
         ),
         format_stage_elapsed as (
            select
                latest_stage_status.file_type,
                latest_stage_status.stage,
                (
                    avg(latest_stage_status.elapsed_ms) filter (
                    where latest_stage_status.status in ('completed', 'failed')
                      and latest_stage_status.elapsed_ms is not null
                    )
                )::bigint as avg_elapsed_ms,
                max(latest_stage_status.elapsed_ms) filter (
                    where latest_stage_status.status in ('completed', 'failed')
                ) as max_elapsed_ms
            from latest_stage_status
            group by latest_stage_status.file_type, latest_stage_status.stage
         ),
         ranked_format_bottleneck as (
            select
                format_stage_elapsed.file_type,
                format_stage_elapsed.stage,
                format_stage_elapsed.avg_elapsed_ms,
                format_stage_elapsed.max_elapsed_ms,
                row_number() over (
                    partition by format_stage_elapsed.file_type
                    order by
                        format_stage_elapsed.avg_elapsed_ms desc nulls last,
                        format_stage_elapsed.max_elapsed_ms desc nulls last,
                        format_stage_elapsed.stage asc
                ) as bottleneck_rank
            from format_stage_elapsed
         )
         select
            current_runs.file_type,
            count(*)::bigint as document_count,
            count(*) filter (where current_runs.status = 'queued')::bigint as queued_count,
            count(*) filter (where current_runs.status = 'processing')::bigint as processing_count,
            count(*) filter (where current_runs.status = 'ready')::bigint as ready_count,
            count(*) filter (where current_runs.status = 'ready_no_graph')::bigint as ready_no_graph_count,
            count(*) filter (where current_runs.status = 'failed')::bigint as failed_count,
            count(*) filter (
                where extracted.ingestion_run_id is not null
                   or coalesce(stage_flags.content_extracted_complete, false)
            )::bigint as content_extracted_count,
            count(*) filter (
                where coalesce(contribution.chunk_count, 0) > 0
                   or coalesce(stage_flags.chunking_complete, false)
            )::bigint as chunked_count,
            count(*) filter (
                where coalesce(stage_flags.embedding_complete, false)
                   or current_runs.current_stage in (
                        'extracting_graph',
                        'merging_graph',
                        'projecting_graph',
                        'finalizing'
                   )
                   or current_runs.status in ('ready', 'ready_no_graph')
            )::bigint as embedded_count,
            count(*) filter (
                where current_runs.status = 'processing'
                  and current_runs.current_stage = 'extracting_graph'
            )::bigint as extracting_graph_count,
            count(*) filter (
                where coalesce(stage_flags.graph_ready_complete, false)
                   or current_runs.current_stage in (
                        'merging_graph',
                        'projecting_graph',
                        'finalizing'
                   )
                   or current_runs.status in ('ready', 'ready_no_graph')
            )::bigint as graph_ready_count,
            (
                avg(current_runs.queue_elapsed_ms) filter (
                    where current_runs.queue_elapsed_ms is not null
                )
            )::bigint as avg_queue_elapsed_ms,
            max(current_runs.queue_elapsed_ms) as max_queue_elapsed_ms,
            (
                avg(current_runs.total_elapsed_ms) filter (
                    where current_runs.total_elapsed_ms is not null
                )
            )::bigint as avg_total_elapsed_ms,
            max(current_runs.total_elapsed_ms) as max_total_elapsed_ms,
            ranked_format_bottleneck.stage as bottleneck_stage,
            ranked_format_bottleneck.avg_elapsed_ms as bottleneck_avg_elapsed_ms,
            ranked_format_bottleneck.max_elapsed_ms as bottleneck_max_elapsed_ms
         from current_runs
         left join extracted
           on extracted.ingestion_run_id = current_runs.ingestion_run_id
         left join stage_flags
           on stage_flags.ingestion_run_id = current_runs.ingestion_run_id
         left join contribution
           on contribution.ingestion_run_id = current_runs.ingestion_run_id
         left join ranked_format_bottleneck
           on ranked_format_bottleneck.file_type = current_runs.file_type
          and ranked_format_bottleneck.bottleneck_rank = 1
         group by
            current_runs.file_type,
            ranked_format_bottleneck.stage,
            ranked_format_bottleneck.avg_elapsed_ms,
            ranked_format_bottleneck.max_elapsed_ms
         order by current_runs.file_type asc",
    )
    .bind(library_id)
    .fetch_all(pool)
    .await
}

/// Refreshes the visible progress marker for one queue slice.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the progress marker.
pub async fn refresh_runtime_library_queue_slice_activity(
    pool: &PgPool,
    library_id: Uuid,
    workspace_id: Uuid,
    last_progress_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into runtime_library_queue_slice (
            library_id, workspace_id, last_progress_at
         ) values ($1, $2, $3)
         on conflict (library_id) do update
         set workspace_id = excluded.workspace_id,
             last_progress_at = excluded.last_progress_at,
             updated_at = now()",
    )
    .bind(library_id)
    .bind(workspace_id)
    .bind(last_progress_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Acquires a library-scoped PostgreSQL advisory lock for canonical graph serialization.
///
/// The returned pooled connection keeps the session lock alive until
/// `release_runtime_library_graph_lock` is called.
///
/// # Errors
/// Returns any `SQLx` error raised while acquiring the connection or advisory lock.
pub async fn acquire_runtime_library_graph_lock(
    pool: &PgPool,
    library_id: Uuid,
) -> Result<sqlx::pool::PoolConnection<Postgres>, sqlx::Error> {
    let mut connection = pool.acquire().await?;
    sqlx::query("select pg_advisory_lock(hashtextextended($1::text, 0))")
        .bind(library_id.to_string())
        .execute(&mut *connection)
        .await?;
    Ok(connection)
}

/// Releases a library-scoped PostgreSQL advisory lock for canonical graph serialization.
///
/// # Errors
/// Returns any `SQLx` error raised while unlocking the advisory key.
pub async fn release_runtime_library_graph_lock(
    mut connection: sqlx::pool::PoolConnection<Postgres>,
    library_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("select pg_advisory_unlock(hashtextextended($1::text, 0))")
        .bind(library_id.to_string())
        .execute(&mut *connection)
        .await?;
    Ok(())
}

/// Loads provider failure classification metadata captured for one graph-extraction attempt.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the checkpoint row.
pub async fn load_runtime_provider_failure_snapshot(
    pool: &PgPool,
    ingestion_run_id: Uuid,
    attempt_no: i32,
) -> Result<Option<RuntimeProviderFailureSnapshotRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeProviderFailureSnapshotRow>(
        "select
            ingestion_run_id,
            attempt_no,
            provider_failure_class,
            request_shape_key,
            request_size_bytes,
            upstream_status,
            retry_outcome,
            computed_at
         from runtime_graph_progress_checkpoint
         where ingestion_run_id = $1
           and attempt_no = $2
           and provider_failure_class is not null",
    )
    .bind(ingestion_run_id)
    .bind(attempt_no)
    .fetch_optional(pool)
    .await
}

/// Persists provider failure classification metadata onto the active graph-progress checkpoint row.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the checkpoint row.
pub async fn record_runtime_graph_progress_failure_classification(
    pool: &PgPool,
    ingestion_run_id: Uuid,
    attempt_no: i32,
    provider_failure_class: Option<&str>,
    request_shape_key: Option<&str>,
    request_size_bytes: Option<i64>,
    upstream_status: Option<&str>,
    retry_outcome: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update runtime_graph_progress_checkpoint
         set provider_failure_class = $3,
             request_shape_key = $4,
             request_size_bytes = $5,
             upstream_status = $6,
             retry_outcome = $7,
             diagnostics_snapshot_at = now()
         where ingestion_run_id = $1
           and attempt_no = $2",
    )
    .bind(ingestion_run_id)
    .bind(attempt_no)
    .bind(provider_failure_class)
    .bind(request_shape_key)
    .bind(request_size_bytes)
    .bind(upstream_status)
    .bind(retry_outcome)
    .execute(pool)
    .await?;
    Ok(())
}

/// Lists model pricing catalog entries.
///
/// # Errors
/// Returns any `SQLx` error raised while loading pricing entries.
pub async fn list_model_pricing_catalog_entries(
    pool: &PgPool,
    workspace_id: Option<Uuid>,
) -> Result<Vec<ModelPricingCatalogEntryRow>, sqlx::Error> {
    match workspace_id {
        Some(workspace_id) => {
            sqlx::query_as::<_, ModelPricingCatalogEntryRow>(
                "select id, workspace_id, provider_kind, model_name, capability, billing_unit,
                    input_price, output_price, currency, status, source_kind, note, effective_from,
                    effective_to, created_at, updated_at
                 from model_pricing_catalog
                 where workspace_id = $1
                 order by effective_from desc, created_at desc",
            )
            .bind(workspace_id)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, ModelPricingCatalogEntryRow>(
                "select id, workspace_id, provider_kind, model_name, capability, billing_unit,
                    input_price, output_price, currency, status, source_kind, note, effective_from,
                    effective_to, created_at, updated_at
                 from model_pricing_catalog
                 order by effective_from desc, created_at desc",
            )
            .fetch_all(pool)
            .await
        }
    }
}

/// Loads one pricing catalog entry by primary key.
///
/// # Errors
/// Returns any `SQLx` error raised while querying one pricing row.
pub async fn get_model_pricing_catalog_entry_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<ModelPricingCatalogEntryRow>, sqlx::Error> {
    sqlx::query_as::<_, ModelPricingCatalogEntryRow>(
        "select id, workspace_id, provider_kind, model_name, capability, billing_unit,
            input_price, output_price, currency, status, source_kind, note, effective_from,
            effective_to, created_at, updated_at
         from model_pricing_catalog
         where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Creates a model pricing catalog entry.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting a pricing entry.
pub async fn create_model_pricing_catalog_entry(
    pool: &PgPool,
    new_row: &NewModelPricingCatalogEntry,
) -> Result<ModelPricingCatalogEntryRow, sqlx::Error> {
    sqlx::query_as::<_, ModelPricingCatalogEntryRow>(
        "insert into model_pricing_catalog (
            id, workspace_id, provider_kind, model_name, capability, billing_unit, input_price,
            output_price, currency, status, source_kind, note, effective_from
         ) values (
            $1, $2, $3, $4, $5, $6, $7,
            $8, $9, 'active', $10, $11, $12
         )
         returning id, workspace_id, provider_kind, model_name, capability, billing_unit,
            input_price, output_price, currency, status, source_kind, note, effective_from,
            effective_to, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(new_row.workspace_id)
    .bind(&new_row.provider_kind)
    .bind(&new_row.model_name)
    .bind(&new_row.capability)
    .bind(&new_row.billing_unit)
    .bind(new_row.input_price)
    .bind(new_row.output_price)
    .bind(&new_row.currency)
    .bind(&new_row.source_kind)
    .bind(new_row.note.as_deref())
    .bind(new_row.effective_from)
    .fetch_one(pool)
    .await
}

/// Supersedes active pricing rows that overlap a new effective pricing window.
///
/// # Errors
/// Returns any `SQLx` error raised while updating overlapping pricing rows.
pub async fn supersede_overlapping_model_pricing_catalog_entries(
    pool: &PgPool,
    workspace_id: Option<Uuid>,
    provider_kind: &str,
    model_name: &str,
    capability: &str,
    billing_unit: &str,
    effective_from: DateTime<Utc>,
) -> Result<Vec<ModelPricingCatalogEntryRow>, sqlx::Error> {
    sqlx::query_as::<_, ModelPricingCatalogEntryRow>(
        "update model_pricing_catalog
         set status = 'superseded',
             effective_to = $6,
             updated_at = now()
         where (($1::uuid is null and workspace_id is null) or workspace_id = $1)
           and provider_kind = $2
           and model_name = $3
           and capability = $4
           and billing_unit = $5
           and status = 'active'
           and effective_from < $6
           and (effective_to is null or effective_to > $6)
         returning id, workspace_id, provider_kind, model_name, capability, billing_unit,
            input_price, output_price, currency, status, source_kind, note, effective_from,
            effective_to, created_at, updated_at",
    )
    .bind(workspace_id)
    .bind(provider_kind)
    .bind(model_name)
    .bind(capability)
    .bind(billing_unit)
    .bind(effective_from)
    .fetch_all(pool)
    .await
}

/// Updates an existing pricing row in place.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the pricing row.
pub async fn update_model_pricing_catalog_entry(
    pool: &PgPool,
    id: Uuid,
    updated_row: &UpdateModelPricingCatalogEntry,
) -> Result<Option<ModelPricingCatalogEntryRow>, sqlx::Error> {
    sqlx::query_as::<_, ModelPricingCatalogEntryRow>(
        "update model_pricing_catalog
         set workspace_id = $2,
             provider_kind = $3,
             model_name = $4,
             capability = $5,
             billing_unit = $6,
             input_price = $7,
             output_price = $8,
             currency = $9,
             note = $10,
             effective_from = $11,
             updated_at = now()
         where id = $1
         returning id, workspace_id, provider_kind, model_name, capability, billing_unit,
            input_price, output_price, currency, status, source_kind, note, effective_from,
            effective_to, created_at, updated_at",
    )
    .bind(id)
    .bind(updated_row.workspace_id)
    .bind(&updated_row.provider_kind)
    .bind(&updated_row.model_name)
    .bind(&updated_row.capability)
    .bind(&updated_row.billing_unit)
    .bind(updated_row.input_price)
    .bind(updated_row.output_price)
    .bind(&updated_row.currency)
    .bind(updated_row.note.as_deref())
    .bind(updated_row.effective_from)
    .fetch_optional(pool)
    .await
}

/// Deactivates a model pricing catalog entry.
///
/// # Errors
/// Returns any `SQLx` error raised while deactivating a pricing entry.
pub async fn deactivate_model_pricing_catalog_entry(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<ModelPricingCatalogEntryRow>, sqlx::Error> {
    sqlx::query_as::<_, ModelPricingCatalogEntryRow>(
        "update model_pricing_catalog
         set status = 'inactive',
             effective_to = coalesce(effective_to, greatest(now(), effective_from)),
             updated_at = now()
         where id = $1
         returning id, workspace_id, provider_kind, model_name, capability, billing_unit,
            input_price, output_price, currency, status, source_kind, note, effective_from,
            effective_to, created_at, updated_at",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Loads the effective model pricing catalog entry at a given point in time.
///
/// # Errors
/// Returns any `SQLx` error raised while resolving an effective price.
pub async fn get_effective_model_pricing_catalog_entry(
    pool: &PgPool,
    workspace_id: Option<Uuid>,
    provider_kind: &str,
    model_name: &str,
    capability: &str,
    billing_unit: &str,
    at: DateTime<Utc>,
) -> Result<Option<ModelPricingCatalogEntryRow>, sqlx::Error> {
    sqlx::query_as::<_, ModelPricingCatalogEntryRow>(
        "select id, workspace_id, provider_kind, model_name, capability, billing_unit,
            input_price, output_price, currency, status, source_kind, note, effective_from,
            effective_to, created_at, updated_at
         from model_pricing_catalog
         where (($1::uuid is null and workspace_id is null) or workspace_id = $1)
           and provider_kind = $2
           and model_name = $3
           and capability = $4
           and billing_unit = $5
           and status = 'active'
           and effective_from <= $6
           and (effective_to is null or effective_to > $6)
         order by effective_from desc, created_at desc
         limit 1",
    )
    .bind(workspace_id)
    .bind(provider_kind)
    .bind(model_name)
    .bind(capability)
    .bind(billing_unit)
    .bind(at)
    .fetch_optional(pool)
    .await
}

/// Loads the effective model pricing catalog entry as a resolution projection row.
///
/// # Errors
/// Returns any `SQLx` error raised while resolving the effective price row.
pub async fn resolve_model_pricing_catalog_entry(
    pool: &PgPool,
    workspace_id: Option<Uuid>,
    provider_kind: &str,
    model_name: &str,
    capability: &str,
    billing_unit: &str,
    at: DateTime<Utc>,
) -> Result<Option<ModelPricingResolutionRow>, sqlx::Error> {
    sqlx::query_as::<_, ModelPricingResolutionRow>(
        "select id as pricing_catalog_entry_id, workspace_id, provider_kind, model_name, capability,
            billing_unit, input_price, output_price, currency, status, source_kind, effective_from,
            effective_to
         from model_pricing_catalog
         where (($1::uuid is null and workspace_id is null) or workspace_id = $1)
           and provider_kind = $2
           and model_name = $3
           and capability = $4
           and billing_unit = $5
           and status = 'active'
           and effective_from <= $6
           and (effective_to is null or effective_to > $6)
         order by effective_from desc, created_at desc
         limit 1",
    )
    .bind(workspace_id)
    .bind(provider_kind)
    .bind(model_name)
    .bind(capability)
    .bind(billing_unit)
    .bind(at)
    .fetch_optional(pool)
    .await
}

/// Returns the current dedicated source-truth version for one library.
///
/// # Errors
/// Returns any `SQLx` error raised while loading the `catalog_library` row.
pub async fn get_library_source_truth_version(
    pool: &PgPool,
    library_id: Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "select coalesce(source_truth_version, 1) from catalog_library where id = $1",
    )
    .bind(library_id)
    .fetch_optional(pool)
    .await
    .map(|version| version.map_or(1, |value| value.max(1)))
}

/// Counts distinct filtered graph artifacts written for one ingestion attempt.
///
/// # Errors
/// Returns any `SQLx` error raised while querying filtered artifact rows.
pub async fn count_runtime_graph_filtered_artifacts_by_ingestion_run(
    pool: &PgPool,
    library_id: Uuid,
    ingestion_run_id: Uuid,
    revision_id: Option<Uuid>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "select count(distinct concat_ws(
                ':',
                coalesce(revision_id::text, 'none'),
                coalesce(ingestion_run_id::text, 'none'),
                target_kind,
                candidate_key,
                filter_reason
            ))
         from runtime_graph_filtered_artifact
         where library_id = $1
           and ingestion_run_id = $2
           and ($3::uuid is null or revision_id = $3)",
    )
    .bind(library_id)
    .bind(ingestion_run_id)
    .bind(revision_id)
    .fetch_one(pool)
    .await
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

/// Upserts an embedding target for a canonical graph node or relation.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting or updating the embedding target.
pub async fn upsert_runtime_vector_target(
    pool: &PgPool,
    library_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
    provider_kind: &str,
    model_name: &str,
    dimensions: Option<i32>,
    embedding_json: serde_json::Value,
) -> Result<RuntimeVectorTargetRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeVectorTargetRow>(
        "insert into runtime_vector_target (
            id, library_id, target_kind, target_id, provider_kind, model_name, dimensions, embedding_json
         ) values ($1, $2, $3, $4, $5, $6, $7, $8)
         on conflict (library_id, target_kind, target_id, provider_kind, model_name) do update
         set dimensions = excluded.dimensions,
             embedding_json = excluded.embedding_json,
             updated_at = now()
         returning id, library_id, target_kind, target_id, provider_kind, model_name,
            dimensions, embedding_json, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(library_id)
    .bind(target_kind)
    .bind(target_id)
    .bind(provider_kind)
    .bind(model_name)
    .bind(dimensions)
    .bind(embedding_json)
    .fetch_one(pool)
    .await
}

fn coalesce_runtime_vector_target_upserts(
    rows: &[RuntimeVectorTargetUpsertInput],
) -> Vec<RuntimeVectorTargetUpsertInput> {
    let mut deduped = BTreeMap::new();
    for row in rows {
        deduped.insert(
            (
                row.library_id,
                row.target_kind.clone(),
                row.target_id,
                row.provider_kind.clone(),
                row.model_name.clone(),
            ),
            row.clone(),
        );
    }
    deduped.into_values().collect()
}

/// Upserts many embedding targets for canonical graph nodes or relations.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting or updating the targets.
pub async fn upsert_runtime_vector_targets(
    pool: &PgPool,
    rows: &[RuntimeVectorTargetUpsertInput],
) -> Result<(), sqlx::Error> {
    let rows = coalesce_runtime_vector_target_upserts(rows);
    if rows.is_empty() {
        return Ok(());
    }

    let mut builder = QueryBuilder::<Postgres>::new(
        "insert into runtime_vector_target (
            id, library_id, target_kind, target_id, provider_kind, model_name, dimensions, embedding_json
         ) ",
    );
    builder.push_values(rows.iter(), |mut row_builder, row| {
        row_builder
            .push_bind(Uuid::now_v7())
            .push_bind(row.library_id)
            .push_bind(&row.target_kind)
            .push_bind(row.target_id)
            .push_bind(&row.provider_kind)
            .push_bind(&row.model_name)
            .push_bind(row.dimensions)
            .push_bind(&row.embedding_json);
    });
    builder.push(
        " on conflict (library_id, target_kind, target_id, provider_kind, model_name) do update
          set dimensions = excluded.dimensions,
              embedding_json = excluded.embedding_json,
              updated_at = now()
          where runtime_vector_target.dimensions is distinct from excluded.dimensions
             or runtime_vector_target.embedding_json is distinct from excluded.embedding_json",
    );
    builder.build().execute(pool).await?;
    Ok(())
}

/// Lists runtime vector targets for one library/kind/provider tuple.
///
/// # Errors
/// Returns any `SQLx` error raised while querying vector targets.
pub async fn list_runtime_vector_targets_by_library_and_kind(
    pool: &PgPool,
    library_id: Uuid,
    target_kind: &str,
    provider_kind: &str,
    model_name: &str,
) -> Result<Vec<RuntimeVectorTargetRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeVectorTargetRow>(
        "select id, library_id, target_kind, target_id, provider_kind, model_name,
            dimensions, embedding_json, created_at, updated_at
         from runtime_vector_target
         where library_id = $1
           and target_kind = $2
           and provider_kind = $3
           and model_name = $4
         order by updated_at desc",
    )
    .bind(library_id)
    .bind(target_kind)
    .bind(provider_kind)
    .bind(model_name)
    .fetch_all(pool)
    .await
}

/// Upserts the runtime provider profile for one library.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting or updating the provider profile.
pub async fn upsert_runtime_provider_profile(
    pool: &PgPool,
    library_id: Uuid,
    indexing_provider_kind: &str,
    indexing_model_name: &str,
    embedding_provider_kind: &str,
    embedding_model_name: &str,
    answer_provider_kind: &str,
    answer_model_name: &str,
    vision_provider_kind: &str,
    vision_model_name: &str,
) -> Result<RuntimeProviderProfileRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeProviderProfileRow>(
        "insert into runtime_provider_profile (
            library_id, indexing_provider_kind, indexing_model_name, embedding_provider_kind,
            embedding_model_name, answer_provider_kind, answer_model_name, vision_provider_kind,
            vision_model_name
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         on conflict (library_id) do update
         set indexing_provider_kind = excluded.indexing_provider_kind,
             indexing_model_name = excluded.indexing_model_name,
             embedding_provider_kind = excluded.embedding_provider_kind,
             embedding_model_name = excluded.embedding_model_name,
             answer_provider_kind = excluded.answer_provider_kind,
             answer_model_name = excluded.answer_model_name,
             vision_provider_kind = excluded.vision_provider_kind,
             vision_model_name = excluded.vision_model_name,
             last_validated_at = case
                 when runtime_provider_profile.indexing_provider_kind is distinct from excluded.indexing_provider_kind
                   or runtime_provider_profile.indexing_model_name is distinct from excluded.indexing_model_name
                   or runtime_provider_profile.embedding_provider_kind is distinct from excluded.embedding_provider_kind
                   or runtime_provider_profile.embedding_model_name is distinct from excluded.embedding_model_name
                   or runtime_provider_profile.answer_provider_kind is distinct from excluded.answer_provider_kind
                   or runtime_provider_profile.answer_model_name is distinct from excluded.answer_model_name
                   or runtime_provider_profile.vision_provider_kind is distinct from excluded.vision_provider_kind
                   or runtime_provider_profile.vision_model_name is distinct from excluded.vision_model_name
                 then null
                 else runtime_provider_profile.last_validated_at
             end,
             last_validation_status = case
                 when runtime_provider_profile.indexing_provider_kind is distinct from excluded.indexing_provider_kind
                   or runtime_provider_profile.indexing_model_name is distinct from excluded.indexing_model_name
                   or runtime_provider_profile.embedding_provider_kind is distinct from excluded.embedding_provider_kind
                   or runtime_provider_profile.embedding_model_name is distinct from excluded.embedding_model_name
                   or runtime_provider_profile.answer_provider_kind is distinct from excluded.answer_provider_kind
                   or runtime_provider_profile.answer_model_name is distinct from excluded.answer_model_name
                   or runtime_provider_profile.vision_provider_kind is distinct from excluded.vision_provider_kind
                   or runtime_provider_profile.vision_model_name is distinct from excluded.vision_model_name
                 then null
                 else runtime_provider_profile.last_validation_status
             end,
             last_validation_error = case
                 when runtime_provider_profile.indexing_provider_kind is distinct from excluded.indexing_provider_kind
                   or runtime_provider_profile.indexing_model_name is distinct from excluded.indexing_model_name
                   or runtime_provider_profile.embedding_provider_kind is distinct from excluded.embedding_provider_kind
                   or runtime_provider_profile.embedding_model_name is distinct from excluded.embedding_model_name
                   or runtime_provider_profile.answer_provider_kind is distinct from excluded.answer_provider_kind
                   or runtime_provider_profile.answer_model_name is distinct from excluded.answer_model_name
                   or runtime_provider_profile.vision_provider_kind is distinct from excluded.vision_provider_kind
                   or runtime_provider_profile.vision_model_name is distinct from excluded.vision_model_name
                 then null
                 else runtime_provider_profile.last_validation_error
             end,
             updated_at = now()
         returning library_id, indexing_provider_kind, indexing_model_name, embedding_provider_kind,
            embedding_model_name, answer_provider_kind, answer_model_name, vision_provider_kind,
            vision_model_name, last_validated_at, last_validation_status, last_validation_error,
            created_at, updated_at",
    )
    .bind(library_id)
    .bind(indexing_provider_kind)
    .bind(indexing_model_name)
    .bind(embedding_provider_kind)
    .bind(embedding_model_name)
    .bind(answer_provider_kind)
    .bind(answer_model_name)
    .bind(vision_provider_kind)
    .bind(vision_model_name)
    .fetch_one(pool)
    .await
}

/// Loads the runtime provider profile for one project.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the provider profile.
pub async fn get_runtime_provider_profile(
    pool: &PgPool,
    library_id: Uuid,
) -> Result<Option<RuntimeProviderProfileRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeProviderProfileRow>(
        "select library_id, indexing_provider_kind, indexing_model_name, embedding_provider_kind,
            embedding_model_name, answer_provider_kind, answer_model_name, vision_provider_kind,
            vision_model_name, last_validated_at, last_validation_status, last_validation_error,
            created_at, updated_at
         from runtime_provider_profile
         where library_id = $1",
    )
    .bind(library_id)
    .fetch_optional(pool)
    .await
}

/// Updates the last validation outcome for one runtime provider profile.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the provider profile.
pub async fn update_runtime_provider_profile_validation(
    pool: &PgPool,
    library_id: Uuid,
    status: &str,
    error_message: Option<&str>,
) -> Result<RuntimeProviderProfileRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeProviderProfileRow>(
        "update runtime_provider_profile
         set last_validated_at = now(),
             last_validation_status = $2,
             last_validation_error = $3,
             updated_at = now()
         where library_id = $1
         returning library_id, indexing_provider_kind, indexing_model_name, embedding_provider_kind,
            embedding_model_name, answer_provider_kind, answer_model_name, vision_provider_kind,
            vision_model_name, last_validated_at, last_validation_status, last_validation_error,
            created_at, updated_at",
    )
    .bind(library_id)
    .bind(status)
    .bind(error_message)
    .fetch_one(pool)
    .await
}

/// Appends a provider validation log entry.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the validation log.
pub async fn append_runtime_provider_validation_log(
    pool: &PgPool,
    library_id: Option<Uuid>,
    provider_kind: &str,
    model_name: &str,
    capability: &str,
    status: &str,
    error_message: Option<&str>,
) -> Result<RuntimeProviderValidationLogRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeProviderValidationLogRow>(
        "insert into runtime_provider_validation_log (
            id, library_id, provider_kind, model_name, capability, status, error_message
         ) values ($1, $2, $3, $4, $5, $6, $7)
         returning id, library_id, provider_kind, model_name, capability, status, error_message, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(library_id)
    .bind(provider_kind)
    .bind(model_name)
    .bind(capability)
    .bind(status)
    .bind(error_message)
    .fetch_one(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn runtime_vector_target_batch_coalesces_duplicate_keys_last_write_wins() {
        let library_id = Uuid::now_v7();
        let target_id = Uuid::now_v7();
        let rows = coalesce_runtime_vector_target_upserts(&[
            RuntimeVectorTargetUpsertInput {
                library_id,
                target_kind: "entity".to_string(),
                target_id,
                provider_kind: "openai".to_string(),
                model_name: "text-embedding-3-small".to_string(),
                dimensions: Some(1536),
                embedding_json: json!([0.1, 0.2]),
            },
            RuntimeVectorTargetUpsertInput {
                library_id,
                target_kind: "entity".to_string(),
                target_id,
                provider_kind: "openai".to_string(),
                model_name: "text-embedding-3-small".to_string(),
                dimensions: Some(1536),
                embedding_json: json!([0.9, 1.0]),
            },
        ]);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].embedding_json, json!([0.9, 1.0]));
    }
}
