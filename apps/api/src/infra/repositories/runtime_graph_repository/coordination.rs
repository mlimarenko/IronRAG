use sqlx::{PgPool, Postgres};
use uuid::Uuid;

/// Acquires a library-scoped PostgreSQL advisory lock for canonical graph serialization.
///
/// The returned pooled connection keeps the session lock alive until
/// `release_runtime_library_graph_lock` is called.
pub async fn acquire_runtime_library_graph_lock(
    pool: &PgPool,
    library_id: Uuid,
) -> Result<sqlx::pool::PoolConnection<Postgres>, sqlx::Error> {
    let mut connection = pool.acquire().await?;
    sqlx::query("select pg_advisory_lock(hashtextextended($1::text, 0))")
        .bind(library_id.to_string())
        .execute(&mut *connection)
        .await?;
    Ok(connection)
}

/// Releases a library-scoped PostgreSQL advisory lock for canonical graph serialization.
pub async fn release_runtime_library_graph_lock(
    mut connection: sqlx::pool::PoolConnection<Postgres>,
    library_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("select pg_advisory_unlock(hashtextextended($1::text, 0))")
        .bind(library_id.to_string())
        .execute(&mut *connection)
        .await?;
    Ok(())
}

/// Counts distinct filtered graph artifacts written for one ingestion attempt.
pub async fn count_runtime_graph_filtered_artifacts_by_ingestion_run(
    pool: &PgPool,
    library_id: Uuid,
    ingestion_run_id: Uuid,
    revision_id: Option<Uuid>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "select count(distinct concat_ws(
                ':',
                coalesce(revision_id::text, 'none'),
                coalesce(ingestion_run_id::text, 'none'),
                target_kind,
                candidate_key,
                filter_reason
            ))
         from runtime_graph_filtered_artifact
         where library_id = $1
           and ingestion_run_id = $2
           and ($3::uuid is null or revision_id = $3)",
    )
    .bind(library_id)
    .bind(ingestion_run_id)
    .bind(revision_id)
    .fetch_one(pool)
    .await
}
