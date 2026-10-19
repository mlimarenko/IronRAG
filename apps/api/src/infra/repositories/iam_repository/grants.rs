use super::*;

/// Input for inserting one `iam_grant` row through a pool or transaction.
#[derive(Debug, Clone)]
pub struct NewGrant<'a> {
    pub principal_id: Uuid,
    pub resource_kind: &'a str,
    pub resource_id: Uuid,
    pub permission_kind: &'a str,
    pub granted_by_principal_id: Option<Uuid>,
    pub expires_at: Option<DateTime<Utc>>,
}

pub async fn create_grant<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    grant: &NewGrant<'_>,
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
    .bind(grant.principal_id)
    .bind(grant.resource_kind)
    .bind(grant.resource_id)
    .bind(grant.permission_kind)
    .bind(grant.granted_by_principal_id)
    .bind(grant.expires_at)
    .fetch_one(executor)
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
    list_resolved_grants_by_principal_ids(postgres, &[principal_id]).await
}

pub async fn list_resolved_grants_by_principal_ids(
    postgres: &PgPool,
    principal_ids: &[Uuid],
) -> Result<Vec<ResolvedIamGrantScopeRow>, sqlx::Error> {
    if principal_ids.is_empty() {
        return Ok(Vec::new());
    }

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
         left join ai_account credential
           on grant_row.resource_kind = 'provider_credential'
          and credential.id = grant_row.resource_id
         left join ai_binding binding
           on grant_row.resource_kind = 'library_binding'
          and binding.id = grant_row.resource_id
         where grant_row.principal_id = any($1)
           and (grant_row.expires_at is null or grant_row.expires_at > now())
         order by grant_row.principal_id asc, grant_row.granted_at desc",
    )
    .bind(principal_ids)
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
