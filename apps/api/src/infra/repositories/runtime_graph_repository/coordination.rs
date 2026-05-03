use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

/// Acquires a library-scoped PostgreSQL advisory lock for canonical graph serialization.
///
/// The returned transaction keeps the lock alive until commit/rollback.
/// Transaction-scoped locks are used deliberately so a cancelled future cannot
/// leak a session lock back into the pool.
pub async fn acquire_runtime_library_graph_lock(
    pool: &PgPool,
    library_id: Uuid,
) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1::text, 0))")
        .bind(library_id.to_string())
        .execute(&mut *transaction)
        .await?;
    Ok(transaction)
}

/// Releases a library-scoped PostgreSQL advisory lock for canonical graph serialization.
pub async fn release_runtime_library_graph_lock(
    transaction: Transaction<'static, Postgres>,
    _library_id: Uuid,
) -> Result<(), sqlx::Error> {
    transaction.commit().await
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
