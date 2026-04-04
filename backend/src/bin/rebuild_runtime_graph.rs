use anyhow::Context;
use rustrag_backend::{
    app::{config::Settings, state::AppState},
    infra::repositories::catalog_repository,
};
use tracing::info;
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let settings = Settings::from_env()?;
    rustrag_backend::shared::telemetry::init(&settings.log_filter);
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
        anyhow::bail!("no libraries matched rebuild target");
    }

    for library in libraries {
        info!(
            library_id = %library.id,
            workspace_id = %library.workspace_id,
            library_name = %library.display_name,
            "rebuilding runtime graph"
        );
        let outcome = state
            .canonical_services
            .graph
            .rebuild_library_graph(&state, library.id)
            .await
            .with_context(|| format!("failed to rebuild graph for library {}", library.id))?;
        info!(
            library_id = %library.id,
            projection_version = outcome.projection_version,
            node_count = outcome.node_count,
            edge_count = outcome.edge_count,
            "runtime graph rebuild completed",
        );
    }

    Ok(())
}
