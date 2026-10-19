pub mod acronym_alias_backfill;
pub(crate) mod backfill;
pub mod community_detection;
pub mod error;
pub(crate) mod extract;
pub mod gc;
pub(crate) mod identity;
pub(crate) mod maintenance;
pub(super) mod merge;
pub(crate) mod normalized_projection;
pub(super) mod projection;
pub(crate) mod projection_guard;
pub(crate) mod quality_guard;
pub(super) mod rebuild;
pub(crate) mod reconciliation_scope;
pub mod service;
pub(crate) mod summary;

#[cfg(feature = "test-support")]
pub use rebuild::RevisionGraphReconcileOutcome;
#[cfg(feature = "test-support")]
pub use summary::{PendingGraphSummaryRefresh, PublishedGraphSummaryRefreshOutcome};

/// Runs the ingest-authorized revision graph reconcile on a caller-supplied
/// connection, without the job/head guard and publication handshake.
///
/// Integration tests use this to exercise the reconcile in a controlled
/// revision/head arrangement (for example, a new revision whose previous
/// active revision is still the readable head) that the production
/// `graph_refresh` guard would otherwise reject as superseded.
#[cfg(feature = "test-support")]
pub async fn reconcile_revision_graph_for_test(
    state: &crate::app::state::AppState,
    authority: crate::infra::knowledge_plane::IngestRevisionWriteAuthority,
    document_id: uuid::Uuid,
    connection: &mut sqlx::PgConnection,
) -> Result<RevisionGraphReconcileOutcome, error::GraphServiceError> {
    rebuild::reconcile_revision_graph_authorized(
        state,
        authority,
        document_id,
        connection,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
}
