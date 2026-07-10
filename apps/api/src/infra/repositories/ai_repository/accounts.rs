use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct AiAccountRow {
    pub id: Uuid,
    pub scope_kind: String,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub provider_catalog_id: Uuid,
    pub label: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub credential_state: String,
    pub created_by_principal_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub async fn list_accounts_exact(
    postgres: &PgPool,
    scope_kind: &str,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
) -> Result<Vec<AiAccountRow>, sqlx::Error> {
    sqlx::query_as::<_, AiAccountRow>(
        "select
            id,
            scope_kind::text as scope_kind,
            workspace_id,
            library_id,
            provider_catalog_id,
            label,
            api_key,
            base_url,
            credential_state::text as credential_state,
            created_by_principal_id,
            created_at,
            updated_at
         from ai_account
         where scope_kind = $1::ai_scope_kind
           and workspace_id is not distinct from $2
           and library_id is not distinct from $3
         order by created_at desc, id desc",
    )
    .bind(scope_kind)
    .bind(workspace_id)
    .bind(library_id)
    .fetch_all(postgres)
    .await
}

pub async fn list_accounts_by_provider_and_label(
    postgres: &PgPool,
    provider_catalog_id: Uuid,
    label: &str,
) -> Result<Vec<AiAccountRow>, sqlx::Error> {
    sqlx::query_as::<_, AiAccountRow>(
        "select
            id,
            scope_kind::text as scope_kind,
            workspace_id,
            library_id,
            provider_catalog_id,
            label,
            api_key,
            base_url,
            credential_state::text as credential_state,
            created_by_principal_id,
            created_at,
            updated_at
         from ai_account
         where provider_catalog_id = $1
           and label = $2
         order by created_at, id",
    )
    .bind(provider_catalog_id)
    .bind(label)
    .fetch_all(postgres)
    .await
}

pub async fn list_visible_accounts(
    postgres: &PgPool,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
) -> Result<Vec<AiAccountRow>, sqlx::Error> {
    match (workspace_id, library_id) {
        (Some(workspace_id), Some(library_id)) => {
            sqlx::query_as::<_, AiAccountRow>(
                "select
                    id,
                    scope_kind::text as scope_kind,
                    workspace_id,
                    library_id,
                    provider_catalog_id,
                    label,
                    api_key,
                    base_url,
                    credential_state::text as credential_state,
                    created_by_principal_id,
                    created_at,
                    updated_at
                 from ai_account
                 where scope_kind = 'instance'
                    or (scope_kind = 'workspace' and workspace_id = $1)
                    or (scope_kind = 'library' and library_id = $2)
                 order by created_at desc, id desc",
            )
            .bind(workspace_id)
            .bind(library_id)
            .fetch_all(postgres)
            .await
        }
        (Some(workspace_id), None) => {
            sqlx::query_as::<_, AiAccountRow>(
                "select
                    id,
                    scope_kind::text as scope_kind,
                    workspace_id,
                    library_id,
                    provider_catalog_id,
                    label,
                    api_key,
                    base_url,
                    credential_state::text as credential_state,
                    created_by_principal_id,
                    created_at,
                    updated_at
                 from ai_account
                 where scope_kind = 'instance'
                    or (scope_kind = 'workspace' and workspace_id = $1)
                 order by created_at desc, id desc",
            )
            .bind(workspace_id)
            .fetch_all(postgres)
            .await
        }
        (None, None) => {
            sqlx::query_as::<_, AiAccountRow>(
                "select
                    id,
                    scope_kind::text as scope_kind,
                    workspace_id,
                    library_id,
                    provider_catalog_id,
                    label,
                    api_key,
                    base_url,
                    credential_state::text as credential_state,
                    created_by_principal_id,
                    created_at,
                    updated_at
                 from ai_account
                 where scope_kind = 'instance'
                 order by created_at desc, id desc",
            )
            .fetch_all(postgres)
            .await
        }
        (None, Some(library_id)) => {
            sqlx::query_as::<_, AiAccountRow>(
                "select
                    account.id,
                    account.scope_kind::text as scope_kind,
                    account.workspace_id,
                    account.library_id,
                    account.provider_catalog_id,
                    account.label,
                    account.api_key,
                    account.base_url,
                    account.credential_state::text as credential_state,
                    account.created_by_principal_id,
                    account.created_at,
                    account.updated_at
                 from ai_account account
                 join catalog_library library on library.id = $1
                 where account.scope_kind = 'instance'
                    or (account.scope_kind = 'workspace' and account.workspace_id = library.workspace_id)
                    or (account.scope_kind = 'library' and account.library_id = library.id)
                 order by account.created_at desc, account.id desc",
            )
            .bind(library_id)
            .fetch_all(postgres)
            .await
        }
    }
}

pub async fn get_account_by_id(
    postgres: &PgPool,
    account_id: Uuid,
) -> Result<Option<AiAccountRow>, sqlx::Error> {
    sqlx::query_as::<_, AiAccountRow>(
        "select
            id,
            scope_kind::text as scope_kind,
            workspace_id,
            library_id,
            provider_catalog_id,
            label,
            api_key,
            base_url,
            credential_state::text as credential_state,
            created_by_principal_id,
            created_at,
            updated_at
         from ai_account
         where id = $1",
    )
    .bind(account_id)
    .fetch_optional(postgres)
    .await
}

pub async fn create_account(
    postgres: &PgPool,
    scope_kind: &str,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
    provider_catalog_id: Uuid,
    label: &str,
    api_key: Option<&str>,
    base_url: Option<&str>,
    created_by_principal_id: Option<Uuid>,
) -> Result<AiAccountRow, sqlx::Error> {
    sqlx::query_as::<_, AiAccountRow>(
        "insert into ai_account (
            id,
            scope_kind,
            workspace_id,
            library_id,
            provider_catalog_id,
            label,
            api_key,
            base_url,
            credential_state,
            created_by_principal_id,
            created_at,
            updated_at
        )
        values ($1, $2::ai_scope_kind, $3, $4, $5, $6, $7, $8, 'active', $9, now(), now())
        returning
            id,
            scope_kind::text as scope_kind,
            workspace_id,
            library_id,
            provider_catalog_id,
            label,
            api_key,
            base_url,
            credential_state::text as credential_state,
            created_by_principal_id,
            created_at,
            updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(scope_kind)
    .bind(workspace_id)
    .bind(library_id)
    .bind(provider_catalog_id)
    .bind(label)
    .bind(api_key)
    .bind(base_url)
    .bind(created_by_principal_id)
    .fetch_one(postgres)
    .await
}

pub async fn update_account(
    postgres: &PgPool,
    account_id: Uuid,
    label: &str,
    api_key: Option<&str>,
    base_url: Option<&str>,
    credential_state: &str,
) -> Result<Option<AiAccountRow>, sqlx::Error> {
    sqlx::query_as::<_, AiAccountRow>(
        "update ai_account
         set label = $2,
             api_key = coalesce($3, api_key),
             base_url = $4,
             credential_state = $5::ai_account_state,
             updated_at = now()
         where id = $1
         returning
            id,
            scope_kind::text as scope_kind,
            workspace_id,
            library_id,
            provider_catalog_id,
            label,
            api_key,
            base_url,
            credential_state::text as credential_state,
            created_by_principal_id,
            created_at,
            updated_at",
    )
    .bind(account_id)
    .bind(label)
    .bind(api_key)
    .bind(base_url)
    .bind(credential_state)
    .fetch_optional(postgres)
    .await
}

pub async fn delete_account(postgres: &PgPool, account_id: Uuid) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("delete from ai_account where id = $1")
        .bind(account_id)
        .execute(postgres)
        .await?;
    Ok(result.rows_affected())
}
