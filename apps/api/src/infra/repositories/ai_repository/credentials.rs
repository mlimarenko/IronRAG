use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct AiProviderCredentialRow {
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

pub async fn list_provider_credentials_exact(
    postgres: &PgPool,
    scope_kind: &str,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
) -> Result<Vec<AiProviderCredentialRow>, sqlx::Error> {
    sqlx::query_as::<_, AiProviderCredentialRow>(
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
         from ai_provider_credential
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

pub async fn list_visible_provider_credentials(
    postgres: &PgPool,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
) -> Result<Vec<AiProviderCredentialRow>, sqlx::Error> {
    match (workspace_id, library_id) {
        (Some(workspace_id), Some(library_id)) => {
            sqlx::query_as::<_, AiProviderCredentialRow>(
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
                 from ai_provider_credential
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
            sqlx::query_as::<_, AiProviderCredentialRow>(
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
                 from ai_provider_credential
                 where scope_kind = 'instance'
                    or (scope_kind = 'workspace' and workspace_id = $1)
                 order by created_at desc, id desc",
            )
            .bind(workspace_id)
            .fetch_all(postgres)
            .await
        }
        (None, None) => {
            sqlx::query_as::<_, AiProviderCredentialRow>(
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
                 from ai_provider_credential
                 where scope_kind = 'instance'
                 order by created_at desc, id desc",
            )
            .fetch_all(postgres)
            .await
        }
        (None, Some(library_id)) => {
            sqlx::query_as::<_, AiProviderCredentialRow>(
                "select
                    credential.id,
                    credential.scope_kind::text as scope_kind,
                    credential.workspace_id,
                    credential.library_id,
                    credential.provider_catalog_id,
                    credential.label,
                    credential.api_key,
                    credential.base_url,
                    credential.credential_state::text as credential_state,
                    credential.created_by_principal_id,
                    credential.created_at,
                    credential.updated_at
                 from ai_provider_credential credential
                 join catalog_library library on library.id = $1
                 where credential.scope_kind = 'instance'
                    or (credential.scope_kind = 'workspace' and credential.workspace_id = library.workspace_id)
                    or (credential.scope_kind = 'library' and credential.library_id = library.id)
                 order by credential.created_at desc, credential.id desc",
            )
            .bind(library_id)
            .fetch_all(postgres)
            .await
        }
    }
}

pub async fn get_provider_credential_by_id(
    postgres: &PgPool,
    credential_id: Uuid,
) -> Result<Option<AiProviderCredentialRow>, sqlx::Error> {
    sqlx::query_as::<_, AiProviderCredentialRow>(
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
         from ai_provider_credential
         where id = $1",
    )
    .bind(credential_id)
    .fetch_optional(postgres)
    .await
}

pub async fn create_provider_credential(
    postgres: &PgPool,
    scope_kind: &str,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
    provider_catalog_id: Uuid,
    label: &str,
    api_key: Option<&str>,
    base_url: Option<&str>,
    created_by_principal_id: Option<Uuid>,
) -> Result<AiProviderCredentialRow, sqlx::Error> {
    sqlx::query_as::<_, AiProviderCredentialRow>(
        "insert into ai_provider_credential (
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

pub async fn update_provider_credential(
    postgres: &PgPool,
    credential_id: Uuid,
    label: &str,
    api_key: Option<&str>,
    base_url: Option<&str>,
    credential_state: &str,
) -> Result<Option<AiProviderCredentialRow>, sqlx::Error> {
    sqlx::query_as::<_, AiProviderCredentialRow>(
        "update ai_provider_credential
         set label = $2,
             api_key = coalesce($3, api_key),
             base_url = coalesce($4, base_url),
             credential_state = $5::ai_credential_state,
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
    .bind(credential_id)
    .bind(label)
    .bind(api_key)
    .bind(base_url)
    .bind(credential_state)
    .fetch_optional(postgres)
    .await
}
