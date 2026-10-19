use super::*;

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

pub async fn touch_session_if_stale(
    postgres: &PgPool,
    session_id: Uuid,
    stale_before: DateTime<Utc>,
) -> Result<Option<IamSessionRow>, sqlx::Error> {
    sqlx::query_as::<_, IamSessionRow>(
        "update iam_session
         set last_seen_at = now()
         where id = $1
           and last_seen_at <= $2
         returning id, principal_id, session_secret_hash, issued_at, expires_at, revoked_at, last_seen_at",
    )
    .bind(session_id)
    .bind(stale_before)
    .fetch_optional(postgres)
    .await
}
