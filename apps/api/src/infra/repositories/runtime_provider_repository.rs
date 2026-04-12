use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

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
