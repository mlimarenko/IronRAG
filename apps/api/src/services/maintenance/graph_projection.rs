//! Converges invalid derived topology from retained, currently published inputs.
//! This class never admits extraction work or changes source visibility/history.
//!
//! Retained inputs are `runtime_graph_extraction` and the canonical current
//! content/revision/chunk identities. Reconciliation never inserts, updates or
//! deletes extraction/model history, content heads, visibility intents, access
//! decisions or ingest jobs. Insufficient retained coverage rolls back.
//!
//! Its write set is derived nodes, edges, evidence and summaries (including
//! dependent bundle-reference cascades), filtered-artifact diagnostics, snapshot
//! identity, and library source generation. The scheduler separately owns the
//! maintenance lease; a committed projection may prewarm the topology cache.

use crate::{
    app::state::AppState,
    infra::repositories::{self, content_fingerprint_repository, runtime_graph_repository},
    services::graph::{error::GraphServiceError, service::GraphRebuildRecovery},
};
use uuid::Uuid;

/// Checks one leased library and reconstructs only an invalid published graph.
/// Native statement and transaction deadlines cover the probe and rebuild;
/// the scheduler owns retry, contention deferral and terminal failure reporting.
pub async fn reconcile(state: &AppState, library_id: Uuid) -> Result<(), GraphServiceError> {
    let seconds = state.settings.runtime_graph_extract_stage_timeout_seconds.max(1);
    let timeout = std::time::Duration::from_secs(seconds);
    let deadline = chrono::Utc::now()
        + chrono::Duration::from_std(timeout)
            .map_err(|error| GraphServiceError::StateConflict { message: error.to_string() })?;
    let mut transaction = state.persistence.postgres.begin().await?;
    runtime_graph_repository::RuntimeGraphSqlDeadline::at(tokio::time::Instant::now() + timeout)
        .install(&mut transaction)
        .await?;
    let snapshot =
        repositories::get_runtime_graph_snapshot_with_executor(&mut *transaction, library_id)
            .await?;
    let invalid = match snapshot {
        Some(snapshot) if snapshot.graph_status == "stale" => true,
        Some(snapshot) if matches!(snapshot.graph_status.as_str(), "ready" | "empty") => {
            !content_fingerprint_repository::graph_projection_is_current_with_executor(
                &mut *transaction,
                snapshot.projection_identity(),
            )
            .await?
        }
        // An absent projection is not a failed build. Active/failed builds keep
        // their owning lifecycle instead of becoming automatic reconstruction.
        _ => false,
    };
    transaction.rollback().await?;
    if invalid {
        state
            .application_services
            .graph
            .rebuild_library_graph(
                state,
                library_id,
                GraphRebuildRecovery::DerivedOnlyWithWriterFence { deadline },
            )
            .await?;
    }
    Ok(())
}
