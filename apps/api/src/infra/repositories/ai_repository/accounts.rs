use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;
use zeroize::Zeroize as _;

use crate::shared::secret_encryption::{EncryptedSecret, SecretPurpose};

fn validate_encrypted_api_key_context(
    account_id: Uuid,
    api_key: Option<&EncryptedSecret>,
) -> Result<(), sqlx::Error> {
    if api_key.is_some_and(|secret| !secret.is_bound_to(SecretPurpose::AiAccountApiKey, account_id))
    {
        return Err(sqlx::Error::Protocol(
            "encrypted AI account credential context does not match target row".to_string(),
        ));
    }
    Ok(())
}

#[derive(FromRow)]
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

impl Drop for AiAccountRow {
    fn drop(&mut self) {
        if let Some(api_key) = self.api_key.as_mut() {
            api_key.zeroize();
        }
    }
}

impl std::fmt::Debug for AiAccountRow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AiAccountRow")
            .field("id", &self.id)
            .field("scope_kind", &self.scope_kind)
            .field("workspace_id", &self.workspace_id)
            .field("library_id", &self.library_id)
            .field("provider_catalog_id", &self.provider_catalog_id)
            .field("label", &self.label)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("base_url", &self.base_url.as_ref().map(|_| "<redacted>"))
            .field("credential_state", &self.credential_state)
            .field("created_by_principal_id", &self.created_by_principal_id)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

/// Secret-free projection used by administrative list and availability views.
///
/// `has_api_key` preserves the UI's configured/not-configured state without
/// selecting ciphertext from `PostgreSQL` or creating plaintext in the process.
#[derive(Debug, Clone, FromRow)]
pub struct AiAccountSummaryRow {
    pub id: Uuid,
    pub scope_kind: String,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub provider_catalog_id: Uuid,
    pub label: String,
    pub base_url: Option<String>,
    pub credential_state: String,
    pub has_api_key: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub async fn list_account_summaries_exact(
    postgres: &PgPool,
    scope_kind: &str,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
) -> Result<Vec<AiAccountSummaryRow>, sqlx::Error> {
    sqlx::query_as::<_, AiAccountSummaryRow>(
        "select
            id,
            scope_kind::text as scope_kind,
            workspace_id,
            library_id,
            provider_catalog_id,
            label,
            base_url,
            credential_state::text as credential_state,
            (api_key is not null and btrim(api_key) <> '') as has_api_key,
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

pub async fn list_visible_account_summaries(
    postgres: &PgPool,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
) -> Result<Vec<AiAccountSummaryRow>, sqlx::Error> {
    sqlx::query_as::<_, AiAccountSummaryRow>(
        "select
            account.id,
            account.scope_kind::text as scope_kind,
            account.workspace_id,
            account.library_id,
            account.provider_catalog_id,
            account.label,
            account.base_url,
            account.credential_state::text as credential_state,
            (account.api_key is not null and btrim(account.api_key) <> '') as has_api_key,
            account.created_at,
            account.updated_at
         from ai_account account
         left join catalog_library requested_library on requested_library.id = $2
         where account.scope_kind = 'instance'
            or (
                account.scope_kind = 'workspace'
                and account.workspace_id = coalesce($1, requested_library.workspace_id)
            )
            or (account.scope_kind = 'library' and account.library_id = $2)
         order by account.created_at desc, account.id desc",
    )
    .bind(workspace_id)
    .bind(library_id)
    .fetch_all(postgres)
    .await
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
    account_id: Uuid,
    scope_kind: &str,
    workspace_id: Option<Uuid>,
    library_id: Option<Uuid>,
    provider_catalog_id: Uuid,
    label: &str,
    api_key: Option<&EncryptedSecret>,
    base_url: Option<&str>,
    created_by_principal_id: Option<Uuid>,
) -> Result<AiAccountRow, sqlx::Error> {
    validate_encrypted_api_key_context(account_id, api_key)?;
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
    .bind(account_id)
    .bind(scope_kind)
    .bind(workspace_id)
    .bind(library_id)
    .bind(provider_catalog_id)
    .bind(label)
    .bind(api_key.map(EncryptedSecret::as_str))
    .bind(base_url)
    .bind(created_by_principal_id)
    .fetch_one(postgres)
    .await
}

pub async fn update_account(
    postgres: &PgPool,
    account_id: Uuid,
    label: &str,
    api_key: Option<&EncryptedSecret>,
    base_url: Option<&str>,
    credential_state: &str,
) -> Result<Option<AiAccountRow>, sqlx::Error> {
    validate_encrypted_api_key_context(account_id, api_key)?;
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
    .bind(api_key.map(EncryptedSecret::as_str))
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use sqlx::postgres::PgPoolOptions;

    use super::*;
    use crate::shared::secret_encryption::{CredentialCipher, SecretPurpose};

    #[tokio::test]
    async fn mismatched_encrypted_account_context_fails_before_database_io() {
        let pool = PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(10))
            .connect_lazy("postgresql://127.0.0.1:9/ironrag_unreachable")
            .expect("lazy pool configuration should parse");
        let encoded_key = STANDARD.encode([73_u8; 32]);
        let cipher = CredentialCipher::from_optional_base64(Some(&encoded_key)).expect("valid key");
        let encrypted_for_another_row = cipher
            .encrypt(SecretPurpose::AiAccountApiKey, Uuid::now_v7(), "synthetic-value")
            .expect("encrypt synthetic value");

        let error = create_account(
            &pool,
            Uuid::now_v7(),
            "instance",
            None,
            None,
            Uuid::now_v7(),
            "synthetic",
            Some(&encrypted_for_another_row),
            None,
            None,
        )
        .await
        .expect_err("mismatched row-bound ciphertext must fail before database access");

        assert!(matches!(error, sqlx::Error::Protocol(_)));
        assert_eq!(pool.size(), 0);
    }
}
