use super::*;

/// The durable account-scoped telemetry preference. Absence of a row means the
/// account has not chosen; the only stored values are `granted` and `denied`.
#[derive(Debug, Clone, FromRow)]
pub struct IamUserPreferenceRow {
    pub principal_id: Uuid,
    pub telemetry_consent: String,
    pub updated_at: DateTime<Utc>,
}

pub async fn get_user_preference(
    postgres: &PgPool,
    principal_id: Uuid,
) -> Result<Option<IamUserPreferenceRow>, sqlx::Error> {
    sqlx::query_as::<_, IamUserPreferenceRow>(
        "select principal_id, telemetry_consent, updated_at
         from iam_user_preference
         where principal_id = $1",
    )
    .bind(principal_id)
    .fetch_optional(postgres)
    .await
}

/// Records the caller's explicit choice, replacing any previous value.
pub async fn upsert_user_preference(
    postgres: &PgPool,
    principal_id: Uuid,
    telemetry_consent: &str,
) -> Result<IamUserPreferenceRow, sqlx::Error> {
    sqlx::query_as::<_, IamUserPreferenceRow>(
        "insert into iam_user_preference (principal_id, telemetry_consent)
         values ($1, $2)
         on conflict (principal_id) do update
         set telemetry_consent = excluded.telemetry_consent,
             updated_at = now()
         returning principal_id, telemetry_consent, updated_at",
    )
    .bind(principal_id)
    .bind(telemetry_consent)
    .fetch_one(postgres)
    .await
}

/// Seeds an unset account from the browser's pre-sign-in choice. A concurrent
/// earlier row wins: `ON CONFLICT DO NOTHING` never overwrites it, and the
/// current row is returned so the caller sees the durable value.
pub async fn seed_user_preference_if_absent(
    postgres: &PgPool,
    principal_id: Uuid,
    telemetry_consent: &str,
) -> Result<Option<IamUserPreferenceRow>, sqlx::Error> {
    sqlx::query(
        "insert into iam_user_preference (principal_id, telemetry_consent)
         values ($1, $2)
         on conflict (principal_id) do nothing",
    )
    .bind(principal_id)
    .bind(telemetry_consent)
    .execute(postgres)
    .await?;

    get_user_preference(postgres, principal_id).await
}
