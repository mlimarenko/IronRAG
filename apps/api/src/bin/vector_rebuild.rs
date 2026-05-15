//! Vector-plane rebuild utility: reconcile Arango's instance-wide vector
//! index dimensions with a source library's active vector binding and rebuild
//! all library vector material that must share those indexes.
//!
//! Usage:
//!   ironrag-vector-rebuild <source-library-uuid>

use anyhow::Context;
use ironrag_backend::app::{config::Settings, state::AppState};
use tracing::info;
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let settings = Settings::from_env()?;
    ironrag_backend::observability::init_tracing()?;
    let state = AppState::new(settings).await?;

    let mut args = std::env::args().skip(1);
    let Some(target_library_id) = args.next().map(|value| Uuid::parse_str(&value)).transpose()?
    else {
        anyhow::bail!("usage: ironrag-vector-rebuild <source-library-uuid>");
    };
    if args.next().is_some() {
        anyhow::bail!("usage: ironrag-vector-rebuild <source-library-uuid>");
    }

    let outcome = state
        .canonical_services
        .search
        .rebuild_vector_plane_from_library_binding(&state, target_library_id)
        .await
        .with_context(|| {
            format!(
                "failed to rebuild Arango vector plane from library binding {target_library_id}"
            )
        })?;

    info!(
        library_id = %target_library_id,
        previous_dimensions = ?outcome.previous_dimensions,
        target_dimensions = outcome.target_dimensions,
        indexes_recreated = outcome.indexes_recreated,
        libraries_rebuilt = outcome.libraries_rebuilt,
        chunk_embeddings_rebuilt = outcome.chunk_embeddings_rebuilt,
        graph_node_embeddings_rebuilt = outcome.graph_node_embeddings_rebuilt,
        "Arango vector-plane rebuild completed",
    );
    ironrag_backend::observability::shutdown_tracing().await;
    Ok(())
}
