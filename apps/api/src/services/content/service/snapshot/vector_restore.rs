use super::*;

/// Writes a batch of per-dim vector rows into the shared `knowledge_*_vector_d*`
/// shard, guarded by a savepoint + bounded deadlock/contention retry
/// (Workstream R / R2 + in-transaction R3).
///
/// Parallel restores into the same shared shard race for the same pages and
/// hit deadlocks (`40P01`); the lazy `CREATE ... IF NOT EXISTS` for a brand-new
/// dim can also race two sessions on the catalog insert. Both abort the
/// statement — and, unguarded, the whole restore transaction. We run the shard
/// create + insert inside a SAVEPOINT (`tx.begin()`), so on a retryable
/// contention we roll back to the savepoint (leaving the outer transaction and
/// its tx-scoped `session_replication_role = 'replica'` intact) and replay the
/// same in-memory rows. `ON CONFLICT (key) DO NOTHING` makes only exact
/// physical-key replay idempotent; a different key for the same logical vector
/// still reaches the logical unique index and fails closed.
pub(super) async fn insert_pg_vector_rows_bulk(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    relation_name: &str,
    rows: Vec<serde_json::Value>,
) -> anyhow::Result<()> {
    let dim = parse_per_dim_vector_collection_dim(relation_name)
        .ok_or_else(|| anyhow!("invalid vector relation `{relation_name}`"))?;
    let count = rows.len();
    let payload = serde_json::Value::Array(rows);
    for attempt in 1..=RESTORE_SAVEPOINT_MAX_ATTEMPTS {
        let mut savepoint = tx.begin().await.context("open vector-shard write savepoint")?;
        match insert_pg_vector_rows_bulk_once(&mut savepoint, relation_name, dim, count, &payload)
            .await
        {
            Ok(()) => {
                savepoint.commit().await.context("release vector-shard write savepoint")?;
                return Ok(());
            }
            Err(error) => {
                // Roll the savepoint back so the outer transaction stays usable,
                // then decide whether the failure is transient.
                savepoint.rollback().await.context("roll back vector-shard write savepoint")?;
                let retryable = error
                    .downcast_ref::<sqlx::Error>()
                    .is_some_and(pg_error_is_retryable_restore_contention);
                if retryable && attempt < RESTORE_SAVEPOINT_MAX_ATTEMPTS {
                    tracing::warn!(
                        relation = %relation_name,
                        attempt,
                        max_attempts = RESTORE_SAVEPOINT_MAX_ATTEMPTS,
                        "vector-shard write hit transient contention; retrying after savepoint rollback",
                    );
                    tokio::time::sleep(RESTORE_SAVEPOINT_BACKOFF_BASE * attempt).await;
                    continue;
                }
                return Err(error);
            }
        }
    }
    unreachable!("vector-shard write retry loop exits via return")
}

const RESTORE_VECTOR_MANIFEST_UPSERT_SQL: &str =
    "RETURNING library_id, dimensions, vector_kind, embedding_model_key
     ), lanes AS MATERIALIZED (
        SELECT DISTINCT library_id, dimensions, vector_kind, embedding_model_key
        FROM inserted
     )
     INSERT INTO knowledge_vector_relation_manifest (
         library_id, dim, vector_kind, embedding_model_key, relation_name,
         is_default, row_count, promoted
     )
     SELECT library_id, dimensions, vector_kind, embedding_model_key,
         $2, true, 0, false
     FROM lanes
     ON CONFLICT (library_id, dim, vector_kind, embedding_model_key)
     DO UPDATE SET relation_name = excluded.relation_name,
                   is_default = true,
                   promoted = false";

/// One attempt of the vector-shard write, executed inside a savepoint by
/// [`insert_pg_vector_rows_bulk`]. Surfaces the raw `sqlx::Error` (wrapped by
/// `anyhow`) so the caller can classify deadlock/contention via
/// [`pg_error_is_retryable_restore_contention`].
pub(super) async fn insert_pg_vector_rows_bulk_once(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    relation_name: &str,
    dim: u64,
    count: usize,
    payload: &serde_json::Value,
) -> anyhow::Result<()> {
    ensure_pg_vector_relation(tx, relation_name, dim).await?;
    let relation = quote_pg_identifier(relation_name)?;
    let storage = PgVectorStorage::for_dim(dim);
    let cast_type = storage.cast_type();
    if is_per_dim_chunk_vector_collection_name(relation_name) {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "WITH rows AS MATERIALIZED (
                SELECT DISTINCT ON (key)
                    key, vector_id, workspace_id, library_id, chunk_id, revision_id,
                    embedding_model_key, vector_kind, dimensions, embedding,
                    freshness_generation, created_at, occurred_at, occurred_until
                FROM jsonb_to_recordset($1) AS row(
                    key text, vector_id uuid, workspace_id uuid, library_id uuid,
                    chunk_id uuid, revision_id uuid, embedding_model_key text,
                    vector_kind text, dimensions integer, embedding text,
                    freshness_generation bigint, created_at timestamptz,
                    occurred_at timestamptz, occurred_until timestamptz
                )
                ORDER BY key, freshness_generation DESC NULLS LAST, created_at DESC NULLS LAST
             ), inserted AS (
                INSERT INTO {relation} (
                    key, vector_id, workspace_id, library_id, chunk_id, revision_id,
                    embedding_model_key, vector_kind, dimensions, embedding,
                    freshness_generation, created_at, occurred_at, occurred_until
                )
                SELECT key, vector_id, workspace_id, library_id, chunk_id, revision_id,
                    embedding_model_key, vector_kind, dimensions, embedding::{cast_type},
                    freshness_generation, created_at, occurred_at, occurred_until
                FROM rows
                {PG_VECTOR_RESTORE_KEY_REPLAY_CONFLICT}
                {RESTORE_VECTOR_MANIFEST_UPSERT_SQL}
             "
        )))
        .bind(payload)
        .bind(relation_name)
        .execute(&mut **tx)
        .await
        .with_context(|| format!("bulk insert {count} chunk vectors into {relation_name}"))?;
    } else {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "WITH rows AS MATERIALIZED (
                SELECT DISTINCT ON (key)
                    key, vector_id, workspace_id, library_id, entity_id,
                    embedding_model_key, vector_kind, dimensions, embedding,
                    freshness_generation, created_at
                FROM jsonb_to_recordset($1) AS row(
                    key text, vector_id uuid, workspace_id uuid, library_id uuid,
                    entity_id uuid, embedding_model_key text, vector_kind text,
                    dimensions integer, embedding text, freshness_generation bigint,
                    created_at timestamptz
                )
                ORDER BY key, freshness_generation DESC NULLS LAST, created_at DESC NULLS LAST
             ), inserted AS (
                INSERT INTO {relation} (
                    key, vector_id, workspace_id, library_id, entity_id,
                    embedding_model_key, vector_kind, dimensions, embedding,
                    freshness_generation, created_at
                )
                SELECT key, vector_id, workspace_id, library_id, entity_id,
                    embedding_model_key, vector_kind, dimensions, embedding::{cast_type},
                    freshness_generation, created_at
                FROM rows
                {PG_VECTOR_RESTORE_KEY_REPLAY_CONFLICT}
                {RESTORE_VECTOR_MANIFEST_UPSERT_SQL}
             "
        )))
        .bind(payload)
        .bind(relation_name)
        .execute(&mut **tx)
        .await
        .with_context(|| format!("bulk insert {count} entity vectors into {relation_name}"))?;
    }
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE knowledge_vector_relation_manifest m
         SET row_count = (
            SELECT count(*)::bigint
            FROM {relation} v
            WHERE v.library_id = m.library_id
              AND v.dimensions = m.dim
              AND v.vector_kind = m.vector_kind
              AND v.embedding_model_key = m.embedding_model_key
         )
         WHERE m.relation_name = $1"
    )))
    .bind(relation_name)
    .execute(&mut **tx)
    .await
    .with_context(|| format!("refresh vector manifest row counts for {relation_name}"))?;
    Ok(())
}

pub(super) async fn ensure_pg_vector_relation(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    relation_name: &str,
    dim: u64,
) -> anyhow::Result<()> {
    let relation = quote_pg_identifier(relation_name)?;
    let storage = PgVectorStorage::for_dim(dim);
    let dim = checked_vector_dim_i32(dim)?;
    let embedding_type = storage.column_type(dim);
    let (id_column, extra_column, identity_columns) =
        if is_per_dim_chunk_vector_collection_name(relation_name) {
            (
                "chunk_id",
                Some("revision_id"),
                "chunk_id uuid not null,
                 revision_id uuid not null",
            )
        } else {
            ("entity_id", None, "entity_id uuid not null")
        };
    let event_columns = if extra_column.is_some() {
        ",
         occurred_at timestamptz,
         occurred_until timestamptz"
    } else {
        ""
    };
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE TABLE IF NOT EXISTS {relation} (
            key text primary key,
            vector_id uuid not null,
            workspace_id uuid not null,
            library_id uuid not null,
            {identity_columns},
            embedding_model_key text not null,
            vector_kind text not null,
            dimensions integer not null check (dimensions = {dim}),
            embedding {embedding_type} not null,
            freshness_generation bigint not null,
            created_at timestamptz not null{event_columns}
        )"
    )))
    .execute(&mut **tx)
    .await
    .with_context(|| format!("create vector relation {relation_name}"))?;
    ensure_pg_vector_relation_indexes(tx, relation_name, id_column, extra_column, storage, dim)
        .await?;
    // Runtime-created shards get their library cascade here; the schema
    // baseline cannot declare a foreign key for a table it never creates.
    ensure_vector_relation_library_cascade(tx, relation_name).await?;
    Ok(())
}

pub(super) async fn ensure_pg_vector_relation_indexes(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    relation_name: &str,
    id_column: &str,
    extra_column: Option<&str>,
    storage: PgVectorStorage,
    dim: i32,
) -> anyhow::Result<()> {
    let relation = quote_pg_identifier(relation_name)?;
    let logical_identity_sql =
        pg_vector_logical_identity_index_sql(relation_name, id_column, extra_column)?;
    sqlx::query(sqlx::AssertSqlSafe(logical_identity_sql))
        .execute(&mut **tx)
        .await
        .with_context(|| format!("create logical identity index on {relation_name}"))?;

    let lane_idx = quote_pg_identifier(&format!("{relation_name}_lane_idx"))?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE INDEX IF NOT EXISTS {lane_idx}
         ON {relation} (library_id, embedding_model_key, vector_kind)"
    )))
    .execute(&mut **tx)
    .await
    .with_context(|| format!("create lane index on {relation_name}"))?;

    let id_idx = quote_pg_identifier(&format!("{relation_name}_{id_column}_idx"))?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE INDEX IF NOT EXISTS {id_idx} ON {relation} ({id_column})"
    )))
    .execute(&mut **tx)
    .await
    .with_context(|| format!("create id index on {relation_name}"))?;

    if let Some(extra_column) = extra_column {
        let extra_idx = quote_pg_identifier(&format!("{relation_name}_{extra_column}_idx"))?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE INDEX IF NOT EXISTS {extra_idx} ON {relation} ({extra_column})"
        )))
        .execute(&mut **tx)
        .await
        .with_context(|| format!("create extra index on {relation_name}"))?;
    }
    ensure_pg_vector_relation_hnsw_index(tx, relation_name, storage, dim).await?;
    Ok(())
}

pub(super) fn pg_vector_logical_identity_index_sql(
    relation_name: &str,
    id_column: &str,
    extra_column: Option<&str>,
) -> anyhow::Result<String> {
    let relation = quote_pg_identifier(relation_name)?;
    let logical_key_idx =
        quote_pg_identifier(&format!("{relation_name}{PG_VECTOR_LOGICAL_KEY_INDEX_SUFFIX}"))?;
    let id_column = quote_pg_identifier(id_column)?;
    let logical_identity_columns = match extra_column {
        Some(extra_column) => format!(
            "library_id, {id_column}, {}, embedding_model_key, vector_kind, freshness_generation",
            quote_pg_identifier(extra_column)?,
        ),
        None => format!(
            "library_id, {id_column}, embedding_model_key, vector_kind, freshness_generation"
        ),
    };
    Ok(format!(
        "create unique index if not exists {logical_key_idx} on {relation} ({logical_identity_columns})"
    ))
}

pub(super) async fn ensure_pg_vector_relation_hnsw_index(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    relation_name: &str,
    storage: PgVectorStorage,
    dim: i32,
) -> anyhow::Result<()> {
    let relation = quote_pg_identifier(relation_name)?;
    let hnsw_idx = quote_pg_identifier(&format!("{relation_name}_hnsw"))?;
    let row_count = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
        "SELECT count(*)::bigint FROM {relation}"
    )))
    .fetch_one(&mut **tx)
    .await
    .with_context(|| format!("count vector rows in {relation_name} for HNSW sizing"))?;
    let row_count = u64::try_from(row_count).context("negative vector shard row count")?;
    let params = pg_hnsw_index_params(row_count, dim, storage)?;
    let ops = storage.cosine_ops();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE INDEX IF NOT EXISTS {hnsw_idx}
         ON {relation} USING hnsw (embedding {ops})
         WITH (m = {m}, ef_construction = {ef_construction})",
        m = params.m,
        ef_construction = params.ef_construction
    )))
    .execute(&mut **tx)
    .await
    .with_context(|| format!("create HNSW index on {relation_name}"))?;
    Ok(())
}

pub(super) fn checked_vector_dim_i32(dim: u64) -> anyhow::Result<i32> {
    anyhow::ensure!(dim > 0, "vector dimension must be positive");
    i32::try_from(dim).context("vector dimension overflowed i32")
}
