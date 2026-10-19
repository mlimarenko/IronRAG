use super::*;

/// Input for creating the shared hashed-token credential principal plus its
/// `iam_api_token` row. Shared by ordinary API tokens and owner-scoped service
/// identities so there is a single credential-creation shape.
#[derive(Debug, Clone)]
pub struct NewCredentialPrincipal<'a> {
    pub principal_kind: &'a str,
    pub workspace_id: Option<Uuid>,
    pub label: &'a str,
    pub token_prefix: &'a str,
    pub parent_principal_id: Option<Uuid>,
    pub issued_by_principal_id: Option<Uuid>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Partially updates an API token's mutable fields. `label` is left
/// untouched when `None`; `expires_at` is left untouched when `None` (the
/// PATCH body omitted the field) and set — including to `NULL`, clearing the
/// expiry — when `Some(_)`. The permission scope has no column here and is
/// intentionally not updatable through this function; see
/// `PatchTokenRequest`'s doc comment for why.
pub async fn update_api_token(
    postgres: &PgPool,
    principal_id: Uuid,
    label: Option<&str>,
    expires_at: Option<Option<DateTime<Utc>>>,
) -> Result<Option<IamApiTokenRow>, sqlx::Error> {
    let has_expires_at = expires_at.is_some();
    let expires_at_value = expires_at.flatten();
    sqlx::query_as::<_, IamApiTokenRow>(
        "update iam_api_token
         set label = coalesce($2, label),
             expires_at = case when $3 then $4 else expires_at end
         where principal_id = $1
         returning
            principal_id,
            workspace_id,
            label,
            token_prefix,
            status::text as status,
            expires_at,
            revoked_at,
            issued_by_principal_id,
            last_used_at",
    )
    .bind(principal_id)
    .bind(label)
    .bind(has_expires_at)
    .bind(expires_at_value)
    .fetch_optional(postgres)
    .await
}

pub async fn create_api_token(
    postgres: &PgPool,
    workspace_id: Option<Uuid>,
    label: &str,
    token_prefix: &str,
    issued_by_principal_id: Option<Uuid>,
    expires_at: Option<DateTime<Utc>>,
) -> Result<IamApiTokenRow, sqlx::Error> {
    let principal = create_principal(postgres, "api_token", label, issued_by_principal_id).await?;
    insert_api_token(
        postgres,
        principal.id,
        &NewCredentialPrincipal {
            principal_kind: "api_token",
            workspace_id,
            label,
            token_prefix,
            parent_principal_id: issued_by_principal_id,
            issued_by_principal_id,
            expires_at,
        },
    )
    .await
}

pub async fn create_api_token_with_transaction(
    transaction: &mut PgConnection,
    workspace_id: Option<Uuid>,
    label: &str,
    token_prefix: &str,
    issued_by_principal_id: Option<Uuid>,
    expires_at: Option<DateTime<Utc>>,
) -> Result<IamApiTokenRow, sqlx::Error> {
    create_credential_principal_with_transaction(
        transaction,
        &NewCredentialPrincipal {
            principal_kind: "api_token",
            workspace_id,
            label,
            token_prefix,
            parent_principal_id: issued_by_principal_id,
            issued_by_principal_id,
            expires_at,
        },
    )
    .await
}

/// Creates the shared hashed-token credential rows for a principal kind.
/// Ordinary API tokens use `api_token`; owner-scoped service identities use
/// `worker`. Both authenticate through the same `iam_api_token` /
/// `iam_api_token_secret` path, so there is no second credential verifier.
pub async fn create_credential_principal_with_transaction(
    transaction: &mut PgConnection,
    credential: &NewCredentialPrincipal<'_>,
) -> Result<IamApiTokenRow, sqlx::Error> {
    let principal = create_principal_with_transaction(
        transaction,
        credential.principal_kind,
        credential.label,
        credential.parent_principal_id,
    )
    .await?;
    insert_api_token(&mut *transaction, principal.id, credential).await
}

async fn insert_api_token<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    principal_id: Uuid,
    credential: &NewCredentialPrincipal<'_>,
) -> Result<IamApiTokenRow, sqlx::Error> {
    sqlx::query_as::<_, IamApiTokenRow>(
        "insert into iam_api_token (
            principal_id,
            workspace_id,
            label,
            token_prefix,
            status,
            expires_at,
            revoked_at,
            issued_by_principal_id,
            last_used_at
        )
        values ($1, $2, $3, $4, 'active', $5, null, $6, null)
        returning
            principal_id,
            workspace_id,
            label,
            token_prefix,
            status::text as status,
            expires_at,
            revoked_at,
            issued_by_principal_id,
            last_used_at",
    )
    .bind(principal_id)
    .bind(credential.workspace_id)
    .bind(credential.label)
    .bind(credential.token_prefix)
    .bind(credential.expires_at)
    .bind(credential.issued_by_principal_id)
    .fetch_one(executor)
    .await
}

pub async fn get_api_token_by_principal_id(
    postgres: &PgPool,
    principal_id: Uuid,
) -> Result<Option<IamApiTokenRow>, sqlx::Error> {
    sqlx::query_as::<_, IamApiTokenRow>(
        "select
            principal_id,
            workspace_id,
            label,
            token_prefix,
            status::text as status,
            expires_at,
            revoked_at,
            issued_by_principal_id,
            last_used_at
         from iam_api_token
         where principal_id = $1",
    )
    .bind(principal_id)
    .fetch_optional(postgres)
    .await
}

/// Loads only a user-managed ordinary API token. Service identities share the
/// credential tables but have their own owner-only management surface.
pub async fn get_regular_api_token_by_principal_id(
    postgres: &PgPool,
    principal_id: Uuid,
) -> Result<Option<IamApiTokenRow>, sqlx::Error> {
    sqlx::query_as::<_, IamApiTokenRow>(
        "select
            token.principal_id,
            token.workspace_id,
            token.label,
            token.token_prefix,
            token.status::text as status,
            token.expires_at,
            token.revoked_at,
            token.issued_by_principal_id,
            token.last_used_at
         from iam_api_token token
         join iam_principal principal on principal.id = token.principal_id
         where token.principal_id = $1
           and principal.principal_kind = 'api_token'",
    )
    .bind(principal_id)
    .fetch_optional(postgres)
    .await
}

pub async fn find_active_api_token_by_secret_hash(
    postgres: &PgPool,
    secret_hash: &str,
) -> Result<Option<AuthenticatedApiTokenRow>, sqlx::Error> {
    sqlx::query_as::<_, AuthenticatedApiTokenRow>(
        "select
            principal.id as principal_id,
            principal.principal_kind::text as principal_kind,
            principal.status::text as principal_status,
            principal.parent_principal_id,
                token.workspace_id,
                token.label,
                token.token_prefix,
                token.status::text as token_status,
                token.expires_at,
                token.last_used_at
         from iam_api_token_secret secret
         join iam_api_token token
           on token.principal_id = secret.token_principal_id
         join iam_principal principal
           on principal.id = token.principal_id
         where secret.secret_hash = $1
           and secret.revoked_at is null
           and token.status = 'active'
           and principal.status = 'active'
           and (token.expires_at is null or token.expires_at > now())
         order by secret.secret_version desc
         limit 1",
    )
    .bind(secret_hash)
    .fetch_optional(postgres)
    .await
}

pub async fn list_api_tokens(
    postgres: &PgPool,
    workspace_id: Option<Uuid>,
) -> Result<Vec<IamApiTokenRow>, sqlx::Error> {
    match workspace_id {
        Some(workspace_id) => {
            sqlx::query_as::<_, IamApiTokenRow>(
                "select
                    token.principal_id,
                    token.workspace_id,
                    token.label,
                    token.token_prefix,
                    token.status::text as status,
                    token.expires_at,
                    token.revoked_at,
                    token.issued_by_principal_id,
                    token.last_used_at
                 from iam_api_token token
                 join iam_principal principal on principal.id = token.principal_id
                 where token.workspace_id = $1
                   and principal.principal_kind = 'api_token'
                 order by token.label asc",
            )
            .bind(workspace_id)
            .fetch_all(postgres)
            .await
        }
        None => {
            sqlx::query_as::<_, IamApiTokenRow>(
                "select
                    token.principal_id,
                    token.workspace_id,
                    token.label,
                    token.token_prefix,
                    token.status::text as status,
                    token.expires_at,
                    token.revoked_at,
                    token.issued_by_principal_id,
                    token.last_used_at
                 from iam_api_token token
                 join iam_principal principal on principal.id = token.principal_id
                 where principal.principal_kind = 'api_token'
                 order by token.label asc",
            )
            .fetch_all(postgres)
            .await
        }
    }
}

pub async fn create_api_token_secret(
    postgres: &PgPool,
    token_principal_id: Uuid,
    secret_hash: &str,
) -> Result<IamApiTokenSecretRow, sqlx::Error> {
    let next_version = sqlx::query_scalar::<_, i32>(
        "select coalesce(max(secret_version), 0) + 1
         from iam_api_token_secret
         where token_principal_id = $1",
    )
    .bind(token_principal_id)
    .fetch_one(postgres)
    .await?;

    sqlx::query_as::<_, IamApiTokenSecretRow>(
        "insert into iam_api_token_secret (
            token_principal_id,
            secret_version,
            secret_hash,
            issued_at,
            revoked_at
        )
        values ($1, $2, $3, now(), null)
        returning token_principal_id, secret_version, secret_hash, issued_at, revoked_at",
    )
    .bind(token_principal_id)
    .bind(next_version)
    .bind(secret_hash)
    .fetch_one(postgres)
    .await
}

pub async fn create_api_token_secret_with_transaction(
    transaction: &mut PgConnection,
    token_principal_id: Uuid,
    secret_hash: &str,
) -> Result<IamApiTokenSecretRow, sqlx::Error> {
    let next_version = sqlx::query_scalar::<_, i32>(
        "select coalesce(max(secret_version), 0) + 1
         from iam_api_token_secret
         where token_principal_id = $1",
    )
    .bind(token_principal_id)
    .fetch_one(&mut *transaction)
    .await?;

    sqlx::query_as::<_, IamApiTokenSecretRow>(
        "insert into iam_api_token_secret (
            token_principal_id,
            secret_version,
            secret_hash,
            issued_at,
            revoked_at
        )
        values ($1, $2, $3, now(), null)
        returning token_principal_id, secret_version, secret_hash, issued_at, revoked_at",
    )
    .bind(token_principal_id)
    .bind(next_version)
    .bind(secret_hash)
    .fetch_one(&mut *transaction)
    .await
}

pub async fn revoke_active_api_token_secrets(
    postgres: &PgPool,
    token_principal_id: Uuid,
) -> Result<Vec<IamApiTokenSecretRow>, sqlx::Error> {
    sqlx::query_as::<_, IamApiTokenSecretRow>(
        "update iam_api_token_secret
         set revoked_at = now()
         where token_principal_id = $1
           and revoked_at is null
         returning token_principal_id, secret_version, secret_hash, issued_at, revoked_at",
    )
    .bind(token_principal_id)
    .fetch_all(postgres)
    .await
}

pub async fn revoke_active_api_token_secrets_with_transaction(
    transaction: &mut PgConnection,
    token_principal_id: Uuid,
) -> Result<Vec<IamApiTokenSecretRow>, sqlx::Error> {
    sqlx::query_as::<_, IamApiTokenSecretRow>(
        "update iam_api_token_secret
         set revoked_at = now()
         where token_principal_id = $1
           and revoked_at is null
         returning token_principal_id, secret_version, secret_hash, issued_at, revoked_at",
    )
    .bind(token_principal_id)
    .fetch_all(&mut *transaction)
    .await
}

pub async fn revoke_api_token(
    postgres: &PgPool,
    principal_id: Uuid,
) -> Result<Option<IamApiTokenRow>, sqlx::Error> {
    sqlx::query_as::<_, IamApiTokenRow>(
        "update iam_api_token
         set status = 'revoked',
             revoked_at = now()
         where principal_id = $1
         returning
            principal_id,
            workspace_id,
            label,
            token_prefix,
            status::text as status,
            expires_at,
            revoked_at,
            issued_by_principal_id,
            last_used_at",
    )
    .bind(principal_id)
    .fetch_optional(postgres)
    .await
}

pub async fn delete_revoked_api_token(
    postgres: &PgPool,
    principal_id: Uuid,
) -> Result<Option<IamApiTokenRow>, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let deleted_token = sqlx::query_as::<_, IamApiTokenRow>(
        "delete from iam_api_token
         where principal_id = $1
           and status = 'revoked'
         returning
            principal_id,
            workspace_id,
            label,
            token_prefix,
            status::text as status,
            expires_at,
            revoked_at,
            issued_by_principal_id,
            last_used_at",
    )
    .bind(principal_id)
    .fetch_optional(&mut *transaction)
    .await?;

    if deleted_token.is_some() {
        sqlx::query("delete from iam_grant where principal_id = $1")
            .bind(principal_id)
            .execute(&mut *transaction)
            .await?;
        sqlx::query(
            "update iam_principal
             set status = 'disabled',
                 disabled_at = coalesce(disabled_at, now())
             where id = $1
               and principal_kind = 'api_token'",
        )
        .bind(principal_id)
        .execute(&mut *transaction)
        .await?;
    }

    transaction.commit().await?;
    Ok(deleted_token)
}

pub async fn touch_api_token_if_stale(
    postgres: &PgPool,
    principal_id: Uuid,
    stale_before: DateTime<Utc>,
) -> Result<Option<IamApiTokenRow>, sqlx::Error> {
    sqlx::query_as::<_, IamApiTokenRow>(
        "update iam_api_token
          set last_used_at = now()
          where principal_id = $1
           and (last_used_at is null or last_used_at <= $2)
          returning
             principal_id,
             workspace_id,
            label,
            token_prefix,
            status::text as status,
            expires_at,
            revoked_at,
            issued_by_principal_id,
            last_used_at",
    )
    .bind(principal_id)
    .bind(stale_before)
    .fetch_optional(postgres)
    .await
}
