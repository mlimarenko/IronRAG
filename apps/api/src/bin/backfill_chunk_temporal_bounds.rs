//! Convergent backfill: populate `content_chunk.occurred_at` and
//! `content_chunk.occurred_until` (plus the Arango `knowledge_chunk`
//! mirror) on existing rows whose `normalized_text` carries the canonical
//! `occurred_at=ISO` JSONL header but whose temporal columns are still
//! NULL because the chunks were ingested before the T1.0 schema landed.
//!
//! Without this backfill the temporal hard-filter in `search_chunks` and
//! `search_chunk_vectors_by_similarity` excludes every JSONL chunk
//! ingested before T1 — turning `temporal_constraints` queries into a
//! "no results" cliff. T1.4 + T1.5 are correct in code but only useful
//! once `occurred_at` is populated on existing chunks; this binary makes
//! that idempotent.
//!
//! Idempotent: chunks where `occurred_at IS NOT NULL` are skipped.
//! Re-runs after partial completion pick up where the previous run
//! left off.
//!
//! Usage:
//!   ironrag-backfill-chunk-temporal-bounds                         # all libraries
//!   ironrag-backfill-chunk-temporal-bounds <library-uuid>          # one library
//!
//! Set `IRONRAG_BACKFILL_DRY_RUN=1` to count without writing.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::{
        arangodb::collections::KNOWLEDGE_CHUNK_COLLECTION, repositories::catalog_repository,
    },
    shared::extraction::record_jsonl::extract_chunk_temporal_bounds,
};
use sqlx::FromRow;
use tracing::{info, warn};
use uuid::Uuid;

const PG_BATCH_SIZE: i64 = 500;

#[derive(Debug, FromRow)]
struct ChunkBackfillRow {
    id: Uuid,
    revision_id: Uuid,
    normalized_text: String,
}

#[derive(Debug, Default, Clone, Copy)]
struct LibraryCounts {
    scanned: i64,
    parsed: i64,
    updated_pg: i64,
    updated_arango: i64,
    skipped_no_header: i64,
    failed_arango_mirror: i64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let settings = Settings::from_env()?;
    ironrag_backend::shared::telemetry::init(&settings.log_filter);
    let state = AppState::new(settings).await?;

    let mut args = std::env::args().skip(1);
    let target_library_id = args.next().map(|value| Uuid::parse_str(&value)).transpose()?;
    if args.next().is_some() {
        anyhow::bail!("usage: ironrag-backfill-chunk-temporal-bounds [library-uuid]");
    }

    let dry_run = matches!(std::env::var("IRONRAG_BACKFILL_DRY_RUN").as_deref(), Ok("1"));
    let libraries = catalog_repository::list_libraries(&state.persistence.postgres, None).await?;
    let libraries: Vec<_> = match target_library_id {
        Some(library_id) => {
            libraries.into_iter().filter(|library| library.id == library_id).collect()
        }
        None => libraries,
    };
    if libraries.is_empty() {
        anyhow::bail!("no libraries matched temporal-bounds backfill target");
    }

    info!(
        dry_run,
        library_count = libraries.len(),
        "starting chunk temporal-bounds backfill"
    );

    let mut totals = LibraryCounts::default();
    for library in libraries {
        match backfill_library(&state, library.id, dry_run).await {
            Ok(counts) => {
                totals.scanned += counts.scanned;
                totals.parsed += counts.parsed;
                totals.updated_pg += counts.updated_pg;
                totals.updated_arango += counts.updated_arango;
                totals.skipped_no_header += counts.skipped_no_header;
                totals.failed_arango_mirror += counts.failed_arango_mirror;
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
            }
            Err(error) => warn!(
                library_id = %library.id,
                library_name = %library.display_name,
                ?error,
                "library backfill failed; continuing with next library",
            ),
        }
    }

    info!(
        dry_run,
        total_scanned = totals.scanned,
        total_parsed = totals.parsed,
        total_updated_pg = totals.updated_pg,
        total_updated_arango = totals.updated_arango,
        total_skipped_no_header = totals.skipped_no_header,
        total_failed_arango_mirror = totals.failed_arango_mirror,
        "chunk temporal-bounds backfill finished"
    );

    // Non-zero exit when any Arango mirror failed so operator scripts
    // can detect partial completion and re-run without manual log grep.
    // Re-runs are safe: the cursor query filters PG `occurred_at IS NULL`
    // and we now Arango-first → PG-flip, so failed rows are still
    // eligible.
    if totals.failed_arango_mirror > 0 {
        anyhow::bail!(
            "{} chunks failed Arango mirror; PG was not flipped for those rows — re-run to retry",
            totals.failed_arango_mirror
        );
    }
    Ok(())
}

async fn backfill_library(
    state: &AppState,
    library_id: Uuid,
    dry_run: bool,
) -> Result<LibraryCounts> {
    let mut counts = LibraryCounts::default();
    // Cursor-paginated by chunk id so re-runs after a crash naturally
    // pick up where they left off without remembering per-revision
    // offsets. The `normalized_text LIKE` predicate keeps the scan
    // cheap on libraries with no JSONL chunks.
    let mut cursor: Option<Uuid> = None;
    loop {
        let rows: Vec<ChunkBackfillRow> = sqlx::query_as(
            "SELECT c.id, c.revision_id, c.normalized_text
             FROM content_chunk c
             JOIN content_revision r ON r.id = c.revision_id
             WHERE r.library_id = $1
               AND c.occurred_at IS NULL
               AND c.normalized_text LIKE '%occurred_at=%'
               AND ($2::uuid IS NULL OR c.id > $2)
             ORDER BY c.id ASC
             LIMIT $3",
        )
        .bind(library_id)
        .bind(cursor)
        .bind(PG_BATCH_SIZE)
        .fetch_all(&state.persistence.postgres)
        .await
        .with_context(|| {
            format!("failed to fetch chunk backfill batch for library {library_id}")
        })?;

        if rows.is_empty() {
            break;
        }

        let last_id = rows.last().map(|row| row.id).expect("non-empty batch");
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
            // filters `c.occurred_at IS NULL`, so a row that flipped in
            // PG would never be retried even if Arango mirror failed.
            // Arango-first preserves idempotent re-run semantics: any
            // failure (Arango or PG) leaves the row eligible for the
            // next pass.
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
                "UPDATE content_chunk
                 SET occurred_at = $1, occurred_until = $2
                 WHERE id = $3",
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
        if rows.len() < PG_BATCH_SIZE as usize {
            break;
        }
    }

    Ok(counts)
}

async fn update_arango_chunk_temporal(
    state: &AppState,
    chunk_id: Uuid,
    occurred_at: DateTime<Utc>,
    occurred_until: DateTime<Utc>,
) -> Result<()> {
    // `_key == chunk_id.to_string()` for every chunk row, so this is an
    // O(1) primary-index UPDATE rather than the O(N) collection scan that
    // a `FOR chunk IN @@collection FILTER chunk.chunk_id == @id` lookup
    // would compile to (no persistent index covers `chunk_id` alone).
    state
        .arango_document_store
        .client()
        .query_json(
            "UPDATE @key WITH {
                 occurred_at: @occurred_at,
                 occurred_until: @occurred_until
             } IN @@collection
             RETURN NEW",
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
