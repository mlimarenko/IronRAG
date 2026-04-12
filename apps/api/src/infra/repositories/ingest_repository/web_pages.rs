use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct WebDiscoveredPageRow {
    pub id: Uuid,
    pub run_id: Uuid,
    pub discovered_url: Option<String>,
    pub normalized_url: String,
    pub final_url: Option<String>,
    pub canonical_url: Option<String>,
    pub depth: i32,
    pub referrer_candidate_id: Option<Uuid>,
    pub host_classification: String,
    pub candidate_state: String,
    pub classification_reason: Option<String>,
    pub content_type: Option<String>,
    pub http_status: Option<i32>,
    pub snapshot_storage_key: Option<String>,
    pub discovered_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub document_id: Option<Uuid>,
    pub result_revision_id: Option<Uuid>,
    pub mutation_item_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct NewWebDiscoveredPage<'a> {
    pub id: Uuid,
    pub run_id: Uuid,
    pub discovered_url: Option<&'a str>,
    pub normalized_url: &'a str,
    pub final_url: Option<&'a str>,
    pub canonical_url: Option<&'a str>,
    pub depth: i32,
    pub referrer_candidate_id: Option<Uuid>,
    pub host_classification: &'a str,
    pub candidate_state: &'a str,
    pub classification_reason: Option<&'a str>,
    pub content_type: Option<&'a str>,
    pub http_status: Option<i32>,
    pub snapshot_storage_key: Option<&'a str>,
    pub discovered_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub document_id: Option<Uuid>,
    pub result_revision_id: Option<Uuid>,
    pub mutation_item_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct UpdateWebDiscoveredPage<'a> {
    pub final_url: Option<&'a str>,
    pub canonical_url: Option<&'a str>,
    pub host_classification: Option<&'a str>,
    pub candidate_state: &'a str,
    pub classification_reason: Option<&'a str>,
    pub content_type: Option<&'a str>,
    pub http_status: Option<i32>,
    pub snapshot_storage_key: Option<&'a str>,
    pub updated_at: Option<DateTime<Utc>>,
    pub document_id: Option<Uuid>,
    pub result_revision_id: Option<Uuid>,
    pub mutation_item_id: Option<Uuid>,
}

pub async fn create_web_discovered_page(
    postgres: &PgPool,
    input: &NewWebDiscoveredPage<'_>,
) -> Result<WebDiscoveredPageRow, sqlx::Error> {
    sqlx::query_as::<_, WebDiscoveredPageRow>(
        "insert into content_web_discovered_page (
            id,
            run_id,
            discovered_url,
            normalized_url,
            final_url,
            canonical_url,
            depth,
            referrer_candidate_id,
            host_classification,
            candidate_state,
            classification_reason,
            content_type,
            http_status,
            snapshot_storage_key,
            discovered_at,
            updated_at,
            document_id,
            result_revision_id,
            mutation_item_id
        )
        values (
            $1,
            $2,
            $3,
            $4,
            $5,
            $6,
            $7,
            $8,
            $9::web_candidate_host_classification,
            $10::web_candidate_state,
            $11,
            $12,
            $13,
            $14,
            coalesce($15, now()),
            coalesce($16, now()),
            $17,
            $18,
            $19
        )
        returning
            id,
            run_id,
            discovered_url,
            normalized_url,
            final_url,
            canonical_url,
            depth,
            referrer_candidate_id,
            host_classification::text as host_classification,
            candidate_state::text as candidate_state,
            classification_reason,
            content_type,
            http_status,
            snapshot_storage_key,
            discovered_at,
            updated_at,
            document_id,
            result_revision_id,
            mutation_item_id",
    )
    .bind(input.id)
    .bind(input.run_id)
    .bind(input.discovered_url)
    .bind(input.normalized_url)
    .bind(input.final_url)
    .bind(input.canonical_url)
    .bind(input.depth)
    .bind(input.referrer_candidate_id)
    .bind(input.host_classification)
    .bind(input.candidate_state)
    .bind(input.classification_reason)
    .bind(input.content_type)
    .bind(input.http_status)
    .bind(input.snapshot_storage_key)
    .bind(input.discovered_at)
    .bind(input.updated_at)
    .bind(input.document_id)
    .bind(input.result_revision_id)
    .bind(input.mutation_item_id)
    .fetch_one(postgres)
    .await
}

pub async fn list_web_discovered_pages(
    postgres: &PgPool,
    run_id: Uuid,
) -> Result<Vec<WebDiscoveredPageRow>, sqlx::Error> {
    sqlx::query_as::<_, WebDiscoveredPageRow>(
        "select
            id,
            run_id,
            discovered_url,
            normalized_url,
            final_url,
            canonical_url,
            depth,
            referrer_candidate_id,
            host_classification::text as host_classification,
            candidate_state::text as candidate_state,
            classification_reason,
            content_type,
            http_status,
            snapshot_storage_key,
            discovered_at,
            updated_at,
            document_id,
            result_revision_id,
            mutation_item_id
         from content_web_discovered_page
         where run_id = $1
         order by depth asc, discovered_at asc, id asc",
    )
    .bind(run_id)
    .fetch_all(postgres)
    .await
}

pub async fn get_web_discovered_page_by_result_revision_id(
    postgres: &PgPool,
    result_revision_id: Uuid,
) -> Result<Option<WebDiscoveredPageRow>, sqlx::Error> {
    sqlx::query_as::<_, WebDiscoveredPageRow>(
        "select
            id,
            run_id,
            discovered_url,
            normalized_url,
            final_url,
            canonical_url,
            depth,
            referrer_candidate_id,
            host_classification::text as host_classification,
            candidate_state::text as candidate_state,
            classification_reason,
            content_type,
            http_status,
            snapshot_storage_key,
            discovered_at,
            updated_at,
            document_id,
            result_revision_id,
            mutation_item_id
         from content_web_discovered_page
         where result_revision_id = $1
         order by updated_at desc, discovered_at desc, id desc
         limit 1",
    )
    .bind(result_revision_id)
    .fetch_optional(postgres)
    .await
}

pub async fn list_web_discovered_pages_by_result_revision_ids(
    postgres: &PgPool,
    result_revision_ids: &[Uuid],
) -> Result<Vec<WebDiscoveredPageRow>, sqlx::Error> {
    if result_revision_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, WebDiscoveredPageRow>(
        "select distinct on (result_revision_id)
            id,
            run_id,
            discovered_url,
            normalized_url,
            final_url,
            canonical_url,
            depth,
            referrer_candidate_id,
            host_classification::text as host_classification,
            candidate_state::text as candidate_state,
            classification_reason,
            content_type,
            http_status,
            snapshot_storage_key,
            discovered_at,
            updated_at,
            document_id,
            result_revision_id,
            mutation_item_id
         from content_web_discovered_page
         where result_revision_id = any($1)
         order by result_revision_id, updated_at desc, discovered_at desc, id desc",
    )
    .bind(result_revision_ids)
    .fetch_all(postgres)
    .await
}

pub async fn get_web_discovered_page_by_id(
    postgres: &PgPool,
    candidate_id: Uuid,
) -> Result<Option<WebDiscoveredPageRow>, sqlx::Error> {
    sqlx::query_as::<_, WebDiscoveredPageRow>(
        "select
            id,
            run_id,
            discovered_url,
            normalized_url,
            final_url,
            canonical_url,
            depth,
            referrer_candidate_id,
            host_classification::text as host_classification,
            candidate_state::text as candidate_state,
            classification_reason,
            content_type,
            http_status,
            snapshot_storage_key,
            discovered_at,
            updated_at,
            document_id,
            result_revision_id,
            mutation_item_id
         from content_web_discovered_page
         where id = $1",
    )
    .bind(candidate_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_web_discovered_page_by_run_and_normalized_url(
    postgres: &PgPool,
    run_id: Uuid,
    normalized_url: &str,
) -> Result<Option<WebDiscoveredPageRow>, sqlx::Error> {
    sqlx::query_as::<_, WebDiscoveredPageRow>(
        "select
            id,
            run_id,
            discovered_url,
            normalized_url,
            final_url,
            canonical_url,
            depth,
            referrer_candidate_id,
            host_classification::text as host_classification,
            candidate_state::text as candidate_state,
            classification_reason,
            content_type,
            http_status,
            snapshot_storage_key,
            discovered_at,
            updated_at,
            document_id,
            result_revision_id,
            mutation_item_id
         from content_web_discovered_page
         where run_id = $1
           and normalized_url = $2
         limit 1",
    )
    .bind(run_id)
    .bind(normalized_url)
    .fetch_optional(postgres)
    .await
}

pub async fn update_web_discovered_page(
    postgres: &PgPool,
    candidate_id: Uuid,
    input: &UpdateWebDiscoveredPage<'_>,
) -> Result<Option<WebDiscoveredPageRow>, sqlx::Error> {
    sqlx::query_as::<_, WebDiscoveredPageRow>(
        "update content_web_discovered_page
         set final_url = $2,
             canonical_url = $3,
             host_classification = coalesce($4::web_candidate_host_classification, host_classification),
             candidate_state = $5::web_candidate_state,
             classification_reason = $6,
             content_type = $7,
             http_status = $8,
             snapshot_storage_key = $9,
             updated_at = coalesce($10, now()),
             document_id = $11,
             result_revision_id = $12,
             mutation_item_id = $13
         where id = $1
         returning
            id,
            run_id,
            discovered_url,
            normalized_url,
            final_url,
            canonical_url,
            depth,
            referrer_candidate_id,
            host_classification::text as host_classification,
            candidate_state::text as candidate_state,
            classification_reason,
            content_type,
            http_status,
            snapshot_storage_key,
            discovered_at,
            updated_at,
            document_id,
            result_revision_id,
            mutation_item_id",
    )
    .bind(candidate_id)
    .bind(input.final_url)
    .bind(input.canonical_url)
    .bind(input.host_classification)
    .bind(input.candidate_state)
    .bind(input.classification_reason)
    .bind(input.content_type)
    .bind(input.http_status)
    .bind(input.snapshot_storage_key)
    .bind(input.updated_at)
    .bind(input.document_id)
    .bind(input.result_revision_id)
    .bind(input.mutation_item_id)
    .fetch_optional(postgres)
    .await
}
