//! Convergent backfill: promote `content_document_head` for documents
//! that have at least one revision with persisted chunks but whose
//! head still carries `readable_revision_id == NULL` AND
//! `active_revision_id == NULL`.
//!
//! Reuses the canonical `promote_document_head` so Postgres `head` and
//! the Arango `knowledge_document` mirror are written through the same
//! path the ingest pipeline uses on success. Idempotent: re-running on
//! an already-promoted document is a no-op upsert.
//!
//! This binary exists because retrieval (`map_chunk_hit`) drops every
//! chunk whose document has no canonical revision pointer — leaving
//! null-head documents completely invisible to grounded answers, even
//! when their chunks are physically present.
//!
//! Usage:
//!   ironrag-promote-null-heads                         # all libraries
//!   ironrag-promote-null-heads <library-uuid>          # one library

use anyhow::Context;
use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::repositories::catalog_repository,
    services::content::service::PromoteHeadCommand,
};
use sqlx::FromRow;
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Debug, FromRow)]
struct NullHeadCandidate {
    document_id: Uuid,
    revision_id: Uuid,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let settings = Settings::from_env()?;
    ironrag_backend::shared::telemetry::init(&settings.log_filter);
    let state = AppState::new(settings).await?;

    let mut args = std::env::args().skip(1);
    let target_library_id = args.next().map(|value| Uuid::parse_str(&value)).transpose()?;

    let libraries = catalog_repository::list_libraries(&state.persistence.postgres, None).await?;
    let libraries: Vec<_> = match target_library_id {
        Some(lib) => libraries.into_iter().filter(|library| library.id == lib).collect(),
        None => libraries,
    };
    if libraries.is_empty() {
        anyhow::bail!("no libraries matched promote target");
    }

    let mut total_promoted = 0usize;
    let mut total_skipped_no_chunks = 0usize;
    for library in libraries {
        // Pick, per-document, the latest revision that has at least one
        // row in `content_chunk` — that's the most recent revision the
        // ingest pipeline is known to have produced material for.
        // `DISTINCT ON` picks one row per document_id; we order by
        // revision_number DESC so the highest-numbered revision wins.
        let candidates: Vec<NullHeadCandidate> = sqlx::query_as(
            "SELECT DISTINCT ON (r.document_id)
                 r.document_id,
                 r.id AS revision_id
             FROM content_revision r
             JOIN content_document d ON d.id = r.document_id
             JOIN content_document_head h ON h.document_id = r.document_id
             WHERE d.library_id = $1
               AND h.readable_revision_id IS NULL
               AND h.active_revision_id IS NULL
               AND EXISTS (SELECT 1 FROM content_chunk c WHERE c.revision_id = r.id)
             ORDER BY r.document_id, r.revision_number DESC",
        )
        .bind(library.id)
        .fetch_all(&state.persistence.postgres)
        .await
        .with_context(|| {
            format!("failed to list null-head candidates for library {}", library.id)
        })?;

        // Count null-head documents that had no chunk-bearing revision —
        // those need re-ingest, not a head promotion.
        let no_chunks_count: i64 = sqlx::query_scalar(
            "SELECT count(DISTINCT h.document_id)
             FROM content_document_head h
             JOIN content_document d ON d.id = h.document_id
             WHERE d.library_id = $1
               AND h.readable_revision_id IS NULL
               AND h.active_revision_id IS NULL
               AND NOT EXISTS (
                   SELECT 1 FROM content_revision r
                   JOIN content_chunk c ON c.revision_id = r.id
                   WHERE r.document_id = h.document_id
               )",
        )
        .bind(library.id)
        .fetch_one(&state.persistence.postgres)
        .await
        .with_context(|| {
            format!("failed to count no-chunk null-head docs for library {}", library.id)
        })?;
        total_skipped_no_chunks += no_chunks_count as usize;

        info!(
            library_id = %library.id,
            library_name = %library.display_name,
            backfill_candidates = candidates.len(),
            skipped_no_chunks = no_chunks_count,
            "promoting null-head documents"
        );

        for candidate in candidates {
            match state
                .canonical_services
                .content
                .promote_document_head(
                    &state,
                    PromoteHeadCommand {
                        document_id: candidate.document_id,
                        active_revision_id: Some(candidate.revision_id),
                        readable_revision_id: Some(candidate.revision_id),
                        latest_mutation_id: None,
                        latest_successful_attempt_id: None,
                    },
                )
                .await
            {
                Ok(_) => {
                    total_promoted += 1;
                }
                Err(error) => {
                    warn!(
                        document_id = %candidate.document_id,
                        revision_id = %candidate.revision_id,
                        ?error,
                        "promote_document_head failed; continuing with next document"
                    );
                }
            }
        }
    }

    info!(total_promoted, total_skipped_no_chunks, "null-head promotion finished");
    Ok(())
}
