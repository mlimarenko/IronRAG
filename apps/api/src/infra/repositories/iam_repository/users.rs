use super::*;

pub async fn get_user_by_principal_id(
    postgres: &PgPool,
    principal_id: Uuid,
) -> Result<Option<IamUserRow>, sqlx::Error> {
    sqlx::query_as::<_, IamUserRow>(
        "select
            principal_id,
            login,
            email,
            display_name,
            password_hash,
            auth_provider_kind,
            external_subject,
            role::text as role
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
            login,
            email,
            display_name,
            password_hash,
            auth_provider_kind,
            external_subject,
            role::text as role
         from iam_user
         where lower(email) = lower($1)",
    )
    .bind(email)
    .fetch_optional(postgres)
    .await
}

pub async fn get_user_by_login(
    postgres: &PgPool,
    login: &str,
) -> Result<Option<IamUserRow>, sqlx::Error> {
    sqlx::query_as::<_, IamUserRow>(
        "select
            principal_id,
            login,
            email,
            display_name,
            password_hash,
            auth_provider_kind,
            external_subject,
            role::text as role
         from iam_user
         where lower(login) = lower($1)",
    )
    .bind(login)
    .fetch_optional(postgres)
    .await
}

/// Loads a user by its immutable external identity. The provider identifier is
/// issuer-specific (for OIDC it is `oidc:<sha256 issuer>`), so identical
/// subjects from different issuers can never be linked accidentally.
pub async fn get_user_by_external_identity(
    postgres: &PgPool,
    auth_provider_kind: &str,
    external_subject: &str,
) -> Result<Option<IamUserRow>, sqlx::Error> {
    sqlx::query_as::<_, IamUserRow>(
        "select
            principal_id,
            login,
            email,
            display_name,
            password_hash,
            auth_provider_kind,
            external_subject,
            role::text as role
         from iam_user
         where auth_provider_kind = $1
           and external_subject = $2",
    )
    .bind(auth_provider_kind)
    .bind(external_subject)
    .fetch_optional(postgres)
    .await
}

/// Result of a serialized OIDC JIT provisioning attempt.
pub enum OidcProvisionUserOutcome {
    /// The durable external identity resolved to this user. `created` is true
    /// only for the transaction that inserted that identity.
    Provisioned { user: IamUserRow, created: bool },
    /// A local or different external identity already owns the generated
    /// login or verified email. Callers must reject rather than link by email.
    IdentityCollision,
}

/// Input for resolving or creating a user from an issuer-specific OIDC
/// subject. `password_hash` must already be produced by the configured Argon2
/// hashing.
#[derive(Clone)]
pub struct NewOidcUser<'a> {
    pub auth_provider_kind: &'a str,
    pub external_subject: &'a str,
    pub login: &'a str,
    pub email: &'a str,
    pub display_name: &'a str,
    pub password_hash: &'a str,
    pub role: SystemRole,
}

/// Resolves or creates a user from an issuer-specific OIDC subject.
///
/// The existing baseline schema deliberately has no external-identity unique
/// index. This function therefore takes a transaction-scoped advisory lock on
/// the complete provider/subject pair before reading or writing it. It is the
/// one implementation path for JIT provisioning and makes same-identity
/// concurrent callbacks produce exactly one user without changing the live
/// baseline migration.
pub async fn provision_oidc_user(
    postgres: &PgPool,
    user: &NewOidcUser<'_>,
) -> Result<OidcProvisionUserOutcome, sqlx::Error> {
    let auth_provider_kind = user.auth_provider_kind;
    let external_subject = user.external_subject;
    let login = user.login;
    let email = user.email;
    let display_name = user.display_name;
    let password_hash = user.password_hash;
    let role = user.role;
    let mut transaction = postgres.begin().await?;
    // Two-key form, not a delimiter-joined single key: `external_subject` is
    // IdP-controlled and Postgres text/hashtext can't carry an embedded NUL,
    // so no in-band separator is safe here regardless of choice.
    sqlx::query("select pg_advisory_xact_lock(hashtext($1), hashtext($2))")
        .bind(auth_provider_kind)
        .bind(external_subject)
        .execute(&mut *transaction)
        .await?;

    if let Some(user) = sqlx::query_as::<_, IamUserRow>(
        "select
            principal_id,
            login,
            email,
            display_name,
            password_hash,
            auth_provider_kind,
            external_subject,
            role::text as role
         from iam_user
         where auth_provider_kind = $1
           and external_subject = $2
         for update",
    )
    .bind(auth_provider_kind)
    .bind(external_subject)
    .fetch_optional(&mut *transaction)
    .await?
    {
        transaction.commit().await?;
        return Ok(OidcProvisionUserOutcome::Provisioned { user, created: false });
    }

    // A matching email is intentionally a collision, not a claim that the
    // IdP user owns a local account. This keeps the password break-glass user
    // independent even when a mutable email address is shared or recycled.
    let identity_collision = sqlx::query_scalar::<_, Uuid>(
        "select principal_id
         from iam_user
         where lower(login) = lower($1)
            or lower(email) = lower($2)
         limit 1
         for update",
    )
    .bind(login)
    .bind(email)
    .fetch_optional(&mut *transaction)
    .await?
    .is_some();
    if identity_collision {
        transaction.rollback().await?;
        return Ok(OidcProvisionUserOutcome::IdentityCollision);
    }

    let principal =
        create_principal_with_transaction(&mut transaction, "user", display_name, None).await?;
    let principal_id = principal.id;

    let user = match sqlx::query_as::<_, IamUserRow>(
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
        values ($1, $2, $3, $4, $5, $6, $7, $8::iam_system_role)
        returning
            principal_id,
            login,
            email,
            display_name,
            password_hash,
            auth_provider_kind,
            external_subject,
            role::text as role",
    )
    .bind(principal_id)
    .bind(login)
    .bind(email)
    .bind(display_name)
    .bind(password_hash)
    .bind(auth_provider_kind)
    .bind(external_subject)
    .bind(role.as_str())
    .fetch_one(&mut *transaction)
    .await
    {
        Ok(user) => user,
        // The identity advisory lock removes the same-provider/subject race.
        // A different subject can still concurrently collide on the baseline
        // unique login/email constraints, so treat that expected conflict as
        // an authentication failure rather than an internal error.
        Err(error)
            if error
                .as_database_error()
                .and_then(|database_error| database_error.code())
                .is_some_and(|code| code == "23505") =>
        {
            transaction.rollback().await?;
            return Ok(OidcProvisionUserOutcome::IdentityCollision);
        }
        Err(error) => return Err(error),
    };

    transaction.commit().await?;
    Ok(OidcProvisionUserOutcome::Provisioned { user, created: true })
}

/// Updates an environment-managed bootstrap password only for the exact local
/// password identity the operator configured. The authentication-provider and
/// profile predicates are part of the write itself, not merely a caller-side
/// precheck, so a concurrent identity change cannot turn reconciliation into
/// an OIDC-password overwrite.
pub async fn update_bootstrap_password_if_matching_local_user(
    postgres: &PgPool,
    principal_id: Uuid,
    login: &str,
    email: &str,
    password_hash: &str,
) -> Result<bool, sqlx::Error> {
    let updated = sqlx::query_scalar::<_, Uuid>(
        "update iam_user
         set password_hash = $4
         where principal_id = $1
           and login = $2
           and email = $3
           and auth_provider_kind = 'password'
         returning principal_id",
    )
    .bind(principal_id)
    .bind(login)
    .bind(email)
    .bind(password_hash)
    .fetch_optional(postgres)
    .await?;
    Ok(updated.is_some())
}

/// Lists every *active* user principal (`login/email/display_name/role/auth`
/// provider), ordered by login for a stable admin surface.
///
/// Filtered to `principal.status = 'active'`: [`delete_user`] keeps the
/// `iam_user` row as an identity tombstone rather than purging it, so an
/// unfiltered listing would show deleted users forever.
pub async fn list_users(postgres: &PgPool) -> Result<Vec<IamUserRow>, sqlx::Error> {
    sqlx::query_as::<_, IamUserRow>(
        "select
            users.principal_id,
            users.login,
            users.email,
            users.display_name,
            users.password_hash,
            users.auth_provider_kind,
            users.external_subject,
            users.role::text as role
         from iam_user as users
         join iam_principal as principal
           on principal.id = users.principal_id
         where principal.status = 'active'
         order by users.login asc",
    )
    .fetch_all(postgres)
    .await
}

/// Counts active user principals currently carrying the `admin` system role.
///
/// Used by the last-admin guard in [`delete_user`]/[`set_user_role`] to
/// prevent removing or demoting the last administrator. Generic over the
/// executor so it can run inside the same transaction as the guarded
/// delete/update.
pub async fn count_admin_users<'e>(
    executor: impl Executor<'e, Database = Postgres>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from iam_user as users
         join iam_principal as principal
           on principal.id = users.principal_id
         where users.role = 'admin'
           and principal.status = 'active'",
    )
    .fetch_one(executor)
    .await
}

/// Serializes every operation that could remove the last active
/// administrator (deleting an admin or demoting one away from `admin`).
///
/// Without this lock, two concurrent last-admin checks can each run under
/// `READ COMMITTED`, both see `admin_count > 1` via an unlocked `count(*)`,
/// and both commit — leaving zero active admins. Taking this advisory lock
/// first forces such operations to serialize, mirroring the
/// `pg_advisory_xact_lock` guard already used by [`claim_bootstrap_user`].
async fn lock_last_admin_guard(transaction: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query("select pg_advisory_xact_lock(hashtext('iam.last_admin_guard'))")
        .execute(transaction)
        .await?;
    Ok(())
}

/// Returns `true` when `principal_id` currently holds the `admin` role and
/// is the last active administrator, i.e. removing/demoting it now would
/// leave zero active admins. Must be called after [`lock_last_admin_guard`]
/// inside the same transaction so the read is authoritative.
async fn last_admin_guard_trips(
    transaction: &mut PgConnection,
    principal_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let current_role =
        sqlx::query_scalar::<_, String>("select role::text from iam_user where principal_id = $1")
            .bind(principal_id)
            .fetch_optional(&mut *transaction)
            .await?;
    if current_role.as_deref() != Some(SystemRole::Admin.as_str()) {
        return Ok(false);
    }
    Ok(count_admin_users(&mut *transaction).await? <= 1)
}

/// Creates a new user principal plus its `iam_user` row with a system role.
///
/// `password_hash` must already be produced by the configured Argon2 hashing
/// used by `setup_bootstrap_admin` (see `services::iam::service::hash_password`).
/// Returns the freshly inserted [`IamUserRow`].
pub async fn create_user(
    postgres: &PgPool,
    login: &str,
    email: &str,
    display_name: &str,
    password_hash: &str,
    role: SystemRole,
) -> Result<IamUserRow, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
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

    let row = sqlx::query_as::<_, IamUserRow>(
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
        values ($1, $2, $3, $4, $5, 'password', null, $6::iam_system_role)
        returning
            principal_id,
            login,
            email,
            display_name,
            password_hash,
            auth_provider_kind,
            external_subject,
            role::text as role",
    )
    .bind(principal_id)
    .bind(login)
    .bind(email)
    .bind(display_name)
    .bind(password_hash)
    .bind(role.as_str())
    .fetch_one(&mut *transaction)
    .await?;

    transaction.commit().await?;
    Ok(row)
}

/// Outcome of [`set_user_role`].
pub enum SetUserRoleOutcome {
    /// The role was updated.
    Updated(IamUserRow),
    /// No `iam_user` row exists for that principal.
    NotFound,
    /// The update was blocked because it would demote the last remaining
    /// active administrator.
    WouldDemoteLastAdmin,
}

/// Sets the system role for the user identified by `principal_id`.
///
/// Demoting the last active administrator away from `admin` is guarded
/// atomically: the last-admin check and the update run in the same
/// transaction under a repository-internal advisory lock, closing the TOCTOU window a
/// separate pre-check would leave open against concurrent admin
/// deletes/demotions.
pub async fn set_user_role(
    postgres: &PgPool,
    principal_id: Uuid,
    role: SystemRole,
) -> Result<SetUserRoleOutcome, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    lock_last_admin_guard(&mut transaction).await?;

    if role != SystemRole::Admin && last_admin_guard_trips(&mut transaction, principal_id).await? {
        transaction.rollback().await?;
        return Ok(SetUserRoleOutcome::WouldDemoteLastAdmin);
    }

    let updated = sqlx::query_as::<_, IamUserRow>(
        "update iam_user
         set role = $2::iam_system_role
         where principal_id = $1
         returning
            principal_id,
            login,
            email,
            display_name,
            password_hash,
            auth_provider_kind,
            external_subject,
            role::text as role",
    )
    .bind(principal_id)
    .bind(role.as_str())
    .fetch_optional(&mut *transaction)
    .await?;

    transaction.commit().await?;
    Ok(match updated {
        Some(row) => SetUserRoleOutcome::Updated(row),
        None => SetUserRoleOutcome::NotFound,
    })
}

/// Outcome of [`delete_user`].
pub enum DeleteUserOutcome {
    /// The user was deleted.
    Deleted(IamUserRow),
    /// No `iam_user` row exists for that principal.
    NotFound,
    /// Deletion was blocked because it would remove the last remaining
    /// active administrator.
    WouldRemoveLastAdmin,
}

/// Deletes a user: the user's grants are hard-deleted, active sessions are
/// revoked, and the paired `iam_principal` row is soft-disabled (mirrors
/// [`delete_revoked_api_token`]'s shape for the token principal case — the
/// principal row stays resolvable by id for historical
/// `actor_principal_id`/`created_by_principal_id` references, which use
/// `ON DELETE SET NULL`, not a hard delete of `iam_principal` itself).
///
/// The `iam_user` row itself is kept as an identity tombstone, not
/// hard-deleted. [`provision_oidc_user`] looks up an existing identity by
/// `(auth_provider_kind, external_subject)` before creating a new one; if
/// that row were purged, a re-provisioning attempt for the same external
/// subject would find nothing and silently mint a fresh, active principal —
/// resurrecting access for an identity an operator just removed. Keeping the
/// row lets that lookup find it and hit the existing
/// `principal.status != "active"` rejection in
/// `authenticate_session`/`authenticate_oidc_session` instead. The tombstone
/// also means the deleted `login`/`email` stay reserved under their UNIQUE
/// constraints indefinitely; a different subject that later JIT-provisions
/// with the same login/email hits [`OidcProvisionUserOutcome::IdentityCollision`]
/// rather than being silently granted the deleted identity's access — a
/// permanent conflict with no admin remedy today, strictly safer than the
/// alternative but worth knowing about.
///
/// The last-admin guard and the delete run in the same transaction under a
/// repository-internal advisory lock, closing the TOCTOU window a separate pre-check
/// would leave open against concurrent admin deletes/demotions.
pub async fn delete_user(
    postgres: &PgPool,
    principal_id: Uuid,
) -> Result<DeleteUserOutcome, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    lock_last_admin_guard(&mut transaction).await?;

    if last_admin_guard_trips(&mut transaction, principal_id).await? {
        transaction.rollback().await?;
        return Ok(DeleteUserOutcome::WouldRemoveLastAdmin);
    }

    let deleted_user = sqlx::query_as::<_, IamUserRow>(
        "select
            principal_id,
            login,
            email,
            display_name,
            password_hash,
            auth_provider_kind,
            external_subject,
            role::text as role
         from iam_user
         where principal_id = $1
         for update",
    )
    .bind(principal_id)
    .fetch_optional(&mut *transaction)
    .await?;

    let Some(deleted_user) = deleted_user else {
        transaction.rollback().await?;
        return Ok(DeleteUserOutcome::NotFound);
    };

    sqlx::query("delete from iam_grant where principal_id = $1")
        .bind(principal_id)
        .execute(&mut *transaction)
        .await?;
    sqlx::query(
        "update iam_session
         set revoked_at = coalesce(revoked_at, now())
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
           and principal_kind = 'user'",
    )
    .bind(principal_id)
    .execute(&mut *transaction)
    .await?;

    transaction.commit().await?;
    Ok(DeleteUserOutcome::Deleted(deleted_user))
}
