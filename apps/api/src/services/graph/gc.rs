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
    #[error("library {library_id} catalog row is busy; graph GC deferred")]
    LibraryBusy { library_id: Uuid },
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
    #[error(
        "{root_kind} {root_id} requires {physical_rows} physical delete mutations, above the per-tick budget {budget}"
    )]
    OversizedClosure { root_kind: &'static str, root_id: Uuid, physical_rows: i64, budget: i64 },
    #[error(
        "runtime graph batch produced {physical_rows} physical delete mutations, above the per-tick budget {budget}; transaction refused"
    )]
    PhysicalMutationBudgetOverrun { physical_rows: i64, budget: i64 },
}

/// One atomic, scheduler-bounded graph deletion batch.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PhysicalDeletionBatchReport {
    pub candidate_roots: i64,
    pub nodes_deleted: i64,
    pub edges_deleted: i64,
    pub evidence_deleted: i64,
    pub dependent_rows_deleted: i64,
    pub physical_rows_deleted: i64,
    pub work_remaining: bool,
}

pub(crate) fn budgeted_root_prefix(
    root_kind: &'static str,
    candidates: &[(Uuid, i64)],
    budget: i64,
) -> Result<Vec<Uuid>, GraphGcError> {
    let mut selected = Vec::new();
    let mut consumed = 0_i64;
    for &(root_id, physical_rows) in candidates {
        if physical_rows > budget {
            if selected.is_empty() {
                return Err(GraphGcError::OversizedClosure {
                    root_kind,
                    root_id,
                    physical_rows,
                    budget,
                });
            }
            break;
        }
        let Some(next) = consumed.checked_add(physical_rows) else {
            break;
        };
        if next > budget {
            break;
        }
        selected.push(root_id);
        consumed = next;
    }
    Ok(selected)
}

/// Deletes graph entities that no longer have evidence tied to current content.
pub async fn gc_zombie_nodes(library_id: Uuid, ctx: &Context) -> Result<GcReport, GraphGcError> {
    match ctx.knowledge_plane_backend.as_str() {
        "postgres" => run_gc_postgres(library_id, &ctx.postgres).await,
        backend => Err(GraphGcError::UnsupportedBackend { backend: backend.to_string() }),
    }
}

async fn ensure_library_has_no_active_ingest_jobs_locked(
    library_id: Uuid,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<(), GraphGcError> {
    let active_job_ids = sqlx::query_scalar::<_, Uuid>(
        "select id
         from ingest_job
         where library_id = $1
           and queue_state in ('queued', 'leased')
           and completed_at is null
         for update",
    )
    .bind(library_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(|source| GraphGcError::ActiveIngestLookup { library_id, source })?;

    if !active_job_ids.is_empty() {
        let active_jobs = i64::try_from(active_job_ids.len()).unwrap_or(i64::MAX);
        return Err(GraphGcError::ActiveIngest { library_id, active_jobs });
    }
    Ok(())
}

pub(crate) async fn run_gc_postgres(
    library_id: Uuid,
    postgres: &PgPool,
) -> Result<GcReport, GraphGcError> {
    let acquired = repositories::acquire_runtime_graph_gc_quiescence_guard(postgres, library_id)
        .await
        .map_err(|source| GraphGcError::LockAcquire { library_id, source })?;
    let mut guard = match acquired {
        repositories::RuntimeGraphGcQuiescenceAcquireOutcome::Acquired(guard) => guard,
        repositories::RuntimeGraphGcQuiescenceAcquireOutcome::LibraryNotFound => {
            return Ok(GcReport::default());
        }
        repositories::RuntimeGraphGcQuiescenceAcquireOutcome::LibraryBusy => {
            return Err(GraphGcError::LibraryBusy { library_id });
        }
        repositories::RuntimeGraphGcQuiescenceAcquireOutcome::ActiveIngest { active_jobs } => {
            return Err(GraphGcError::ActiveIngest { library_id, active_jobs });
        }
    };
    let outcome = run_gc_postgres_transaction(library_id, guard.transaction_mut()).await;
    match outcome {
        Ok(report) => {
            guard
                .commit()
                .await
                .map_err(|source| GraphGcError::LockRelease { library_id, source })?;
            Ok(report)
        }
        Err(error) => {
            if let Err(rollback_error) = guard.rollback().await {
                tracing::error!(%library_id, ?rollback_error, "graph GC rollback failed");
            }
            Err(error)
        }
    }
}

pub(crate) async fn run_gc_postgres_transaction(
    library_id: Uuid,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<GcReport, GraphGcError> {
    ensure_library_has_no_active_ingest_jobs_locked(library_id, tx).await?;
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

/// Deletes complete zombie-edge closures while capping committed physical row
/// mutations. Candidate edge rows are locked before their evidence closures are
/// costed, so a bundle reference cannot be added after it was admitted to the
/// budget. Runtime-graph writers are excluded by the caller's graph lock.
pub(crate) async fn delete_zombie_edge_closure_batch(
    library_id: Uuid,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    budget: i64,
) -> Result<PhysicalDeletionBatchReport, GraphGcError> {
    let candidate_ids = sqlx::query_scalar::<_, Uuid>(
        "with zombie_nodes as materialized (
             select node.id
             from runtime_graph_node node
             where node.library_id = $1
               and not exists (
                   select 1 from runtime_graph_evidence evidence
                   where evidence.library_id = node.library_id
                     and evidence.target_kind = 'node'
                     and evidence.target_id = node.id
               )
         )
         select edge.id
         from runtime_graph_edge edge
         where edge.library_id = $1
           and (
               not exists (
                   select 1 from runtime_graph_evidence evidence
                   where evidence.library_id = edge.library_id
                     and evidence.target_kind = 'edge'
                     and evidence.target_id = edge.id
               )
               or edge.from_node_id in (select id from zombie_nodes)
               or edge.to_node_id in (select id from zombie_nodes)
           )
         order by edge.id
         limit $2
         for update of edge",
    )
    .bind(library_id)
    .bind(budget)
    .fetch_all(&mut **tx)
    .await
    .map_err(|source| GraphGcError::Postgres { library_id, source })?;
    if candidate_ids.is_empty() {
        return Ok(PhysicalDeletionBatchReport::default());
    }

    // Evidence has no target FK, so lock it explicitly. Its bundle-reference
    // FK then prevents a new dependent row from appearing while costs are used.
    sqlx::query(
        "select id
         from runtime_graph_evidence
         where library_id = $1
           and target_kind = 'edge'
           and target_id = any($2)
         for update",
    )
    .bind(library_id)
    .bind(&candidate_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(|source| GraphGcError::Postgres { library_id, source })?;

    let costs = sqlx::query_as::<_, (Uuid, i64)>(
        "select edge.id,
                1
                + (select count(*) from knowledge_bundle_relation ref
                   where ref.relation_id = edge.id)
                + (select count(*) from runtime_graph_evidence evidence
                   where evidence.library_id = edge.library_id
                     and evidence.target_kind = 'edge'
                     and evidence.target_id = edge.id)
                + (select count(*)
                   from knowledge_bundle_evidence ref
                   join runtime_graph_evidence evidence on evidence.id = ref.evidence_id
                   where evidence.library_id = edge.library_id
                     and evidence.target_kind = 'edge'
                     and evidence.target_id = edge.id)::bigint as physical_rows
         from runtime_graph_edge edge
         where edge.library_id = $1 and edge.id = any($2)
         order by edge.id",
    )
    .bind(library_id)
    .bind(&candidate_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(|source| GraphGcError::Postgres { library_id, source })?;
    let selected = budgeted_root_prefix("runtime graph edge", &costs, budget)?;
    let candidate_roots = i64::try_from(candidate_ids.len()).unwrap_or(i64::MAX);
    let work_remaining = candidate_roots >= budget || selected.len() < candidate_ids.len();

    let bundle_evidence_deleted = sqlx::query(
        "delete from knowledge_bundle_evidence ref
         using runtime_graph_evidence evidence
         where evidence.library_id = $1
           and evidence.target_kind = 'edge'
           and evidence.target_id = any($2)
           and ref.evidence_id = evidence.id",
    )
    .bind(library_id)
    .bind(&selected)
    .execute(&mut **tx)
    .await
    .map_err(|source| GraphGcError::Postgres { library_id, source })?
    .rows_affected();
    let evidence_deleted = sqlx::query(
        "delete from runtime_graph_evidence
         where library_id = $1 and target_kind = 'edge' and target_id = any($2)",
    )
    .bind(library_id)
    .bind(&selected)
    .execute(&mut **tx)
    .await
    .map_err(|source| GraphGcError::Postgres { library_id, source })?
    .rows_affected();
    let bundle_relations_deleted =
        sqlx::query("delete from knowledge_bundle_relation where relation_id = any($1)")
            .bind(&selected)
            .execute(&mut **tx)
            .await
            .map_err(|source| GraphGcError::Postgres { library_id, source })?
            .rows_affected();
    let edges_deleted =
        sqlx::query("delete from runtime_graph_edge where library_id = $1 and id = any($2)")
            .bind(library_id)
            .bind(&selected)
            .execute(&mut **tx)
            .await
            .map_err(|source| GraphGcError::Postgres { library_id, source })?
            .rows_affected();
    let dependent_rows_deleted = bundle_evidence_deleted.saturating_add(bundle_relations_deleted);
    let physical_rows_deleted =
        dependent_rows_deleted.saturating_add(evidence_deleted).saturating_add(edges_deleted);
    let physical_rows_deleted_i64 = i64::try_from(physical_rows_deleted).unwrap_or(i64::MAX);
    if physical_rows_deleted_i64 > budget {
        return Err(GraphGcError::PhysicalMutationBudgetOverrun {
            physical_rows: physical_rows_deleted_i64,
            budget,
        });
    }

    Ok(PhysicalDeletionBatchReport {
        candidate_roots,
        edges_deleted: i64::try_from(edges_deleted).unwrap_or(i64::MAX),
        evidence_deleted: i64::try_from(evidence_deleted).unwrap_or(i64::MAX),
        dependent_rows_deleted: i64::try_from(dependent_rows_deleted).unwrap_or(i64::MAX),
        physical_rows_deleted: physical_rows_deleted_i64,
        work_remaining,
        ..PhysicalDeletionBatchReport::default()
    })
}

/// Deletes evidence-free, edge-free zombie nodes and their bundle references.
pub(crate) async fn delete_zombie_node_closure_batch(
    library_id: Uuid,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    budget: i64,
) -> Result<PhysicalDeletionBatchReport, GraphGcError> {
    let candidate_ids = sqlx::query_scalar::<_, Uuid>(
        "select node.id
         from runtime_graph_node node
         where node.library_id = $1
           and not exists (
               select 1 from runtime_graph_evidence evidence
               where evidence.library_id = node.library_id
                 and evidence.target_kind = 'node'
                 and evidence.target_id = node.id
           )
           and not exists (
               select 1 from runtime_graph_edge edge
               where edge.from_node_id = node.id or edge.to_node_id = node.id
           )
         order by node.id
         limit $2
         for update of node",
    )
    .bind(library_id)
    .bind(budget)
    .fetch_all(&mut **tx)
    .await
    .map_err(|source| GraphGcError::Postgres { library_id, source })?;
    if candidate_ids.is_empty() {
        return Ok(PhysicalDeletionBatchReport::default());
    }
    let costs = sqlx::query_as::<_, (Uuid, i64)>(
        "select node.id,
                1 + (select count(*) from knowledge_bundle_entity ref
                     where ref.entity_id = node.id)::bigint as physical_rows
         from runtime_graph_node node
         where node.library_id = $1 and node.id = any($2)
         order by node.id",
    )
    .bind(library_id)
    .bind(&candidate_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(|source| GraphGcError::Postgres { library_id, source })?;
    let selected = budgeted_root_prefix("runtime graph node", &costs, budget)?;
    let candidate_roots = i64::try_from(candidate_ids.len()).unwrap_or(i64::MAX);
    let work_remaining = candidate_roots >= budget || selected.len() < candidate_ids.len();
    let dependent = sqlx::query("delete from knowledge_bundle_entity where entity_id = any($1)")
        .bind(&selected)
        .execute(&mut **tx)
        .await
        .map_err(|source| GraphGcError::Postgres { library_id, source })?
        .rows_affected();
    let nodes =
        sqlx::query("delete from runtime_graph_node where library_id = $1 and id = any($2)")
            .bind(library_id)
            .bind(&selected)
            .execute(&mut **tx)
            .await
            .map_err(|source| GraphGcError::Postgres { library_id, source })?
            .rows_affected();
    let physical = dependent.saturating_add(nodes);
    let physical_i64 = i64::try_from(physical).unwrap_or(i64::MAX);
    if physical_i64 > budget {
        return Err(GraphGcError::PhysicalMutationBudgetOverrun {
            physical_rows: physical_i64,
            budget,
        });
    }
    Ok(PhysicalDeletionBatchReport {
        candidate_roots,
        nodes_deleted: i64::try_from(nodes).unwrap_or(i64::MAX),
        dependent_rows_deleted: i64::try_from(dependent).unwrap_or(i64::MAX),
        physical_rows_deleted: physical_i64,
        work_remaining,
        ..PhysicalDeletionBatchReport::default()
    })
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

    #[test]
    fn physical_budget_selects_only_complete_root_closures() {
        let first = Uuid::from_u128(1);
        let second = Uuid::from_u128(2);
        let third = Uuid::from_u128(3);
        let selected =
            budgeted_root_prefix("runtime graph edge", &[(first, 4), (second, 6), (third, 1)], 10)
                .unwrap();
        assert_eq!(selected, vec![first, second]);
    }

    #[test]
    fn indivisible_closure_above_budget_fails_explicitly() {
        let root_id = Uuid::from_u128(1);
        let error = budgeted_root_prefix("runtime graph edge", &[(root_id, 11)], 10)
            .expect_err("oversized closure must not be partially deleted");
        assert!(matches!(
            error,
            GraphGcError::OversizedClosure {
                root_kind: "runtime graph edge",
                root_id: found,
                physical_rows: 11,
                budget: 10,
            } if found == root_id
        ));
    }

    #[test]
    fn report_deserialization_defaults_missing_evidence_count() {
        let report: GcReport = serde_json::from_value(serde_json::json!({
            "entitiesDeleted": 2,
            "relationsDeleted": 3,
            "librariesScanned": 1,
        }))
        .unwrap();

        assert_eq!(report.evidence_deleted, 0);
        assert_eq!(report.entities_deleted, 2);
    }

    #[tokio::test]
    async fn context_preserves_explicit_knowledge_plane_backend() {
        let postgres = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/neutral")
            .unwrap();

        let context = Context::with_backend(postgres, "unsupported");

        assert_eq!(context.knowledge_plane_backend, "unsupported");
    }

    /// A disposable, freshly migrated database for this test module. Connecting
    /// straight to the shared dev database would compare its already-applied
    /// migration checksum against whatever this workstation's copy of
    /// `0001_init.sql` currently hashes to, which drifts across a
    /// session that edits the baseline migration; a scratch database sidesteps
    /// that entirely by always migrating from empty.
    struct DisposableDatabase {
        name: String,
        admin_url: String,
        pool: PgPool,
    }

    impl DisposableDatabase {
        async fn create() -> anyhow::Result<Self> {
            use anyhow::Context as _;

            let settings = crate::app::config::Settings::from_env()
                .context("load settings for graph gc test")?;
            let mut admin_url = url::Url::parse(&settings.database_url)
                .context("parse database url for graph gc test")?;
            admin_url.set_path("/postgres");
            let admin_url = admin_url.to_string();
            let name = format!("graph_gc_test_{}", Uuid::now_v7().simple());

            let admin_pool = sqlx::postgres::PgPoolOptions::new()
                .max_connections(1)
                .connect(&admin_url)
                .await
                .context("connect graph gc test admin postgres")?;
            sqlx::query(sqlx::AssertSqlSafe(format!("create database \"{name}\"")))
                .execute(&admin_pool)
                .await
                .context("create graph gc test database")?;
            admin_pool.close().await;

            let mut database_url = url::Url::parse(&settings.database_url)
                .context("parse database url for graph gc test")?;
            database_url.set_path(&format!("/{name}"));
            let pool = sqlx::postgres::PgPoolOptions::new()
                .max_connections(2)
                .connect(database_url.as_str())
                .await
                .context("connect graph gc test database")?;
            sqlx::migrate!("./migrations")
                .run(&pool)
                .await
                .context("apply migrations for graph gc test")?;

            Ok(Self { name, admin_url, pool })
        }

        async fn drop(self) -> anyhow::Result<()> {
            use anyhow::Context as _;

            self.pool.close().await;
            let admin_pool = sqlx::postgres::PgPoolOptions::new()
                .max_connections(1)
                .connect(&self.admin_url)
                .await
                .context("reconnect graph gc test admin postgres for cleanup")?;
            sqlx::query(
                "select pg_terminate_backend(pid)
                 from pg_stat_activity
                 where datname = $1
                   and pid <> pg_backend_pid()",
            )
            .bind(&self.name)
            .execute(&admin_pool)
            .await
            .context("terminate graph gc test database connections")?;
            sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{}\"", self.name)))
                .execute(&admin_pool)
                .await
                .context("drop graph gc test database")?;
            Ok(())
        }
    }

    #[tokio::test]
    #[ignore = "requires local postgres service"]
    async fn run_gc_postgres_reports_zero_effect_for_an_idle_empty_library() -> anyhow::Result<()> {
        use anyhow::Context as _;

        let db = DisposableDatabase::create().await?;
        let postgres = db.pool.clone();
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = repositories::catalog_repository::create_workspace(
            &postgres,
            &format!("graph-gc-workspace-{suffix}"),
            "Graph GC Workspace",
            None,
        )
        .await
        .context("failed to create graph gc workspace")?;
        let library = repositories::catalog_repository::create_library(
            &postgres,
            workspace.id,
            &format!("graph-gc-library-{suffix}"),
            "Graph GC Library",
            None,
            None,
        )
        .await
        .context("failed to create graph gc library")?;

        let report = run_gc_postgres(library.id, &postgres)
            .await
            .context("expected an idle empty library to GC cleanly")?;
        assert_eq!(report.entities_deleted, 0);
        assert_eq!(report.relations_deleted, 0);
        assert_eq!(report.evidence_deleted, 0);
        assert_eq!(report.libraries_scanned, 1);

        db.drop().await?;
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires local postgres service"]
    async fn run_gc_postgres_rejects_a_library_with_an_active_ingest_job() -> anyhow::Result<()> {
        use anyhow::Context as _;

        let db = DisposableDatabase::create().await?;
        let postgres = db.pool.clone();
        let suffix = Uuid::now_v7().simple().to_string();
        let workspace = repositories::catalog_repository::create_workspace(
            &postgres,
            &format!("graph-gc-blocked-workspace-{suffix}"),
            "Graph GC Blocked Workspace",
            None,
        )
        .await
        .context("failed to create graph gc blocked workspace")?;
        let library = repositories::catalog_repository::create_library(
            &postgres,
            workspace.id,
            &format!("graph-gc-blocked-library-{suffix}"),
            "Graph GC Blocked Library",
            None,
            None,
        )
        .await
        .context("failed to create graph gc blocked library")?;

        let job_id = Uuid::now_v7();
        sqlx::query(
            "insert into ingest_job (
                id, workspace_id, library_id, job_kind, queue_state, priority,
                queued_at, available_at
             ) values ($1, $2, $3, 'content_mutation', 'queued', 100, now(), now())",
        )
        .bind(job_id)
        .bind(workspace.id)
        .bind(library.id)
        .execute(&postgres)
        .await
        .context("failed to insert active ingest job")?;

        let error = run_gc_postgres(library.id, &postgres)
            .await
            .expect_err("expected GC to reject a library with an active ingest job");
        assert!(matches!(
            error,
            GraphGcError::ActiveIngest { library_id, active_jobs: 1 } if library_id == library.id
        ));

        db.drop().await?;
        Ok(())
    }
}
