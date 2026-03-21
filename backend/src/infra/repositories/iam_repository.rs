use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct IamPrincipalRow {
    pub id: Uuid,
    pub principal_kind: String,
    pub status: String,
    pub display_label: String,
    pub created_at: DateTime<Utc>,
    pub disabled_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct IamUserRow {
    pub principal_id: Uuid,
    pub email: String,
    pub display_name: String,
    pub password_hash: String,
    pub auth_provider_kind: String,
    pub external_subject: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
pub struct IamSessionRow {
    pub id: Uuid,
    pub principal_id: Uuid,
    pub session_secret_hash: String,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub last_seen_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct IamApiTokenRow {
    pub principal_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub label: String,
    pub token_prefix: String,
    pub status: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub issued_by_principal_id: Option<Uuid>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct IamApiTokenSecretRow {
    pub token_principal_id: Uuid,
    pub secret_version: i32,
    pub secret_hash: String,
    pub issued_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct IamWorkspaceMembershipRow {
    pub workspace_id: Uuid,
    pub principal_id: Uuid,
    pub membership_state: String,
    pub joined_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct IamGrantRow {
    pub id: Uuid,
    pub principal_id: Uuid,
    pub resource_kind: String,
    pub resource_id: Uuid,
    pub permission_kind: String,
    pub granted_at: DateTime<Utc>,
    pub granted_by_principal_id: Option<Uuid>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct AuthenticatedApiTokenRow {
    pub principal_id: Uuid,
    pub principal_kind: String,
    pub principal_status: String,
    pub parent_principal_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
    pub label: String,
    pub token_prefix: String,
    pub token_status: String,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct ResolvedIamGrantScopeRow {
    pub id: Uuid,
    pub principal_id: Uuid,
    pub resource_kind: String,
    pub resource_id: Uuid,
    pub permission_kind: String,
    pub granted_at: DateTime<Utc>,
    pub granted_by_principal_id: Option<Uuid>,
    pub expires_at: Option<DateTime<Utc>>,
    pub workspace_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    pub document_id: Option<Uuid>,
}

#[derive(Debug, Clone, FromRow)]
pub struct BootstrapClaimRow {
    pub principal_id: Uuid,
    pub email: String,
    pub display_name: String,
    pub claimed_at: DateTime<Utc>,
}

pub async fn create_principal(
    postgres: &PgPool,
    principal_kind: &str,
    display_label: &str,
    parent_principal_id: Option<Uuid>,
) -> Result<IamPrincipalRow, sqlx::Error> {
    sqlx::query_as::<_, IamPrincipalRow>(
        "insert into iam_principal (
            id,
            principal_kind,
            display_label,
            status,
            parent_principal_id,
            created_at,
            disabled_at
        )
        values ($1, $2::iam_principal_kind, $3, 'active', $4, now(), null)
        returning
            id,
            principal_kind::text as principal_kind,
            status::text as status,
            display_label,
            created_at,
            disabled_at",
    )
    .bind(Uuid::now_v7())
    .bind(principal_kind)
    .bind(display_label)
    .bind(parent_principal_id)
    .fetch_one(postgres)
    .await
}

pub async fn get_principal_by_id(
    postgres: &PgPool,
    principal_id: Uuid,
) -> Result<Option<IamPrincipalRow>, sqlx::Error> {
    sqlx::query_as::<_, IamPrincipalRow>(
        "select
            id,
            principal_kind::text as principal_kind,
            status::text as status,
            display_label,
            created_at,
            disabled_at
         from iam_principal
         where id = $1",
    )
    .bind(principal_id)
    .fetch_optional(postgres)
    .await
}

pub async fn list_principals(postgres: &PgPool) -> Result<Vec<IamPrincipalRow>, sqlx::Error> {
    sqlx::query_as::<_, IamPrincipalRow>(
        "select
            id,
            principal_kind::text as principal_kind,
            status::text as status,
            display_label,
            created_at,
            disabled_at
         from iam_principal
         order by created_at desc",
    )
    .fetch_all(postgres)
    .await
}

pub async fn disable_principal(
    postgres: &PgPool,
    principal_id: Uuid,
) -> Result<Option<IamPrincipalRow>, sqlx::Error> {
    sqlx::query_as::<_, IamPrincipalRow>(
        "update iam_principal
         set status = 'disabled',
             disabled_at = now()
         where id = $1
         returning
            id,
            principal_kind::text as principal_kind,
            status::text as status,
            display_label,
            created_at,
            disabled_at",
    )
    .bind(principal_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_user_by_principal_id(
    postgres: &PgPool,
    principal_id: Uuid,
) -> Result<Option<IamUserRow>, sqlx::Error> {
    sqlx::query_as::<_, IamUserRow>(
        "select
            principal_id,
            email,
            display_name,
            password_hash,
            auth_provider_kind,
            external_subject
         from iam_user
         where principal_id = $1",
    )
    .bind(principal_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_user_by_email(
    postgres: &PgPool,
    email: &str,
) -> Result<Option<IamUserRow>, sqlx::Error> {
    sqlx::query_as::<_, IamUserRow>(
        "select
            principal_id,
            email,
            display_name,
            password_hash,
            auth_provider_kind,
            external_subject
         from iam_user
         where lower(email) = lower($1)",
    )
    .bind(email)
    .fetch_optional(postgres)
    .await
}

pub async fn create_session(
    postgres: &PgPool,
    principal_id: Uuid,
    session_secret_hash: &str,
    expires_at: DateTime<Utc>,
) -> Result<IamSessionRow, sqlx::Error> {
    sqlx::query_as::<_, IamSessionRow>(
        "insert into iam_session (
            id,
            principal_id,
            session_secret_hash,
            issued_at,
            expires_at,
            revoked_at,
            last_seen_at
        )
        values ($1, $2, $3, now(), $4, null, now())
        returning id, principal_id, session_secret_hash, issued_at, expires_at, revoked_at, last_seen_at",
    )
    .bind(Uuid::now_v7())
    .bind(principal_id)
    .bind(session_secret_hash)
    .bind(expires_at)
    .fetch_one(postgres)
    .await
}

pub async fn get_session_by_id(
    postgres: &PgPool,
    session_id: Uuid,
) -> Result<Option<IamSessionRow>, sqlx::Error> {
    sqlx::query_as::<_, IamSessionRow>(
        "select id, principal_id, session_secret_hash, issued_at, expires_at, revoked_at, last_seen_at
         from iam_session
         where id = $1",
    )
    .bind(session_id)
    .fetch_optional(postgres)
    .await
}

pub async fn revoke_session(
    postgres: &PgPool,
    session_id: Uuid,
) -> Result<Option<IamSessionRow>, sqlx::Error> {
    sqlx::query_as::<_, IamSessionRow>(
        "update iam_session
         set revoked_at = now()
         where id = $1
         returning id, principal_id, session_secret_hash, issued_at, expires_at, revoked_at, last_seen_at",
    )
    .bind(session_id)
    .fetch_optional(postgres)
    .await
}

pub async fn touch_session(
    postgres: &PgPool,
    session_id: Uuid,
) -> Result<Option<IamSessionRow>, sqlx::Error> {
    sqlx::query_as::<_, IamSessionRow>(
        "update iam_session
         set last_seen_at = now()
         where id = $1
         returning id, principal_id, session_secret_hash, issued_at, expires_at, revoked_at, last_seen_at",
    )
    .bind(session_id)
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
    .bind(principal.id)
    .bind(workspace_id)
    .bind(label)
    .bind(token_prefix)
    .bind(expires_at)
    .bind(issued_by_principal_id)
    .fetch_one(postgres)
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
            token.expires_at
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
                 where workspace_id = $1
                 order by label asc",
            )
            .bind(workspace_id)
            .fetch_all(postgres)
            .await
        }
        None => {
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
                 order by label asc",
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

pub async fn touch_api_token(
    postgres: &PgPool,
    principal_id: Uuid,
) -> Result<Option<IamApiTokenRow>, sqlx::Error> {
    sqlx::query_as::<_, IamApiTokenRow>(
        "update iam_api_token
         set last_used_at = now()
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

pub async fn upsert_workspace_membership(
    postgres: &PgPool,
    workspace_id: Uuid,
    principal_id: Uuid,
    membership_state: &str,
) -> Result<IamWorkspaceMembershipRow, sqlx::Error> {
    sqlx::query_as::<_, IamWorkspaceMembershipRow>(
        "insert into iam_workspace_membership (
            workspace_id,
            principal_id,
            membership_state,
            joined_at,
            ended_at
        )
        values ($1, $2, $3::iam_membership_state, now(), null)
        on conflict (workspace_id, principal_id)
        do update set membership_state = excluded.membership_state,
                      ended_at = case when excluded.membership_state = 'ended' then now() else null end
        returning
            workspace_id,
            principal_id,
            membership_state::text as membership_state,
            joined_at,
            ended_at",
    )
    .bind(workspace_id)
    .bind(principal_id)
    .bind(membership_state)
    .fetch_one(postgres)
    .await
}

pub async fn list_workspace_memberships(
    postgres: &PgPool,
    workspace_id: Uuid,
) -> Result<Vec<IamWorkspaceMembershipRow>, sqlx::Error> {
    sqlx::query_as::<_, IamWorkspaceMembershipRow>(
        "select
            workspace_id,
            principal_id,
            membership_state::text as membership_state,
            joined_at,
            ended_at
         from iam_workspace_membership
         where workspace_id = $1
         order by joined_at asc",
    )
    .bind(workspace_id)
    .fetch_all(postgres)
    .await
}

pub async fn list_workspace_memberships_by_principal(
    postgres: &PgPool,
    principal_id: Uuid,
) -> Result<Vec<IamWorkspaceMembershipRow>, sqlx::Error> {
    sqlx::query_as::<_, IamWorkspaceMembershipRow>(
        "select
            workspace_id,
            principal_id,
            membership_state::text as membership_state,
            joined_at,
            ended_at
         from iam_workspace_membership
         where principal_id = $1
         order by joined_at asc",
    )
    .bind(principal_id)
    .fetch_all(postgres)
    .await
}

pub async fn create_grant(
    postgres: &PgPool,
    principal_id: Uuid,
    resource_kind: &str,
    resource_id: Uuid,
    permission_kind: &str,
    granted_by_principal_id: Option<Uuid>,
    expires_at: Option<DateTime<Utc>>,
) -> Result<IamGrantRow, sqlx::Error> {
    sqlx::query_as::<_, IamGrantRow>(
        "insert into iam_grant (
            id,
            principal_id,
            resource_kind,
            resource_id,
            permission_kind,
            granted_by_principal_id,
            granted_at,
            expires_at
        )
        values ($1, $2, $3::iam_grant_resource_kind, $4, $5::iam_permission_kind, $6, now(), $7)
        returning
            id,
            principal_id,
            resource_kind::text as resource_kind,
            resource_id,
            permission_kind::text as permission_kind,
            granted_at,
            granted_by_principal_id,
            expires_at",
    )
    .bind(Uuid::now_v7())
    .bind(principal_id)
    .bind(resource_kind)
    .bind(resource_id)
    .bind(permission_kind)
    .bind(granted_by_principal_id)
    .bind(expires_at)
    .fetch_one(postgres)
    .await
}

pub async fn list_grants_by_principal(
    postgres: &PgPool,
    principal_id: Uuid,
) -> Result<Vec<IamGrantRow>, sqlx::Error> {
    sqlx::query_as::<_, IamGrantRow>(
        "select
            id,
            principal_id,
            resource_kind::text as resource_kind,
            resource_id,
            permission_kind::text as permission_kind,
            granted_at,
            granted_by_principal_id,
            expires_at
         from iam_grant
         where principal_id = $1
           and (expires_at is null or expires_at > now())
         order by granted_at desc",
    )
    .bind(principal_id)
    .fetch_all(postgres)
    .await
}

pub async fn list_resolved_grants_by_principal(
    postgres: &PgPool,
    principal_id: Uuid,
) -> Result<Vec<ResolvedIamGrantScopeRow>, sqlx::Error> {
    sqlx::query_as::<_, ResolvedIamGrantScopeRow>(
        "select
            grant_row.id,
            grant_row.principal_id,
            grant_row.resource_kind::text as resource_kind,
            grant_row.resource_id,
            grant_row.permission_kind::text as permission_kind,
            grant_row.granted_at,
            grant_row.granted_by_principal_id,
            grant_row.expires_at,
            case
                when grant_row.resource_kind = 'workspace' then grant_row.resource_id
                when grant_row.resource_kind = 'library' then library.workspace_id
                when grant_row.resource_kind = 'document' then document.workspace_id
                when grant_row.resource_kind = 'query_session' then query_session.workspace_id
                when grant_row.resource_kind = 'async_operation' then operation.workspace_id
                when grant_row.resource_kind = 'connector' then connector.workspace_id
                when grant_row.resource_kind = 'provider_credential' then credential.workspace_id
                when grant_row.resource_kind = 'library_binding' then binding.workspace_id
                else null
            end as workspace_id,
            case
                when grant_row.resource_kind = 'library' then grant_row.resource_id
                when grant_row.resource_kind = 'document' then document.library_id
                when grant_row.resource_kind = 'query_session' then query_session.library_id
                when grant_row.resource_kind = 'async_operation' then operation.library_id
                when grant_row.resource_kind = 'connector' then connector.library_id
                when grant_row.resource_kind = 'library_binding' then binding.library_id
                else null
            end as library_id,
            case
                when grant_row.resource_kind = 'document' then grant_row.resource_id
                else null
            end as document_id
         from iam_grant grant_row
         left join catalog_library library
           on grant_row.resource_kind = 'library'
          and library.id = grant_row.resource_id
         left join content_document document
           on grant_row.resource_kind = 'document'
          and document.id = grant_row.resource_id
         left join query_conversation query_session
           on grant_row.resource_kind = 'query_session'
          and query_session.id = grant_row.resource_id
         left join ops_async_operation operation
           on grant_row.resource_kind = 'async_operation'
          and operation.id = grant_row.resource_id
         left join catalog_library_connector connector
           on grant_row.resource_kind = 'connector'
          and connector.id = grant_row.resource_id
         left join ai_provider_credential credential
           on grant_row.resource_kind = 'provider_credential'
          and credential.id = grant_row.resource_id
         left join ai_library_model_binding binding
           on grant_row.resource_kind = 'library_binding'
          and binding.id = grant_row.resource_id
         where grant_row.principal_id = $1
           and (grant_row.expires_at is null or grant_row.expires_at > now())
         order by grant_row.granted_at desc",
    )
    .bind(principal_id)
    .fetch_all(postgres)
    .await
}

pub async fn delete_grant(
    postgres: &PgPool,
    grant_id: Uuid,
) -> Result<Option<IamGrantRow>, sqlx::Error> {
    sqlx::query_as::<_, IamGrantRow>(
        "delete from iam_grant
         where id = $1
         returning
            id,
            principal_id,
            resource_kind::text as resource_kind,
            resource_id,
            permission_kind::text as permission_kind,
            granted_at,
            granted_by_principal_id,
            expires_at",
    )
    .bind(grant_id)
    .fetch_optional(postgres)
    .await
}

pub async fn claim_bootstrap_user(
    postgres: &PgPool,
    email: &str,
    display_name: &str,
    password_hash: &str,
) -> Result<Option<BootstrapClaimRow>, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtext('iam.bootstrap.claim'))")
        .execute(&mut *transaction)
        .await?;
    let existing_admins = sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from iam_principal
         where principal_kind = 'user'
           and status = 'active'",
    )
    .fetch_one(&mut *transaction)
    .await?;
    if existing_admins > 0 {
        transaction.rollback().await?;
        return Ok(None);
    }

    let principal_id = Uuid::now_v7();

    sqlx::query(
        "insert into iam_principal (
            id,
            principal_kind,
            display_label,
            status,
            parent_principal_id,
            created_at,
            disabled_at
        )
        values ($1, 'user', $2, 'active', null, now(), null)",
    )
    .bind(principal_id)
    .bind(display_name)
    .execute(&mut *transaction)
    .await?;

    let row = sqlx::query_as::<_, BootstrapClaimRow>(
        "insert into iam_user (
            principal_id,
            email,
            display_name,
            password_hash,
            auth_provider_kind,
            external_subject
        )
        values ($1, $2, $3, $4, 'password', null)
        returning principal_id, email, display_name, now() as claimed_at",
    )
    .bind(principal_id)
    .bind(email)
    .bind(display_name)
    .bind(password_hash)
    .fetch_one(&mut *transaction)
    .await?;

    sqlx::query(
        "insert into iam_grant (
            id,
            principal_id,
            resource_kind,
            resource_id,
            permission_kind,
            granted_by_principal_id,
            granted_at,
            expires_at
        )
        values ($1, $2, 'system', $3, 'iam_admin', null, now(), null)",
    )
    .bind(Uuid::now_v7())
    .bind(principal_id)
    .bind(Uuid::nil())
    .execute(&mut *transaction)
    .await?;

    transaction.commit().await?;
    Ok(Some(row))
}
