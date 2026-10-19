// ---------------------------------------------------------------------------
// rebuild
// ---------------------------------------------------------------------------

use super::*;
use maintenance_rebuild as rebuild;

#[allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "operator maintenance commands intentionally stream reports to the terminal"
)]
pub(super) async fn run_rebuild(state: &AppState, command: RebuildCommand) -> Result<()> {
    match command {
        RebuildCommand::VectorPlane { source_library } => {
            rebuild::vector_plane(state, source_library).await?;
            println!("rebuild.vector-plane completed for source library {source_library}");
        }
        RebuildCommand::RuntimeGraph { library } => {
            rebuild::runtime_graph(state, library).await?;
            println!(
                "rebuild.runtime-graph completed for {}",
                library.map_or_else(|| "all libraries".to_string(), |id| id.to_string()),
            );
        }
        RebuildCommand::EntityEmbeddings { library } => {
            let started = std::time::Instant::now();
            let vectors_upserted = rebuild::entity_embeddings(state, library).await?;
            println!(
                "rebuild.entity-embeddings completed entities_processed={vectors_upserted} vectors_upserted={vectors_upserted} elapsed_ms={} library_id={library}",
                started.elapsed().as_millis(),
            );
        }
    }
    Ok(())
}
