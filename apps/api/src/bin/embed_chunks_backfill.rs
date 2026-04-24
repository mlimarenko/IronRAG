//! Backfill utility: embed every chunk in one or all libraries using
//! the library's active `EmbedChunk` binding and persist the vectors
//! into Arango's `knowledge_chunk_vector` collection.
//!
//! Needed as a one-time migration because the ingest worker's
//! `embed_chunk` stage was a deferred no-op until the fix shipped
//! alongside this binary. Libraries ingested before that fix have
//! `vector_state: ready` revisions but zero rows in
//! `knowledge_chunk_vector`, which silently skips the vector lane in
//! retrieval.
//!
//! Usage:
//!   ironrag-embed-chunks-backfill                           # all libraries
//!   ironrag-embed-chunks-backfill <library-uuid>            # one library

use anyhow::Context;
use ironrag_backend::{
    app::{config::Settings, state::AppState},
    infra::repositories::catalog_repository,
};
use tracing::{info, warn};
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let settings = Settings::from_env()?;
    ironrag_backend::shared::telemetry::init(&settings.log_filter);
    let state = AppState::new(settings).await?;

    let mut args = std::env::args().skip(1);
    let target_library_id = args.next().map(|value| Uuid::parse_str(&value)).transpose()?;

    let libraries = match target_library_id {
        Some(library_id) => catalog_repository::list_libraries(&state.persistence.postgres, None)
            .await?
            .into_iter()
            .filter(|library| library.id == library_id)
            .collect::<Vec<_>>(),
        None => catalog_repository::list_libraries(&state.persistence.postgres, None).await?,
    };

    if libraries.is_empty() {
        anyhow::bail!("no libraries matched backfill target");
    }

    let mut total_rebuilt = 0usize;
    for library in libraries {
        info!(
            library_id = %library.id,
            workspace_id = %library.workspace_id,
            library_name = %library.display_name,
            "backfilling chunk embeddings"
        );
        match state
            .canonical_services
            .search
            .rebuild_chunk_embeddings(&state, library.id)
            .await
            .with_context(|| {
                format!("failed to backfill chunk embeddings for library {}", library.id)
            }) {
            Ok(rebuilt) => {
                info!(
                    library_id = %library.id,
                    chunks_rebuilt = rebuilt,
                    "chunk embedding backfill completed",
                );
                total_rebuilt += rebuilt;
            }
            Err(error) => {
                warn!(
                    library_id = %library.id,
                    ?error,
                    "chunk embedding backfill failed; continuing with next library",
                );
            }
        }
    }

    info!(total_chunks_rebuilt = total_rebuilt, "chunk embedding backfill finished");
    Ok(())
}
