//! `migrate.*` one-shot data migrations.
//!
//! Each migration carries a built-in idempotency contract: a second run
//! after successful completion is a no-op. They are deliberately
//! **not** wired into the recurring scheduler — keeping a "migration"
//! class alive forever masks whether the migration has actually
//! finished. The operator runs them once, observes the report, and
//! moves on.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::{
        arangodb::{
            client::ArangoClient,
            collections::{
                KNOWLEDGE_CHUNK_COLLECTION, KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
                KNOWLEDGE_ENTITY_VECTOR_COLLECTION, chunk_vector_collection_for_dim,
                chunk_vector_collection_for_library, entity_vector_collection_for_dim,
                parse_per_dim_chunk_vector_dim,
            },
        },
        repositories::catalog_repository,
    },
    shared::extraction::record_jsonl::extract_chunk_temporal_bounds,
};

// ============================================================================
// vector_per_dim — legacy single-dim collection → per-dim shards
// ============================================================================

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct VectorPerDimReport {
    pub chunk_rows_moved: i64,
    pub entity_rows_moved: i64,
}

#[derive(Debug, Clone, Copy)]
enum VectorShardKind {
    Chunk,
    Entity,
}

impl VectorShardKind {
    fn per_dim_collection_name(self, dim: u64) -> String {
        match self {
            Self::Chunk => chunk_vector_collection_for_dim(dim),
            Self::Entity => entity_vector_collection_for_dim(dim),
        }
    }

    async fn ensure_shard(
        self,
        arango: &ArangoClient,
        dim: u64,
        n_lists: u64,
        default_n_probe: u64,
        training_iterations: u64,
    ) -> Result<()> {
        match self {
            Self::Chunk => {
                arango
                    .ensure_chunk_vector_collection_for_dim(
                        dim,
                        n_lists,
                        default_n_probe,
                        training_iterations,
                    )
                    .await
            }
            Self::Entity => {
                arango
                    .ensure_entity_vector_collection_for_dim(
                        dim,
                        n_lists,
                        default_n_probe,
                        training_iterations,
                    )
                    .await
            }
        }
    }
}

// LEGACY-SHIM(arango-era, remove>=0.7.0): drains the non-suffixed single-dim
// Arango vector collections into per-dim shards — safe to delete once all
// deployments have completed migration and those collections are dropped.
/// Move every row from the legacy `knowledge_chunk_vector` /
/// `knowledge_entity_vector` collections into the per-dim shards
/// (`knowledge_*_vector_d<dim>`). Idempotent: a second run after the
/// legacy collections are empty is a no-op.
pub async fn vector_per_dim(state: &AppState) -> Result<VectorPerDimReport> {
    ensure_arango_migration_backend(state)?;

    let params = crate::infra::arangodb::search_store::VectorIndexParams {
        n_lists: state.settings.arangodb_vector_index_n_lists,
        default_n_probe: state.settings.arangodb_vector_index_default_n_probe,
        training_iterations: state.settings.arangodb_vector_index_training_iterations,
    };
    let arango = &state.arango_client;

    let chunk_rows_moved = migrate_legacy_collection(
        arango,
        KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
        VectorShardKind::Chunk,
        params.n_lists,
        params.default_n_probe,
        params.training_iterations,
    )
    .await
    .context("migrate legacy chunk vector collection to per-dim shards")?;
    info!(rows_moved = chunk_rows_moved, "chunk vector migration complete");

    let entity_rows_moved = migrate_legacy_collection(
        arango,
        KNOWLEDGE_ENTITY_VECTOR_COLLECTION,
        VectorShardKind::Entity,
        params.n_lists,
        params.default_n_probe,
        params.training_iterations,
    )
    .await
    .context("migrate legacy entity vector collection to per-dim shards")?;
    info!(rows_moved = entity_rows_moved, "entity vector migration complete");

    Ok(VectorPerDimReport { chunk_rows_moved, entity_rows_moved })
}

async fn migrate_legacy_collection(
    arango: &ArangoClient,
    legacy_collection: &str,
    kind: VectorShardKind,
    n_lists: u64,
    default_n_probe: u64,
    training_iterations: u64,
) -> Result<i64> {
    let distinct_dims =
        distinct_vector_dims(arango, legacy_collection).await.with_context(|| {
            format!("failed to inspect distinct vector dims in {legacy_collection}")
        })?;
    if distinct_dims.is_empty() {
        info!(
            collection = %legacy_collection,
            "no rows remain in legacy vector collection; nothing to migrate",
        );
        return Ok(0);
    }

    let mut total_moved: i64 = 0;
    for dim in distinct_dims {
        if dim == 0 {
            warn!(
                collection = %legacy_collection,
                "skipping zero-length vector rows in legacy collection; manual cleanup required",
            );
            continue;
        }
        kind.ensure_shard(arango, dim, n_lists, default_n_probe, training_iterations)
            .await
            .with_context(|| {
                format!(
                    "failed to ensure per-dim shard for dim {dim} while migrating {legacy_collection}",
                )
            })?;
        let dest_collection = kind.per_dim_collection_name(dim);
        let moved = move_rows_for_dim(arango, legacy_collection, &dest_collection, dim)
            .await
            .with_context(|| {
                format!(
                    "failed to move dim {dim} rows from {legacy_collection} into {dest_collection}",
                )
            })?;
        info!(
            source = %legacy_collection,
            dest = %dest_collection,
            dim,
            rows_moved = moved,
            "moved per-dim rows from legacy collection",
        );
        total_moved += moved;
    }
    Ok(total_moved)
}

/// Sample size for distinct-dim detection. Embedding bindings within a
/// single library produce vectors of one fixed dimensionality, so the first
/// few thousand rows are enough to enumerate every dim that actually needs a
/// shard. A full DISTINCT scan on a multi-hundred-thousand-row legacy
/// collection trips Arango's server-side cursor memory limit and tears the
/// HTTP connection down before the migration can begin.
const DIM_SAMPLE_LIMIT: u64 = 5000;
async fn distinct_vector_dims(arango: &ArangoClient, legacy_collection: &str) -> Result<Vec<u64>> {
    let cursor = arango
        .query_json_with_options(
            "FOR row IN @@collection LIMIT @sample RETURN DISTINCT LENGTH(row.vector)",
            serde_json::json!({"@collection": legacy_collection, "sample": DIM_SAMPLE_LIMIT}),
            serde_json::json!({"maxRuntime": 600}),
        )
        .await
        .with_context(|| format!("query distinct dims from {legacy_collection}"))?;
    let result = cursor.get("result").cloned().with_context(|| {
        format!("missing result in distinct-dim cursor for {legacy_collection}")
    })?;
    serde_json::from_value(result)
        .with_context(|| format!("decode distinct dims for {legacy_collection}"))
}

/// Batch size for the looped LIMIT migration. 500 rows × 3072 floats × 4 bytes
/// ≈ 6 MB per AQL pass — well within the default `--query.memory-limit`.
const MIGRATE_BATCH_ROWS: u64 = 500;

async fn move_rows_for_dim(
    arango: &ArangoClient,
    legacy_collection: &str,
    dest_collection: &str,
    dim: u64,
) -> Result<i64> {
    let total = count_rows_for_dim(arango, legacy_collection, dim)
        .await
        .with_context(|| format!("count rows for dim {dim} in {legacy_collection}"))?;

    let aql = "FOR row IN @@source \
        FILTER LENGTH(row.vector) == @dim \
        LIMIT @batch \
        INSERT row IN @@dest OPTIONS { ignoreErrors: true } \
        REMOVE row IN @@source \
        RETURN 1";
    let bind_vars = serde_json::json!({
        "@source": legacy_collection,
        "@dest": dest_collection,
        "dim": dim,
        "batch": MIGRATE_BATCH_ROWS,
    });

    let mut moved: i64 = 0;
    loop {
        let cursor = arango
            .query_json_with_options(aql, bind_vars.clone(), serde_json::json!({"maxRuntime": 600}))
            .await
            .with_context(|| {
                format!(
                    "AQL move from {legacy_collection} to {dest_collection} for dim {dim} failed",
                )
            })?;
        let batch_rows = cursor
            .get("result")
            .and_then(serde_json::Value::as_array)
            .map(|arr| arr.len() as i64)
            .unwrap_or(0);
        if batch_rows == 0 {
            break;
        }
        moved += batch_rows;
        info!(
            source = %legacy_collection,
            dest = %dest_collection,
            dim,
            migrated = moved,
            total,
            "migration batch complete",
        );
    }
    Ok(moved)
}

async fn count_rows_for_dim(arango: &ArangoClient, collection: &str, dim: u64) -> Result<i64> {
    let cursor = arango
        .query_json_with_options(
            "RETURN LENGTH(FOR row IN @@collection FILTER LENGTH(row.vector) == @dim RETURN 1)",
            serde_json::json!({"@collection": collection, "dim": dim}),
            serde_json::json!({"maxRuntime": 600}),
        )
        .await
        .with_context(|| format!("count rows for dim {dim} in {collection}"))?;
    Ok(cursor
        .get("result")
        .and_then(serde_json::Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0))
}

// ============================================================================
// chunk_vector_per_library — shared per-dim chunk shard → per-library shards
// ============================================================================

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct ChunkVectorPerLibraryReport {
    pub rows_moved: i64,
    pub shards_drained: i64,
}

/// Move chunk-vector rows out of the shared per-dim shards
/// (`knowledge_chunk_vector_d{dim}`) into the per-(library, dim) shards
/// (`knowledge_chunk_vector_d{dim}_l{library_hex}`), grouped by `library_id`.
///
/// Entity vectors are intentionally untouched: they stay on the shared per-dim
/// shard. Idempotent — once the shared shards hold no more rows for a library,
/// a re-run is a no-op. Safe to interleave with live writes: new writes are
/// already born in the per-library shard, and this only drains what remains in
/// the shared shard. It heals the split-brain window where a per-library shard
/// was created by a new write before the library's older rows migrated.
pub async fn chunk_vector_per_library(state: &AppState) -> Result<ChunkVectorPerLibraryReport> {
    ensure_arango_migration_backend(state)?;

    let arango = &state.arango_client;

    // Discover the shared per-dim chunk shards. `list_per_dim_chunk_vector_collections`
    // matches the `knowledge_chunk_vector_d` prefix, which also covers the
    // per-library shards — `parse_per_dim_chunk_vector_dim` returns `None` for
    // those, so they are filtered out and never used as a migration source.
    let candidate_shards = arango
        .list_per_dim_chunk_vector_collections()
        .await
        .context("failed to list per-dim chunk vector collections for per-library migration")?;

    let mut report = ChunkVectorPerLibraryReport::default();
    for shard in candidate_shards {
        let Some(dim) = parse_per_dim_chunk_vector_dim(&shard) else {
            // Per-library shard or unexpected name — not a shared source.
            continue;
        };
        let library_ids = distinct_library_ids(arango, &shard)
            .await
            .with_context(|| format!("failed to enumerate libraries in shared shard {shard}"))?;
        if library_ids.is_empty() {
            continue;
        }
        report.shards_drained += 1;
        for library_id in library_ids {
            let dest = chunk_vector_collection_for_library(dim, library_id);
            // Materialize the per-library shard before moving rows. nLists is
            // sized for a small shard by `ensure_chunk_vector_shard_for_library`
            // from the destination's current row count.
            state
                .arango_search_store
                .ensure_chunk_vector_shard_for_library(dim, library_id)
                .await
                .with_context(|| {
                    format!("failed to ensure per-library shard {dest} during migration")
                })?;
            let moved =
                move_library_rows(arango, &shard, &dest, library_id).await.with_context(|| {
                    format!("failed to move library {library_id} rows from {shard} into {dest}")
                })?;
            info!(
                source = %shard,
                dest = %dest,
                dim,
                %library_id,
                rows_moved = moved,
                "moved per-library chunk vectors from shared shard",
            );
            report.rows_moved += moved;
        }
    }
    Ok(report)
}

/// Enumerate EVERY distinct `library_id` in a shared shard. `COLLECT` over the
/// persistent `library_id` index streams the distinct set (one entry per
/// library — tiny) instead of buffering rows, so it does not trip Arango's
/// cursor memory limit even on a multi-hundred-thousand-row shard. A previous
/// version `LIMIT`-sampled the first N rows, which on a shard dominated by one
/// large library (its rows clustered in the scan window) hid every other
/// library and left them un-migrated.
async fn distinct_library_ids(arango: &ArangoClient, shard: &str) -> Result<Vec<Uuid>> {
    let cursor = arango
        .query_json_with_options(
            "FOR row IN @@collection COLLECT lib = row.library_id RETURN lib",
            serde_json::json!({ "@collection": shard }),
            serde_json::json!({"maxRuntime": 600}),
        )
        .await
        .with_context(|| format!("query distinct library ids from {shard}"))?;
    let result = cursor
        .get("result")
        .cloned()
        .with_context(|| format!("missing result in distinct-library cursor for {shard}"))?;
    let raw: Vec<Option<Uuid>> = serde_json::from_value(result)
        .with_context(|| format!("decode distinct library ids for {shard}"))?;
    Ok(raw.into_iter().flatten().collect())
}

async fn move_library_rows(
    arango: &ArangoClient,
    source: &str,
    dest: &str,
    library_id: Uuid,
) -> Result<i64> {
    // UPSERT by `_key` into the destination so a re-run (or a row already
    // copied by a concurrent live write) updates in place instead of
    // erroring, then REMOVE the source row. Looping LIMIT batches keeps each
    // AQL pass inside the default query memory budget.
    let aql = "FOR row IN @@source \
        FILTER row.library_id == @library_id \
        LIMIT @batch \
        UPSERT { _key: row._key } \
        INSERT row \
        UPDATE row \
        IN @@dest \
        REMOVE row IN @@source \
        RETURN 1";
    let bind_vars = serde_json::json!({
        "@source": source,
        "@dest": dest,
        "library_id": library_id,
        "batch": MIGRATE_BATCH_ROWS,
    });

    let mut moved: i64 = 0;
    loop {
        let cursor = arango
            .query_json_with_options(aql, bind_vars.clone(), serde_json::json!({"maxRuntime": 600}))
            .await
            .with_context(|| {
                format!("AQL move from {source} to {dest} for library {library_id} failed")
            })?;
        let batch_rows = cursor
            .get("result")
            .and_then(serde_json::Value::as_array)
            .map(|arr| arr.len() as i64)
            .unwrap_or(0);
        if batch_rows == 0 {
            break;
        }
        moved += batch_rows;
        info!(
            source = %source,
            dest = %dest,
            %library_id,
            migrated = moved,
            "per-library migration batch complete",
        );
    }
    Ok(moved)
}

// ============================================================================
// chunk_temporal_bounds — populate occurred_at/until on existing chunks
// ============================================================================

const TEMPORAL_BACKFILL_BATCH: i64 = 500;

#[derive(Debug, FromRow)]
struct ChunkBackfillRow {
    id: Uuid,
    revision_id: Uuid,
    normalized_text: String,
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct ChunkTemporalBackfillReport {
    pub scanned: i64,
    pub parsed: i64,
    pub updated_pg: i64,
    pub updated_arango: i64,
    pub skipped_no_header: i64,
    pub failed_arango_mirror: i64,
}

impl ChunkTemporalBackfillReport {
    fn merge(self, other: Self) -> Self {
        Self {
            scanned: self.scanned + other.scanned,
            parsed: self.parsed + other.parsed,
            updated_pg: self.updated_pg + other.updated_pg,
            updated_arango: self.updated_arango + other.updated_arango,
            skipped_no_header: self.skipped_no_header + other.skipped_no_header,
            failed_arango_mirror: self.failed_arango_mirror + other.failed_arango_mirror,
        }
    }
}

/// Populate `content_chunk.occurred_at` and `content_chunk.occurred_until`
/// (plus the Arango `knowledge_chunk` mirror) on chunks whose
/// `normalized_text` carries the canonical `occurred_at=ISO` JSONL
/// header but whose temporal columns are still NULL.
///
/// Idempotent: chunks where `occurred_at IS NOT NULL` are skipped, so a
/// second run after partial completion picks up where the previous run
/// left off.
pub async fn chunk_temporal_bounds(
    state: &AppState,
    library_filter: Option<Uuid>,
    dry_run: bool,
) -> Result<ChunkTemporalBackfillReport> {
    ensure_arango_migration_backend(state)?;

    let mut libraries =
        catalog_repository::list_libraries(&state.persistence.postgres, None).await?;
    if let Some(target) = library_filter {
        libraries.retain(|library| library.id == target);
    }
    if libraries.is_empty() {
        anyhow::bail!("no libraries matched temporal-bounds backfill target");
    }

    let mut totals = ChunkTemporalBackfillReport::default();
    for library in libraries {
        match backfill_library(state, library.id, dry_run).await {
            Ok(counts) => {
                info!(
                    library_id = %library.id,
                    library_name = %library.display_name,
                    dry_run,
                    scanned = counts.scanned,
                    parsed = counts.parsed,
                    updated_pg = counts.updated_pg,
                    updated_arango = counts.updated_arango,
                    skipped_no_header = counts.skipped_no_header,
                    failed_arango_mirror = counts.failed_arango_mirror,
                    "library backfill completed",
                );
                totals = totals.merge(counts);
            }
            Err(error) => warn!(
                library_id = %library.id,
                library_name = %library.display_name,
                ?error,
                "library backfill failed; continuing with next library",
            ),
        }
    }

    if totals.failed_arango_mirror > 0 {
        anyhow::bail!(
            "{} chunks failed Arango mirror; PG was not flipped for those rows — re-run to retry",
            totals.failed_arango_mirror,
        );
    }
    Ok(totals)
}

async fn backfill_library(
    state: &AppState,
    library_id: Uuid,
    dry_run: bool,
) -> Result<ChunkTemporalBackfillReport> {
    let mut counts = ChunkTemporalBackfillReport::default();
    let mut cursor: Option<Uuid> = None;
    loop {
        let rows: Vec<ChunkBackfillRow> = sqlx::query_as(
            "SELECT c.id, c.revision_id, c.normalized_text \
             FROM content_chunk c \
             JOIN content_revision r ON r.id = c.revision_id \
             WHERE r.library_id = $1 \
               AND c.occurred_at IS NULL \
               AND c.normalized_text LIKE '%occurred_at=%' \
               AND ($2::uuid IS NULL OR c.id > $2) \
             ORDER BY c.id ASC \
             LIMIT $3",
        )
        .bind(library_id)
        .bind(cursor)
        .bind(TEMPORAL_BACKFILL_BATCH)
        .fetch_all(&state.persistence.postgres)
        .await
        .with_context(|| {
            format!("failed to fetch chunk backfill batch for library {library_id}")
        })?;

        if rows.is_empty() {
            break;
        }
        let Some(last_id) = rows.last().map(|row| row.id) else {
            break;
        };
        for row in &rows {
            counts.scanned += 1;
            let Some((occurred_at, occurred_until)) =
                extract_chunk_temporal_bounds(&row.normalized_text)
            else {
                counts.skipped_no_header += 1;
                continue;
            };
            counts.parsed += 1;
            if dry_run {
                continue;
            }
            // Arango mirror FIRST; PG flip second. The cursor query
            // filters `c.occurred_at IS NULL`, so a row that flipped
            // in PG would never be retried even if Arango mirror
            // failed. Arango-first preserves idempotent re-run
            // semantics.
            match update_arango_chunk_temporal(state, row.id, occurred_at, occurred_until).await {
                Ok(()) => counts.updated_arango += 1,
                Err(error) => {
                    counts.failed_arango_mirror += 1;
                    warn!(
                        chunk_id = %row.id,
                        revision_id = %row.revision_id,
                        ?error,
                        "Arango mirror update failed; PG flip skipped — re-run will retry",
                    );
                    continue;
                }
            }

            sqlx::query(
                "UPDATE content_chunk SET occurred_at = $1, occurred_until = $2 WHERE id = $3",
            )
            .bind(occurred_at)
            .bind(occurred_until)
            .bind(row.id)
            .execute(&state.persistence.postgres)
            .await
            .with_context(|| format!("failed to UPDATE PG content_chunk {}", row.id))?;
            counts.updated_pg += 1;
        }

        cursor = Some(last_id);
        if rows.len() < TEMPORAL_BACKFILL_BATCH as usize {
            break;
        }
    }
    Ok(counts)
}

fn ensure_arango_migration_backend(state: &AppState) -> Result<()> {
    match state.settings.knowledge_plane_backend.as_str() {
        "arango" => Ok(()),
        "postgres" => anyhow::bail!(
            "automatic migration is not supported on the postgres knowledge plane; use snapshot restore"
        ),
        backend => anyhow::bail!("unsupported knowledge_plane_backend `{backend}`"),
    }
}

async fn update_arango_chunk_temporal(
    state: &AppState,
    chunk_id: Uuid,
    occurred_at: DateTime<Utc>,
    occurred_until: DateTime<Utc>,
) -> Result<()> {
    // `_key == chunk_id.to_string()` for every chunk row, so this is an
    // O(1) primary-index UPDATE rather than the O(N) collection scan a
    // `FOR chunk IN @@collection FILTER chunk.chunk_id == @id` would
    // compile to.
    state
        .arango_client
        .query_json(
            "UPDATE @key WITH { occurred_at: @occurred_at, occurred_until: @occurred_until } \
             IN @@collection RETURN NEW",
            serde_json::json!({
                "@collection": KNOWLEDGE_CHUNK_COLLECTION,
                "key": chunk_id.to_string(),
                "occurred_at": occurred_at.to_rfc3339(),
                "occurred_until": occurred_until.to_rfc3339(),
            }),
        )
        .await
        .with_context(|| format!("failed to mirror temporal bounds to Arango for chunk {chunk_id}"))
        .map(|_| ())
}
