use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{FromRow, PgPool};
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
    pub lifecycle_state: String,
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

pub async fn list_workspaces(postgres: &PgPool) -> Result<Vec<CatalogWorkspaceRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogWorkspaceRow>(
        "select id, slug, display_name, lifecycle_state, created_at, updated_at
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
        "select id, slug, display_name, lifecycle_state, created_at, updated_at
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
        "select id, slug, display_name, lifecycle_state, created_at, updated_at
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
        returning id, slug, display_name, lifecycle_state, created_at, updated_at",
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
         returning id, slug, display_name, lifecycle_state, created_at, updated_at",
    )
    .bind(workspace_id)
    .bind(slug)
    .bind(display_name)
    .bind(lifecycle_state)
    .fetch_optional(postgres)
    .await
}

pub async fn list_libraries(
    postgres: &PgPool,
    workspace_id: Option<Uuid>,
) -> Result<Vec<CatalogLibraryRow>, sqlx::Error> {
    match workspace_id {
        Some(workspace_id) => {
            sqlx::query_as::<_, CatalogLibraryRow>(
                "select id, workspace_id, slug, display_name, description, lifecycle_state, created_at, updated_at
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
                "select id, workspace_id, slug, display_name, description, lifecycle_state, created_at, updated_at
                 from catalog_library
                 order by created_at desc",
            )
            .fetch_all(postgres)
            .await
        }
    }
}

pub async fn get_library_by_id(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<Option<CatalogLibraryRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogLibraryRow>(
        "select id, workspace_id, slug, display_name, description, lifecycle_state, created_at, updated_at
         from catalog_library
         where id = $1",
    )
    .bind(library_id)
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
    sqlx::query_as::<_, CatalogLibraryRow>(
        "insert into catalog_library (
            id,
            workspace_id,
            slug,
            display_name,
            description,
            lifecycle_state,
            created_by_principal_id,
            created_at,
            updated_at
        )
        values ($1, $2, $3, $4, $5, 'active', $6, now(), now())
        returning id, workspace_id, slug, display_name, description, lifecycle_state, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(workspace_id)
    .bind(slug)
    .bind(display_name)
    .bind(description)
    .bind(created_by_principal_id)
    .fetch_one(postgres)
    .await
}

pub async fn update_library(
    postgres: &PgPool,
    library_id: Uuid,
    slug: &str,
    display_name: &str,
    description: Option<&str>,
    lifecycle_state: &str,
) -> Result<Option<CatalogLibraryRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogLibraryRow>(
        "update catalog_library
         set slug = $2,
             display_name = $3,
             description = $4,
             lifecycle_state = $5::catalog_library_lifecycle_state,
             updated_at = now()
         where id = $1
         returning id, workspace_id, slug, display_name, description, lifecycle_state, created_at, updated_at",
    )
    .bind(library_id)
    .bind(slug)
    .bind(display_name)
    .bind(description)
    .bind(lifecycle_state)
    .fetch_optional(postgres)
    .await
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
