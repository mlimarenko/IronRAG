//! `migrate.*` one-shot data migrations.

use anyhow::{Context, Result};
use serde::Serialize;
use sqlx::FromRow;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState, infra::repositories::catalog_repository,
    shared::extraction::record_jsonl::extract_chunk_temporal_bounds,
};

// ============================================================================
// chunk_temporal_bounds — populate occurred_at/until on existing chunks
// ============================================================================

const TEMPORAL_BACKFILL_BATCH: i64 = 500;

#[derive(Debug, FromRow)]
struct ChunkBackfillRow {
    id: Uuid,
    normalized_text: String,
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct ChunkTemporalBackfillReport {
    pub scanned: i64,
    pub parsed: i64,
    pub updated_pg: i64,
    pub skipped_no_header: i64,
}

impl ChunkTemporalBackfillReport {
    fn merge(self, other: Self) -> Self {
        Self {
            scanned: self.scanned + other.scanned,
            parsed: self.parsed + other.parsed,
            updated_pg: self.updated_pg + other.updated_pg,
            skipped_no_header: self.skipped_no_header + other.skipped_no_header,
        }
    }
}

/// Populate `content_chunk.occurred_at` and `content_chunk.occurred_until`
/// on chunks whose `normalized_text` carries the canonical
/// `occurred_at=ISO` JSONL header but whose temporal columns are still NULL.
pub async fn chunk_temporal_bounds(
    state: &AppState,
    library_filter: Option<Uuid>,
    dry_run: bool,
) -> Result<ChunkTemporalBackfillReport> {
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
                    skipped_no_header = counts.skipped_no_header,
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
            "SELECT c.id, c.normalized_text \
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
