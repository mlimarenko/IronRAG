use super::*;

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
        NewCatalogLibrary {
            workspace_id,
            slug,
            display_name,
            description,
            recognition_policy: serde_json::json!({ "rasterImageEngine": "vision" }),
            created_by_principal_id,
        },
    )
    .await
}

/// Inputs for creating one `catalog_library` row.
#[derive(Debug, Clone)]
pub struct NewCatalogLibrary<'a> {
    pub workspace_id: Uuid,
    pub slug: &'a str,
    pub display_name: &'a str,
    pub description: Option<&'a str>,
    pub recognition_policy: Value,
    pub created_by_principal_id: Option<Uuid>,
}

pub async fn create_library_with_recognition_policy(
    postgres: &PgPool,
    library: NewCatalogLibrary<'_>,
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
        returning id, workspace_id, slug, display_name, description, extraction_prompt, web_ingest_policy, recognition_policy, retrieval_config, lifecycle_state::text as lifecycle_state, include_document_hint_in_mcp_answers, chunking_template, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(library.workspace_id)
    .bind(library.slug)
    .bind(library.display_name)
    .bind(library.description)
    .bind(library.recognition_policy)
    .bind(library.created_by_principal_id)
    .fetch_one(postgres)
    .await
}

pub async fn create_library_with_recognition_policy_with_transaction(
    transaction: &mut PgConnection,
    library: NewCatalogLibrary<'_>,
    provisioned_by_principal_id: Uuid,
) -> Result<Option<CatalogLibraryRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogLibraryRow>(
        "insert into catalog_library (
            id, workspace_id, slug, display_name, description, recognition_policy,
            lifecycle_state, created_by_principal_id, provisioned_by_principal_id,
            created_at, updated_at
         ) values ($1, $2, $3, $4, $5, $6, 'active', $7, $8, now(), now())
         on conflict (workspace_id, slug) do nothing
         returning id, workspace_id, slug, display_name, description, extraction_prompt,
            web_ingest_policy, recognition_policy, retrieval_config,
            lifecycle_state::text as lifecycle_state, include_document_hint_in_mcp_answers,
            chunking_template, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(library.workspace_id)
    .bind(library.slug)
    .bind(library.display_name)
    .bind(library.description)
    .bind(library.recognition_policy)
    .bind(library.created_by_principal_id)
    .bind(provisioned_by_principal_id)
    .fetch_optional(&mut *transaction)
    .await
}

pub async fn get_library_by_workspace_and_slug_with_transaction(
    transaction: &mut PgConnection,
    workspace_id: Uuid,
    slug: &str,
) -> Result<Option<CatalogLibraryRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogLibraryRow>(
        "select id, workspace_id, slug, display_name, description, extraction_prompt,
            web_ingest_policy, recognition_policy, retrieval_config,
            lifecycle_state::text as lifecycle_state, include_document_hint_in_mcp_answers,
            chunking_template, created_at, updated_at
         from catalog_library
         where workspace_id = $1 and slug = $2
         for update",
    )
    .bind(workspace_id)
    .bind(slug)
    .fetch_optional(&mut *transaction)
    .await
}

pub async fn count_active_libraries_with_transaction(
    transaction: &mut PgConnection,
    workspace_id: Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "select count(*) from catalog_library
         where workspace_id = $1 and lifecycle_state = 'active'",
    )
    .bind(workspace_id)
    .fetch_one(&mut *transaction)
    .await
}

pub async fn lock_provisioned_library(
    transaction: &mut PgConnection,
    library_id: Uuid,
    workspace_id: Uuid,
    provisioned_by_principal_id: Uuid,
) -> Result<Option<CatalogLibraryRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogLibraryRow>(
        "select id, workspace_id, slug, display_name, description, extraction_prompt,
            web_ingest_policy, recognition_policy, retrieval_config,
            lifecycle_state::text as lifecycle_state, include_document_hint_in_mcp_answers,
            chunking_template, created_at, updated_at
         from catalog_library
         where id = $1
           and workspace_id = $2
           and provisioned_by_principal_id = $3
           and lifecycle_state = 'active'
           and not exists (
             select 1
             from ops_async_operation
             where subject_kind = 'catalog_library'
               and subject_id = catalog_library.id
               and operation_kind = 'delete_library'
               and status in ('accepted', 'processing')
           )
         for update",
    )
    .bind(library_id)
    .bind(workspace_id)
    .bind(provisioned_by_principal_id)
    .fetch_optional(&mut *transaction)
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

/// Inputs for [`update_library`].
#[derive(Debug, Clone)]
pub struct CatalogLibraryUpdate<'a> {
    pub library_id: Uuid,
    pub slug: &'a str,
    pub display_name: &'a str,
    pub description: Option<&'a str>,
    pub extraction_prompt: Option<&'a str>,
    pub lifecycle_state: &'a str,
    pub include_document_hint_in_mcp_answers: bool,
}

pub async fn update_library(
    postgres: &PgPool,
    library: CatalogLibraryUpdate<'_>,
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
         returning id, workspace_id, slug, display_name, description, extraction_prompt, web_ingest_policy, recognition_policy, retrieval_config, lifecycle_state::text as lifecycle_state, include_document_hint_in_mcp_answers, chunking_template, created_at, updated_at",
    )
    .bind(library.library_id)
    .bind(library.slug)
    .bind(library.display_name)
    .bind(library.description)
    .bind(library.extraction_prompt)
    .bind(library.lifecycle_state)
    .bind(library.include_document_hint_in_mcp_answers)
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
         returning id, workspace_id, slug, display_name, description, extraction_prompt, web_ingest_policy, recognition_policy, retrieval_config, lifecycle_state::text as lifecycle_state, include_document_hint_in_mcp_answers, chunking_template, created_at, updated_at",
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
         returning id, workspace_id, slug, display_name, description, extraction_prompt, web_ingest_policy, recognition_policy, retrieval_config, lifecycle_state::text as lifecycle_state, include_document_hint_in_mcp_answers, chunking_template, created_at, updated_at",
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
         returning id, workspace_id, slug, display_name, description, extraction_prompt, web_ingest_policy, recognition_policy, retrieval_config, lifecycle_state::text as lifecycle_state, include_document_hint_in_mcp_answers, chunking_template, created_at, updated_at",
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
         returning id, workspace_id, slug, display_name, description, extraction_prompt, web_ingest_policy, recognition_policy, retrieval_config, lifecycle_state::text as lifecycle_state, include_document_hint_in_mcp_answers, chunking_template, created_at, updated_at",
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
