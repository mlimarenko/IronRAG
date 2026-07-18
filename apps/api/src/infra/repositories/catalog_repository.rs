use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct CatalogWorkspaceRow {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub lifecycle_state: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct CatalogLibraryRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub description: Option<String>,
    pub extraction_prompt: Option<String>,
    pub web_ingest_policy: Value,
    pub recognition_policy: Value,
    pub retrieval_config: Value,
    pub lifecycle_state: String,
    pub include_document_hint_in_mcp_answers: bool,
    #[sqlx(default)]
    pub chunking_template: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct CatalogLibraryConnectorRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub connector_kind: String,
    pub display_name: String,
    pub configuration_json: Value,
    pub sync_mode: String,
    pub last_sync_requested_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogDeleteBlockers {
    pub undispatched_webhook_lifecycle_events: usize,
    pub active_webhook_delivery_jobs: usize,
    pub unresolved_webhook_delivery_attempts: usize,
}

impl CatalogDeleteBlockers {
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.undispatched_webhook_lifecycle_events == 0
            && self.active_webhook_delivery_jobs == 0
            && self.unresolved_webhook_delivery_attempts == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogLibraryDeleteOutcome {
    Deleted,
    NotFound,
    Blocked(CatalogDeleteBlockers),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogWorkspaceDeleteOutcome {
    Deleted,
    NotFound,
    Blocked(CatalogDeleteBlockers),
}

pub async fn list_workspaces(postgres: &PgPool) -> Result<Vec<CatalogWorkspaceRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogWorkspaceRow>(
        "select id, slug, display_name, lifecycle_state::text as lifecycle_state, created_at, updated_at
         from catalog_workspace
         order by created_at desc",
    )
    .fetch_all(postgres)
    .await
}

pub async fn get_workspace_by_id(
    postgres: &PgPool,
    workspace_id: Uuid,
) -> Result<Option<CatalogWorkspaceRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogWorkspaceRow>(
        "select id, slug, display_name, lifecycle_state::text as lifecycle_state, created_at, updated_at
         from catalog_workspace
         where id = $1",
    )
    .bind(workspace_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_workspace_by_slug(
    postgres: &PgPool,
    slug: &str,
) -> Result<Option<CatalogWorkspaceRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogWorkspaceRow>(
        "select id, slug, display_name, lifecycle_state::text as lifecycle_state, created_at, updated_at
         from catalog_workspace
         where slug = $1",
    )
    .bind(slug)
    .fetch_optional(postgres)
    .await
}

pub async fn create_workspace(
    postgres: &PgPool,
    slug: &str,
    display_name: &str,
    created_by_principal_id: Option<Uuid>,
) -> Result<CatalogWorkspaceRow, sqlx::Error> {
    sqlx::query_as::<_, CatalogWorkspaceRow>(
        "insert into catalog_workspace (
            id,
            slug,
            display_name,
            lifecycle_state,
            created_by_principal_id,
            created_at,
            updated_at
        )
        values ($1, $2, $3, 'active', $4, now(), now())
        returning id, slug, display_name, lifecycle_state::text as lifecycle_state, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(slug)
    .bind(display_name)
    .bind(created_by_principal_id)
    .fetch_one(postgres)
    .await
}

pub async fn update_workspace(
    postgres: &PgPool,
    workspace_id: Uuid,
    slug: &str,
    display_name: &str,
    lifecycle_state: &str,
) -> Result<Option<CatalogWorkspaceRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogWorkspaceRow>(
        "update catalog_workspace
         set slug = $2,
             display_name = $3,
             lifecycle_state = $4::catalog_workspace_lifecycle_state,
             updated_at = now()
         where id = $1
         returning id, slug, display_name, lifecycle_state::text as lifecycle_state, created_at, updated_at",
    )
    .bind(workspace_id)
    .bind(slug)
    .bind(display_name)
    .bind(lifecycle_state)
    .fetch_optional(postgres)
    .await
}

pub async fn archive_workspace(
    postgres: &PgPool,
    workspace_id: Uuid,
) -> Result<Option<CatalogWorkspaceRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogWorkspaceRow>(
        "update catalog_workspace
         set lifecycle_state = 'archived',
             updated_at = now()
         where id = $1
         returning id, slug, display_name, lifecycle_state::text as lifecycle_state, created_at, updated_at",
    )
    .bind(workspace_id)
    .fetch_optional(postgres)
    .await
}

pub async fn delete_workspace(
    postgres: &PgPool,
    workspace_id: Uuid,
) -> Result<CatalogWorkspaceDeleteOutcome, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    // AI configuration statements take this transaction lock in a BEFORE
    // STATEMENT trigger, before locking binding/account/model rows. Take it
    // before the catalog parent so delete and generation invalidation share
    // one global lock order instead of parent -> child / child -> parent.
    sqlx::query(
        "select pg_advisory_xact_lock(
            hashtextextended('ironrag:ai-config-generation', 0)
         )",
    )
    .execute(&mut *transaction)
    .await?;
    let locked_workspace_id = sqlx::query_scalar::<_, Uuid>(
        "select id
         from catalog_workspace
         where id = $1
         for update",
    )
    .bind(workspace_id)
    .fetch_optional(&mut *transaction)
    .await?;
    if locked_workspace_id.is_none() {
        transaction.rollback().await?;
        return Ok(CatalogWorkspaceDeleteOutcome::NotFound);
    }

    // A workspace delete cascades through every library. Lock each library so
    // a lifecycle event or delivery job cannot acquire its FK key-share lock
    // after the guard has inspected that library.
    let locked_library_ids = sqlx::query_scalar::<_, Uuid>(
        "select id
         from catalog_library
         where workspace_id = $1
         order by id
         for update",
    )
    .bind(workspace_id)
    .fetch_all(&mut *transaction)
    .await?;
    let blockers = lock_catalog_delete_blockers(&mut transaction, &locked_library_ids).await?;
    if !blockers.is_empty() {
        transaction.rollback().await?;
        return Ok(CatalogWorkspaceDeleteOutcome::Blocked(blockers));
    }

    let result = sqlx::query("delete from catalog_workspace where id = $1")
        .bind(workspace_id)
        .execute(&mut *transaction)
        .await?;
    if result.rows_affected() != 1 {
        return Err(sqlx::Error::Protocol(
            "locked catalog workspace disappeared before deletion".to_string(),
        ));
    }
    transaction.commit().await?;
    Ok(CatalogWorkspaceDeleteOutcome::Deleted)
}

pub async fn list_libraries(
    postgres: &PgPool,
    workspace_id: Option<Uuid>,
) -> Result<Vec<CatalogLibraryRow>, sqlx::Error> {
    match workspace_id {
        Some(workspace_id) => {
            sqlx::query_as::<_, CatalogLibraryRow>(
                "select id, workspace_id, slug, display_name, description, extraction_prompt, web_ingest_policy, recognition_policy, retrieval_config, lifecycle_state::text as lifecycle_state, include_document_hint_in_mcp_answers, coalesce(chunking_template, 'naive') as chunking_template, created_at, updated_at
                 from catalog_library
                 where workspace_id = $1
                 order by created_at desc",
            )
            .bind(workspace_id)
            .fetch_all(postgres)
            .await
        }
        None => {
            sqlx::query_as::<_, CatalogLibraryRow>(
                "select id, workspace_id, slug, display_name, description, extraction_prompt, web_ingest_policy, recognition_policy, retrieval_config, lifecycle_state::text as lifecycle_state, include_document_hint_in_mcp_answers, coalesce(chunking_template, 'naive') as chunking_template, created_at, updated_at
                 from catalog_library
                 order by created_at desc",
            )
            .fetch_all(postgres)
            .await
        }
    }
}

const QUERY_AUTHORIZED_LIBRARY_CANDIDATES_SQL: &str = "select id
     from catalog_library
     where lifecycle_state = 'active'::catalog_library_lifecycle_state
       and (
           $1::boolean
           or workspace_id = any($2::uuid[])
           or id = any($3::uuid[])
       )
     order by id
     limit 2";

/// Return at most two active library ids covered by an already-authorized
/// global/workspace/library scope.
///
/// Two rows are sufficient to distinguish the only valid inference case
/// (exactly one candidate) from both zero and ambiguous candidates. Keeping
/// the projection to `id` prevents catalog prompts and configuration from
/// crossing tenant boundaries merely to make that decision.
pub async fn list_query_authorized_library_candidates(
    postgres: &PgPool,
    all_libraries: bool,
    workspace_ids: &[Uuid],
    library_ids: &[Uuid],
) -> Result<Vec<Uuid>, sqlx::Error> {
    if !all_libraries && workspace_ids.is_empty() && library_ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_scalar::<_, Uuid>(QUERY_AUTHORIZED_LIBRARY_CANDIDATES_SQL)
        .bind(all_libraries)
        .bind(workspace_ids)
        .bind(library_ids)
        .fetch_all(postgres)
        .await
}

pub async fn get_library_by_id(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<Option<CatalogLibraryRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogLibraryRow>(
        "select id, workspace_id, slug, display_name, description, extraction_prompt, web_ingest_policy, recognition_policy, retrieval_config, lifecycle_state::text as lifecycle_state, include_document_hint_in_mcp_answers, coalesce(chunking_template, 'naive') as chunking_template, created_at, updated_at
         from catalog_library
         where id = $1",
    )
    .bind(library_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_library_by_workspace_and_slug(
    postgres: &PgPool,
    workspace_id: Uuid,
    slug: &str,
) -> Result<Option<CatalogLibraryRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogLibraryRow>(
        "select id, workspace_id, slug, display_name, description, extraction_prompt, web_ingest_policy, recognition_policy, retrieval_config, lifecycle_state::text as lifecycle_state, include_document_hint_in_mcp_answers, coalesce(chunking_template, 'naive') as chunking_template, created_at, updated_at
         from catalog_library
         where workspace_id = $1 and slug = $2",
    )
    .bind(workspace_id)
    .bind(slug)
    .fetch_optional(postgres)
    .await
}

pub async fn create_library(
    postgres: &PgPool,
    workspace_id: Uuid,
    slug: &str,
    display_name: &str,
    description: Option<&str>,
    created_by_principal_id: Option<Uuid>,
) -> Result<CatalogLibraryRow, sqlx::Error> {
    create_library_with_recognition_policy(
        postgres,
        workspace_id,
        slug,
        display_name,
        description,
        serde_json::json!({ "rasterImageEngine": "vision" }),
        created_by_principal_id,
    )
    .await
}

pub async fn create_library_with_recognition_policy(
    postgres: &PgPool,
    workspace_id: Uuid,
    slug: &str,
    display_name: &str,
    description: Option<&str>,
    recognition_policy: Value,
    created_by_principal_id: Option<Uuid>,
) -> Result<CatalogLibraryRow, sqlx::Error> {
    sqlx::query_as::<_, CatalogLibraryRow>(
        "insert into catalog_library (
            id,
            workspace_id,
            slug,
            display_name,
            description,
            recognition_policy,
            lifecycle_state,
            created_by_principal_id,
            created_at,
            updated_at
        )
        values ($1, $2, $3, $4, $5, $6, 'active', $7, now(), now())
        returning id, workspace_id, slug, display_name, description, extraction_prompt, web_ingest_policy, recognition_policy, retrieval_config, lifecycle_state::text as lifecycle_state, include_document_hint_in_mcp_answers, coalesce(chunking_template, 'naive') as chunking_template, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(workspace_id)
    .bind(slug)
    .bind(display_name)
    .bind(description)
    .bind(recognition_policy)
    .bind(created_by_principal_id)
    .fetch_one(postgres)
    .await
}

pub async fn touch_library_source_truth_version(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<i64, sqlx::Error> {
    touch_library_source_truth_version_with_executor(postgres, library_id).await
}

/// Advances the source generation inside an existing content transaction.
/// Cache identities and the content mutation commit therefore become visible
/// atomically across API replicas.
pub async fn touch_library_source_truth_version_with_executor<'e, E>(
    executor: E,
    library_id: Uuid,
) -> Result<i64, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Postgres>,
{
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
    .fetch_one(executor)
    .await
    .map(|version| version.max(1))
}

pub async fn get_library_source_truth_version(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "select coalesce(source_truth_version, 1) from catalog_library where id = $1",
    )
    .bind(library_id)
    .fetch_one(postgres)
    .await
    .map(|version| version.max(1))
}

pub async fn update_library(
    postgres: &PgPool,
    library_id: Uuid,
    slug: &str,
    display_name: &str,
    description: Option<&str>,
    extraction_prompt: Option<&str>,
    lifecycle_state: &str,
    include_document_hint_in_mcp_answers: bool,
) -> Result<Option<CatalogLibraryRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogLibraryRow>(
        "update catalog_library
         set slug = $2,
             display_name = $3,
             description = $4,
             extraction_prompt = $5,
             lifecycle_state = $6::catalog_library_lifecycle_state,
             include_document_hint_in_mcp_answers = $7,
             source_truth_version = greatest(
                coalesce(source_truth_version, 0) + 1,
                (extract(epoch from clock_timestamp()) * 1000000)::bigint
             ),
             updated_at = now()
         where id = $1
         returning id, workspace_id, slug, display_name, description, extraction_prompt, web_ingest_policy, recognition_policy, retrieval_config, lifecycle_state::text as lifecycle_state, include_document_hint_in_mcp_answers, coalesce(chunking_template, 'naive') as chunking_template, created_at, updated_at",
    )
    .bind(library_id)
    .bind(slug)
    .bind(display_name)
    .bind(description)
    .bind(extraction_prompt)
    .bind(lifecycle_state)
    .bind(include_document_hint_in_mcp_answers)
    .fetch_optional(postgres)
    .await
}

pub async fn update_library_web_ingest_policy(
    postgres: &PgPool,
    library_id: Uuid,
    web_ingest_policy: Value,
) -> Result<Option<CatalogLibraryRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogLibraryRow>(
        "update catalog_library
         set web_ingest_policy = $2,
             updated_at = now()
         where id = $1
         returning id, workspace_id, slug, display_name, description, extraction_prompt, web_ingest_policy, recognition_policy, retrieval_config, lifecycle_state::text as lifecycle_state, include_document_hint_in_mcp_answers, coalesce(chunking_template, 'naive') as chunking_template, created_at, updated_at",
    )
    .bind(library_id)
    .bind(web_ingest_policy)
    .fetch_optional(postgres)
    .await
}

pub async fn update_library_recognition_policy(
    postgres: &PgPool,
    library_id: Uuid,
    recognition_policy: Value,
) -> Result<Option<CatalogLibraryRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogLibraryRow>(
        "update catalog_library
         set recognition_policy = $2,
             updated_at = now()
         where id = $1
         returning id, workspace_id, slug, display_name, description, extraction_prompt, web_ingest_policy, recognition_policy, retrieval_config, lifecycle_state::text as lifecycle_state, include_document_hint_in_mcp_answers, coalesce(chunking_template, 'naive') as chunking_template, created_at, updated_at",
    )
    .bind(library_id)
    .bind(recognition_policy)
    .fetch_optional(postgres)
    .await
}

pub async fn update_library_retrieval_config(
    postgres: &PgPool,
    library_id: Uuid,
    retrieval_config: Value,
) -> Result<Option<CatalogLibraryRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogLibraryRow>(
        "update catalog_library
         set retrieval_config = $2,
             source_truth_version = greatest(
                coalesce(source_truth_version, 0) + 1,
                (extract(epoch from clock_timestamp()) * 1000000)::bigint
             ),
             updated_at = now()
         where id = $1
         returning id, workspace_id, slug, display_name, description, extraction_prompt, web_ingest_policy, recognition_policy, retrieval_config, lifecycle_state::text as lifecycle_state, include_document_hint_in_mcp_answers, coalesce(chunking_template, 'naive') as chunking_template, created_at, updated_at",
    )
    .bind(library_id)
    .bind(retrieval_config)
    .fetch_optional(postgres)
    .await
}

pub async fn archive_library(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<Option<CatalogLibraryRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogLibraryRow>(
        "update catalog_library
         set lifecycle_state = 'archived',
             source_truth_version = greatest(
                coalesce(source_truth_version, 0) + 1,
                (extract(epoch from clock_timestamp()) * 1000000)::bigint
             ),
             updated_at = now()
         where id = $1
         returning id, workspace_id, slug, display_name, description, extraction_prompt, web_ingest_policy, recognition_policy, retrieval_config, lifecycle_state::text as lifecycle_state, include_document_hint_in_mcp_answers, coalesce(chunking_template, 'naive') as chunking_template, created_at, updated_at",
    )
    .bind(library_id)
    .fetch_optional(postgres)
    .await
}

/// Acquires the parent-row lock required before a lifecycle producer locks any
/// document, ingest-attempt, or ingest-job row that can later emit an outbox
/// event. Catalog deletion takes `FOR UPDATE` on the same row first; taking this
/// conflicting lock first everywhere gives both paths one global lock order
/// and prevents parent/child deadlocks. `NO KEY UPDATE` is intentional: these
/// short producer transactions can later touch a child head row and advance
/// the library source generation. Taking write strength before any child lock
/// prevents a parent/head lock-upgrade cycle with concurrent deletion.
///
/// Returns `false` when the library is missing or does not belong to the
/// supplied workspace. Callers must stop the producer transaction in that
/// case rather than continuing with child-row state changes.
///
/// # Errors
/// Returns any `SQLx` error raised while acquiring the parent-row lock.
pub async fn lock_library_for_lifecycle_event_with_executor<'e, E>(
    executor: E,
    workspace_id: Uuid,
    library_id: Uuid,
) -> Result<bool, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Postgres>,
{
    let locked = sqlx::query_scalar::<_, Uuid>(
        "select id
         from catalog_library
         where id = $1 and workspace_id = $2
         for no key update",
    )
    .bind(library_id)
    .bind(workspace_id)
    .fetch_optional(executor)
    .await?;
    Ok(locked.is_some())
}

pub async fn delete_library(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<CatalogLibraryDeleteOutcome, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    sqlx::query(
        "select pg_advisory_xact_lock(
            hashtextextended('ironrag:ai-config-generation', 0)
         )",
    )
    .execute(&mut *transaction)
    .await?;
    let locked_library_id = sqlx::query_scalar::<_, Uuid>(
        "select id
         from catalog_library
         where id = $1
         for update",
    )
    .bind(library_id)
    .fetch_optional(&mut *transaction)
    .await?;

    if locked_library_id.is_none() {
        transaction.rollback().await?;
        return Ok(CatalogLibraryDeleteOutcome::NotFound);
    }

    let library_ids = [library_id];
    let blockers = lock_catalog_delete_blockers(&mut transaction, &library_ids).await?;
    if !blockers.is_empty() {
        transaction.rollback().await?;
        return Ok(CatalogLibraryDeleteOutcome::Blocked(blockers));
    }

    let result = sqlx::query("delete from catalog_library where id = $1")
        .bind(library_id)
        .execute(&mut *transaction)
        .await?;
    if result.rows_affected() != 1 {
        return Err(sqlx::Error::Protocol(
            "locked catalog library disappeared before deletion".to_string(),
        ));
    }
    transaction.commit().await?;
    Ok(CatalogLibraryDeleteOutcome::Deleted)
}

async fn lock_catalog_delete_blockers(
    transaction: &mut Transaction<'_, Postgres>,
    library_ids: &[Uuid],
) -> Result<CatalogDeleteBlockers, sqlx::Error> {
    if library_ids.is_empty() {
        return Ok(CatalogDeleteBlockers {
            undispatched_webhook_lifecycle_events: 0,
            active_webhook_delivery_jobs: 0,
            unresolved_webhook_delivery_attempts: 0,
        });
    }

    // Lock every unresolved delivery job before evaluating the scope. Parent
    // row locks prevent new FK-backed jobs or outbox rows from being inserted.
    // Only the two non-resumable terminal queue states are allowlisted; a new
    // enum value therefore fails closed until its lifecycle is reviewed here.
    let active_webhook_delivery_jobs = sqlx::query_scalar::<_, i64>(
        "with blockers as materialized (
            select id
            from ingest_job
            where job_kind = 'webhook_delivery'
              and queue_state::text not in ('completed', 'canceled')
              and (
                  library_id = any($1)
                  or id in (
                      select job_id
                      from webhook_delivery_attempt
                      where library_id = any($1) and job_id is not null
                  )
              )
            for update
         )
         select count(*)::bigint from blockers",
    )
    .bind(library_ids)
    .fetch_one(&mut **transaction)
    .await?;
    let unresolved_webhook_delivery_attempts = sqlx::query_scalar::<_, i64>(
        "with blockers as materialized (
            select delivery.id
            from webhook_delivery_attempt delivery
            where delivery.library_id = any($1)
              and (
                  delivery.delivery_state in ('pending', 'delivering')
                  or (
                      delivery.delivery_state = 'failed'
                      and delivery.next_attempt_at is not null
                  )
              )
              and not exists (
                  select 1
                  from ingest_job job
                  where job.id = delivery.job_id
                    and job.job_kind = 'webhook_delivery'
                    and job.queue_state::text not in ('completed', 'canceled')
              )
            for update
         )
         select count(*)::bigint from blockers",
    )
    .bind(library_ids)
    .fetch_one(&mut **transaction)
    .await?;
    let undispatched_webhook_lifecycle_events = sqlx::query_scalar::<_, i64>(
        "with blockers as materialized (
            select id
            from webhook_lifecycle_outbox
            where library_id = any($1)
              and dispatch_state not in ('dispatched', 'resolved')
            for update
         )
         select count(*)::bigint from blockers",
    )
    .bind(library_ids)
    .fetch_one(&mut **transaction)
    .await?;

    Ok(CatalogDeleteBlockers {
        undispatched_webhook_lifecycle_events: usize::try_from(
            undispatched_webhook_lifecycle_events,
        )
        .map_err(|_| sqlx::Error::Protocol("negative webhook outbox blocker count".to_string()))?,
        active_webhook_delivery_jobs: usize::try_from(active_webhook_delivery_jobs).map_err(
            |_| sqlx::Error::Protocol("negative webhook delivery blocker count".to_string()),
        )?,
        unresolved_webhook_delivery_attempts: usize::try_from(unresolved_webhook_delivery_attempts)
            .map_err(|_| {
                sqlx::Error::Protocol("negative webhook delivery-attempt blocker count".to_string())
            })?,
    })
}

pub async fn list_connectors_by_library(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<Vec<CatalogLibraryConnectorRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogLibraryConnectorRow>(
        "select
            id,
            workspace_id,
            library_id,
            connector_kind::text as connector_kind,
            display_name,
            configuration_json,
            sync_mode::text as sync_mode,
            last_sync_requested_at,
            created_at,
            updated_at
         from catalog_library_connector
         where library_id = $1
         order by created_at desc",
    )
    .bind(library_id)
    .fetch_all(postgres)
    .await
}

pub async fn get_connector_by_id(
    postgres: &PgPool,
    connector_id: Uuid,
) -> Result<Option<CatalogLibraryConnectorRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogLibraryConnectorRow>(
        "select
            id,
            workspace_id,
            library_id,
            connector_kind::text as connector_kind,
            display_name,
            configuration_json,
            sync_mode::text as sync_mode,
            last_sync_requested_at,
            created_at,
            updated_at
         from catalog_library_connector
         where id = $1",
    )
    .bind(connector_id)
    .fetch_optional(postgres)
    .await
}

pub async fn create_connector(
    postgres: &PgPool,
    workspace_id: Uuid,
    library_id: Uuid,
    connector_kind: &str,
    display_name: &str,
    configuration_json: Value,
    sync_mode: &str,
    created_by_principal_id: Option<Uuid>,
) -> Result<CatalogLibraryConnectorRow, sqlx::Error> {
    sqlx::query_as::<_, CatalogLibraryConnectorRow>(
        "insert into catalog_library_connector (
            id,
            workspace_id,
            library_id,
            connector_kind,
            display_name,
            configuration_json,
            sync_mode,
            last_sync_requested_at,
            created_by_principal_id,
            created_at,
            updated_at
        )
        values (
            $1,
            $2,
            $3,
            $4::catalog_connector_kind,
            $5,
            $6,
            $7::catalog_connector_sync_mode,
            null,
            $8,
            now(),
            now()
        )
        returning
            id,
            workspace_id,
            library_id,
            connector_kind::text as connector_kind,
            display_name,
            configuration_json,
            sync_mode::text as sync_mode,
            last_sync_requested_at,
            created_at,
            updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(workspace_id)
    .bind(library_id)
    .bind(connector_kind)
    .bind(display_name)
    .bind(configuration_json)
    .bind(sync_mode)
    .bind(created_by_principal_id)
    .fetch_one(postgres)
    .await
}

pub async fn update_connector(
    postgres: &PgPool,
    connector_id: Uuid,
    display_name: &str,
    configuration_json: Value,
    sync_mode: &str,
    last_sync_requested_at: Option<DateTime<Utc>>,
) -> Result<Option<CatalogLibraryConnectorRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogLibraryConnectorRow>(
        "update catalog_library_connector
         set display_name = $2,
             configuration_json = $3,
             sync_mode = $4::catalog_connector_sync_mode,
             last_sync_requested_at = $5,
             updated_at = now()
         where id = $1
         returning
            id,
            workspace_id,
            library_id,
            connector_kind::text as connector_kind,
            display_name,
            configuration_json,
            sync_mode::text as sync_mode,
            last_sync_requested_at,
            created_at,
            updated_at",
    )
    .bind(connector_id)
    .bind(display_name)
    .bind(configuration_json)
    .bind(sync_mode)
    .bind(last_sync_requested_at)
    .fetch_optional(postgres)
    .await
}

#[cfg(test)]
mod tests {
    use super::QUERY_AUTHORIZED_LIBRARY_CANDIDATES_SQL;

    #[test]
    fn query_authorized_library_candidates_are_active_minimal_and_bounded() {
        let normalized = QUERY_AUTHORIZED_LIBRARY_CANDIDATES_SQL.to_ascii_lowercase();

        assert!(normalized.contains("select id"));
        assert!(normalized.contains("lifecycle_state = 'active'"));
        assert!(normalized.contains("workspace_id = any($2::uuid[])"));
        assert!(normalized.contains("id = any($3::uuid[])"));
        assert!(normalized.contains("limit 2"));
        assert!(!normalized.contains("extraction_prompt"));
        assert!(!normalized.contains("retrieval_config"));
        assert!(!normalized.contains("select *"));
    }
}
