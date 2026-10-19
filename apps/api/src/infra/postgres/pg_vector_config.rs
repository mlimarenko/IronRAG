use std::collections::HashMap;

use anyhow::Context;
use uuid::Uuid;

use super::pg_search_store::{
    CHUNK_VECTOR_RELATION_PREFIX, ENTITY_VECTOR_RELATION_PREFIX,
    VECTOR_RELATION_DDL_ADVISORY_LOCK_KEY, decrement_vector_manifest_counts_in_transaction,
};

const PGVECTOR_HNSW_VECTOR_MAX_DIM: u64 = 2_000;
const PG_HNSW_DEFAULT_BUILD_BUDGET_BYTES: u64 = 3_000_000_000;
const PG_HNSW_MIN_M: u64 = 8;
const PG_HNSW_MID_M: u64 = 16;
const PG_HNSW_LARGE_M: u64 = 24;

#[derive(Debug, Clone, Copy)]
pub(crate) enum PgVectorStorage {
    Vector,
    Halfvec,
}

impl PgVectorStorage {
    pub(crate) const fn for_dim(dim: u64) -> Self {
        if dim > PGVECTOR_HNSW_VECTOR_MAX_DIM { Self::Halfvec } else { Self::Vector }
    }

    pub(crate) fn column_type(self, dim: i32) -> String {
        match self {
            Self::Vector => format!("vector({dim})"),
            Self::Halfvec => format!("halfvec({dim})"),
        }
    }

    pub(crate) const fn cast_type(self) -> &'static str {
        match self {
            Self::Vector => "vector",
            Self::Halfvec => "halfvec",
        }
    }

    pub(crate) const fn cosine_ops(self) -> &'static str {
        match self {
            Self::Vector => "vector_cosine_ops",
            Self::Halfvec => "halfvec_cosine_ops",
        }
    }

    const fn bytes_per_component(self) -> u64 {
        match self {
            Self::Vector => 4,
            Self::Halfvec => 2,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PgHnswIndexParams {
    pub(crate) m: u64,
    pub(crate) ef_construction: u64,
}

pub(crate) fn pg_hnsw_index_params(
    row_count: u64,
    dim: i32,
    storage: PgVectorStorage,
) -> anyhow::Result<PgHnswIndexParams> {
    let dim = u64::try_from(dim).context("vector dimension must be positive")?;
    let configured_m = read_env_u64("IRONRAG_PG_HNSW_M");
    let configured_ef_construction = read_env_u64("IRONRAG_PG_HNSW_EF_CONSTRUCTION");
    let m = configured_m.map_or_else(
        || memory_safe_hnsw_m(row_count, dim, storage),
        |value| value.clamp(PG_HNSW_MIN_M, PG_HNSW_LARGE_M),
    );
    let ef_construction = configured_ef_construction.unwrap_or_else(|| m.saturating_mul(4)).max(m);
    Ok(PgHnswIndexParams { m, ef_construction })
}

fn memory_safe_hnsw_m(row_count: u64, dim: u64, storage: PgVectorStorage) -> u64 {
    let target = if row_count >= 100_000 {
        PG_HNSW_LARGE_M
    } else if row_count >= 1_000 {
        PG_HNSW_MID_M
    } else {
        PG_HNSW_MIN_M
    };
    let budget = pg_hnsw_build_budget_bytes();
    [target, PG_HNSW_MID_M, PG_HNSW_MIN_M]
        .into_iter()
        .find(|&m| estimated_hnsw_build_bytes(row_count, dim, storage, m) <= budget)
        .unwrap_or(PG_HNSW_MIN_M)
}

fn estimated_hnsw_build_bytes(row_count: u64, dim: u64, storage: PgVectorStorage, m: u64) -> u128 {
    let rows = u128::from(row_count.max(1));
    let vector_bytes = u128::from(dim) * u128::from(storage.bytes_per_component());
    let graph_bytes = u128::from(m) * 16;
    rows * (vector_bytes.saturating_mul(2) + graph_bytes)
}

fn pg_hnsw_build_budget_bytes() -> u128 {
    pg_hnsw_build_budget_bytes_from_override(read_env_u64("IRONRAG_PG_HNSW_BUILD_BUDGET_BYTES"))
}

fn pg_hnsw_build_budget_bytes_from_override(configured: Option<u64>) -> u128 {
    u128::from(configured.unwrap_or(PG_HNSW_DEFAULT_BUILD_BUDGET_BYTES))
}

pub(crate) fn read_env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            trimmed.parse::<u64>().ok().filter(|value| *value > 0)
        }
    })
}

/// Name of the foreign key tying a per-dim vector shard to its library.
pub(crate) fn vector_relation_library_cascade_name(relation_name: &str) -> String {
    format!("{relation_name}_library_id_workspace_id_fkey")
}

pub(crate) const VECTOR_RELATION_LIBRARY_CASCADE_PREDICATE: &str = "exists (
         select 1
         from pg_constraint constraint_row
         where constraint_row.conname = $1
           and constraint_row.conrelid = to_regclass($2)::oid
           and constraint_row.contype = 'f'
           and constraint_row.confrelid = 'catalog_library'::regclass
           and constraint_row.confdeltype = 'c'
           and constraint_row.convalidated
           and constraint_row.conkey = array[
               (select attnum::smallint from pg_attribute
                where attrelid = to_regclass($2)::oid
                  and attname = 'library_id' and not attisdropped),
               (select attnum::smallint from pg_attribute
                where attrelid = to_regclass($2)::oid
                  and attname = 'workspace_id' and not attisdropped)
           ]
           and constraint_row.confkey = array[
               (select attnum::smallint from pg_attribute
                where attrelid = 'catalog_library'::regclass
                  and attname = 'id' and not attisdropped),
               (select attnum::smallint from pg_attribute
                where attrelid = 'catalog_library'::regclass
                  and attname = 'workspace_id' and not attisdropped)
           ]
     )";

/// One orphaned `(library, embedding model, kind)` lane inside a shard.
#[derive(Debug, Clone)]
pub(crate) struct OrphanedVectorLane {
    pub(crate) library_id: Uuid,
    pub(crate) embedding_model_key: String,
    pub(crate) vector_kind: String,
    pub(crate) row_count: i64,
}

/// Every orphaned lane of one physical vector shard.
#[derive(Debug, Clone)]
pub(crate) struct OrphanedVectorRelation {
    pub(crate) relation_name: String,
    pub(crate) lanes: Vec<OrphanedVectorLane>,
}

/// Attaches `ON DELETE CASCADE` from a per-dim vector shard to `catalog_library`.
///
/// Per-dim shards are created at runtime, so the schema baseline cannot declare
/// their foreign keys — and without one, deleting a library leaves its vectors
/// behind forever, where they later collide with a re-import of the same
/// archive. Both shard-creating paths (the search store's lazy ensure and the
/// snapshot restore) call this.
///
/// It runs on every ensure, not only on create, because `create table if not
/// exists` is a no-op for a shard that already exists: a create-time-only fix
/// would skip every shard a deployment already has.
///
/// A row whose library no longer exists blocks the validated constraint, and
/// the ensure path refuses with the exact orphaned lanes instead of deleting
/// them: an implicit delete would hide data loss. The explicit escape hatch is
/// `ironrag-maintenance repair orphan-vector-rows`
/// ([`reconcile_orphaned_vector_rows`]).
pub(crate) async fn ensure_vector_relation_library_cascade(
    executor: &mut sqlx::PgConnection,
    relation_name: &str,
) -> anyhow::Result<()> {
    let constraint_name = vector_relation_library_cascade_name(relation_name);
    let already_present: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select {VECTOR_RELATION_LIBRARY_CASCADE_PREDICATE}"
    )))
    .bind(&constraint_name)
    .bind(relation_name)
    .fetch_one(&mut *executor)
    .await
    .with_context(|| format!("look up library cascade on {relation_name}"))?;
    let conflicting_constraints = sqlx::query_scalar::<_, String>(
        "select constraint_row.conname
         from pg_constraint constraint_row
         where constraint_row.contype = 'f'
           and constraint_row.conrelid = to_regclass($1)
           and constraint_row.confrelid = 'catalog_library'::regclass
           and cardinality(constraint_row.conkey) = 2
           and constraint_row.conkey @> array[
               (select attnum::smallint from pg_attribute
                where attrelid = to_regclass($1)::oid
                  and attname = 'library_id' and not attisdropped),
               (select attnum::smallint from pg_attribute
                where attrelid = to_regclass($1)::oid
                  and attname = 'workspace_id' and not attisdropped)
           ]
           and not (
               constraint_row.conname = $2
               and constraint_row.confdeltype = 'c'
               and constraint_row.convalidated
               and constraint_row.confkey = array[
                   (select attnum::smallint from pg_attribute
                    where attrelid = 'catalog_library'::regclass
                      and attname = 'id' and not attisdropped),
                   (select attnum::smallint from pg_attribute
                    where attrelid = 'catalog_library'::regclass
                      and attname = 'workspace_id' and not attisdropped)
               ]
           )
         order by constraint_row.conname",
    )
    .bind(relation_name)
    .bind(&constraint_name)
    .fetch_all(&mut *executor)
    .await
    .with_context(|| format!("find conflicting library foreign keys on {relation_name}"))?;
    let relation = quote_vector_identifier(relation_name)?;
    if !already_present {
        let orphaned_lanes = discover_orphaned_vector_lanes(executor, relation_name).await?;
        anyhow::ensure!(
            orphaned_lanes.is_empty(),
            "{}",
            orphaned_vector_lanes_diagnostic(relation_name, &orphaned_lanes)
        );
    }

    // The orphan check above writes nothing, so a refusal leaves the shard and
    // its manifest untouched. Relation-changing DDL starts only here.
    for conflicting_constraint in conflicting_constraints {
        let conflicting_constraint = quote_vector_identifier(&conflicting_constraint)?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "alter table {relation} drop constraint {conflicting_constraint}"
        )))
        .execute(&mut *executor)
        .await
        .with_context(|| format!("drop conflicting library foreign key on {relation_name}"))?;
    }
    if already_present {
        return Ok(());
    }
    let constraint = quote_vector_identifier(&constraint_name)?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "alter table {relation} drop constraint if exists {constraint}"
    )))
    .execute(&mut *executor)
    .await
    .with_context(|| format!("drop non-canonical library cascade on {relation_name}"))?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "alter table {relation}
         add constraint {constraint}
         foreign key (library_id, workspace_id)
         references catalog_library (id, workspace_id)
         on delete cascade"
    )))
    .execute(&mut *executor)
    .await
    .with_context(|| format!("attach library cascade to {relation_name}"))?;
    Ok(())
}

/// Serializes vector-shard DDL and repairs the catalog cascade on every
/// physical shard in a self-contained transaction. Catalog deletion completes
/// this repair before opening its parent-row transaction, so snapshot restore
/// cannot form a catalog-row -> DDL / DDL -> catalog-row lock cycle.
pub(crate) async fn lock_and_ensure_all_vector_relation_library_cascades(
    postgres: &sqlx::PgPool,
) -> anyhow::Result<()> {
    let mut transaction =
        postgres.begin().await.context("begin vector cascade repair before catalog deletion")?;
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1::text, 0))")
        .bind(VECTOR_RELATION_DDL_ADVISORY_LOCK_KEY)
        .execute(&mut *transaction)
        .await
        .context("serialize vector relation DDL before catalog deletion")?;
    let relation_names = discover_vector_relation_names(&mut transaction).await?;
    for relation_name in relation_names {
        ensure_vector_relation_library_cascade(&mut transaction, &relation_name).await?;
    }
    transaction.commit().await.context("commit vector cascade repair")?;
    Ok(())
}

/// Removes every orphaned vector-shard row and re-attaches the canonical
/// library cascade on every physical shard. Operator-invoked through
/// `repair orphan-vector-rows`; [`ensure_vector_relation_library_cascade`]
/// deliberately refuses to remove orphans implicitly so the deletion is always
/// explicit and auditable.
///
/// The cascade is ensured on orphan-free shards too: a deployment can have a
/// shard whose constraint was never attached or was dropped, and an
/// orphan-free shard would otherwise never reach the ensure branch. One
/// transaction holds the vector-DDL advisory lock, so it cannot race a shard's
/// lazy ensure or a catalog delete's own repair. `dry_run` reports the orphaned
/// lanes and writes nothing.
pub(crate) async fn reconcile_orphaned_vector_rows(
    postgres: &sqlx::PgPool,
    dry_run: bool,
) -> anyhow::Result<Vec<OrphanedVectorRelation>> {
    let mut transaction = postgres.begin().await.context("begin orphaned vector row reconcile")?;
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1::text, 0))")
        .bind(VECTOR_RELATION_DDL_ADVISORY_LOCK_KEY)
        .execute(&mut *transaction)
        .await
        .context("serialize vector relation DDL before orphan reconcile")?;
    let relation_names = discover_vector_relation_names(&mut transaction).await?;
    let mut reconciled = Vec::new();
    for relation_name in relation_names {
        if let Some(relation) =
            reconcile_vector_relation(&mut transaction, &relation_name, dry_run).await?
        {
            reconciled.push(relation);
        }
    }
    transaction.commit().await.context("commit orphaned vector row reconcile")?;
    Ok(reconciled)
}

/// Reconciles one physical shard. Reports its orphaned lanes, and on a real run
/// removes them and attaches the canonical cascade whether or not any orphan
/// remains. Returns `None` when the shard carries no orphaned lane.
async fn reconcile_vector_relation(
    executor: &mut sqlx::PgConnection,
    relation_name: &str,
    dry_run: bool,
) -> anyhow::Result<Option<OrphanedVectorRelation>> {
    let orphaned_lanes = discover_orphaned_vector_lanes(&mut *executor, relation_name).await?;
    if !dry_run {
        if !orphaned_lanes.is_empty() {
            remove_orphaned_vector_lanes(executor, relation_name, &orphaned_lanes).await?;
        } else {
            // A manifest lane can drift with no matching physical orphan (the
            // pre-reconcile repair left such lanes at row_count = 0). Delete
            // those too, or the baseline manifest foreign key can never be
            // re-validated.
            delete_orphaned_vector_manifest_lanes(executor, relation_name).await?;
        }
        ensure_vector_relation_library_cascade(executor, relation_name).await?;
    }
    if orphaned_lanes.is_empty() {
        return Ok(None);
    }
    Ok(Some(OrphanedVectorRelation {
        relation_name: relation_name.to_string(),
        lanes: orphaned_lanes
            .into_iter()
            .map(|(library_id, embedding_model_key, vector_kind, row_count)| OrphanedVectorLane {
                library_id,
                embedding_model_key,
                vector_kind,
                row_count,
            })
            .collect(),
    }))
}

/// Removes the orphaned physical rows of one shard and the manifest lanes of
/// libraries that no longer exist, then leaves the manifest counts exact for
/// the lanes that keep a live library.
///
/// The manifest lanes are locked before the physical delete. That preserves the
/// order the orphan and mutation paths use when the library is absent, but it
/// is not an absolute deadlock guard: the fenced vector-delete path locks the
/// manifest only after deleting physical rows, so a live library with
/// mismatched-workspace orphans can deadlock against a concurrent fenced delete
/// and the repair must simply be retried.
///
/// A manifest lane whose library is absent from the catalog is deleted before
/// the decrement: no decrement can reconcile such a lane, and a drifted
/// `row_count` on it would otherwise abort the whole repair. A live library's
/// lane survives even at `row_count = 0`.
async fn remove_orphaned_vector_lanes(
    executor: &mut sqlx::PgConnection,
    relation_name: &str,
    orphaned_lanes: &[(Uuid, String, String, i64)],
) -> anyhow::Result<()> {
    for (library_id, embedding_model_key, vector_kind, _) in orphaned_lanes {
        sqlx::query_scalar::<_, i64>(
            "select row_count
             from knowledge_vector_relation_manifest
             where library_id = $1
               and relation_name = $2
               and embedding_model_key = $3
               and vector_kind = $4
             for update",
        )
        .bind(library_id)
        .bind(relation_name)
        .bind(embedding_model_key)
        .bind(vector_kind)
        .fetch_optional(&mut *executor)
        .await
        .with_context(|| format!("lock orphan manifest lane in {relation_name}"))?;
    }

    let relation = quote_vector_identifier(relation_name)?;
    let deleted_lanes =
        sqlx::query_as::<_, (Uuid, String, String, i64)>(sqlx::AssertSqlSafe(format!(
            "with deleted as (
                 delete from {relation} v
                 where not exists (
                     select 1 from catalog_library l
                     where l.id = v.library_id and l.workspace_id = v.workspace_id
                 )
                 returning library_id, embedding_model_key, vector_kind
             )
             select library_id, embedding_model_key, vector_kind, count(*)::bigint
             from deleted
             group by library_id, embedding_model_key, vector_kind"
        )))
        .fetch_all(&mut *executor)
        .await
        .with_context(|| format!("drop orphaned rows from {relation_name}"))?;

    delete_orphaned_vector_manifest_lanes(&mut *executor, relation_name).await?;

    for (library_id, embedding_model_key, vector_kind, deleted) in deleted_lanes {
        decrement_vector_manifest_counts_in_transaction(
            &mut *executor,
            library_id,
            relation_name,
            HashMap::from([((embedding_model_key, vector_kind), deleted)]),
        )
        .await
        .with_context(|| format!("reconcile orphan cleanup in {relation_name}"))?;
    }
    Ok(())
}

/// Deletes the manifest lanes of one shard whose library is absent from the
/// catalog. These lanes can never be decremented back to a valid count, and a
/// surviving orphan row blocks re-validating the baseline manifest foreign key.
async fn delete_orphaned_vector_manifest_lanes(
    executor: &mut sqlx::PgConnection,
    relation_name: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "delete from knowledge_vector_relation_manifest m
         where m.relation_name = $1
           and not exists (select 1 from catalog_library l where l.id = m.library_id)",
    )
    .bind(relation_name)
    .execute(&mut *executor)
    .await
    .with_context(|| format!("delete orphaned manifest lanes in {relation_name}"))?;
    Ok(())
}

/// Physical per-dim vector shards in the current schema, both prefixes.
async fn discover_vector_relation_names(
    executor: &mut sqlx::PgConnection,
) -> anyhow::Result<Vec<String>> {
    sqlx::query_scalar::<_, String>(
        "select c.relname
         from pg_catalog.pg_class c
         join pg_catalog.pg_namespace n on n.oid = c.relnamespace
         where n.nspname = current_schema()
           and c.relkind = 'r'
           and (
               left(c.relname, char_length($1)) = $1
               or left(c.relname, char_length($2)) = $2
           )
         order by c.relname",
    )
    .bind(CHUNK_VECTOR_RELATION_PREFIX)
    .bind(ENTITY_VECTOR_RELATION_PREFIX)
    .fetch_all(&mut *executor)
    .await
    .context("discover physical vector shards")
}

/// Rows of one shard whose `(library_id, workspace_id)` is not in the catalog,
/// grouped and ordered by lane exactly as the diagnostic lists them.
async fn discover_orphaned_vector_lanes(
    executor: &mut sqlx::PgConnection,
    relation_name: &str,
) -> anyhow::Result<Vec<(Uuid, String, String, i64)>> {
    let relation = quote_vector_identifier(relation_name)?;
    sqlx::query_as::<_, (Uuid, String, String, i64)>(sqlx::AssertSqlSafe(format!(
        "select v.library_id,
                v.embedding_model_key,
                v.vector_kind,
                count(*)::bigint
         from {relation} v
         where not exists (
             select 1 from catalog_library l
             where l.id = v.library_id and l.workspace_id = v.workspace_id
         )
         group by v.library_id, v.embedding_model_key, v.vector_kind
         order by v.library_id, v.embedding_model_key, v.vector_kind"
    )))
    .fetch_all(&mut *executor)
    .await
    .with_context(|| format!("discover orphaned rows in {relation_name}"))
}

/// Fail-closed refusal text for [`ensure_vector_relation_library_cascade`]: it
/// names the relation, every orphaned lane and the explicit reconcile command.
fn orphaned_vector_lanes_diagnostic(
    relation_name: &str,
    lanes: &[(Uuid, String, String, i64)],
) -> String {
    let lane_count = lanes.len();
    let row_count = lanes.iter().map(|(_, _, _, count)| count).sum::<i64>();
    let mut message = format!(
        "refusing to attach the library cascade to {relation_name}: {lane_count} orphaned vector \
         lane(s) / {row_count} row(s) belong to no catalog library; run \
         `ironrag-maintenance repair orphan-vector-rows` to remove them explicitly"
    );
    for (library_id, embedding_model_key, vector_kind, count) in lanes {
        message.push_str(&format!(
            "\n  library_id={library_id} embedding_model_key={embedding_model_key} \
             vector_kind={vector_kind} rows={count}"
        ));
    }
    message
}

/// Quotes an identifier for the DDL above, rejecting anything that is not a
/// plain lower-case identifier so no caller can smuggle SQL through a shard
/// name.
fn quote_vector_identifier(identifier: &str) -> anyhow::Result<String> {
    let acceptable = !identifier.is_empty()
        && identifier.len() <= 63
        && identifier
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        && identifier.as_bytes()[0].is_ascii_lowercase();
    anyhow::ensure!(acceptable, "unsafe vector relation identifier `{identifier}`");
    Ok(format!("\"{identifier}\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hnsw_build_budget_uses_only_supported_override() {
        assert_eq!(pg_hnsw_build_budget_bytes_from_override(Some(4_000_000_000)), 4_000_000_000);
        assert_eq!(
            pg_hnsw_build_budget_bytes_from_override(None),
            u128::from(PG_HNSW_DEFAULT_BUILD_BUDGET_BYTES)
        );
    }
}
