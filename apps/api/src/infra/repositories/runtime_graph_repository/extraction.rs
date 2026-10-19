use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Acquire, Executor, FromRow, PgPool, Postgres};
use uuid::Uuid;

use crate::infra::knowledge_plane::{IngestAttemptMutationOutcome, IngestRevisionWriteAuthority};
use crate::shared::text_encoding::{
    escape_json_transport_non_ascii, json_contains_repairable_utf8_latin1_mojibake,
    repair_json_string_values, repair_utf8_latin1_mojibake,
};

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, utoipa::ToSchema)]
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

/// `PostgreSQL` JSONB rejects lone surrogate code points (`\uD800`–`\uDFFF`)
/// and other invalid Unicode escapes that some LLM providers emit.
///
/// Strategy: serialize the value to a JSON string, walk through it
/// character by character replacing `\uXXXX` sequences that encode
/// surrogate halves or noncharacters with `\uFFFD`, then re-parse.
fn sanitize_json_for_postgres(value: serde_json::Value) -> serde_json::Value {
    let text = value.to_string();
    let sanitized = replace_surrogate_unicode_escapes(&text);
    if sanitized == text {
        return value;
    }
    serde_json::from_str(&sanitized).unwrap_or(value)
}

fn sanitize_runtime_graph_normalized_json(value: serde_json::Value) -> serde_json::Value {
    let before_has_mojibake = json_contains_repairable_utf8_latin1_mojibake(&value);
    let recursively_repaired = repair_json_string_values(value.clone());
    let serialized = recursively_repaired.to_string();
    let serialized_repaired = repair_utf8_latin1_mojibake(&serialized);
    let repaired = if serialized_repaired == serialized {
        recursively_repaired
    } else {
        serde_json::from_str(&serialized_repaired).unwrap_or(recursively_repaired)
    };
    let after_has_mojibake = json_contains_repairable_utf8_latin1_mojibake(&repaired);
    let changed = repaired != value;
    if before_has_mojibake || after_has_mojibake || changed {
        tracing::warn!(
            before_has_mojibake,
            after_has_mojibake,
            changed,
            "runtime graph normalized JSON persistence encoding repair"
        );
    }
    sanitize_json_for_postgres(repaired)
}

fn sanitized_runtime_graph_normalized_json_text(value: serde_json::Value) -> String {
    escape_json_transport_non_ascii(&sanitize_runtime_graph_normalized_json(value).to_string())
}

fn sanitized_json_for_postgres_text(value: serde_json::Value) -> String {
    escape_json_transport_non_ascii(&sanitize_json_for_postgres(value).to_string())
}

/// Replace `\uXXXX` escape sequences for surrogate code points
/// (D800–DFFF) and noncharacters (FFFE–FFFF) with `\uFFFD`.
fn replace_surrogate_unicode_escapes(json: &str) -> String {
    let bytes = json.as_bytes();
    let mut out = String::with_capacity(json.len());
    let mut index = 0;
    while index < bytes.len() {
        if let Some(consumed) = copy_unicode_escape(json, bytes, index, &mut out) {
            index += consumed;
        } else {
            index += copy_next_character(json, index, &mut out);
        }
    }
    out
}

fn copy_unicode_escape(json: &str, bytes: &[u8], index: usize, out: &mut String) -> Option<usize> {
    let code_point = unicode_escape_at(bytes, index)?;
    if is_high_surrogate(code_point)
        && unicode_escape_at(bytes, index + 6).is_some_and(is_low_surrogate)
    {
        out.push_str(&json[index..index + 12]);
        return Some(12);
    }
    if is_surrogate_or_noncharacter(code_point) {
        out.push_str(r"\uFFFD");
        return Some(6);
    }
    None
}

fn unicode_escape_at(bytes: &[u8], index: usize) -> Option<u16> {
    let escape = bytes.get(index..index.saturating_add(6))?;
    (escape[0] == b'\\' && escape[1] == b'u').then(|| decode_hex_u16(&escape[2..6])).flatten()
}

fn copy_next_character(json: &str, index: usize, out: &mut String) -> usize {
    let Some(character) = json[index..].chars().next() else {
        return 0;
    };
    out.push(character);
    character.len_utf8()
}

fn decode_hex_u16(hex: &[u8]) -> Option<u16> {
    if hex.len() < 4 {
        return None;
    }
    let mut val: u16 = 0;
    for &b in &hex[..4] {
        val = val.wrapping_mul(16).wrapping_add(match b {
            b'0'..=b'9' => u16::from(b - b'0'),
            b'a'..=b'f' => u16::from(b - b'a' + 10),
            b'A'..=b'F' => u16::from(b - b'A' + 10),
            _ => return None,
        });
    }
    Some(val)
}

const fn is_surrogate_or_noncharacter(cp: u16) -> bool {
    matches!(cp, 0xD800..=0xDFFF | 0xFFFE | 0xFFFF)
}

const fn is_high_surrogate(cp: u16) -> bool {
    matches!(cp, 0xD800..=0xDBFF)
}

const fn is_low_surrogate(cp: u16) -> bool {
    matches!(cp, 0xDC00..=0xDFFF)
}

/// Persists one chunk-level graph extraction record.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the graph extraction record.
pub async fn create_runtime_graph_extraction_record(
    pool: &PgPool,
    input: &CreateRuntimeGraphExtractionRecordInput,
) -> Result<RuntimeGraphExtractionRecordRow, sqlx::Error> {
    create_runtime_graph_extraction_record_with_executor(pool, input).await
}

async fn create_runtime_graph_extraction_record_with_executor<'e, E>(
    executor: E,
    input: &CreateRuntimeGraphExtractionRecordInput,
) -> Result<RuntimeGraphExtractionRecordRow, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, RuntimeGraphExtractionRecordRow>(
        "insert into runtime_graph_extraction (
            id, runtime_execution_id, library_id, document_id, chunk_id, provider_kind, model_name,
            extraction_version, prompt_hash, status, raw_output_json, normalized_output_json,
            glean_pass_count, error_message
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11::jsonb, $12::jsonb, $13, $14)
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
    .bind(sanitized_json_for_postgres_text(input.raw_output_json.clone()))
    .bind(sanitized_runtime_graph_normalized_json_text(input.normalized_output_json.clone()))
    .bind(input.glean_pass_count)
    .bind(input.error_message.as_deref())
    .fetch_one(executor)
    .await
}

/// Creates one ingest-owned graph extraction record only while the supplied
/// attempt is still the latest leased owner of the exact revision.
pub async fn create_runtime_graph_extraction_record_fenced(
    pool: &PgPool,
    authority: IngestRevisionWriteAuthority,
    input: &CreateRuntimeGraphExtractionRecordInput,
) -> Result<IngestAttemptMutationOutcome<RuntimeGraphExtractionRecordRow>, sqlx::Error> {
    let Some(mut transaction) =
        super::acquire_ingest_revision_graph_write_guard(pool, authority).await?
    else {
        return Ok(IngestAttemptMutationOutcome::AuthorityLost);
    };
    if !graph_extraction_input_matches_authority(&mut transaction, authority, input).await? {
        transaction.rollback().await?;
        return Err(sqlx::Error::Protocol(
            "graph extraction input does not match its ingest revision authority".to_string(),
        ));
    }
    let row =
        create_runtime_graph_extraction_record_with_executor(&mut *transaction, input).await?;
    transaction.commit().await?;
    Ok(IngestAttemptMutationOutcome::Applied(row))
}

async fn graph_extraction_input_matches_authority(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    authority: IngestRevisionWriteAuthority,
    input: &CreateRuntimeGraphExtractionRecordInput,
) -> Result<bool, sqlx::Error> {
    if input.library_id != authority.library_id {
        return Ok(false);
    }
    sqlx::query_scalar::<_, bool>(
        "select true
         from content_chunk as chunk
         join content_revision as revision on revision.id = chunk.revision_id
         where chunk.id = $1
           and revision.id = $2
           and revision.document_id = $3
           and revision.workspace_id = $4
           and revision.library_id = $5
         for share of chunk, revision",
    )
    .bind(input.chunk_id)
    .bind(authority.revision_id)
    .bind(input.document_id)
    .bind(authority.workspace_id)
    .bind(authority.library_id)
    .fetch_optional(&mut **transaction)
    .await
    .map(|matched| matched.unwrap_or(false))
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
    update_runtime_graph_extraction_record_with_executor(pool, id, input).await
}

async fn update_runtime_graph_extraction_record_with_executor<'e, E>(
    executor: E,
    id: Uuid,
    input: &UpdateRuntimeGraphExtractionRecordInput,
) -> Result<Option<RuntimeGraphExtractionRecordRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    update_runtime_graph_extraction_record_json_with_executor(
        executor,
        id,
        input,
        sanitized_json_for_postgres_text(input.raw_output_json.clone()),
        sanitized_runtime_graph_normalized_json_text(input.normalized_output_json.clone()),
    )
    .await
}

async fn update_runtime_graph_extraction_record_json_with_executor<'e, E>(
    executor: E,
    id: Uuid,
    input: &UpdateRuntimeGraphExtractionRecordInput,
    raw_output_json: String,
    normalized_output_json: String,
) -> Result<Option<RuntimeGraphExtractionRecordRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, RuntimeGraphExtractionRecordRow>(
        "update runtime_graph_extraction
         set provider_kind = $2,
             model_name = $3,
             prompt_hash = $4,
             status = $5,
             raw_output_json = $6::jsonb,
             normalized_output_json = $7::jsonb,
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
    .bind(raw_output_json)
    .bind(normalized_output_json)
    .bind(input.glean_pass_count)
    .bind(input.error_message.as_deref())
    .fetch_optional(executor)
    .await
}

const PG_SQLSTATE_UNTRANSLATABLE_CHARACTER: &str = "22P05";

/// Returns `true` when `error` is a `PostgreSQL` Unicode escape rejection (22P05).
fn is_unicode_escape_error(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| is_unicode_escape_sqlstate(code.as_ref()))
}

fn is_unicode_escape_sqlstate(code: &str) -> bool {
    code == PG_SQLSTATE_UNTRANSLATABLE_CHARACTER
}

/// Same as [`update_runtime_graph_extraction_record`] but when the primary
/// call fails with a Unicode escape error the update is retried with empty
/// JSON objects so the chunk-level failure is recorded instead of aborting
/// the entire document pipeline.
pub async fn update_runtime_graph_extraction_record_safe(
    pool: &PgPool,
    id: Uuid,
    input: &UpdateRuntimeGraphExtractionRecordInput,
) -> Result<Option<RuntimeGraphExtractionRecordRow>, sqlx::Error> {
    match update_runtime_graph_extraction_record(pool, id, input).await {
        Ok(row) => Ok(row),
        Err(e) if is_unicode_escape_error(&e) => {
            tracing::warn!(
                id = %id,
                "retrying graph extraction update with empty JSON after Unicode escape error"
            );
            update_runtime_graph_extraction_record_empty_json_with_executor(pool, id, input).await
        }
        Err(e) => Err(e),
    }
}

async fn update_runtime_graph_extraction_record_empty_json_with_executor<'e, E>(
    executor: E,
    id: Uuid,
    input: &UpdateRuntimeGraphExtractionRecordInput,
) -> Result<Option<RuntimeGraphExtractionRecordRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    update_runtime_graph_extraction_record_json_with_executor(
        executor,
        id,
        input,
        "{}".to_string(),
        "{}".to_string(),
    )
    .await
}

/// Attempt-fenced form of [`update_runtime_graph_extraction_record_safe`].
/// The authority check, record-scope check, terminal write, and Unicode
/// fallback commit together; a replacement attempt therefore cannot be
/// overwritten by a delayed provider response.
pub async fn update_runtime_graph_extraction_record_safe_fenced(
    pool: &PgPool,
    authority: IngestRevisionWriteAuthority,
    id: Uuid,
    input: &UpdateRuntimeGraphExtractionRecordInput,
) -> Result<IngestAttemptMutationOutcome<Option<RuntimeGraphExtractionRecordRow>>, sqlx::Error> {
    let Some(mut transaction) =
        super::acquire_ingest_revision_graph_write_guard(pool, authority).await?
    else {
        return Ok(IngestAttemptMutationOutcome::AuthorityLost);
    };
    let record_matches = sqlx::query_scalar::<_, bool>(
        "select true
         from runtime_graph_extraction as extraction
         join content_chunk as chunk on chunk.id = extraction.chunk_id
         join content_revision as revision on revision.id = chunk.revision_id
         where extraction.id = $1
           and extraction.library_id = $2
           and revision.id = $3
           and revision.workspace_id = $4
           and revision.library_id = $2
           and extraction.document_id = revision.document_id
         for update of extraction",
    )
    .bind(id)
    .bind(authority.library_id)
    .bind(authority.revision_id)
    .bind(authority.workspace_id)
    .fetch_optional(&mut *transaction)
    .await?
    .unwrap_or(false);
    if !record_matches {
        transaction.rollback().await?;
        return Err(sqlx::Error::Protocol(
            "graph extraction record does not match its ingest revision authority".to_string(),
        ));
    }

    let row = apply_fenced_extraction_record_update(&mut transaction, id, input).await?;
    transaction.commit().await?;
    Ok(IngestAttemptMutationOutcome::Applied(row))
}

/// Runs the terminal extraction-record write on a child savepoint and resolves
/// it, falling back to an empty-JSON write only when the same statement fails
/// with a PostgreSQL untranslatable-character (`22P05`) error.
///
/// The savepoint, the write, and the Unicode fallback keep the original
/// ordering: the savepoint is committed on success, rolled back before the
/// fallback, and rolled back before propagating any other error. The parent
/// transaction (and thus the authority guard) stays open for the caller's
/// single commit.
async fn apply_fenced_extraction_record_update(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    id: Uuid,
    input: &UpdateRuntimeGraphExtractionRecordInput,
) -> Result<Option<RuntimeGraphExtractionRecordRow>, sqlx::Error> {
    let mut savepoint = transaction.begin().await?;
    match update_runtime_graph_extraction_record_with_executor(&mut *savepoint, id, input).await {
        Ok(row) => savepoint.commit().await.map(|()| row),
        Err(error) => {
            savepoint.rollback().await?;
            if !is_unicode_escape_error(&error) {
                return Err(error);
            }
            retry_fenced_extraction_with_empty_json(transaction, id, input).await
        }
    }
}

async fn retry_fenced_extraction_with_empty_json(
    connection: &mut sqlx::PgConnection,
    id: Uuid,
    input: &UpdateRuntimeGraphExtractionRecordInput,
) -> Result<Option<RuntimeGraphExtractionRecordRow>, sqlx::Error> {
    tracing::warn!(
        id = %id,
        "retrying fenced graph extraction update with empty JSON after Unicode escape error"
    );
    update_runtime_graph_extraction_record_empty_json_with_executor(connection, id, input).await
}

/// Marks interrupted chunk-level graph extraction rows as canceled before a
/// restarted revision run resumes from ready checkpoints.
///
/// # Errors
/// Returns any `SQLx` error raised while updating graph extraction rows.
pub async fn cancel_processing_runtime_graph_extractions_for_document(
    pool: &PgPool,
    library_id: Uuid,
    document_id: Uuid,
    reason: &str,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "update runtime_graph_extraction
         set status = 'canceled',
             error_message = $3,
             raw_output_json = case
                 when raw_output_json = '{}'::jsonb then jsonb_build_object('canceled_reason', $3)
                 else raw_output_json
             end
         where library_id = $1
           and document_id = $2
           and status = 'processing'",
    )
    .bind(library_id)
    .bind(document_id)
    .bind(reason)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Clears attempt-owned graph materialization staging for a revision while the
/// caller still owns the latest lease. Historical ready extraction rows remain
/// available as semantic-cache sources until the replacement attempt has
/// materialized its complete ready set.
pub async fn prepare_revision_graph_materialization_fenced(
    pool: &PgPool,
    authority: IngestRevisionWriteAuthority,
    document_id: Uuid,
    reason: &str,
) -> Result<IngestAttemptMutationOutcome<u64>, sqlx::Error> {
    let Some(mut transaction) =
        super::acquire_ingest_revision_graph_write_guard(pool, authority).await?
    else {
        return Ok(IngestAttemptMutationOutcome::AuthorityLost);
    };
    let scope_matches = sqlx::query_scalar::<_, bool>(
        "select true
         from content_revision
         where id = $1
           and document_id = $2
           and workspace_id = $3
           and library_id = $4
         for share",
    )
    .bind(authority.revision_id)
    .bind(document_id)
    .bind(authority.workspace_id)
    .bind(authority.library_id)
    .fetch_optional(&mut *transaction)
    .await?
    .unwrap_or(false);
    if !scope_matches {
        transaction.rollback().await?;
        return Err(sqlx::Error::Protocol(
            "graph materialization scope does not match its ingest revision authority".to_string(),
        ));
    }

    let canceled = sqlx::query(
        "update runtime_graph_extraction as extraction
         set status = 'canceled',
             error_message = $4,
             raw_output_json = case
                 when extraction.raw_output_json = '{}'::jsonb
                 then jsonb_build_object('canceled_reason', $4)
                 else extraction.raw_output_json
             end
         from content_chunk as chunk
         where extraction.chunk_id = chunk.id
           and extraction.library_id = $1
           and extraction.document_id = $2
           and chunk.revision_id = $3
           and extraction.status = 'processing'",
    )
    .bind(authority.library_id)
    .bind(document_id)
    .bind(authority.revision_id)
    .bind(reason)
    .execute(&mut *transaction)
    .await?
    .rows_affected();
    // Candidate staging is revision-scoped. Deleting both collections inside
    // this authority transaction prevents a delayed attempt from clearing a
    // replacement attempt's candidates.
    let relation_candidates = sqlx::query(
        "delete from knowledge_relation_candidate
         where workspace_id = $1 and library_id = $2 and revision_id = $3",
    )
    .bind(authority.workspace_id)
    .bind(authority.library_id)
    .bind(authority.revision_id)
    .execute(&mut *transaction)
    .await?
    .rows_affected();
    let entity_candidates = sqlx::query(
        "delete from knowledge_entity_candidate
         where workspace_id = $1 and library_id = $2 and revision_id = $3",
    )
    .bind(authority.workspace_id)
    .bind(authority.library_id)
    .bind(authority.revision_id)
    .execute(&mut *transaction)
    .await?
    .rows_affected();
    transaction.commit().await?;
    Ok(IngestAttemptMutationOutcome::Applied(
        canceled.saturating_add(relation_candidates).saturating_add(entity_candidates),
    ))
}

/// Retires ready/processing extraction generations from older attempts only
/// after the current attempt has completed materialization. Exact-attempt
/// selection during reconcile then cannot admit historical rows.
pub async fn retire_superseded_revision_graph_extractions_fenced(
    pool: &PgPool,
    authority: IngestRevisionWriteAuthority,
    document_id: Uuid,
) -> Result<IngestAttemptMutationOutcome<u64>, sqlx::Error> {
    let Some(mut transaction) =
        super::acquire_ingest_revision_graph_write_guard(pool, authority).await?
    else {
        return Ok(IngestAttemptMutationOutcome::AuthorityLost);
    };
    let result = sqlx::query(
        "update runtime_graph_extraction as extraction
         set status = 'canceled',
             error_message = 'superseded by a newer ingest attempt for the same revision'
         from content_chunk as chunk
         where extraction.chunk_id = chunk.id
           and extraction.library_id = $1
           and extraction.document_id = $2
           and chunk.revision_id = $3
           and extraction.status in ('processing', 'ready')
           and coalesce(
                 extraction.raw_output_json #>> '{lifecycle,activated_by_attempt_id}',
                 ''
               ) <> $4",
    )
    .bind(authority.library_id)
    .bind(document_id)
    .bind(authority.revision_id)
    .bind(authority.attempt_id.to_string())
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(IngestAttemptMutationOutcome::Applied(result.rows_affected()))
}

/// Lists graph extraction records for one document.
///
/// # Errors
/// Returns any `SQLx` error raised while querying graph extraction records.
pub async fn list_runtime_graph_extraction_records_by_document(
    pool: &PgPool,
    document_id: Uuid,
    extraction_version: &str,
) -> Result<Vec<RuntimeGraphExtractionRecordRow>, sqlx::Error> {
    list_runtime_graph_extraction_records_by_document_with_executor(
        pool,
        document_id,
        extraction_version,
    )
    .await
}

pub async fn list_runtime_graph_extraction_records_by_document_with_executor<'e, E>(
    executor: E,
    document_id: Uuid,
    extraction_version: &str,
) -> Result<Vec<RuntimeGraphExtractionRecordRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, RuntimeGraphExtractionRecordRow>(
        "select id, runtime_execution_id, library_id, document_id, chunk_id, provider_kind,
            model_name, extraction_version, prompt_hash, status, raw_output_json,
            normalized_output_json, glean_pass_count, error_message, created_at
         from runtime_graph_extraction
         where document_id = $1
           and extraction_version = $2
         order by created_at asc, id asc",
    )
    .bind(document_id)
    .bind(extraction_version)
    .fetch_all(executor)
    .await
}

/// Loads the newest ready graph extraction that matches the exact semantic cache key.
///
/// The key is the stable extraction identity: library, chunk text checksum,
/// `extraction_version`, provider, model and `extraction_contract_hash`. All six
/// must match exactly. Any drift in the provider/model profile or in the static
/// prompt contract invalidates the entry, so a template change must go back to
/// the provider rather than reuse output produced by a different contract.
///
/// The identity is stored in `raw_output_json->>'extraction_contract_hash'`
/// (IRG-197) rather than in `prompt_hash`, because `prompt_hash` keeps the full
/// rendered prompt hash for audit — a value that drifts with the chunk ordinal
/// and the library's live `sub_type` occurrence counts and must not decide cache
/// validity. `extraction_version` remains the invalidation boundary for output
/// semantics, so the two are not substitutes for each other.
///
/// Named identity of a semantic-cache lookup for one graph extraction record.
///
/// The named payload keeps the library, text, version and provider/model
/// identity distinct from the contract hash at the call site.
pub struct RuntimeGraphExtractionSemanticCacheKey<'a> {
    pub library_id: Uuid,
    pub text_checksum: &'a str,
    pub extraction_version: &'a str,
    pub provider_kind: &'a str,
    pub model_name: &'a str,
    pub extraction_contract_hash: &'a str,
}

/// # Errors
/// Returns any `SQLx` error raised while querying graph extraction records.
pub async fn find_ready_runtime_graph_extraction_record_by_semantic_cache_key(
    pool: &PgPool,
    key: RuntimeGraphExtractionSemanticCacheKey<'_>,
) -> Result<Option<RuntimeGraphExtractionRecordRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphExtractionRecordRow>(
        "select extraction.id, extraction.runtime_execution_id, extraction.library_id,
            extraction.document_id, extraction.chunk_id, extraction.provider_kind,
            extraction.model_name, extraction.extraction_version, extraction.prompt_hash,
            extraction.status, extraction.raw_output_json, extraction.normalized_output_json,
            extraction.glean_pass_count, extraction.error_message, extraction.created_at
         from runtime_graph_extraction as extraction
         join content_chunk as chunk
           on chunk.id = extraction.chunk_id
         join content_revision as revision
           on revision.id = chunk.revision_id
          and revision.library_id = extraction.library_id
         where extraction.library_id = $1
           and chunk.text_checksum = $2
           and extraction.extraction_version = $3
           and extraction.provider_kind = $4
           and extraction.model_name = $5
           and extraction.raw_output_json ->> 'extraction_contract_hash' = $6
           and extraction.status = 'ready'
         order by extraction.created_at desc,
            extraction.id desc
         limit 1",
    )
    .bind(key.library_id)
    .bind(key.text_checksum)
    .bind(key.extraction_version)
    .bind(key.provider_kind)
    .bind(key.model_name)
    .bind(key.extraction_contract_hash)
    .fetch_optional(pool)
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
    extraction_version: &str,
) -> Result<Vec<RuntimeGraphExtractionRecordRow>, sqlx::Error> {
    list_runtime_graph_extraction_records_by_library_with_executor(
        pool,
        library_id,
        extraction_version,
    )
    .await
}

/// Lists graph extraction records for one library using the supplied database
/// executor so callers can keep the read inside a wider transaction snapshot.
///
/// # Errors
/// Returns any `SQLx` error raised while querying graph extraction records.
pub async fn list_runtime_graph_extraction_records_by_library_with_executor<'e, E>(
    executor: E,
    library_id: Uuid,
    extraction_version: &str,
) -> Result<Vec<RuntimeGraphExtractionRecordRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, RuntimeGraphExtractionRecordRow>(
        "select id, runtime_execution_id, library_id, document_id, chunk_id, provider_kind,
            model_name, extraction_version, prompt_hash, status, raw_output_json,
            normalized_output_json, glean_pass_count, error_message, created_at
         from runtime_graph_extraction
         where library_id = $1
           and extraction_version = $2
         order by created_at asc, id asc",
    )
    .bind(library_id)
    .bind(extraction_version)
    .fetch_all(executor)
    .await
}

#[cfg(test)]
mod tests {
    use super::{
        is_unicode_escape_error, is_unicode_escape_sqlstate, replace_surrogate_unicode_escapes,
        sanitize_json_for_postgres, sanitize_runtime_graph_normalized_json,
    };
    use crate::shared::text_encoding::repair_json_string_values;

    #[test]
    fn sanitizes_runtime_graph_normalized_json_encoding_damage() {
        let value = serde_json::json!({
            "entities": [
                {
                    "label": "\u{00d0}\u{00a1}\u{00d0}\u{00be}\u{00d0}\u{00be}\u{00d0}\u{00b1}\u{00d1}\u{0089}\u{00d0}\u{00b5}\u{00d0}\u{00bd}\u{00d0}\u{00b8}\u{00d0}\u{00b5} TransferCallReturned",
                    "node_type": "artifact",
                    "aliases": [],
                    "sub_type": "message",
                    "summary": "\u{00d0}\u{00a1}\u{00d0}\u{00be}\u{00d0}\u{00be}\u{00d0}\u{00b1}\u{00d1}\u{0089}\u{00d0}\u{00b5}\u{00d0}\u{00bd}\u{00d0}\u{00b8}\u{00d0}\u{00b5} TransferCallReturned."
                }
            ],
            "relations": []
        });

        let direct_repaired = repair_json_string_values(value.clone());
        assert_eq!(direct_repaired["entities"][0]["label"], "Сообщение TransferCallReturned");
        let postgres_sanitized = sanitize_json_for_postgres(direct_repaired.clone());
        assert_eq!(postgres_sanitized["entities"][0]["label"], "Сообщение TransferCallReturned");

        let repaired = sanitize_runtime_graph_normalized_json(value);

        assert_eq!(repaired["entities"][0]["label"], "Сообщение TransferCallReturned");
        assert_eq!(repaired["entities"][0]["summary"], "Сообщение TransferCallReturned.");
    }

    #[test]
    fn replaces_final_invalid_unicode_escape() {
        assert_eq!(replace_surrogate_unicode_escapes(r#""\uFFFF""#), r#""\uFFFD""#);
    }

    #[test]
    fn preserves_valid_surrogate_pair_escapes() {
        let sanitized = replace_surrogate_unicode_escapes(r#"{"label":"\uD83D\uDE00"}"#);
        let value: serde_json::Value = serde_json::from_str(&sanitized).unwrap();

        assert_eq!(value["label"], "😀");
    }

    #[test]
    fn unicode_escape_retry_uses_sqlstate_and_ignores_misleading_prose() {
        assert!(is_unicode_escape_sqlstate("22P05"));
        assert!(!is_unicode_escape_sqlstate("22021"));
        assert!(!is_unicode_escape_sqlstate("23505"));

        let misleading = sqlx::Error::Protocol(
            "unsupported Unicode escape sequence with no database code".to_string(),
        );
        assert!(!is_unicode_escape_error(&misleading));
    }
}
