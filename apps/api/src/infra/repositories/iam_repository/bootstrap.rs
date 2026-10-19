use super::*;

pub async fn claim_bootstrap_user(
    postgres: &PgPool,
    login: &str,
    email: &str,
    display_name: &str,
    password_hash: &str,
) -> Result<Option<BootstrapClaimRow>, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtext('iam.bootstrap.claim'))")
        .execute(&mut *transaction)
        .await?;
    let existing_admins = count_admin_users(&mut *transaction).await?;
    if existing_admins > 0 {
        transaction.rollback().await?;
        return Ok(None);
    }

    let principal =
        create_principal_with_transaction(&mut transaction, "user", display_name, None).await?;
    let principal_id = principal.id;

    let row = sqlx::query_as::<_, BootstrapClaimRow>(
        "insert into iam_user (
            principal_id,
            login,
            email,
            display_name,
            password_hash,
            auth_provider_kind,
            external_subject,
            role
        )
        values ($1, $2, $3, $4, $5, 'password', null, 'admin')
        returning principal_id, login, email, display_name, now() as claimed_at",
    )
    .bind(principal_id)
    .bind(login)
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
