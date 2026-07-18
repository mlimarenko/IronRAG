//! `migrate.*` one-shot data migrations.

use anyhow::{Context, Result};
use serde::Serialize;
use sqlx::{FromRow, Postgres, Transaction};
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

const TEMPORAL_BACKFILL_CANDIDATES_SQL: &str = "WITH library_revisions AS MATERIALIZED ( \
        SELECT revision.id, revision.document_id, revision.library_id \
        FROM content_revision AS revision \
        WHERE revision.library_id = $1 \
     ) \
     SELECT \
        chunk.id, \
        chunk.normalized_text, \
        (document.id IS NOT NULL AND head.document_id IS NOT NULL AND chunk.raptor_level = 0) \
            AS answer_visible, \
        knowledge.library_id AS knowledge_library_id \
     FROM library_revisions AS revision \
     CROSS JOIN LATERAL ( \
        SELECT candidate.id, candidate.normalized_text, \
               candidate.occurred_at, candidate.occurred_until, candidate.raptor_level \
        FROM content_chunk AS candidate \
        WHERE candidate.revision_id = revision.id \
          AND ($2::uuid IS NULL OR candidate.id > $2) \
          AND candidate.normalized_text LIKE '%occurred_at=%' \
        OFFSET 0 \
     ) AS chunk \
     LEFT JOIN knowledge_chunk AS knowledge ON knowledge.chunk_id = chunk.id \
     LEFT JOIN content_document AS document \
       ON document.id = revision.document_id \
      AND document.library_id = revision.library_id \
      AND document.document_state = 'active' \
      AND document.deleted_at IS NULL \
     LEFT JOIN content_document_head AS head \
       ON head.document_id = revision.document_id \
      AND head.readable_revision_id = revision.id \
     WHERE ( \
            chunk.occurred_at IS NULL \
            OR chunk.occurred_until IS NULL \
            OR ( \
                knowledge.chunk_id IS NOT NULL \
                AND ( \
                    knowledge.library_id IS DISTINCT FROM revision.library_id \
                    OR \
                    knowledge.occurred_at IS DISTINCT FROM chunk.occurred_at \
                    OR knowledge.occurred_until IS DISTINCT FROM chunk.occurred_until \
                ) \
            ) \
            OR ( \
                knowledge.chunk_id IS NULL \
                AND document.id IS NOT NULL \
                AND head.document_id IS NOT NULL \
                AND chunk.raptor_level = 0 \
            ) \
       ) \
     ORDER BY chunk.id ASC \
     LIMIT $3";

const TEMPORAL_BACKFILL_APPLY_SQL: &str = "WITH input AS MATERIALIZED ( \
        SELECT candidate.chunk_id, candidate.occurred_at, candidate.occurred_until \
        FROM unnest($1::uuid[], $2::timestamptz[], $3::timestamptz[]) \
             AS candidate(chunk_id, occurred_at, occurred_until) \
     ), updated_content AS ( \
        UPDATE content_chunk AS chunk \
        SET occurred_at = input.occurred_at, \
            occurred_until = input.occurred_until \
        FROM input \
        WHERE chunk.id = input.chunk_id \
          AND ( \
              chunk.occurred_at IS DISTINCT FROM input.occurred_at \
              OR chunk.occurred_until IS DISTINCT FROM input.occurred_until \
          ) \
        RETURNING chunk.id AS chunk_id \
     ), updated_knowledge AS ( \
        UPDATE knowledge_chunk AS knowledge \
        SET occurred_at = input.occurred_at, \
            occurred_until = input.occurred_until \
        FROM input \
        WHERE knowledge.chunk_id = input.chunk_id \
          AND knowledge.library_id = $4 \
          AND ( \
              knowledge.occurred_at IS DISTINCT FROM input.occurred_at \
              OR knowledge.occurred_until IS DISTINCT FROM input.occurred_until \
          ) \
        RETURNING knowledge.chunk_id \
     ), changed_chunks AS ( \
        SELECT chunk_id FROM updated_content \
        UNION \
        SELECT chunk_id FROM updated_knowledge \
     ) \
     SELECT \
        (SELECT count(*) FROM updated_content) AS content_rows, \
        (SELECT count(*) FROM updated_knowledge) AS knowledge_rows, \
        (SELECT count(*) FROM changed_chunks) AS changed_chunks";

const TEMPORAL_BACKFILL_VERIFY_SQL: &str = "WITH input AS MATERIALIZED ( \
        SELECT \
            candidate.chunk_id, \
            candidate.occurred_at, \
            candidate.occurred_until, \
            candidate.require_knowledge \
        FROM unnest( \
            $1::uuid[], $2::timestamptz[], $3::timestamptz[], $4::boolean[] \
        ) AS candidate(chunk_id, occurred_at, occurred_until, require_knowledge) \
     ) \
     SELECT count(*) \
     FROM input \
     LEFT JOIN content_chunk AS chunk ON chunk.id = input.chunk_id \
     LEFT JOIN content_revision AS content ON content.id = chunk.revision_id \
     LEFT JOIN knowledge_chunk AS knowledge ON knowledge.chunk_id = input.chunk_id \
     WHERE chunk.id IS NULL \
        OR content.library_id IS DISTINCT FROM $5 \
        OR chunk.occurred_at IS DISTINCT FROM input.occurred_at \
        OR chunk.occurred_until IS DISTINCT FROM input.occurred_until \
        OR ( \
            knowledge.chunk_id IS NOT NULL \
            AND ( \
                knowledge.library_id IS DISTINCT FROM $5 \
                OR knowledge.occurred_at IS DISTINCT FROM input.occurred_at \
                OR knowledge.occurred_until IS DISTINCT FROM input.occurred_until \
            ) \
        ) \
        OR (input.require_knowledge AND knowledge.chunk_id IS NULL)";

#[derive(Debug, FromRow)]
struct ChunkBackfillRow {
    id: Uuid,
    normalized_text: String,
    answer_visible: bool,
    knowledge_library_id: Option<Uuid>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, FromRow)]
struct ChunkTemporalWriteOutcome {
    content_rows: i64,
    knowledge_rows: i64,
    changed_chunks: i64,
}

impl ChunkTemporalWriteOutcome {
    const fn changed(self) -> bool {
        self.changed_chunks > 0
    }
}

fn validate_projection_pairing(row: &ChunkBackfillRow, library_id: Uuid) -> Result<()> {
    if let Some(knowledge_library_id) = row.knowledge_library_id {
        anyhow::ensure!(
            knowledge_library_id == library_id,
            "chunk {} has a knowledge mirror outside its canonical library",
            row.id
        );
    } else {
        anyhow::ensure!(
            !row.answer_visible,
            "answer-visible chunk {} is missing its knowledge mirror",
            row.id
        );
    }
    Ok(())
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct ChunkTemporalBackfillReport {
    pub scanned: i64,
    pub parsed: i64,
    pub updated_pg: i64,
    pub skipped_no_header: i64,
}

impl ChunkTemporalBackfillReport {
    const fn merge(self, other: Self) -> Self {
        Self {
            scanned: self.scanned + other.scanned,
            parsed: self.parsed + other.parsed,
            updated_pg: self.updated_pg + other.updated_pg,
            skipped_no_header: self.skipped_no_header + other.skipped_no_header,
        }
    }
}

/// Populate canonical chunk temporal bounds from the `occurred_at=ISO` JSONL
/// header and repair temporal drift on any existing retrieval-plane mirror.
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
    let mut failed_libraries = 0_usize;
    for library in libraries {
        match backfill_library(state, library.workspace_id, library.id, dry_run).await {
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
            Err(error) => {
                failed_libraries += 1;
                warn!(
                    library_id = %library.id,
                    library_name = %library.display_name,
                    ?error,
                    "library backfill failed; continuing with next library",
                );
            }
        }
    }

    anyhow::ensure!(
        failed_libraries == 0,
        "temporal-bounds backfill failed for {failed_libraries} librar{}",
        if failed_libraries == 1 { "y" } else { "ies" }
    );

    Ok(totals)
}

async fn backfill_library(
    state: &AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    dry_run: bool,
) -> Result<ChunkTemporalBackfillReport> {
    let mut counts = ChunkTemporalBackfillReport::default();
    let mut cursor: Option<Uuid> = None;
    loop {
        let mut transaction = state.persistence.postgres.begin().await.with_context(|| {
            format!("failed to start chunk backfill transaction for library {library_id}")
        })?;
        if !dry_run {
            // Match the canonical answer-visible lock order: library parent
            // first, then content/knowledge child rows, then generation bump.
            let parent_locked = catalog_repository::lock_library_for_lifecycle_event_with_executor(
                &mut *transaction,
                workspace_id,
                library_id,
            )
            .await
            .with_context(|| {
                format!("failed to lock library {library_id} for chunk temporal backfill")
            })?;
            anyhow::ensure!(
                parent_locked,
                "library {library_id} disappeared during chunk temporal backfill"
            );
        }

        let rows: Vec<ChunkBackfillRow> = sqlx::query_as(TEMPORAL_BACKFILL_CANDIDATES_SQL)
            .bind(library_id)
            .bind(cursor)
            .bind(TEMPORAL_BACKFILL_BATCH)
            .fetch_all(&mut *transaction)
            .await
            .with_context(|| {
                format!("failed to fetch chunk backfill batch for library {library_id}")
            })?;

        if rows.is_empty() {
            transaction.commit().await.with_context(|| {
                format!("failed to finish empty chunk backfill batch for library {library_id}")
            })?;
            break;
        }
        let Some(last_id) = rows.last().map(|row| row.id) else {
            break;
        };
        let mut parsed_chunk_ids = Vec::with_capacity(rows.len());
        let mut parsed_occurred_at = Vec::with_capacity(rows.len());
        let mut parsed_occurred_until = Vec::with_capacity(rows.len());
        let mut require_knowledge = Vec::with_capacity(rows.len());
        for row in &rows {
            counts.scanned += 1;
            validate_projection_pairing(row, library_id)?;
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
            parsed_chunk_ids.push(row.id);
            parsed_occurred_at.push(occurred_at);
            parsed_occurred_until.push(occurred_until);
            require_knowledge.push(row.answer_visible || row.knowledge_library_id.is_some());
        }

        let write_outcome = update_chunk_temporal_bounds(
            &mut transaction,
            library_id,
            &parsed_chunk_ids,
            &parsed_occurred_at,
            &parsed_occurred_until,
            &require_knowledge,
        )
        .await?;
        if write_outcome.changed() {
            // Report changed chunks, not physical canonical + mirror rows.
            // This preserves the historical per-chunk `updated_pg` meaning.
            counts.updated_pg += write_outcome.changed_chunks;
            catalog_repository::touch_library_source_truth_version_with_executor(
                &mut *transaction,
                library_id,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to advance source generation after chunk temporal backfill for library {library_id}"
                )
            })?;
        }
        transaction.commit().await.with_context(|| {
            format!("failed to commit chunk temporal backfill batch for library {library_id}")
        })?;

        cursor = Some(last_id);
        if rows.len() < TEMPORAL_BACKFILL_BATCH as usize {
            break;
        }
    }
    Ok(counts)
}

async fn update_chunk_temporal_bounds(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    chunk_ids: &[Uuid],
    occurred_at: &[chrono::DateTime<chrono::Utc>],
    occurred_until: &[chrono::DateTime<chrono::Utc>],
    require_knowledge: &[bool],
) -> Result<ChunkTemporalWriteOutcome> {
    anyhow::ensure!(
        chunk_ids.len() == occurred_at.len()
            && chunk_ids.len() == occurred_until.len()
            && chunk_ids.len() == require_knowledge.len(),
        "chunk temporal backfill arrays have different lengths"
    );
    if chunk_ids.is_empty() {
        return Ok(ChunkTemporalWriteOutcome::default());
    }

    let outcome = sqlx::query_as::<_, ChunkTemporalWriteOutcome>(TEMPORAL_BACKFILL_APPLY_SQL)
        .bind(chunk_ids)
        .bind(occurred_at)
        .bind(occurred_until)
        .bind(library_id)
        .fetch_one(&mut **transaction)
        .await
        .with_context(|| {
            format!("failed to update chunk temporal bounds for library {library_id}")
        })?;
    let parity_violations = sqlx::query_scalar::<_, i64>(TEMPORAL_BACKFILL_VERIFY_SQL)
        .bind(chunk_ids)
        .bind(occurred_at)
        .bind(occurred_until)
        .bind(require_knowledge)
        .bind(library_id)
        .fetch_one(&mut **transaction)
        .await
        .with_context(|| {
            format!("failed to verify chunk temporal projection for library {library_id}")
        })?;
    anyhow::ensure!(
        parity_violations == 0,
        "chunk temporal projection verification failed for {parity_violations} row(s)"
    );
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normalized_sql(sql: &str) -> String {
        sql.split_whitespace().collect::<Vec<_>>().join(" ").to_ascii_lowercase()
    }

    #[test]
    fn temporal_candidates_include_existing_projection_drift() {
        let sql = normalized_sql(TEMPORAL_BACKFILL_CANDIDATES_SQL);

        assert!(sql.contains("with library_revisions as materialized"));
        assert!(sql.contains("cross join lateral"));
        assert!(sql.contains("offset 0"));
        assert!(sql.contains("left join knowledge_chunk"));
        assert!(sql.contains("left join content_document_head"));
        assert!(sql.contains("chunk.occurred_until is null"));
        assert!(sql.contains("knowledge.occurred_at is distinct from chunk.occurred_at"));
        assert!(sql.contains("knowledge.occurred_until is distinct from chunk.occurred_until"));
        assert!(sql.contains("knowledge.chunk_id is not null"));
        assert!(sql.contains("knowledge.chunk_id is null"));
        assert!(sql.contains("document.document_state = 'active'"));
        assert!(sql.contains("document.deleted_at is null"));
        assert!(sql.contains("chunk.raptor_level = 0"));
        assert!(sql.contains("knowledge.library_id is distinct from revision.library_id"));
    }

    #[test]
    fn temporal_batch_writes_are_atomic_and_idempotent_on_both_answer_planes() {
        let sql = normalized_sql(TEMPORAL_BACKFILL_APPLY_SQL);

        assert!(sql.contains("from unnest($1::uuid[], $2::timestamptz[], $3::timestamptz[])"));
        assert!(sql.contains("update content_chunk as chunk"));
        assert!(sql.contains("update knowledge_chunk as knowledge"));
        assert!(sql.contains("chunk.occurred_at is distinct from input.occurred_at"));
        assert!(sql.contains("knowledge.occurred_at is distinct from input.occurred_at"));
        assert!(sql.contains(
            "select chunk_id from updated_content union select chunk_id from updated_knowledge"
        ));
    }

    #[test]
    fn temporal_batch_verification_requires_scope_and_required_mirror_parity() {
        let sql = normalized_sql(TEMPORAL_BACKFILL_VERIFY_SQL);

        assert!(sql.contains("content.library_id is distinct from $5"));
        assert!(sql.contains("knowledge.library_id is distinct from $5"));
        assert!(sql.contains("input.require_knowledge and knowledge.chunk_id is null"));
        assert!(sql.contains("knowledge.occurred_at is distinct from input.occurred_at"));
    }

    #[test]
    fn readable_candidate_without_mirror_is_rejected() {
        let row = ChunkBackfillRow {
            id: Uuid::now_v7(),
            normalized_text: "occurred_at=2026-01-01T00:00:00Z".to_string(),
            answer_visible: true,
            knowledge_library_id: None,
        };

        assert!(validate_projection_pairing(&row, Uuid::now_v7()).is_err());
    }

    #[test]
    fn pre_projection_candidate_without_mirror_is_allowed() {
        let row = ChunkBackfillRow {
            id: Uuid::now_v7(),
            normalized_text: "occurred_at=2026-01-01T00:00:00Z".to_string(),
            answer_visible: false,
            knowledge_library_id: None,
        };

        assert!(validate_projection_pairing(&row, Uuid::now_v7()).is_ok());
    }

    #[test]
    fn candidate_with_wrong_library_mirror_is_rejected() {
        let library_id = Uuid::now_v7();
        let row = ChunkBackfillRow {
            id: Uuid::now_v7(),
            normalized_text: "occurred_at=2026-01-01T00:00:00Z".to_string(),
            answer_visible: false,
            knowledge_library_id: Some(Uuid::now_v7()),
        };

        assert!(validate_projection_pairing(&row, library_id).is_err());
    }

    #[test]
    fn source_generation_advances_only_after_a_real_write() {
        assert!(!ChunkTemporalWriteOutcome::default().changed());
        assert!(
            ChunkTemporalWriteOutcome { content_rows: 1, knowledge_rows: 0, changed_chunks: 1 }
                .changed()
        );
        assert!(
            ChunkTemporalWriteOutcome { content_rows: 0, knowledge_rows: 1, changed_chunks: 1 }
                .changed()
        );
    }
}
