use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{app::state::AppState, infra::repositories};

#[derive(Clone)]
pub struct Context {
    postgres: PgPool,
    knowledge_plane_backend: String,
}

impl Context {
    #[must_use]
    pub fn new(postgres: PgPool) -> Self {
        Self::with_backend(postgres, "postgres")
    }

    #[must_use]
    pub fn with_backend(postgres: PgPool, knowledge_plane_backend: impl Into<String>) -> Self {
        Self { postgres, knowledge_plane_backend: knowledge_plane_backend.into() }
    }

    #[must_use]
    pub fn from_state(state: &AppState) -> Self {
        Self::with_backend(
            state.persistence.postgres.clone(),
            state.settings.knowledge_plane_backend.clone(),
        )
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GcReport {
    pub entities_deleted: u32,
    pub relations_deleted: u32,
    #[serde(default)]
    pub evidence_deleted: u32,
    pub libraries_scanned: u32,
}

impl GcReport {
    #[must_use]
    pub const fn merge(self, other: Self) -> Self {
        Self {
            entities_deleted: self.entities_deleted.saturating_add(other.entities_deleted),
            relations_deleted: self.relations_deleted.saturating_add(other.relations_deleted),
            evidence_deleted: self.evidence_deleted.saturating_add(other.evidence_deleted),
            libraries_scanned: self.libraries_scanned.saturating_add(other.libraries_scanned),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GraphGcError {
    #[error("library {library_id} has {active_jobs} active ingest jobs")]
    ActiveIngest { library_id: Uuid, active_jobs: i64 },
    #[error("failed to inspect active ingest jobs for library {library_id}: {source}")]
    ActiveIngestLookup { library_id: Uuid, source: sqlx::Error },
    #[error("failed to acquire graph GC lock for library {library_id}: {source}")]
    LockAcquire { library_id: Uuid, source: sqlx::Error },
    #[error("failed to release graph GC lock for library {library_id}: {source}")]
    LockRelease { library_id: Uuid, source: sqlx::Error },
    #[error("postgres error during graph GC for library {library_id}: {source}")]
    Postgres { library_id: Uuid, source: sqlx::Error },
    #[error("unsupported knowledge plane backend `{backend}` for graph GC")]
    UnsupportedBackend { backend: String },
}

/// Deletes graph entities that no longer have evidence tied to current content.
pub async fn gc_zombie_nodes(library_id: Uuid, ctx: &Context) -> Result<GcReport, GraphGcError> {
    let graph_lock = repositories::acquire_runtime_library_graph_lock(&ctx.postgres, library_id)
        .await
        .map_err(|source| GraphGcError::LockAcquire { library_id, source })?;

    let result = async {
        ensure_library_has_no_active_ingest_jobs(library_id, &ctx.postgres).await?;
        match ctx.knowledge_plane_backend.as_str() {
            "postgres" => run_gc_postgres(library_id, &ctx.postgres).await,
            backend => Err(GraphGcError::UnsupportedBackend { backend: backend.to_string() }),
        }
    }
    .await;

    let release_result = repositories::release_runtime_library_graph_lock(graph_lock, library_id)
        .await
        .map_err(|source| GraphGcError::LockRelease { library_id, source });

    match (result, release_result) {
        (Ok(report), Ok(())) => Ok(report),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(release_error)) => {
            tracing::error!(
                %library_id,
                ?error,
                ?release_error,
                "graph GC failed and advisory lock release also failed"
            );
            Err(error)
        }
    }
}

async fn ensure_library_has_no_active_ingest_jobs(
    library_id: Uuid,
    postgres: &PgPool,
) -> Result<(), GraphGcError> {
    let active_jobs = sqlx::query_scalar::<_, i64>(
        "select count(*)
         from ingest_job
         where library_id = $1
           and queue_state in ('queued', 'leased')
           and completed_at is null",
    )
    .bind(library_id)
    .fetch_one(postgres)
    .await
    .map_err(|source| GraphGcError::ActiveIngestLookup { library_id, source })?;

    if active_jobs > 0 {
        return Err(GraphGcError::ActiveIngest { library_id, active_jobs });
    }
    Ok(())
}

pub(crate) async fn run_gc_postgres(
    library_id: Uuid,
    postgres: &PgPool,
) -> Result<GcReport, GraphGcError> {
    let mut tx =
        postgres.begin().await.map_err(|source| GraphGcError::Postgres { library_id, source })?;
    let report = run_gc_postgres_transaction(library_id, &mut tx).await?;
    tx.commit().await.map_err(|source| GraphGcError::Postgres { library_id, source })?;
    Ok(report)
}

pub(crate) async fn run_gc_postgres_transaction(
    library_id: Uuid,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<GcReport, GraphGcError> {
    let orphan_evidence_before = delete_orphan_runtime_graph_evidence(library_id, tx).await?;
    let (edge_evidence_deleted, edges_deleted) =
        delete_zombie_runtime_graph_edges(library_id, tx).await?;
    let (node_evidence_deleted, nodes_deleted) =
        delete_zombie_runtime_graph_nodes(library_id, tx).await?;
    let orphan_evidence_after = delete_orphan_runtime_graph_evidence(library_id, tx).await?;

    Ok(GcReport {
        entities_deleted: count_to_u32(nodes_deleted),
        relations_deleted: count_to_u32(edges_deleted),
        evidence_deleted: count_to_u32(
            orphan_evidence_before
                + edge_evidence_deleted
                + node_evidence_deleted
                + orphan_evidence_after,
        ),
        libraries_scanned: 1,
    })
}

async fn delete_orphan_runtime_graph_evidence(
    library_id: Uuid,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<i64, GraphGcError> {
    sqlx::query_scalar::<_, i64>(
        "with deleted as ( \
             delete from runtime_graph_evidence evidence \
             where evidence.library_id = $1 \
               and ( \
                   (evidence.target_kind = 'node' and not exists ( \
                       select 1 from runtime_graph_node node \
                       where node.library_id = evidence.library_id \
                         and node.id = evidence.target_id \
                   )) \
                   or \
                   (evidence.target_kind = 'edge' and not exists ( \
                       select 1 from runtime_graph_edge edge \
                       where edge.library_id = evidence.library_id \
                         and edge.id = evidence.target_id \
                   )) \
               ) \
             returning 1 \
         ) \
         select count(*)::bigint from deleted",
    )
    .bind(library_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|source| GraphGcError::Postgres { library_id, source })
}

async fn delete_zombie_runtime_graph_edges(
    library_id: Uuid,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<(i64, i64), GraphGcError> {
    sqlx::query_as::<_, (i64, i64)>(
        "with zombie_nodes as ( \
             select node.id \
             from runtime_graph_node node \
             where node.library_id = $1 \
               and not exists ( \
                   select 1 from runtime_graph_evidence evidence \
                   where evidence.library_id = node.library_id \
                     and evidence.target_kind = 'node' \
                     and evidence.target_id = node.id \
               ) \
         ), \
         zombie_edges as ( \
             select edge.id \
             from runtime_graph_edge edge \
             where edge.library_id = $1 \
               and ( \
                   not exists ( \
                       select 1 from runtime_graph_evidence evidence \
                       where evidence.library_id = edge.library_id \
                         and evidence.target_kind = 'edge' \
                         and evidence.target_id = edge.id \
                   ) \
                   or exists (select 1 from zombie_nodes node where node.id = edge.from_node_id) \
                   or exists (select 1 from zombie_nodes node where node.id = edge.to_node_id) \
               ) \
         ), \
         deleted_edge_evidence as ( \
             delete from runtime_graph_evidence evidence \
             using zombie_edges \
             where evidence.library_id = $1 \
               and evidence.target_kind = 'edge' \
               and evidence.target_id = zombie_edges.id \
             returning 1 \
         ), \
         deleted_edges as ( \
             delete from runtime_graph_edge edge \
             using zombie_edges \
             where edge.library_id = $1 \
               and edge.id = zombie_edges.id \
             returning 1 \
         ) \
         select \
             (select count(*)::bigint from deleted_edge_evidence), \
             (select count(*)::bigint from deleted_edges)",
    )
    .bind(library_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|source| GraphGcError::Postgres { library_id, source })
}

async fn delete_zombie_runtime_graph_nodes(
    library_id: Uuid,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<(i64, i64), GraphGcError> {
    sqlx::query_as::<_, (i64, i64)>(
        "with zombie_nodes as ( \
             select node.id \
             from runtime_graph_node node \
             where node.library_id = $1 \
               and not exists ( \
                   select 1 from runtime_graph_evidence evidence \
                   where evidence.library_id = node.library_id \
                     and evidence.target_kind = 'node' \
                     and evidence.target_id = node.id \
               ) \
         ), \
         deleted_node_evidence as ( \
             delete from runtime_graph_evidence evidence \
             using zombie_nodes \
             where evidence.library_id = $1 \
               and evidence.target_kind = 'node' \
               and evidence.target_id = zombie_nodes.id \
             returning 1 \
         ), \
         deleted_nodes as ( \
             delete from runtime_graph_node node \
             using zombie_nodes \
             where node.library_id = $1 \
               and node.id = zombie_nodes.id \
             returning 1 \
         ) \
         select \
             (select count(*)::bigint from deleted_node_evidence), \
             (select count(*)::bigint from deleted_nodes)",
    )
    .bind(library_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|source| GraphGcError::Postgres { library_id, source })
}

fn count_to_u32(count: i64) -> u32 {
    u32::try_from(count.max(0)).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_merge_is_saturating_and_additive() {
        let left = GcReport {
            entities_deleted: 10,
            relations_deleted: u32::MAX,
            evidence_deleted: 3,
            libraries_scanned: 1,
        };
        let right = GcReport {
            entities_deleted: 5,
            relations_deleted: 1,
            evidence_deleted: 7,
            libraries_scanned: 2,
        };

        let merged = left.merge(right);
        assert_eq!(merged.entities_deleted, 15);
        assert_eq!(merged.relations_deleted, u32::MAX);
        assert_eq!(merged.evidence_deleted, 10);
        assert_eq!(merged.libraries_scanned, 3);
    }

    #[test]
    fn count_to_u32_clamps_negative_and_large_counts() {
        assert_eq!(count_to_u32(-1), 0);
        assert_eq!(count_to_u32(i64::from(u32::MAX) + 1), u32::MAX);
    }
}
