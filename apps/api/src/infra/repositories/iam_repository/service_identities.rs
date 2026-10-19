use super::*;

const SERVICE_IDENTITY_COLUMNS: &str = "
    token.principal_id,
    token.workspace_id,
    token.label,
    token.token_prefix,
    token.status::text as status,
    token.expires_at,
    token.revoked_at,
    token.issued_by_principal_id,
    token.last_used_at,
    identity.owner_auth_provider_kind,
    identity.owner_external_subject,
    identity.created_by_principal_id,
    identity.created_at,
    identity.deleted_at,
    principal.parent_principal_id,
    identity.provisioning_authority";

fn service_identity_select(predicate: &str) -> sqlx::AssertSqlSafe<String> {
    sqlx::AssertSqlSafe(format!(
        "select {SERVICE_IDENTITY_COLUMNS}
         from iam_service_identity identity
         join iam_api_token token on token.principal_id = identity.principal_id
         join iam_principal principal on principal.id = identity.principal_id
         where principal.principal_kind = 'worker'
           and identity.deleted_at is null
           and {predicate}"
    ))
}

pub async fn lock_service_identity_idempotency(
    transaction: &mut PgConnection,
    owner_auth_provider_kind: &str,
    owner_external_subject: &str,
    idempotency_key: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "select pg_advisory_xact_lock(
            hashtextextended(jsonb_build_array($1::text, $2::text, $3::text)::text, 0)
         )",
    )
    .bind(owner_auth_provider_kind)
    .bind(owner_external_subject)
    .bind(idempotency_key)
    .execute(&mut *transaction)
    .await?;
    Ok(())
}

pub async fn create_service_identity_with_transaction(
    transaction: &mut PgConnection,
    principal_id: Uuid,
    owner_auth_provider_kind: &str,
    owner_external_subject: &str,
    created_by_principal_id: Uuid,
    provisioning_authority: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into iam_service_identity (
            principal_id,
            owner_auth_provider_kind,
            owner_external_subject,
            created_by_principal_id,
            created_at,
            deleted_at,
            provisioning_authority
         ) values ($1, $2, $3, $4, now(), null, $5)",
    )
    .bind(principal_id)
    .bind(owner_auth_provider_kind)
    .bind(owner_external_subject)
    .bind(created_by_principal_id)
    .bind(provisioning_authority)
    .execute(&mut *transaction)
    .await?;
    Ok(())
}

pub async fn get_provisioning_identity_by_principal_id(
    postgres: &PgPool,
    principal_id: Uuid,
) -> Result<Option<IamServiceIdentityRow>, sqlx::Error> {
    sqlx::query_as::<_, IamServiceIdentityRow>(service_identity_select(
        "identity.principal_id = $1 and identity.provisioning_authority = 'integration'",
    ))
    .bind(principal_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_child_service_identity_for_parent_with_transaction(
    transaction: &mut PgConnection,
    principal_id: Uuid,
    parent_principal_id: Uuid,
    owner_auth_provider_kind: &str,
    owner_external_subject: &str,
) -> Result<Option<IamServiceIdentityRow>, sqlx::Error> {
    let mut query = service_identity_select(
        "identity.principal_id = $1
         and principal.parent_principal_id = $2
         and identity.owner_auth_provider_kind = $3
         and identity.owner_external_subject = $4",
    )
    .0;
    query.push_str(" for update of identity, token, principal");
    sqlx::query_as::<_, IamServiceIdentityRow>(sqlx::AssertSqlSafe(query))
        .bind(principal_id)
        .bind(parent_principal_id)
        .bind(owner_auth_provider_kind)
        .bind(owner_external_subject)
        .fetch_optional(&mut *transaction)
        .await
}

pub async fn list_child_service_identities(
    postgres: &PgPool,
    parent_principal_id: Uuid,
    owner_auth_provider_kind: &str,
    owner_external_subject: &str,
    workspace_id: Option<Uuid>,
) -> Result<Vec<IamServiceIdentityRow>, sqlx::Error> {
    sqlx::query_as::<_, IamServiceIdentityRow>(service_identity_select(
        "principal.parent_principal_id = $1
         and identity.owner_auth_provider_kind = $2
         and identity.owner_external_subject = $3
         and ($4::uuid is null or token.workspace_id = $4)
         order by token.label asc, identity.principal_id asc",
    ))
    .bind(parent_principal_id)
    .bind(owner_auth_provider_kind)
    .bind(owner_external_subject)
    .bind(workspace_id)
    .fetch_all(postgres)
    .await
}

pub async fn delete_workspace_grants_with_transaction(
    transaction: &mut PgConnection,
    principal_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("delete from iam_grant where principal_id = $1 and resource_kind = 'workspace'")
        .bind(principal_id)
        .execute(&mut *transaction)
        .await?;
    Ok(())
}

pub async fn list_active_child_principal_ids_with_transaction(
    transaction: &mut PgConnection,
    parent_principal_id: Uuid,
) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "select identity.principal_id
         from iam_service_identity identity
         join iam_principal principal on principal.id = identity.principal_id
         join iam_api_token token on token.principal_id = identity.principal_id
         where principal.parent_principal_id = $1
           and identity.deleted_at is null
           and token.status = 'active'
         for update of identity, principal, token",
    )
    .bind(parent_principal_id)
    .fetch_all(&mut *transaction)
    .await
}

pub async fn count_child_keys_with_transaction(
    transaction: &mut PgConnection,
    parent_principal_id: Uuid,
    workspace_id: Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "select count(*) from iam_service_identity identity
         join iam_principal principal on principal.id = identity.principal_id
         join iam_api_token token on token.principal_id = identity.principal_id
         where principal.parent_principal_id = $1 and token.workspace_id = $2
           and identity.deleted_at is null",
    )
    .bind(parent_principal_id)
    .bind(workspace_id)
    .fetch_one(&mut *transaction)
    .await
}

pub async fn count_active_child_keys_with_transaction(
    transaction: &mut PgConnection,
    parent_principal_id: Uuid,
    workspace_id: Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "select count(*) from iam_service_identity identity
         join iam_principal principal on principal.id = identity.principal_id
         join iam_api_token token on token.principal_id = identity.principal_id
         where principal.parent_principal_id = $1 and token.workspace_id = $2
           and identity.deleted_at is null and token.status = 'active'
           and (token.expires_at is null or token.expires_at > now())",
    )
    .bind(parent_principal_id)
    .bind(workspace_id)
    .fetch_one(&mut *transaction)
    .await
}

/// Input for recording one service-identity lifecycle operation row.
#[derive(Clone)]
pub struct NewServiceIdentityOperation<'a> {
    pub operation_id: Uuid,
    pub service_identity_principal_id: Uuid,
    pub owner_auth_provider_kind: &'a str,
    pub owner_external_subject: &'a str,
    pub actor_principal_id: Uuid,
    pub operation_kind: &'a str,
    pub idempotency_key: &'a str,
    pub request_fingerprint: &'a str,
    pub credential_secret_version: Option<i32>,
    pub encrypted_credential: Option<&'a str>,
}

pub async fn create_service_identity_operation_with_transaction(
    transaction: &mut PgConnection,
    operation: &NewServiceIdentityOperation<'_>,
) -> Result<IamServiceIdentityOperationRow, sqlx::Error> {
    sqlx::query_as::<_, IamServiceIdentityOperationRow>(
        "insert into iam_service_identity_operation (
            id,
            service_identity_principal_id,
            owner_auth_provider_kind,
            owner_external_subject,
            actor_principal_id,
            operation_kind,
            idempotency_key,
            request_fingerprint,
            credential_secret_version,
            encrypted_credential,
            created_at,
            superseded_at
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, now(), null)
         returning
            id,
            service_identity_principal_id,
            owner_auth_provider_kind,
            owner_external_subject,
            actor_principal_id,
            operation_kind,
            idempotency_key,
            request_fingerprint,
            credential_secret_version,
            encrypted_credential,
            created_at,
            superseded_at",
    )
    .bind(operation.operation_id)
    .bind(operation.service_identity_principal_id)
    .bind(operation.owner_auth_provider_kind)
    .bind(operation.owner_external_subject)
    .bind(operation.actor_principal_id)
    .bind(operation.operation_kind)
    .bind(operation.idempotency_key)
    .bind(operation.request_fingerprint)
    .bind(operation.credential_secret_version)
    .bind(operation.encrypted_credential)
    .fetch_one(&mut *transaction)
    .await
}

pub async fn get_service_identity_operation_by_key_with_transaction(
    transaction: &mut PgConnection,
    owner_auth_provider_kind: &str,
    owner_external_subject: &str,
    idempotency_key: &str,
) -> Result<Option<IamServiceIdentityOperationRow>, sqlx::Error> {
    sqlx::query_as::<_, IamServiceIdentityOperationRow>(
        "select
            id,
            service_identity_principal_id,
            owner_auth_provider_kind,
            owner_external_subject,
            actor_principal_id,
            operation_kind,
            idempotency_key,
            request_fingerprint,
            credential_secret_version,
            encrypted_credential,
            created_at,
            superseded_at
         from iam_service_identity_operation
         where owner_auth_provider_kind = $1
           and owner_external_subject = $2
           and idempotency_key = $3
         for update",
    )
    .bind(owner_auth_provider_kind)
    .bind(owner_external_subject)
    .bind(idempotency_key)
    .fetch_optional(&mut *transaction)
    .await
}

pub async fn list_service_identities_for_owner(
    postgres: &PgPool,
    owner_auth_provider_kind: &str,
    owner_external_subject: &str,
    workspace_id: Option<Uuid>,
) -> Result<Vec<IamServiceIdentityRow>, sqlx::Error> {
    sqlx::query_as::<_, IamServiceIdentityRow>(service_identity_select(
        "identity.owner_auth_provider_kind = $1
         and identity.owner_external_subject = $2
         and ($3::uuid is null or token.workspace_id = $3)
         order by token.label asc, identity.principal_id asc",
    ))
    .bind(owner_auth_provider_kind)
    .bind(owner_external_subject)
    .bind(workspace_id)
    .fetch_all(postgres)
    .await
}

pub async fn get_service_identity_for_owner(
    postgres: &PgPool,
    principal_id: Uuid,
    owner_auth_provider_kind: &str,
    owner_external_subject: &str,
) -> Result<Option<IamServiceIdentityRow>, sqlx::Error> {
    sqlx::query_as::<_, IamServiceIdentityRow>(service_identity_select(
        "identity.principal_id = $1
         and identity.owner_auth_provider_kind = $2
         and identity.owner_external_subject = $3",
    ))
    .bind(principal_id)
    .bind(owner_auth_provider_kind)
    .bind(owner_external_subject)
    .fetch_optional(postgres)
    .await
}

pub async fn get_service_identity_for_owner_with_transaction(
    transaction: &mut PgConnection,
    principal_id: Uuid,
    owner_auth_provider_kind: &str,
    owner_external_subject: &str,
) -> Result<Option<IamServiceIdentityRow>, sqlx::Error> {
    let mut query = service_identity_select(
        "identity.principal_id = $1
         and identity.owner_auth_provider_kind = $2
         and identity.owner_external_subject = $3",
    )
    .0;
    query.push_str(" for update of identity, token, principal");
    sqlx::query_as::<_, IamServiceIdentityRow>(sqlx::AssertSqlSafe(query))
        .bind(principal_id)
        .bind(owner_auth_provider_kind)
        .bind(owner_external_subject)
        .fetch_optional(&mut *transaction)
        .await
}

pub async fn update_service_identity_metadata_with_transaction(
    transaction: &mut PgConnection,
    principal_id: Uuid,
    label: Option<&str>,
    expires_at: Option<Option<DateTime<Utc>>>,
) -> Result<(), sqlx::Error> {
    let has_expires_at = expires_at.is_some();
    let expires_at_value = expires_at.flatten();
    sqlx::query(
        "update iam_api_token
         set label = coalesce($2, label),
             expires_at = case when $3 then $4 else expires_at end
         where principal_id = $1",
    )
    .bind(principal_id)
    .bind(label)
    .bind(has_expires_at)
    .bind(expires_at_value)
    .execute(&mut *transaction)
    .await?;

    if let Some(label) = label {
        sqlx::query("update iam_principal set display_label = $2 where id = $1")
            .bind(principal_id)
            .bind(label)
            .execute(&mut *transaction)
            .await?;
    }
    Ok(())
}

pub async fn update_service_identity_token_prefix_with_transaction(
    transaction: &mut PgConnection,
    principal_id: Uuid,
    token_prefix: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("update iam_api_token set token_prefix = $2 where principal_id = $1")
        .bind(principal_id)
        .bind(token_prefix)
        .execute(&mut *transaction)
        .await?;
    Ok(())
}

pub async fn delete_service_identity_grants_with_transaction(
    transaction: &mut PgConnection,
    principal_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("delete from iam_grant where principal_id = $1")
        .bind(principal_id)
        .execute(&mut *transaction)
        .await?;
    Ok(())
}

pub async fn supersede_service_identity_credentials_with_transaction(
    transaction: &mut PgConnection,
    principal_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "update iam_service_identity_operation
         set encrypted_credential = null,
             superseded_at = now()
         where service_identity_principal_id = $1
           and encrypted_credential is not null",
    )
    .bind(principal_id)
    .execute(&mut *transaction)
    .await?;
    Ok(result.rows_affected())
}

pub async fn revoke_service_identity_with_transaction(
    transaction: &mut PgConnection,
    principal_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update iam_api_token_secret
         set revoked_at = coalesce(revoked_at, now())
         where token_principal_id = $1
           and revoked_at is null",
    )
    .bind(principal_id)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "update iam_api_token
         set status = 'revoked',
             revoked_at = coalesce(revoked_at, now())
         where principal_id = $1",
    )
    .bind(principal_id)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "update iam_principal
         set status = 'disabled',
             disabled_at = coalesce(disabled_at, now())
         where id = $1
           and principal_kind = 'worker'",
    )
    .bind(principal_id)
    .execute(&mut *transaction)
    .await?;
    Ok(())
}

pub async fn tombstone_revoked_service_identity_with_transaction(
    transaction: &mut PgConnection,
    principal_id: Uuid,
) -> Result<bool, sqlx::Error> {
    delete_service_identity_grants_with_transaction(transaction, principal_id).await?;
    let deleted = sqlx::query_scalar::<_, Uuid>(
        "update iam_service_identity identity
         set deleted_at = now()
         from iam_api_token token
         where identity.principal_id = $1
           and token.principal_id = identity.principal_id
           and token.status = 'revoked'
           and identity.deleted_at is null
         returning identity.principal_id",
    )
    .bind(principal_id)
    .fetch_optional(&mut *transaction)
    .await?
    .is_some();
    Ok(deleted)
}
