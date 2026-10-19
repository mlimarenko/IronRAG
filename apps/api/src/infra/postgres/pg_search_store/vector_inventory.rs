//! Bounded, read-only global vector-lifecycle inventory probe.
//!
//! The vector-lifecycle observability contract
//! (`services::maintenance::vector_lifecycle_observability`) declares the
//! `inventory.*` series. This module is the single source of the inventory facts
//! those series need: one canonical read-only snapshot per tick, classified
//! globally in SQL once per shared shard. A nullable library scope keeps the
//! same query usable for operator diagnostics and isolation tests, rather than a
//! second query path. Classification uses the same SQL CTE as the operator
//! collector, embedded from `vector-lifecycle-inventory.sql`.
//! GC backlog is a separate fact: it uses the canonical sweep predicate, not a
//! claim that every retained vector is current or every orphan is reclaimable.
//!
//! The probe is deliberately bounded: it issues one grouped aggregate per chunk
//! relation plus one size read per shard, so no vector row ever crosses the
//! client boundary. Relation names are discovered from `pg_catalog` and
//! interpolated only through `quote_identifier` after
//! `validate_relation_name`, exactly as the mutation paths do. Each read runs in
//! a short read-only transaction with a statement timeout so a probe can never
//! pin a connection behind the foreground workload.

use super::*;

/// The lifecycle category integer tags the probe emits, mirroring the canonical
/// collector `CATEGORY_NAMES` values in
/// `scripts/ops/collect_vector_lifecycle_state.py`.
pub(crate) const LIFECYCLE_TAG_CURRENT_READABLE: i64 = 0;
pub(crate) const LIFECYCLE_TAG_PROTECTED_ACTIVE_INGEST: i64 = 1;
pub(crate) const LIFECYCLE_TAG_SUPERSEDED_READABLE_HISTORY: i64 = 2;
pub(crate) const LIFECYCLE_TAG_INCONSISTENT_OR_ORPHAN: i64 = 3;

/// Statement timeout for one bounded inventory aggregate, in milliseconds. A
/// probe that cannot finish inside the budget is abandoned by PostgreSQL rather
/// than holding a connection.
const INVENTORY_STATEMENT_TIMEOUT_MS: i64 = 30_000;

/// One chunk `(dimensions, lifecycle_category)` count and logical byte sum.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VectorLifecycleRow {
    pub(crate) dimensions: u64,
    pub(crate) lifecycle_category: i64,
    pub(crate) vector_rows: i64,
    pub(crate) logical_embedding_value_bytes: i64,
}

/// One physical shard's exact relation size, shared by every library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PhysicalVectorShard {
    pub(crate) vector_kind: String,
    pub(crate) dimensions: u64,
    pub(crate) physical_shard_bytes: i64,
}

/// A bounded read-only inventory sample. `scope = Some(library)` restricts it to
/// that library for operator diagnostics and isolation tests; `None` is the
/// canonical global snapshot the worker emits once per tick.
#[derive(Debug, Clone, Default)]
pub(crate) struct VectorInventory {
    pub(crate) lifecycle_rows: Vec<VectorLifecycleRow>,
    pub(crate) physical_shards: Vec<PhysicalVectorShard>,
    /// Vectors the superseded sweep would reclaim, from the canonical
    /// current-vector predicate complement. This is the bounded backlog signal.
    pub(crate) backlog_vectors: i64,
    /// Age in seconds of the oldest reclaimable vector across every chunk
    /// relation. `None` means there is no backlog, never a fabricated zero.
    pub(crate) backlog_oldest_age_seconds: Option<f64>,
}

/// Read the bounded lifecycle inventory, globally (`scope = None`) or for one
/// library (`scope = Some`). The same canonical classification CTE runs once per
/// shared shard; the nullable predicate is the only scope switch.
///
/// # Errors
/// Returns any `SQLx` error or a relation-name validation failure.
pub(crate) async fn read_vector_inventory(
    pool: &PgPool,
    scope: Option<Uuid>,
) -> anyhow::Result<VectorInventory> {
    let mut transaction =
        pool.begin().await.context("begin vector inventory read-only snapshot")?;
    apply_inventory_transaction_settings(&mut transaction).await?;

    let mut inventory = VectorInventory::default();
    let chunk_relations = list_physical_vector_relations_in_transaction(
        &mut transaction,
        CHUNK_VECTOR_RELATION_PREFIX,
    )
    .await?;
    let entity_relations = list_physical_vector_relations_in_transaction(
        &mut transaction,
        ENTITY_VECTOR_RELATION_PREFIX,
    )
    .await?;

    for relation_name in &chunk_relations {
        inventory
            .lifecycle_rows
            .extend(read_chunk_relation(&mut transaction, relation_name, scope).await?);
        let (vectors, age) = read_chunk_backlog(&mut transaction, relation_name, scope).await?;
        inventory.backlog_vectors = inventory.backlog_vectors.saturating_add(vectors);
        if let Some(age) = age {
            inventory.backlog_oldest_age_seconds =
                Some(inventory.backlog_oldest_age_seconds.map_or(age, |current| current.max(age)));
        }
    }

    for relation_name in chunk_relations.iter().chain(entity_relations.iter()) {
        let is_chunk = chunk_relations.contains(relation_name);
        inventory
            .physical_shards
            .push(read_physical_shard(&mut transaction, relation_name, is_chunk).await?);
    }

    transaction.commit().await.context("commit vector inventory read-only snapshot")?;
    Ok(inventory)
}

/// Apply the bounded read-only transaction settings.
///
/// `statement_timeout` is a GUC, not a utility statement that accepts bind
/// parameters: `SET LOCAL` cannot take `$1`, so the canonical `set_config`
/// function is used instead, with the timeout passed as text.
async fn apply_inventory_transaction_settings(
    transaction: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<()> {
    sqlx::query("set transaction isolation level repeatable read read only")
        .execute(&mut **transaction)
        .await
        .context("mark vector inventory transaction repeatable read and read only")?;
    sqlx::query("select set_config('statement_timeout', $1, true)")
        .bind(INVENTORY_STATEMENT_TIMEOUT_MS.to_string())
        .execute(&mut **transaction)
        .await
        .context("bound vector inventory statement timeout")?;
    Ok(())
}

/// Render the collector's canonical CTE for a validated shard with a nullable
/// library scope bound at `$1` (psql interpolates the same `%3$L` placeholder).
fn classification_sql(relation_name: &str) -> anyhow::Result<String> {
    const SOURCE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../scripts/ops/vector-lifecycle-inventory.sql"
    ));
    validate_relation_name(relation_name, CHUNK_VECTOR_RELATION_PREFIX)?;
    let dimensions = vector_relation_dimensions(relation_name, CHUNK_VECTOR_RELATION_PREFIX)?;
    let (_, tail) = SOURCE
        .split_once("-- BEGIN CHUNK VECTOR LIFECYCLE CLASSIFICATION")
        .context("collector classification start is missing")?;
    let (cte, _) = tail
        .split_once("-- END CHUNK VECTOR LIFECYCLE CLASSIFICATION")
        .context("collector classification end is missing")?;
    Ok(cte
        .replace("%1$s", &dimensions.to_string())
        .replace("%2$s", &quote_identifier(relation_name)?)
        .replace("%3$L", "$1"))
}

fn lifecycle_category_tag(category: &str) -> anyhow::Result<i64> {
    match category {
        "current_readable" => Ok(LIFECYCLE_TAG_CURRENT_READABLE),
        "protected_active_ingest" => Ok(LIFECYCLE_TAG_PROTECTED_ACTIVE_INGEST),
        "superseded_readable_history" => Ok(LIFECYCLE_TAG_SUPERSEDED_READABLE_HISTORY),
        "inconsistent_or_orphan" => Ok(LIFECYCLE_TAG_INCONSISTENT_OR_ORPHAN),
        _ => anyhow::bail!("collector returned an unknown vector lifecycle category"),
    }
}

/// Emit all four categories, including zeros that clear a previous sample.
async fn read_chunk_relation(
    transaction: &mut Transaction<'_, Postgres>,
    relation_name: &str,
    scope: Option<Uuid>,
) -> anyhow::Result<Vec<VectorLifecycleRow>> {
    let dimensions = vector_relation_dimensions(relation_name, CHUNK_VECTOR_RELATION_PREFIX)?;
    let cte = classification_sql(relation_name)?;
    let counts: Vec<(String, i64, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{cte} select lifecycle_category, count(*)::bigint,
         coalesce(sum(logical_embedding_value_bytes), 0)::bigint
         from categorized group by lifecycle_category"
    )))
    .bind(scope)
    .fetch_all(&mut **transaction)
    .await
    .context("read canonical chunk lifecycle classification")?;
    let mut rows: Vec<_> = (0..4)
        .map(|category| VectorLifecycleRow {
            dimensions,
            lifecycle_category: category,
            vector_rows: 0,
            logical_embedding_value_bytes: 0,
        })
        .collect();
    for (category, vector_rows, logical_embedding_value_bytes) in counts {
        let tag = lifecycle_category_tag(&category)?;
        let row = &mut rows[usize::try_from(tag)?];
        row.vector_rows = vector_rows;
        row.logical_embedding_value_bytes = logical_embedding_value_bytes;
    }
    Ok(rows)
}

async fn read_chunk_backlog(
    transaction: &mut Transaction<'_, Postgres>,
    relation_name: &str,
    scope: Option<Uuid>,
) -> anyhow::Result<(i64, Option<f64>)> {
    let relation = quote_identifier(relation_name)?;
    let predicate = selectable_chunk_vector_rows_sql();
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select count(*)::bigint,
         case when count(*) = 0 then null::double precision
         else greatest(0, extract(epoch from now() - min(page.created_at)))::double precision end
         from {relation} page
         left join knowledge_chunk chunk on chunk.chunk_id = page.chunk_id
           and chunk.revision_id = page.revision_id and chunk.library_id = page.library_id
         left join knowledge_document document on document.document_id = chunk.document_id
           and document.library_id = page.library_id
         where ($1::uuid is null or page.library_id = $1::uuid) and not {predicate}"
    )))
    .bind(scope)
    .bind(false)
    .fetch_one(&mut **transaction)
    .await
    .context("read canonical sweep backlog")
}

/// Exact shared relation size for one `(kind, dimensions)` shard.
async fn read_physical_shard(
    transaction: &mut Transaction<'_, Postgres>,
    relation_name: &str,
    is_chunk: bool,
) -> anyhow::Result<PhysicalVectorShard> {
    let prefix =
        if is_chunk { CHUNK_VECTOR_RELATION_PREFIX } else { ENTITY_VECTOR_RELATION_PREFIX };
    validate_relation_name(relation_name, prefix)?;
    // `pg_total_relation_size` takes a `regclass`; a quoted identifier in the
    // function call parses as a column, so the validated relation name is bound
    // as a string and cast by PostgreSQL.
    let physical_shard_bytes: i64 =
        sqlx::query_scalar("select pg_total_relation_size($1::regclass)::bigint")
            .bind(relation_name)
            .fetch_one(&mut **transaction)
            .await
            .with_context(|| format!("read physical size for `{relation_name}`"))?;
    Ok(PhysicalVectorShard {
        vector_kind: if is_chunk { "chunk".to_string() } else { "entity".to_string() },
        dimensions: vector_relation_dimensions(relation_name, prefix)?,
        physical_shard_bytes,
    })
}

fn vector_relation_dimensions(relation_name: &str, prefix: &str) -> anyhow::Result<u64> {
    relation_name
        .strip_prefix(prefix)
        .filter(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()))
        .context("vector relation does not follow the dimension-shard protocol")?
        .parse::<u64>()
        .context("vector relation dimension overflowed u64")
}

#[cfg(test)]
mod tests;
