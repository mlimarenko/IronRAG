//! Published graph-topology snapshot identity validation.
//!
//! Both the compact topology stream/stream cache (`graph_stream`) and the
//! in-memory projection cache (`runtime_read`) admit a snapshot only when it
//! is published with a consistent nonpositive-free identity and topology
//! counts. The rule lives here once so the two admission paths cannot drift.

use uuid::Uuid;

use crate::{infra::repositories, services::knowledge::error::KnowledgeServiceError};

pub(super) fn graph_integrity_error(library_id: Uuid, message: &str) -> KnowledgeServiceError {
    KnowledgeServiceError::GraphIntegrity { message: format!("library {library_id}: {message}") }
}

pub(super) fn published_snapshot_identity(
    snapshot: &repositories::RuntimeGraphSnapshotRow,
    library_id: Uuid,
) -> Result<(i64, i64), KnowledgeServiceError> {
    if snapshot.graph_status != "ready" && snapshot.graph_status != "empty" {
        return Err(graph_integrity_error(library_id, "snapshot is not published"));
    }
    if snapshot.projection_version <= 0 || snapshot.topology_generation <= 0 {
        return Err(graph_integrity_error(
            library_id,
            "published snapshot has a nonpositive identity",
        ));
    }
    if snapshot.graph_status == "empty" && (snapshot.node_count != 0 || snapshot.edge_count != 0) {
        return Err(graph_integrity_error(library_id, "empty snapshot has topology counts"));
    }
    if snapshot.graph_status == "ready" && (snapshot.node_count < 0 || snapshot.edge_count < 0) {
        return Err(graph_integrity_error(
            library_id,
            "ready snapshot has negative topology counts",
        ));
    }
    Ok((snapshot.projection_version, snapshot.topology_generation))
}
