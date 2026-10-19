//! PostgreSQL maintenance sweepers for derived retrieval state.
//!
//! `gc.superseded-vectors` removes chunk vectors whose revision is neither
//! readable nor the active revision owned by an exact leased content-mutation
//! job/latest attempt. Source revisions, content chunks and knowledge chunks
//! remain available for history, citations and bounded structural expansion.
//!
//! `gc.historical-projection` is a separate class: it purges the historical
//! knowledge-plane revisions a document no longer retains under the shared
//! retention window, leaving the current projection and the authoritative
//! source plane untouched. It shares no transaction or guard with the vector
//! sweeper and the runtime-graph sweeper.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use thiserror::Error;
use tracing::warn;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::content::HISTORICAL_PROJECTION_RETENTION_REVISIONS,
    infra::{
        postgres::pg_search_store::sweep_superseded_chunk_vectors,
        repositories::{
            self, catalog_repository, content_knowledge_projection_repository, ingest_repository,
        },
    },
    services::graph::gc as graph_gc,
};

/// Aggregate of one `gc.superseded-vectors` pass.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SupersededVectorGcReport {
    pub documents_visited: i64,
    pub documents_with_superseded_vectors: i64,
    pub superseded_vectors_removed: i64,
    pub null_head_docs_total: i64,
    pub null_head_docs_processed: i64,
    pub null_head_vectors_removed: i64,
    pub work_remaining: bool,
    #[serde(skip)]
    pub(crate) next_cursor_json: Option<serde_json::Value>,
}

impl SupersededVectorGcReport {
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        Self {
            documents_visited: self.documents_visited + other.documents_visited,
            documents_with_superseded_vectors: self.documents_with_superseded_vectors
                + other.documents_with_superseded_vectors,
            superseded_vectors_removed: self.superseded_vectors_removed
                + other.superseded_vectors_removed,
            null_head_docs_total: self.null_head_docs_total + other.null_head_docs_total,
            null_head_docs_processed: self.null_head_docs_processed
                + other.null_head_docs_processed,
            null_head_vectors_removed: self.null_head_vectors_removed
                + other.null_head_vectors_removed,
            work_remaining: self.work_remaining || other.work_remaining,
            next_cursor_json: None,
        }
    }

    #[must_use]
    pub const fn total_rows_removed(&self) -> i64 {
        self.superseded_vectors_removed + self.null_head_vectors_removed
    }
}

#[cfg(feature = "test-support")]
impl SupersededVectorGcReport {
    #[must_use]
    pub fn next_cursor_for_test(&self) -> Option<&serde_json::Value> {
        self.next_cursor_json.as_ref()
    }
}

/// Options for a single sweep.
#[derive(Debug, Default, Clone, Copy)]
pub struct GcSupersededVectorsOptions {
    /// When `true` the sweeper only counts what it would remove and
    /// reports it back without issuing destructive deletes.
    pub dry_run: bool,
    /// Include vectors belonging to documents with neither readable nor active
    /// head. Off by default because those documents may still be recoverable.
    pub include_null_head: bool,
    /// Maximum committed delete batches in this invocation. `None` drains the
    /// backlog; the scheduler uses one batch so foreground query load gets a
    /// scheduling window between maintenance transactions.
    pub max_batches: Option<u32>,
}

#[derive(Debug, Error)]
pub enum GcError {
    #[error("library {library_id} has {active_jobs} active ingest jobs; maintenance gc refused")]
    ActiveIngest { library_id: Uuid, active_jobs: i64 },
    #[error("library {library_id} catalog row is busy; maintenance gc deferred")]
    LibraryBusy { library_id: Uuid },
    #[error("postgres error during maintenance gc: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("vector-plane error during maintenance gc: {0}")]
    Vector(#[source] anyhow::Error),
    #[error("runtime-graph error during maintenance gc: {0}")]
    Graph(#[source] graph_gc::GraphGcError),
    #[error("runtime graph maintenance cursor is invalid: {0}")]
    RuntimeGraphCursor(#[source] serde_json::Error),
    #[error(
        "runtime graph maintenance cursor schema version {found} is not supported (expected {expected})"
    )]
    UnsupportedRuntimeGraphCursorVersion { found: u8, expected: u8 },
    #[error("runtime graph physical delete mutation budget must be greater than zero, got {0}")]
    InvalidPhysicalMutationBudget(i64),
}

impl GcError {
    /// Stable error code for lease-table failures and
    /// Prometheus labels.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ActiveIngest { .. } => "active_ingest",
            Self::LibraryBusy { .. } => "library_busy",
            Self::Sqlx(_) => "postgres",
            Self::Vector(_) => "vector_plane",
            Self::Graph(graph_gc::GraphGcError::OversizedClosure { .. }) => {
                "runtime_graph_oversized_closure"
            }
            Self::Graph(graph_gc::GraphGcError::PhysicalMutationBudgetOverrun { .. }) => {
                "runtime_graph_budget_overrun"
            }
            Self::Graph(graph_gc::GraphGcError::LibraryBusy { .. }) => "library_busy",
            Self::Graph(_) => "runtime_graph",
            Self::RuntimeGraphCursor(_) => "runtime_graph_cursor",
            Self::UnsupportedRuntimeGraphCursorVersion { .. } => "runtime_graph_cursor_version",
            Self::InvalidPhysicalMutationBudget(_) => "runtime_graph_budget",
        }
    }
}

/// Logical root counts from the unbounded operator/service full drain.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RuntimeGraphGcReport {
    pub nodes_removed: i64,
    pub edges_removed: i64,
    pub evidence_removed: i64,
}

impl RuntimeGraphGcReport {
    #[must_use]
    pub const fn total_rows_removed(self) -> i64 {
        self.nodes_removed + self.edges_removed + self.evidence_removed
    }
}

/// Exact counters for one scheduler-bounded runtime-graph transaction.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RuntimeGraphGcSchedulerReport {
    pub candidate_roots: i64,
    pub nodes_removed: i64,
    pub edges_removed: i64,
    pub evidence_removed: i64,
    pub dependent_rows_removed: i64,
    pub physical_rows_removed: i64,
    pub work_remaining: bool,
    #[serde(skip)]
    pub(crate) next_cursor_json: Option<serde_json::Value>,
}

impl RuntimeGraphGcSchedulerReport {
    #[must_use]
    pub const fn total_rows_removed(&self) -> i64 {
        self.physical_rows_removed
    }
}

#[cfg(feature = "test-support")]
impl RuntimeGraphGcSchedulerReport {
    #[must_use]
    pub fn next_cursor_for_test(&self) -> Option<&serde_json::Value> {
        self.next_cursor_json.as_ref()
    }
}

/// A scheduler tick commits at most this many physical delete mutations. Query
/// planning, index reads and row-lock waits are deliberately outside this unit.
pub(crate) const RUNTIME_GRAPH_DELETE_MUTATION_BUDGET: i64 = 10_000;

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RuntimeGraphGcPhase {
    #[default]
    StaleRevisionEvidence,
    PhantomChunkEvidence,
    OrphanEvidenceBefore,
    ZombieEdges,
    ZombieNodes,
    OrphanEvidenceAfter,
}

impl RuntimeGraphGcPhase {
    const fn next(self) -> Option<Self> {
        match self {
            Self::StaleRevisionEvidence => Some(Self::PhantomChunkEvidence),
            Self::PhantomChunkEvidence => Some(Self::OrphanEvidenceBefore),
            Self::OrphanEvidenceBefore => Some(Self::ZombieEdges),
            Self::ZombieEdges => Some(Self::ZombieNodes),
            Self::ZombieNodes => Some(Self::OrphanEvidenceAfter),
            Self::OrphanEvidenceAfter => None,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RuntimeGraphGcCursor {
    version: u8,
    #[serde(default)]
    phase: RuntimeGraphGcPhase,
}

impl Default for RuntimeGraphGcCursor {
    fn default() -> Self {
        Self { version: runtime_graph_cursor_version(), phase: RuntimeGraphGcPhase::default() }
    }
}

const fn runtime_graph_cursor_version() -> u8 {
    1
}

async fn acquire_runtime_graph_gc_guard(
    pool: &PgPool,
    library_id: Uuid,
) -> Result<Option<repositories::RuntimeGraphGcQuiescenceGuard>, GcError> {
    match repositories::acquire_runtime_graph_gc_quiescence_guard(pool, library_id).await? {
        repositories::RuntimeGraphGcQuiescenceAcquireOutcome::Acquired(guard) => Ok(Some(guard)),
        repositories::RuntimeGraphGcQuiescenceAcquireOutcome::LibraryNotFound => Ok(None),
        repositories::RuntimeGraphGcQuiescenceAcquireOutcome::LibraryBusy => {
            Err(GcError::LibraryBusy { library_id })
        }
        repositories::RuntimeGraphGcQuiescenceAcquireOutcome::ActiveIngest { active_jobs } => {
            Err(GcError::ActiveIngest { library_id, active_jobs })
        }
    }
}

async fn finish_runtime_graph_gc_guard<T>(
    guard: repositories::RuntimeGraphGcQuiescenceGuard,
    library_id: Uuid,
    outcome: Result<T, GcError>,
) -> Result<T, GcError> {
    match outcome {
        Ok(value) => {
            guard.commit().await?;
            Ok(value)
        }
        Err(error) => {
            if let Err(rollback_error) = guard.rollback().await {
                tracing::error!(%library_id, ?rollback_error, "runtime graph GC rollback failed");
            }
            Err(error)
        }
    }
}

/// Preserve the pre-split automatic graph/evidence maintenance behavior under
/// one explicit graph-plane class, independent from chunk-vector GC.
pub async fn run_runtime_graph(
    state: &AppState,
    library_id: Uuid,
) -> Result<RuntimeGraphGcReport, GcError> {
    let pool = &state.persistence.postgres;
    let Some(mut guard) = acquire_runtime_graph_gc_guard(pool, library_id).await? else {
        return Ok(RuntimeGraphGcReport::default());
    };
    let outcome = async {
        let stale_evidence =
            run_stale_evidence_transaction(library_id, guard.transaction_mut()).await?;
        let graph = graph_gc::run_gc_postgres_transaction(library_id, guard.transaction_mut())
            .await
            .map_err(|error| match error {
                graph_gc::GraphGcError::ActiveIngest { library_id, active_jobs } => {
                    GcError::ActiveIngest { library_id, active_jobs }
                }
                error => GcError::Graph(error),
            })?;
        let nodes_removed = i64::from(graph.entities_deleted);
        let edges_removed = i64::from(graph.relations_deleted);
        let evidence_removed =
            stale_evidence.total_rows_removed() + i64::from(graph.evidence_deleted);
        Ok(RuntimeGraphGcReport { nodes_removed, edges_removed, evidence_removed })
    }
    .await;
    finish_runtime_graph_gc_guard(guard, library_id, outcome).await
}

fn decode_runtime_graph_cursor(
    cursor_json: Option<&serde_json::Value>,
) -> Result<RuntimeGraphGcCursor, GcError> {
    let Some(value) =
        cursor_json.filter(|value| !value.as_object().is_some_and(serde_json::Map::is_empty))
    else {
        return Ok(RuntimeGraphGcCursor::default());
    };
    let cursor: RuntimeGraphGcCursor =
        serde_json::from_value(value.clone()).map_err(GcError::RuntimeGraphCursor)?;
    // A version this code does not recognize must fail the dispatch, not
    // silently restart from phase one. Restarting hides the mismatch behind
    // what looks like ordinary backlog and can replay already-completed
    // phases against a cursor whose `phase` values may not even mean the
    // same thing under the unrecognized schema.
    if cursor.version == runtime_graph_cursor_version() {
        Ok(cursor)
    } else {
        Err(GcError::UnsupportedRuntimeGraphCursorVersion {
            found: cursor.version,
            expected: runtime_graph_cursor_version(),
        })
    }
}

fn runtime_graph_cursor_after_batch(
    cursor: &RuntimeGraphGcCursor,
    phase_work_remaining: bool,
) -> Result<(bool, serde_json::Value), GcError> {
    if phase_work_remaining {
        return Ok((true, serde_json::to_value(cursor).map_err(GcError::RuntimeGraphCursor)?));
    }
    if let Some(next_phase) = cursor.phase.next() {
        return Ok((
            true,
            serde_json::to_value(RuntimeGraphGcCursor {
                version: runtime_graph_cursor_version(),
                phase: next_phase,
            })
            .map_err(GcError::RuntimeGraphCursor)?,
        ));
    }
    Ok((false, serde_json::json!({})))
}

async fn delete_evidence_closure_batch(
    library_id: Uuid,
    phase: RuntimeGraphGcPhase,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    budget: i64,
) -> Result<graph_gc::PhysicalDeletionBatchReport, GcError> {
    let candidate_sql = match phase {
        RuntimeGraphGcPhase::StaleRevisionEvidence => {
            "select evidence.id
             from runtime_graph_evidence evidence
             join content_document_head head on head.document_id = evidence.document_id
             where evidence.library_id = $1
               and evidence.revision_id is not null
               and evidence.revision_id not in (
                   coalesce(head.readable_revision_id, '00000000-0000-0000-0000-000000000000'::uuid),
                   coalesce(head.active_revision_id, '00000000-0000-0000-0000-000000000000'::uuid)
               )
               and not exists (
                   select 1 from ingest_job job
                   where job.library_id = evidence.library_id
                     and job.knowledge_document_id = evidence.document_id
                     and job.queue_state in ('queued', 'leased', 'paused')
                     and job.completed_at is null
               )
             order by evidence.id
             limit $2
             for update of evidence"
        }
        RuntimeGraphGcPhase::PhantomChunkEvidence => {
            "select evidence.id
             from runtime_graph_evidence evidence
             where evidence.library_id = $1
               and evidence.chunk_id is not null
               and not exists (select 1 from content_chunk chunk where chunk.id = evidence.chunk_id)
             order by evidence.id
             limit $2
             for update of evidence"
        }
        RuntimeGraphGcPhase::OrphanEvidenceBefore
        | RuntimeGraphGcPhase::OrphanEvidenceAfter => {
            "select evidence.id
             from runtime_graph_evidence evidence
             where evidence.library_id = $1
               and (
                   (evidence.target_kind = 'node' and not exists (
                       select 1 from runtime_graph_node node
                       where node.library_id = evidence.library_id and node.id = evidence.target_id
                   ))
                   or (evidence.target_kind = 'edge' and not exists (
                       select 1 from runtime_graph_edge edge
                       where edge.library_id = evidence.library_id and edge.id = evidence.target_id
                   ))
               )
             order by evidence.id
             limit $2
             for update of evidence"
        }
        RuntimeGraphGcPhase::ZombieEdges | RuntimeGraphGcPhase::ZombieNodes => {
            unreachable!("graph roots use graph-owned closure sweepers")
        }
    };
    let candidate_ids = sqlx::query_scalar::<_, Uuid>(candidate_sql)
        .bind(library_id)
        .bind(budget)
        .fetch_all(&mut **tx)
        .await?;
    if candidate_ids.is_empty() {
        return Ok(graph_gc::PhysicalDeletionBatchReport::default());
    }
    let costs = sqlx::query_as::<_, (Uuid, i64)>(
        "select evidence.id,
                1 + (select count(*) from knowledge_bundle_evidence ref
                     where ref.evidence_id = evidence.id)::bigint as physical_rows
         from runtime_graph_evidence evidence
         where evidence.library_id = $1 and evidence.id = any($2)
         order by evidence.id",
    )
    .bind(library_id)
    .bind(&candidate_ids)
    .fetch_all(&mut **tx)
    .await?;
    let selected = graph_gc::budgeted_root_prefix("runtime graph evidence", &costs, budget)
        .map_err(GcError::Graph)?;
    let candidate_roots = i64::try_from(candidate_ids.len()).unwrap_or(i64::MAX);
    let work_remaining = candidate_roots >= budget || selected.len() < candidate_ids.len();
    let dependent =
        sqlx::query("delete from knowledge_bundle_evidence where evidence_id = any($1)")
            .bind(&selected)
            .execute(&mut **tx)
            .await?
            .rows_affected();
    let evidence =
        sqlx::query("delete from runtime_graph_evidence where library_id = $1 and id = any($2)")
            .bind(library_id)
            .bind(&selected)
            .execute(&mut **tx)
            .await?
            .rows_affected();
    let physical = dependent.saturating_add(evidence);
    let physical_i64 = i64::try_from(physical).unwrap_or(i64::MAX);
    if physical_i64 > budget {
        return Err(GcError::Graph(graph_gc::GraphGcError::PhysicalMutationBudgetOverrun {
            physical_rows: physical_i64,
            budget,
        }));
    }
    Ok(graph_gc::PhysicalDeletionBatchReport {
        candidate_roots,
        evidence_deleted: i64::try_from(evidence).unwrap_or(i64::MAX),
        dependent_rows_deleted: i64::try_from(dependent).unwrap_or(i64::MAX),
        physical_rows_deleted: physical_i64,
        work_remaining,
        ..graph_gc::PhysicalDeletionBatchReport::default()
    })
}

/// Executes one cursor phase for the scheduler-owned `gc.runtime-graph` class.
/// The budget counts committed DELETE mutations, including bundle-reference
/// dependents, but intentionally does not claim to cap reads, planning or waits.
/// Database mutation and lease-cursor persistence are at-least-once rather than
/// atomic: losing lease ownership after commit replays the same phase, whose
/// predicates and complete-closure deletes are idempotent.
pub(crate) async fn run_runtime_graph_scheduler_batch(
    state: &AppState,
    library_id: Uuid,
    cursor_json: Option<&serde_json::Value>,
) -> Result<RuntimeGraphGcSchedulerReport, GcError> {
    run_runtime_graph_scheduler_batch_with_budget(
        state,
        library_id,
        cursor_json,
        RUNTIME_GRAPH_DELETE_MUTATION_BUDGET,
    )
    .await
}

#[cfg(feature = "test-support")]
pub async fn run_runtime_graph_scheduler_batch_with_budget_for_test(
    state: &AppState,
    library_id: Uuid,
    cursor_json: Option<&serde_json::Value>,
    physical_delete_mutation_budget: i64,
) -> Result<RuntimeGraphGcSchedulerReport, GcError> {
    run_runtime_graph_scheduler_batch_with_budget(
        state,
        library_id,
        cursor_json,
        physical_delete_mutation_budget,
    )
    .await
}

async fn run_runtime_graph_scheduler_batch_with_budget(
    state: &AppState,
    library_id: Uuid,
    cursor_json: Option<&serde_json::Value>,
    physical_delete_mutation_budget: i64,
) -> Result<RuntimeGraphGcSchedulerReport, GcError> {
    if physical_delete_mutation_budget <= 0 {
        return Err(GcError::InvalidPhysicalMutationBudget(physical_delete_mutation_budget));
    }
    let cursor = decode_runtime_graph_cursor(cursor_json)?;
    let pool = &state.persistence.postgres;
    let Some(mut guard) = acquire_runtime_graph_gc_guard(pool, library_id).await? else {
        return Ok(RuntimeGraphGcSchedulerReport::default());
    };
    let outcome = async {
        let batch = match cursor.phase {
            RuntimeGraphGcPhase::StaleRevisionEvidence
            | RuntimeGraphGcPhase::PhantomChunkEvidence
            | RuntimeGraphGcPhase::OrphanEvidenceBefore
            | RuntimeGraphGcPhase::OrphanEvidenceAfter => {
                delete_evidence_closure_batch(
                    library_id,
                    cursor.phase,
                    guard.transaction_mut(),
                    physical_delete_mutation_budget,
                )
                .await?
            }
            RuntimeGraphGcPhase::ZombieEdges => graph_gc::delete_zombie_edge_closure_batch(
                library_id,
                guard.transaction_mut(),
                physical_delete_mutation_budget,
            )
            .await
            .map_err(GcError::Graph)?,
            RuntimeGraphGcPhase::ZombieNodes => graph_gc::delete_zombie_node_closure_batch(
                library_id,
                guard.transaction_mut(),
                physical_delete_mutation_budget,
            )
            .await
            .map_err(GcError::Graph)?,
        };
        let (work_remaining, next_cursor_json) =
            runtime_graph_cursor_after_batch(&cursor, batch.work_remaining)?;
        Ok(RuntimeGraphGcSchedulerReport {
            candidate_roots: batch.candidate_roots,
            nodes_removed: batch.nodes_deleted,
            edges_removed: batch.edges_deleted,
            evidence_removed: batch.evidence_deleted,
            dependent_rows_removed: batch.dependent_rows_deleted,
            physical_rows_removed: batch.physical_rows_deleted,
            work_remaining,
            next_cursor_json: Some(next_cursor_json),
        })
    }
    .await;
    finish_runtime_graph_gc_guard(guard, library_id, outcome).await
}

/// Run `gc.superseded-vectors` against one library.
pub async fn run_superseded_vectors_for_library(
    state: &AppState,
    _workspace_id: Uuid,
    library_id: Uuid,
    options: GcSupersededVectorsOptions,
) -> Result<SupersededVectorGcReport, GcError> {
    run_superseded_vectors_for_library_with_cursor(state, _workspace_id, library_id, options, None)
        .await
}

pub(crate) async fn run_superseded_vectors_for_library_with_cursor(
    state: &AppState,
    _workspace_id: Uuid,
    library_id: Uuid,
    options: GcSupersededVectorsOptions,
    cursor_json: Option<&serde_json::Value>,
) -> Result<SupersededVectorGcReport, GcError> {
    let lock =
        repositories::acquire_runtime_library_graph_lock(&state.persistence.postgres, library_id)
            .await?;
    let outcome = async {
        // Vector GC is intentionally outside the runtime-graph quiescence
        // guard: each vector batch owns and advances the catalog source-truth
        // row in its own transaction, so holding that row from this connection
        // would self-block. Its existing vector/source fence remains canonical.
        ensure_no_active_ingest(&state.persistence.postgres, library_id).await?;
        let vector_report = sweep_superseded_chunk_vectors(
            &state.persistence.postgres,
            library_id,
            options.dry_run,
            options.include_null_head,
            options.max_batches,
            cursor_json,
        )
        .await
        .map_err(GcError::Vector)?;
        Ok(map_superseded_vector_report(vector_report, options.include_null_head))
    }
    .await;
    let release = repositories::release_runtime_library_graph_lock(lock, library_id).await;
    match (outcome, release) {
        (Ok(report), Ok(())) => Ok(report),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(release_error)) => Err(GcError::Sqlx(release_error)),
        (Err(error), Err(release_error)) => {
            tracing::error!(
                %library_id,
                ?error,
                ?release_error,
                "gc.superseded-vectors failed and advisory lock release also failed",
            );
            Err(error)
        }
    }
}

fn map_superseded_vector_report(
    vector_report: crate::infra::postgres::pg_search_store::SupersededChunkVectorSweepReport,
    include_null_head: bool,
) -> SupersededVectorGcReport {
    SupersededVectorGcReport {
        documents_visited: vector_report.documents_visited,
        documents_with_superseded_vectors: vector_report.documents_with_superseded_vectors,
        superseded_vectors_removed: vector_report.superseded_vectors,
        null_head_docs_total: vector_report.null_head_documents,
        null_head_docs_processed: if include_null_head {
            vector_report.null_head_documents
        } else {
            0
        },
        null_head_vectors_removed: vector_report.null_head_vectors,
        work_remaining: vector_report.work_remaining,
        next_cursor_json: vector_report.next_cursor_json,
    }
}

#[cfg(feature = "test-support")]
pub async fn run_superseded_vectors_without_preflight_for_test(
    state: &AppState,
    library_id: Uuid,
    options: GcSupersededVectorsOptions,
) -> Result<SupersededVectorGcReport, GcError> {
    let report = sweep_superseded_chunk_vectors(
        &state.persistence.postgres,
        library_id,
        options.dry_run,
        options.include_null_head,
        options.max_batches,
        None,
    )
    .await
    .map_err(GcError::Vector)?;
    Ok(map_superseded_vector_report(report, options.include_null_head))
}

#[cfg(feature = "test-support")]
pub async fn run_superseded_vectors_for_library_from_cursor_for_test(
    state: &AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    options: GcSupersededVectorsOptions,
    cursor_json: Option<&serde_json::Value>,
) -> Result<SupersededVectorGcReport, GcError> {
    run_superseded_vectors_for_library_with_cursor(
        state,
        workspace_id,
        library_id,
        options,
        cursor_json,
    )
    .await
}

/// Counts the ingest jobs that keep a library's revision set mutable: queued,
/// leased or paused and not yet completed. The single query behind both the
/// refusing preflight and the historical-projection window, and shared so the
/// window can count inside the library lifecycle transaction.
async fn count_active_ingest_jobs_with_executor<'e, E>(
    executor: E,
    library_id: Uuid,
) -> Result<i64, GcError>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let active_jobs: i64 = sqlx::query_scalar(
        "select count(*) from ingest_job \
         where library_id = $1 \
           and queue_state in ('queued', 'leased', 'paused') \
           and completed_at is null",
    )
    .bind(library_id)
    .fetch_one(executor)
    .await?;
    Ok(active_jobs)
}

async fn count_active_ingest_jobs(pool: &PgPool, library_id: Uuid) -> Result<i64, GcError> {
    count_active_ingest_jobs_with_executor(pool, library_id).await
}

async fn ensure_no_active_ingest(pool: &PgPool, library_id: Uuid) -> Result<(), GcError> {
    let active_jobs = count_active_ingest_jobs(pool, library_id).await?;
    if active_jobs > 0 {
        return Err(GcError::ActiveIngest { library_id, active_jobs });
    }
    Ok(())
}

/// One library's superseded-vector sweep failed, with its bounded stable error
/// code — never raw database text or identifiers beyond the library scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupersededVectorGcFailure {
    pub library_id: Uuid,
    pub error_code: &'static str,
}

/// The typed failure of a multi-library `gc.superseded-vectors` run. It carries
/// one bounded entry per failed library so a caller can observe each failure
/// separately, while the overall run still fails rather than reporting a
/// misleading aggregate success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupersededVectorGcRunError {
    pub failures: Vec<SupersededVectorGcFailure>,
}

impl std::fmt::Display for SupersededVectorGcRunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "gc.superseded-vectors failed for {} of the swept libraries",
            self.failures.len(),
        )
    }
}

impl std::error::Error for SupersededVectorGcRunError {}

/// Run `gc.superseded-vectors` across every library.
///
/// # Errors
/// Returns a [`SupersededVectorGcRunError`] carrying one bounded, typed entry
/// per failed library when any library failed. The per-library failures survive
/// the aggregate rather than collapsing into one string.
pub async fn run_superseded_vectors_for_all_libraries(
    state: &AppState,
    options: GcSupersededVectorsOptions,
) -> anyhow::Result<SupersededVectorGcReport> {
    let libraries = catalog_repository::list_libraries(&state.persistence.postgres, None).await?;
    let mut totals = SupersededVectorGcReport::default();
    let mut failures = Vec::new();
    for library in libraries {
        match run_superseded_vectors_for_library(state, library.workspace_id, library.id, options)
            .await
        {
            Ok(report) => {
                totals = totals.merge(report);
            }
            Err(GcError::ActiveIngest { active_jobs, .. }) => {
                log_superseded_vectors_skipped_active_ingest(library.id, active_jobs);
            }
            Err(error) => {
                let error_code = error.code();
                log_superseded_vectors_library_failed(library.id, error_code, &error);
                failures.push(SupersededVectorGcFailure { library_id: library.id, error_code });
            }
        }
    }
    if failures.is_empty() {
        return Ok(totals);
    }
    Err(aggregate_library_failures(failures))
}

/// Turn the per-library failures into the aggregate error. Kept pure so the
/// typed-metadata contract is unit-testable without a database.
fn aggregate_library_failures(failures: Vec<SupersededVectorGcFailure>) -> anyhow::Error {
    anyhow::Error::new(SupersededVectorGcRunError { failures })
}

fn log_superseded_vectors_skipped_active_ingest(library_id: Uuid, active_jobs: i64) {
    warn!(
        library_id = %library_id,
        active_jobs,
        "gc.superseded-vectors skipped: library has active ingest",
    );
}

fn log_superseded_vectors_library_failed(library_id: Uuid, error_code: &str, error: &GcError) {
    warn!(
        library_id = %library_id,
        error_code,
        ?error,
        "gc.superseded-vectors failed; continuing with next library",
    );
}

// ============================================================================
// gc.historical-projection — knowledge-plane revision-window purge
// ============================================================================

/// One stored revision candidate offered to the historical-projection planner.
#[derive(Debug, Clone, Copy)]
pub struct HistoricalProjectionRevisionCandidate<'a> {
    pub revision_id: Uuid,
    pub document_id: Uuid,
    pub revision_number: i64,
    pub text_state: &'a str,
    pub preparation_state: &'a str,
}

/// The stored revision state of one document, as the planner sees it.
#[derive(Debug, Clone, Copy)]
pub struct HistoricalProjectionDocumentInput<'a> {
    pub document_id: Uuid,
    pub current_revision_id: Option<Uuid>,
    pub active_revision_id: Option<Uuid>,
    pub revisions: &'a [HistoricalProjectionRevisionCandidate<'a>],
}

/// The purge decision for one document under the shared retention window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoricalProjectionRetentionPlan {
    pub document_id: Uuid,
    pub refused_for_active_ingest: bool,
    pub retained_revision_ids: Vec<Uuid>,
    pub purgeable_revision_ids: Vec<Uuid>,
}

/// Aggregate of one `gc.historical-projection` pass.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HistoricalProjectionRetentionGcReport {
    pub documents_visited: u64,
    pub documents_purged: u64,
    pub knowledge_revisions_removed: u64,
    pub chunk_vector_rows_removed: u64,
    pub graph_evidence_rows_removed: u64,
    pub graph_rebuilds_completed: u64,
    pub graph_rebuilds_skipped: u64,
    /// `true` when this pass was a dry run. Nothing is deleted then:
    /// `documents_purged` and `knowledge_revisions_removed` are the planned
    /// totals, and the vector and graph-evidence counters are not queried and
    /// stay `0`.
    pub planned: bool,
}

impl HistoricalProjectionRetentionGcReport {
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        Self {
            documents_visited: self.documents_visited + other.documents_visited,
            documents_purged: self.documents_purged + other.documents_purged,
            knowledge_revisions_removed: self.knowledge_revisions_removed
                + other.knowledge_revisions_removed,
            chunk_vector_rows_removed: self.chunk_vector_rows_removed
                + other.chunk_vector_rows_removed,
            graph_evidence_rows_removed: self.graph_evidence_rows_removed
                + other.graph_evidence_rows_removed,
            graph_rebuilds_completed: self.graph_rebuilds_completed
                + other.graph_rebuilds_completed,
            graph_rebuilds_skipped: self.graph_rebuilds_skipped + other.graph_rebuilds_skipped,
            planned: self.planned || other.planned,
        }
    }

    #[must_use]
    pub const fn total_rows_removed(&self) -> u64 {
        self.knowledge_revisions_removed
            + self.chunk_vector_rows_removed
            + self.graph_evidence_rows_removed
    }
}

/// Decides which revisions of one document survive the shared retention window.
///
/// Pure and deterministic so a purge only ever reaches SQL with a decision that
/// can be reasoned about and replayed. An active ingest anywhere in the library
/// refuses every document; a document with no current revision is left alone;
/// otherwise the current revision and the newest readable revisions fill the
/// window, the active revision is force-retained even when it is not readable,
/// and every candidate outside that set is purgeable.
#[must_use]
pub fn plan_historical_projection_retention(
    library_active_ingest_jobs: i64,
    document: &HistoricalProjectionDocumentInput<'_>,
    limit: usize,
) -> HistoricalProjectionRetentionPlan {
    let candidate_ids =
        document.revisions.iter().map(|revision| revision.revision_id).collect::<Vec<_>>();
    if library_active_ingest_jobs > 0 {
        return HistoricalProjectionRetentionPlan {
            document_id: document.document_id,
            refused_for_active_ingest: true,
            retained_revision_ids: candidate_ids,
            purgeable_revision_ids: Vec::new(),
        };
    }
    let Some(current_revision_id) = document.current_revision_id else {
        return HistoricalProjectionRetentionPlan {
            document_id: document.document_id,
            refused_for_active_ingest: false,
            retained_revision_ids: candidate_ids,
            purgeable_revision_ids: Vec::new(),
        };
    };

    let mut ordered = document.revisions.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        right
            .revision_number
            .cmp(&left.revision_number)
            .then_with(|| right.revision_id.cmp(&left.revision_id))
    });
    let candidates = ordered
        .iter()
        .filter(|revision| revision.preparation_state == "prepared")
        .map(|revision| crate::domains::content::HistoricalProjectionRevision {
            revision_id: revision.revision_id,
            document_id: revision.document_id,
            text_state: revision.text_state,
        })
        .collect::<Vec<_>>();
    let mut retained_revision_ids =
        crate::domains::content::historical_projection_retained_revision_ids(
            document.document_id,
            current_revision_id,
            &candidates,
            limit,
        );
    if let Some(active_revision_id) = document.active_revision_id
        && !retained_revision_ids.contains(&active_revision_id)
    {
        retained_revision_ids.push(active_revision_id);
    }
    let retained = retained_revision_ids.iter().copied().collect::<std::collections::BTreeSet<_>>();
    let purgeable_revision_ids =
        candidate_ids.into_iter().filter(|revision_id| !retained.contains(revision_id)).collect();

    HistoricalProjectionRetentionPlan {
        document_id: document.document_id,
        refused_for_active_ingest: false,
        retained_revision_ids,
        purgeable_revision_ids,
    }
}

/// The stored head state of one knowledge document for the retention planner.
struct HistoricalProjectionDocumentRow {
    document_id: Uuid,
    current_revision_id: Option<Uuid>,
    active_revision_id: Option<Uuid>,
}

/// Loads every knowledge document of a library with the head pointers the
/// planner decides on. `readable_revision_id` falls back to
/// `active_revision_id` because that is the head a document without readable
/// text still projects. Runs on the caller's lifecycle transaction so the plan
/// is read under the same lock the purge commits under.
async fn load_historical_projection_documents(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    library_id: Uuid,
) -> Result<Vec<HistoricalProjectionDocumentRow>, GcError> {
    let rows = sqlx::query_as::<_, (Uuid, Option<Uuid>, Option<Uuid>)>(
        "select document_id,
                coalesce(readable_revision_id, active_revision_id) as current_revision_id,
                active_revision_id
         from knowledge_document
         where library_id = $1",
    )
    .bind(library_id)
    .fetch_all(&mut **transaction)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(document_id, current_revision_id, active_revision_id)| {
            HistoricalProjectionDocumentRow { document_id, current_revision_id, active_revision_id }
        })
        .collect())
}

/// Plans the historical-projection window for every document of one library.
///
/// Reads each document's revisions through the document store under the
/// caller's lifecycle transaction. `active_jobs` is threaded into the pure
/// planner, so an active ingest refuses every document without this helper
/// needing a branch of its own.
async fn plan_historical_projection_window(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    active_jobs: i64,
    documents: &[HistoricalProjectionDocumentRow],
) -> Result<Vec<HistoricalProjectionRetentionPlan>, GcError> {
    let mut plans = Vec::with_capacity(documents.len());
    for document in documents {
        let revisions = sqlx::query_as::<_, (Uuid, Uuid, i64, String, Option<String>)>(
            "select revision.revision_id, revision.document_id, revision.revision_number,
                    revision.text_state, structured.preparation_state
             from knowledge_revision revision
             left join knowledge_structured_revision structured
               on structured.revision_id = revision.revision_id
             where revision.document_id = $1",
        )
        .bind(document.document_id)
        .fetch_all(&mut **transaction)
        .await?;
        let candidates = revisions
            .iter()
            .map(|(revision_id, document_id, revision_number, text_state, preparation_state)| {
                HistoricalProjectionRevisionCandidate {
                    revision_id: *revision_id,
                    document_id: *document_id,
                    revision_number: *revision_number,
                    text_state,
                    preparation_state: preparation_state.as_deref().unwrap_or("pending"),
                }
            })
            .collect::<Vec<_>>();
        let input = HistoricalProjectionDocumentInput {
            document_id: document.document_id,
            current_revision_id: document.current_revision_id,
            active_revision_id: document.active_revision_id,
            revisions: &candidates,
        };
        plans.push(plan_historical_projection_retention(
            active_jobs,
            &input,
            HISTORICAL_PROJECTION_RETENTION_REVISIONS,
        ));
    }
    Ok(plans)
}

/// Adds a dry run's planned totals to the report. Nothing is deleted on a dry
/// run, so only the plan's own counts are reported.
fn accumulate_planned_historical_projection_totals(
    report: &mut HistoricalProjectionRetentionGcReport,
    plans: &[HistoricalProjectionRetentionPlan],
) {
    for plan in plans {
        if plan.purgeable_revision_ids.is_empty() {
            continue;
        }
        report.documents_purged += 1;
        report.knowledge_revisions_removed +=
            u64::try_from(plan.purgeable_revision_ids.len()).unwrap_or(u64::MAX);
    }
}

/// Purges every planned revision set on the caller's lifecycle transaction.
///
/// Returns whether at least one revision was actually removed. The purge
/// returns exactly the revisions it removed, so a document whose guard admitted
/// nothing purged nothing and is not counted, even if a stale plan listed
/// candidates.
async fn purge_planned_historical_projection_revisions(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    library_id: Uuid,
    plans: &[HistoricalProjectionRetentionPlan],
    report: &mut HistoricalProjectionRetentionGcReport,
) -> Result<bool, GcError> {
    let mut purged_any = false;
    for plan in plans {
        if plan.purgeable_revision_ids.is_empty() {
            continue;
        }
        let purged =
            content_knowledge_projection_repository::purge_historical_projection_revisions_with_transaction(
                &mut *transaction,
                library_id,
                plan.document_id,
                &plan.purgeable_revision_ids,
            )
            .await?;
        if purged.knowledge_revisions == 0 {
            continue;
        }
        purged_any = true;
        report.documents_purged += 1;
        report.knowledge_revisions_removed += purged.knowledge_revisions;
        report.chunk_vector_rows_removed += purged.chunk_vector_rows;
        report.graph_evidence_rows_removed += purged.graph_evidence_rows;
    }
    Ok(purged_any)
}

/// Re-projects the runtime graph after a committed purge, best effort.
///
/// A graph failure is warned about rather than surfaced: the purge has already
/// committed, and re-running the rebuild is idempotent.
async fn rebuild_graph_after_historical_projection(
    state: &AppState,
    library_id: Uuid,
    report: &mut HistoricalProjectionRetentionGcReport,
) {
    match state.application_services.graph.rebuild_library_graph(state, library_id).await {
        Ok(_) => report.graph_rebuilds_completed += 1,
        Err(error) => {
            report.graph_rebuilds_skipped += 1;
            warn!(
                library_id = %library_id,
                ?error,
                "runtime graph re-projection failed after historical projection purge; \
                 run `rebuild runtime-graph --library` once graph state settles",
            );
        }
    }
}

/// Releases the lifecycle lock without writing and records a dry run's plan.
///
/// The transaction is consumed by the rollback. `planned` is set and the
/// planned totals are summed; the vector and graph-evidence counters stay `0`
/// because nothing was queried.
async fn finish_dry_run_historical_projection(
    transaction: sqlx::Transaction<'_, sqlx::Postgres>,
    plans: &[HistoricalProjectionRetentionPlan],
    mut report: HistoricalProjectionRetentionGcReport,
) -> Result<HistoricalProjectionRetentionGcReport, GcError> {
    transaction.rollback().await?;
    report.planned = true;
    accumulate_planned_historical_projection_totals(&mut report, plans);
    Ok(report)
}

/// Purges the planned revisions, advances source truth and commits.
///
/// The transaction is committed only when the purge touched nothing or the
/// source-truth version was advanced. Returns whether anything was purged so
/// the caller can decide whether to re-project the runtime graph.
async fn commit_historical_projection_purge(
    mut transaction: sqlx::Transaction<'_, sqlx::Postgres>,
    library_id: Uuid,
    plans: &[HistoricalProjectionRetentionPlan],
    report: &mut HistoricalProjectionRetentionGcReport,
) -> Result<bool, GcError> {
    let purged_any =
        purge_planned_historical_projection_revisions(&mut transaction, library_id, plans, report)
            .await?;
    if purged_any {
        catalog_repository::touch_library_source_truth_version_with_executor(
            &mut *transaction,
            library_id,
        )
        .await?;
    }
    transaction.commit().await?;
    Ok(purged_any)
}

/// Runs `gc.historical-projection` against one library.
///
/// The library lifecycle lock is taken first, so a concurrent lifecycle event
/// either wins first — and this pass observes `LibraryBusy` — or waits. Inside
/// that lock the active-ingest count is read in the same transaction and the
/// document heads and their revision candidates are planned against the shared
/// retention window; a library with an active ingest job returns `ActiveIngest`
/// and touches nothing. `dry_run` releases the lock without writing and returns
/// the plan totals with `planned` set. Historical revisions, their chunk
/// vectors and their runtime-graph evidence are removed together, the
/// source-truth generation advances so caches re-evaluate, and the runtime
/// graph is re-projected best-effort after commit. A graph failure is warned
/// about rather than surfaced: the purge has already committed, and re-running
/// the rebuild is idempotent.
///
/// A plan that goes stale after it is computed is not destructive: the purge SQL
/// re-applies the readable/active head and in-flight ingest guards in one
/// statement and sweeps derived planes only for the revisions that statement
/// actually removed.
pub async fn run_historical_projection_window_for_library(
    state: &AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    dry_run: bool,
) -> Result<HistoricalProjectionRetentionGcReport, GcError> {
    let pool = &state.persistence.postgres;
    let mut transaction = pool.begin().await?;
    if !catalog_repository::lock_library_for_lifecycle_event_with_executor(
        &mut *transaction,
        workspace_id,
        library_id,
    )
    .await?
    {
        transaction.rollback().await?;
        return Err(GcError::LibraryBusy { library_id });
    }
    let active_jobs = count_active_ingest_jobs_with_executor(&mut *transaction, library_id).await?;
    if active_jobs == 0 && !dry_run {
        // A failed revision may have been a head during the startup backfill.
        // Reconcile it once superseded, inside this existing library sweep.
        ingest_repository::reconcile_terminal_failed_preparations_in_transaction(
            &mut transaction,
            None,
            Some(library_id),
        )
        .await?;
    }

    let documents = load_historical_projection_documents(&mut transaction, library_id).await?;
    let mut report = HistoricalProjectionRetentionGcReport {
        documents_visited: u64::try_from(documents.len()).unwrap_or(u64::MAX),
        ..HistoricalProjectionRetentionGcReport::default()
    };
    let plans =
        plan_historical_projection_window(&mut transaction, active_jobs, &documents).await?;

    // A library with an active ingest job may still be writing the revisions
    // this pass would judge. The planner refuses every document for that count;
    // roll back the read-only transaction without writing anything.
    if active_jobs > 0 {
        transaction.rollback().await?;
        return Err(GcError::ActiveIngest { library_id, active_jobs });
    }

    if dry_run {
        return finish_dry_run_historical_projection(transaction, &plans, report).await;
    }

    let purged_any =
        commit_historical_projection_purge(transaction, library_id, &plans, &mut report).await?;
    if purged_any {
        rebuild_graph_after_historical_projection(state, library_id, &mut report).await;
    }
    Ok(report)
}

// ============================================================================
// gc.stale-evidence — PG runtime_graph_evidence sweeper
// ============================================================================

/// One-shot report for `gc.stale-evidence`.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct StaleEvidenceReport {
    /// Rows removed because their revision is not the readable/active head
    /// of the source document anymore.
    pub stale_revision_rows: i64,
    /// Rows removed because their `chunk_id` no longer exists in
    /// `content_chunk` after another lifecycle operation removed it.
    pub phantom_chunk_rows: i64,
}

impl StaleEvidenceReport {
    /// Total rows the sweeper removed, summed across lanes. Used by
    /// the scheduler so it can report a single `rows_removed` metric
    /// without needing to know the report shape.
    #[must_use]
    pub const fn total_rows_removed(self) -> i64 {
        self.stale_revision_rows + self.phantom_chunk_rows
    }
}

/// Sweep `runtime_graph_evidence` for one library.
///
/// Two lanes:
///
/// 1. **stale-revision** — `revision_id` is non-null and does not match
///    either `readable_revision_id` or `active_revision_id` of the
///    document's head. A document with neither head set (null-head)
///    is left alone — the row may still be the only record of a
///    failed ingest worth recovering.
/// 2. **phantom-chunk** — `chunk_id` is non-null but the referenced
///    chunk has been deleted. Such rows are unreachable from any live
///    retrieval path.
///
/// Both lanes skip rows whose document has an active ingest job
/// (`queue_state IN ('queued','leased','paused') AND completed_at IS
/// NULL`). The acquired graph advisory lock plus the active-ingest
/// guard prevent this sweeper from racing graph extraction.
pub async fn run_stale_evidence(
    state: &AppState,
    library_id: Uuid,
) -> Result<StaleEvidenceReport, GcError> {
    let pool = &state.persistence.postgres;
    let Some(mut guard) = acquire_runtime_graph_gc_guard(pool, library_id).await? else {
        return Ok(StaleEvidenceReport::default());
    };
    let outcome = run_stale_evidence_transaction(library_id, guard.transaction_mut()).await;
    finish_runtime_graph_gc_guard(guard, library_id, outcome).await
}

async fn run_stale_evidence_transaction(
    library_id: Uuid,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<StaleEvidenceReport, GcError> {
    let stale_revision_rows = sqlx::query_scalar::<_, i64>(
        "with deleted as ( \
             delete from runtime_graph_evidence ev \
             using content_document_head h \
             where ev.library_id = $1 \
               and ev.document_id = h.document_id \
               and ev.revision_id is not null \
               and ev.revision_id not in ( \
                   coalesce(h.readable_revision_id, '00000000-0000-0000-0000-000000000000'::uuid), \
                   coalesce(h.active_revision_id,   '00000000-0000-0000-0000-000000000000'::uuid) \
               ) \
               and not exists ( \
                   select 1 from ingest_job j \
                   where j.library_id = ev.library_id \
                     and j.knowledge_document_id = ev.document_id \
                     and j.queue_state in ('queued','leased','paused') \
                     and j.completed_at is null \
               ) \
             returning 1 \
         ) \
         select count(*)::bigint from deleted",
    )
    .bind(library_id)
    .fetch_one(&mut **tx)
    .await?;

    let phantom_chunk_rows = sqlx::query_scalar::<_, i64>(
        "with deleted as ( \
             delete from runtime_graph_evidence ev \
             where ev.library_id = $1 \
               and ev.chunk_id is not null \
               and not exists ( \
                   select 1 from content_chunk c where c.id = ev.chunk_id \
               ) \
             returning 1 \
         ) \
         select count(*)::bigint from deleted",
    )
    .bind(library_id)
    .fetch_one(&mut **tx)
    .await?;

    Ok(StaleEvidenceReport { stale_revision_rows, phantom_chunk_rows })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_merge_is_additive() {
        let a = SupersededVectorGcReport {
            documents_visited: 10,
            documents_with_superseded_vectors: 3,
            superseded_vectors_removed: 50,
            null_head_docs_total: 7,
            null_head_docs_processed: 0,
            null_head_vectors_removed: 0,
            work_remaining: false,
            next_cursor_json: None,
        };
        let b = SupersededVectorGcReport {
            documents_visited: 5,
            documents_with_superseded_vectors: 2,
            superseded_vectors_removed: 7,
            null_head_docs_total: 3,
            null_head_docs_processed: 3,
            null_head_vectors_removed: 9,
            work_remaining: true,
            next_cursor_json: None,
        };
        let merged = a.merge(b);
        assert_eq!(merged.documents_visited, 15);
        assert_eq!(merged.documents_with_superseded_vectors, 5);
        assert_eq!(merged.superseded_vectors_removed, 57);
        assert_eq!(merged.null_head_docs_total, 10);
        assert_eq!(merged.null_head_docs_processed, 3);
        assert_eq!(merged.null_head_vectors_removed, 9);
        assert!(merged.work_remaining);
    }

    #[test]
    fn total_rows_removed_sums_all_lanes() {
        let report = SupersededVectorGcReport {
            superseded_vectors_removed: 20,
            null_head_vectors_removed: 40,
            ..SupersededVectorGcReport::default()
        };
        assert_eq!(report.total_rows_removed(), 60);
    }

    #[test]
    fn runtime_graph_report_sums_every_removed_relation() {
        let report = RuntimeGraphGcSchedulerReport {
            candidate_roots: 4,
            nodes_removed: 1,
            edges_removed: 1,
            evidence_removed: 2,
            dependent_rows_removed: 6,
            physical_rows_removed: 10,
            ..RuntimeGraphGcSchedulerReport::default()
        };
        assert_eq!(report.total_rows_removed(), 10);
        assert_eq!(report.candidate_roots, 4);
        assert_eq!(report.dependent_rows_removed, 6);
    }

    #[test]
    fn runtime_graph_phase_cursor_advances_without_persisting_root_ids() {
        let mut phase = RuntimeGraphGcPhase::default();
        let mut visited = Vec::new();
        loop {
            visited.push(phase);
            let Some(next) = phase.next() else {
                break;
            };
            phase = next;
        }
        assert_eq!(visited.len(), 6);
        let cursor = serde_json::to_value(RuntimeGraphGcCursor::default()).unwrap();
        assert_eq!(cursor.get("version").and_then(serde_json::Value::as_u64), Some(1));
        assert!(cursor.get("phase").is_some());
        assert!(cursor.get("root_id").is_none());
    }

    #[test]
    fn exhausted_phase_keeps_the_same_cursor_for_at_least_once_replay() {
        let cursor = RuntimeGraphGcCursor {
            version: runtime_graph_cursor_version(),
            phase: RuntimeGraphGcPhase::ZombieEdges,
        };
        let (work_remaining, persisted) = runtime_graph_cursor_after_batch(&cursor, true).unwrap();
        assert!(work_remaining);
        let replay = decode_runtime_graph_cursor(Some(&persisted)).unwrap();
        assert_eq!(replay.phase, RuntimeGraphGcPhase::ZombieEdges);
        assert_eq!(replay.version, runtime_graph_cursor_version());
    }

    #[test]
    fn unknown_cursor_version_is_rejected_not_silently_restarted() {
        let future_cursor = serde_json::json!({"version": 2, "phase": "zombie_edges"});
        let error = decode_runtime_graph_cursor(Some(&future_cursor)).unwrap_err();
        match error {
            GcError::UnsupportedRuntimeGraphCursorVersion { found, expected } => {
                assert_eq!(found, 2);
                assert_eq!(expected, runtime_graph_cursor_version());
            }
            other => panic!("expected UnsupportedRuntimeGraphCursorVersion, got {other:?}"),
        }
        assert_eq!(error.code(), "runtime_graph_cursor_version");
    }

    #[test]
    fn cursor_missing_version_is_rejected_not_defaulted() {
        let versionless_cursor = serde_json::json!({"phase": "zombie_edges"});
        let error = decode_runtime_graph_cursor(Some(&versionless_cursor)).unwrap_err();
        assert!(
            matches!(error, GcError::RuntimeGraphCursor(_)),
            "expected a typed deserialize rejection, got {error:?}"
        );
    }

    #[test]
    fn error_codes_are_stable() {
        assert_eq!(
            GcError::ActiveIngest { library_id: Uuid::nil(), active_jobs: 2 }.code(),
            "active_ingest"
        );
        assert_eq!(GcError::LibraryBusy { library_id: Uuid::nil() }.code(), "library_busy");
    }

    #[test]
    fn multi_library_failure_keeps_each_library_bounded_and_distinct() {
        let first = Uuid::from_u128(1);
        let second = Uuid::from_u128(2);
        let error = aggregate_library_failures(vec![
            SupersededVectorGcFailure { library_id: first, error_code: "postgres" },
            SupersededVectorGcFailure { library_id: second, error_code: "vector_plane" },
        ]);
        // The per-library typed metadata must survive the aggregate: a caller
        // downcasts to observe each library's bounded code separately.
        let recovered = error
            .downcast_ref::<SupersededVectorGcRunError>()
            .expect("aggregate error must carry typed per-library failures");
        assert_eq!(recovered.failures.len(), 2);
        assert_eq!(recovered.failures[0].library_id, first);
        assert_eq!(recovered.failures[0].error_code, "postgres");
        assert_eq!(recovered.failures[1].library_id, second);
        assert_eq!(recovered.failures[1].error_code, "vector_plane");
        assert!(error.to_string().contains('2'));
        assert!(!error.to_string().contains(&first.to_string()));
    }

    const RETENTION_LIMIT: usize =
        crate::domains::content::HISTORICAL_PROJECTION_RETENTION_REVISIONS;

    fn revision_candidate(
        document_id: Uuid,
        index: u128,
        revision_number: i64,
        text_state: &'static str,
    ) -> HistoricalProjectionRevisionCandidate<'static> {
        HistoricalProjectionRevisionCandidate {
            revision_id: Uuid::from_u128(index),
            document_id,
            revision_number,
            text_state,
            preparation_state: "prepared",
        }
    }

    /// `count` revisions newest-first (revision_number `count..1`), the newest
    /// one carrying the current revision id when `count > 0`.
    fn newest_first(
        document_id: Uuid,
        count: usize,
    ) -> Vec<HistoricalProjectionRevisionCandidate<'static>> {
        (0..count)
            .map(|index| HistoricalProjectionRevisionCandidate {
                revision_id: if index == 0 {
                    CURRENT_REVISION_ID
                } else {
                    Uuid::from_u128(200 + index as u128)
                },
                document_id,
                revision_number: count as i64 - index as i64,
                text_state: "text_readable",
                preparation_state: "prepared",
            })
            .collect()
    }

    const DOCUMENT_ID: Uuid = Uuid::from_u128(1);
    const CURRENT_REVISION_ID: Uuid = Uuid::from_u128(100);

    #[test]
    fn plan_purges_failed_and_never_prepared_revisions_without_using_readable_slots() {
        let mut candidates = newest_first(DOCUMENT_ID, RETENTION_LIMIT);
        let failed_id = Uuid::from_u128(900);
        let preparing_id = Uuid::from_u128(901);
        candidates.insert(
            1,
            HistoricalProjectionRevisionCandidate {
                revision_id: failed_id,
                document_id: DOCUMENT_ID,
                revision_number: 99,
                text_state: "text_readable",
                preparation_state: "failed",
            },
        );
        candidates.insert(
            2,
            HistoricalProjectionRevisionCandidate {
                revision_id: preparing_id,
                document_id: DOCUMENT_ID,
                revision_number: 98,
                text_state: "text_readable",
                preparation_state: "preparing",
            },
        );
        let input = HistoricalProjectionDocumentInput {
            document_id: DOCUMENT_ID,
            current_revision_id: Some(CURRENT_REVISION_ID),
            active_revision_id: Some(CURRENT_REVISION_ID),
            revisions: &candidates,
        };
        let plan = plan_historical_projection_retention(0, &input, RETENTION_LIMIT);
        assert_eq!(plan.retained_revision_ids.len(), RETENTION_LIMIT);
        assert!(plan.purgeable_revision_ids.contains(&failed_id));
        assert!(plan.purgeable_revision_ids.contains(&preparing_id));
        assert!(!plan.purgeable_revision_ids.contains(&CURRENT_REVISION_ID));
    }

    #[test]
    fn plan_purges_only_revisions_outside_the_window() {
        let candidates = newest_first(DOCUMENT_ID, 9);
        let input = HistoricalProjectionDocumentInput {
            document_id: DOCUMENT_ID,
            current_revision_id: Some(CURRENT_REVISION_ID),
            active_revision_id: Some(CURRENT_REVISION_ID),
            revisions: &candidates,
        };

        let plan = plan_historical_projection_retention(0, &input, RETENTION_LIMIT);

        assert_eq!(plan.purgeable_revision_ids, vec![Uuid::from_u128(208)]);
        assert_eq!(plan.retained_revision_ids.len(), RETENTION_LIMIT);
        assert!(!plan.refused_for_active_ingest);
    }

    #[test]
    fn plan_is_empty_for_a_document_without_a_current_revision() {
        let candidates = newest_first(DOCUMENT_ID, 4);
        let input = HistoricalProjectionDocumentInput {
            document_id: DOCUMENT_ID,
            current_revision_id: None,
            active_revision_id: Some(Uuid::from_u128(201)),
            revisions: &candidates,
        };

        let plan = plan_historical_projection_retention(0, &input, RETENTION_LIMIT);

        assert_eq!(plan.purgeable_revision_ids, Vec::<Uuid>::new());
        assert_eq!(
            plan.retained_revision_ids,
            candidates.iter().map(|candidate| candidate.revision_id).collect::<Vec<_>>()
        );
        assert!(!plan.refused_for_active_ingest);
    }

    #[test]
    fn plan_retains_every_revision_when_history_fits_the_window() {
        for total in [0_usize, 1, RETENTION_LIMIT] {
            let candidates = newest_first(DOCUMENT_ID, total);
            let input = HistoricalProjectionDocumentInput {
                document_id: DOCUMENT_ID,
                current_revision_id: Some(CURRENT_REVISION_ID),
                active_revision_id: Some(CURRENT_REVISION_ID),
                revisions: &candidates,
            };

            let plan = plan_historical_projection_retention(0, &input, RETENTION_LIMIT);

            assert!(
                plan.purgeable_revision_ids.is_empty(),
                "a {total}-revision history must fit the window",
            );
        }
    }

    #[test]
    fn plan_protects_the_active_revision_even_when_unreadable() {
        let active_revision_id = Uuid::from_u128(300);
        let mut candidates = newest_first(DOCUMENT_ID, 9);
        candidates.push(revision_candidate(DOCUMENT_ID, 300, 0, "extracting"));
        let input = HistoricalProjectionDocumentInput {
            document_id: DOCUMENT_ID,
            current_revision_id: Some(CURRENT_REVISION_ID),
            active_revision_id: Some(active_revision_id),
            revisions: &candidates,
        };

        let plan = plan_historical_projection_retention(0, &input, RETENTION_LIMIT);

        assert!(plan.retained_revision_ids.contains(&active_revision_id));
        assert!(!plan.purgeable_revision_ids.contains(&active_revision_id));
    }

    #[test]
    fn plan_refuses_every_document_while_the_library_has_an_active_ingest_job() {
        let candidates = newest_first(DOCUMENT_ID, 9);
        let input = HistoricalProjectionDocumentInput {
            document_id: DOCUMENT_ID,
            current_revision_id: Some(CURRENT_REVISION_ID),
            active_revision_id: Some(CURRENT_REVISION_ID),
            revisions: &candidates,
        };

        let plan = plan_historical_projection_retention(1, &input, RETENTION_LIMIT);

        assert!(plan.refused_for_active_ingest);
        assert!(plan.purgeable_revision_ids.is_empty());
        assert_eq!(
            plan.retained_revision_ids,
            candidates.iter().map(|candidate| candidate.revision_id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn plan_is_idempotent_after_the_first_pass() {
        let candidates = newest_first(DOCUMENT_ID, 9);
        let input = HistoricalProjectionDocumentInput {
            document_id: DOCUMENT_ID,
            current_revision_id: Some(CURRENT_REVISION_ID),
            active_revision_id: Some(CURRENT_REVISION_ID),
            revisions: &candidates,
        };
        let first = plan_historical_projection_retention(0, &input, RETENTION_LIMIT);
        assert!(!first.purgeable_revision_ids.is_empty());

        let survivors = candidates
            .iter()
            .copied()
            .filter(|candidate| !first.purgeable_revision_ids.contains(&candidate.revision_id))
            .collect::<Vec<_>>();
        let second_input = HistoricalProjectionDocumentInput {
            document_id: DOCUMENT_ID,
            current_revision_id: Some(CURRENT_REVISION_ID),
            active_revision_id: Some(CURRENT_REVISION_ID),
            revisions: &survivors,
        };

        let second = plan_historical_projection_retention(0, &second_input, RETENTION_LIMIT);

        assert_eq!(second.purgeable_revision_ids, Vec::<Uuid>::new());
    }

    #[test]
    fn historical_projection_report_totals_are_additive() {
        let first = HistoricalProjectionRetentionGcReport {
            documents_visited: 3,
            documents_purged: 2,
            knowledge_revisions_removed: 5,
            chunk_vector_rows_removed: 7,
            graph_evidence_rows_removed: 2,
            graph_rebuilds_completed: 1,
            graph_rebuilds_skipped: 0,
            planned: true,
        };
        let second = HistoricalProjectionRetentionGcReport {
            documents_visited: 4,
            documents_purged: 1,
            knowledge_revisions_removed: 3,
            chunk_vector_rows_removed: 1,
            graph_evidence_rows_removed: 0,
            graph_rebuilds_completed: 0,
            graph_rebuilds_skipped: 1,
            planned: false,
        };

        let merged = first.merge(second);

        assert_eq!(merged.documents_visited, 7);
        assert_eq!(merged.documents_purged, 3);
        assert_eq!(merged.knowledge_revisions_removed, 8);
        assert_eq!(merged.chunk_vector_rows_removed, 8);
        assert_eq!(merged.graph_evidence_rows_removed, 2);
        assert_eq!(merged.graph_rebuilds_completed, 1);
        assert_eq!(merged.graph_rebuilds_skipped, 1);
        assert!(merged.planned, "a dry-run pass must keep the merged report planned");
        assert_eq!(merged.total_rows_removed(), 18);
    }

    #[test]
    fn historical_projection_report_defaults_to_not_planned() {
        assert!(!HistoricalProjectionRetentionGcReport::default().planned);
    }
}
