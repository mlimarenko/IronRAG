//! `gc.*` sweepers: delete content that is no longer reachable from a
//! canonical head.
//!
//! Phase C — this revision covers `gc.stale-chunks`: it removes chunks
//! (and their vectors across every dim shard) whose revision is not the
//! readable or active head of their document. The sweeper acquires the
//! per-library graph advisory lock and refuses to run while any ingest
//! job is in flight on that library, mirroring the safety contract of
//! [`crate::services::graph::gc::gc_zombie_nodes`].
//!
//! `include_null_head` opts in to deleting chunks/vectors for documents
//! whose head carries neither `readable_revision_id` nor
//! `active_revision_id`. By default such documents are skipped — there
//! is no canonical state to compare against, and an aggressive sweep
//! would erase a doc whose ingest is still recoverable. The operator
//! flag exists so the recovery path can also be told to give up on the
//! tail.

use std::collections::HashSet;

use anyhow::{Context, anyhow};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sqlx::PgPool;
use thiserror::Error;
use tracing::warn;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::{
        arangodb::{
            client::ArangoClient,
            collections::{
                KNOWLEDGE_CHUNK_COLLECTION, KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
                KNOWLEDGE_DOCUMENT_COLLECTION, KNOWLEDGE_ENTITY_VECTOR_COLLECTION,
            },
        },
        repositories::{self, catalog_repository},
    },
    services::{
        content::service::snapshot::clear_library_arango_footprint,
        graph::gc as graph_gc,
        maintenance::audit::{OrphanLibrariesAudit, orphan_library_ids},
    },
};

/// Per-document AQL: count null-head documents (heads NULL for both
/// readable and active). Reported separately so the operator can see how
/// many docs are sitting in failed-ingest state.
const COUNT_NULL_HEAD_DOCS_AQL: &str = r"
RETURN LENGTH(
    FOR doc IN @@document_collection
        FILTER doc.library_id == @library_id
        FILTER doc.readable_revision_id == null
            AND doc.active_revision_id == null
        RETURN 1
)";

/// Count or delete chunks for one document whose revision is not in the
/// `canonical_revision_ids` list. Passing an empty list is the
/// canonical "delete all chunks of this document" form — used when we
/// process null-head docs with `include_null_head=true`.
const COUNT_STALE_CHUNKS_FOR_DOC_AQL: &str = r"
RETURN LENGTH(
    FOR chunk IN @@chunk_collection
        FILTER chunk.document_id == @document_id
        FILTER LENGTH(@canonical_revision_ids) == 0
            OR chunk.revision_id NOT IN @canonical_revision_ids
        RETURN 1
)";

const DELETE_STALE_CHUNKS_FOR_DOC_AQL: &str = r"
RETURN LENGTH(
    FOR chunk IN @@chunk_collection
        FILTER chunk.document_id == @document_id
        FILTER LENGTH(@canonical_revision_ids) == 0
            OR chunk.revision_id NOT IN @canonical_revision_ids
        REMOVE chunk IN @@chunk_collection
        RETURN 1
)";

/// Count or delete vectors for the listed stale revisions in a given
/// vector collection (legacy or per-dim shard).
const COUNT_STALE_VECTORS_FOR_DOC_AQL: &str = r"
RETURN LENGTH(
    FOR vector IN @@vector_collection
        FILTER vector.library_id == @library_id
        FILTER vector.revision_id IN @stale_revision_ids
        RETURN 1
)";

const DELETE_STALE_VECTORS_FOR_DOC_AQL: &str = r"
RETURN LENGTH(
    FOR vector IN @@vector_collection
        FILTER vector.library_id == @library_id
        FILTER vector.revision_id IN @stale_revision_ids
        REMOVE vector IN @@vector_collection
        RETURN 1
)";

/// Aggregate of one `gc.stale-chunks` pass over a library (or a sum
/// across libraries).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LibraryGcReport {
    pub documents_visited: i64,
    pub documents_with_stale: i64,
    pub stale_chunks_removed: i64,
    pub stale_vectors_removed: i64,
    pub null_head_docs_total: i64,
    pub null_head_docs_processed: i64,
    pub null_head_chunks_removed: i64,
    pub null_head_vectors_removed: i64,
    pub runtime_graph_nodes_removed: i64,
    pub runtime_graph_edges_removed: i64,
    pub runtime_graph_evidence_removed: i64,
}

impl LibraryGcReport {
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        Self {
            documents_visited: self.documents_visited + other.documents_visited,
            documents_with_stale: self.documents_with_stale + other.documents_with_stale,
            stale_chunks_removed: self.stale_chunks_removed + other.stale_chunks_removed,
            stale_vectors_removed: self.stale_vectors_removed + other.stale_vectors_removed,
            null_head_docs_total: self.null_head_docs_total + other.null_head_docs_total,
            null_head_docs_processed: self.null_head_docs_processed
                + other.null_head_docs_processed,
            null_head_chunks_removed: self.null_head_chunks_removed
                + other.null_head_chunks_removed,
            null_head_vectors_removed: self.null_head_vectors_removed
                + other.null_head_vectors_removed,
            runtime_graph_nodes_removed: self.runtime_graph_nodes_removed
                + other.runtime_graph_nodes_removed,
            runtime_graph_edges_removed: self.runtime_graph_edges_removed
                + other.runtime_graph_edges_removed,
            runtime_graph_evidence_removed: self.runtime_graph_evidence_removed
                + other.runtime_graph_evidence_removed,
        }
    }

    /// Sum of chunks deleted across the canonical and null-head paths.
    /// Useful for the scheduler `rows_removed` metric where the caller
    /// does not care which lane removed each row.
    #[must_use]
    pub fn total_rows_removed(self) -> i64 {
        self.stale_chunks_removed
            + self.stale_vectors_removed
            + self.null_head_chunks_removed
            + self.null_head_vectors_removed
            + self.runtime_graph_nodes_removed
            + self.runtime_graph_edges_removed
            + self.runtime_graph_evidence_removed
    }
}

/// Options for a single sweep.
#[derive(Debug, Default, Clone, Copy)]
pub struct GcStaleChunksOptions {
    /// When `true` the sweeper only counts what it would remove and
    /// reports it back without issuing destructive AQL.
    pub dry_run: bool,
    /// When `true` documents with both heads NULL also have their
    /// chunks/vectors swept — appropriate when the operator has accepted
    /// that those documents cannot be recovered.
    pub include_null_head: bool,
}

#[derive(Debug, Error)]
pub enum GcStaleChunksError {
    #[error("library {library_id} has {active_jobs} active ingest jobs; gc.stale-chunks refused")]
    ActiveIngest { library_id: Uuid, active_jobs: i64 },
    #[error("postgres error during gc.stale-chunks: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("arango error during gc.stale-chunks for library {library_id}: {source}")]
    Arango { library_id: Uuid, source: anyhow::Error },
    #[error("unsupported knowledge plane backend `{backend}` for gc.stale-chunks")]
    UnsupportedBackend { backend: String },
}

impl GcStaleChunksError {
    /// Canonical stable error code for lease-table failures and
    /// Prometheus labels.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::ActiveIngest { .. } => "active_ingest",
            Self::Sqlx(_) => "postgres",
            Self::Arango { .. } => "arango",
            Self::UnsupportedBackend { .. } => "unsupported_backend",
        }
    }
}

/// Run `gc.stale-chunks` against one library.
///
/// Holds the per-library graph advisory lock for the full pass to
/// serialise against concurrent extractor finalisation. Refuses to run
/// while any `ingest_job` for the library is in `queued`/`leased`/
/// `paused` state (the queue states under which chunks may still be
/// written). The sweeper is otherwise safe for read traffic — it does
/// not touch indexes used by retrieve and only removes chunks/vectors
/// the read path has already deselected via the head pointer.
pub async fn run_for_library(
    state: &AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    options: GcStaleChunksOptions,
) -> Result<LibraryGcReport, GcStaleChunksError> {
    let lock =
        repositories::acquire_runtime_library_graph_lock(&state.persistence.postgres, library_id)
            .await?;
    let outcome = async {
        ensure_no_active_ingest(&state.persistence.postgres, library_id).await?;
        run_under_lock(state, workspace_id, library_id, options).await
    }
    .await;
    let release = repositories::release_runtime_library_graph_lock(lock, library_id).await;
    match (outcome, release) {
        (Ok(report), Ok(())) => Ok(report),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(release_error)) => Err(GcStaleChunksError::Sqlx(release_error)),
        (Err(error), Err(release_error)) => {
            tracing::error!(
                %library_id,
                ?error,
                ?release_error,
                "gc.stale-chunks failed and advisory lock release also failed",
            );
            Err(error)
        }
    }
}

async fn ensure_no_active_ingest(
    pool: &PgPool,
    library_id: Uuid,
) -> Result<(), GcStaleChunksError> {
    let active_jobs: i64 = sqlx::query_scalar(
        "select count(*) from ingest_job \
         where library_id = $1 \
           and queue_state in ('queued', 'leased', 'paused') \
           and completed_at is null",
    )
    .bind(library_id)
    .fetch_one(pool)
    .await?;
    if active_jobs > 0 {
        return Err(GcStaleChunksError::ActiveIngest { library_id, active_jobs });
    }
    Ok(())
}

async fn run_under_lock(
    state: &AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    options: GcStaleChunksOptions,
) -> Result<LibraryGcReport, GcStaleChunksError> {
    match state.settings.knowledge_plane_backend.as_str() {
        "arango" => run_arango_under_lock(state, workspace_id, library_id, options).await,
        "postgres" => run_postgres_under_lock(state, library_id, options).await,
        backend => Err(GcStaleChunksError::UnsupportedBackend { backend: backend.to_string() }),
    }
}

async fn run_arango_under_lock(
    state: &AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    options: GcStaleChunksOptions,
) -> Result<LibraryGcReport, GcStaleChunksError> {
    let null_head_total = arango_scalar_i64(
        &state.arango_client,
        COUNT_NULL_HEAD_DOCS_AQL,
        serde_json::json!({
            "@document_collection": KNOWLEDGE_DOCUMENT_COLLECTION,
            "library_id": library_id,
        }),
    )
    .await
    .map_err(|source| GcStaleChunksError::Arango { library_id, source })?;

    let documents = state
        .arango_document_store
        .list_documents_by_library(workspace_id, library_id, options.include_null_head)
        .await
        .map_err(|source| GcStaleChunksError::Arango { library_id, source: anyhow!(source) })?;

    let mut report =
        LibraryGcReport { null_head_docs_total: null_head_total, ..LibraryGcReport::default() };

    for document in &documents {
        report.documents_visited += 1;
        let is_null_head =
            document.readable_revision_id.is_none() && document.active_revision_id.is_none();
        if is_null_head && !options.include_null_head {
            continue;
        }
        match gc_document(state, library_id, document, options).await {
            Ok(per_doc) => {
                if per_doc.chunks_removed > 0 || per_doc.vectors_removed > 0 {
                    report.documents_with_stale += 1;
                }
                if is_null_head {
                    report.null_head_docs_processed += 1;
                    report.null_head_chunks_removed += per_doc.chunks_removed;
                    report.null_head_vectors_removed += per_doc.vectors_removed;
                } else {
                    report.stale_chunks_removed += per_doc.chunks_removed;
                    report.stale_vectors_removed += per_doc.vectors_removed;
                }
            }
            Err(error) => {
                warn!(
                    library_id = %library_id,
                    document_id = %document.document_id,
                    ?error,
                    "gc.stale-chunks per-document pass failed; continuing",
                );
            }
        }
    }
    Ok(report)
}

async fn run_postgres_under_lock(
    state: &AppState,
    library_id: Uuid,
    options: GcStaleChunksOptions,
) -> Result<LibraryGcReport, GcStaleChunksError> {
    let pool = &state.persistence.postgres;
    let mut tx = pool.begin().await?;
    let report = if options.dry_run {
        count_postgres_gc_transaction(library_id, &mut tx).await?
    } else {
        let stale_evidence = run_stale_evidence_transaction(library_id, &mut tx).await?;
        let graph_report = graph_gc::run_gc_postgres_transaction(library_id, &mut tx)
            .await
            .map_err(graph_gc_error_to_stale_chunks_error)?;
        LibraryGcReport {
            runtime_graph_nodes_removed: i64::from(graph_report.entities_deleted),
            runtime_graph_edges_removed: i64::from(graph_report.relations_deleted),
            runtime_graph_evidence_removed: stale_evidence.total_rows_removed()
                + i64::from(graph_report.evidence_deleted),
            ..LibraryGcReport::default()
        }
    };
    tx.commit().await?;
    Ok(report)
}

async fn count_postgres_gc_transaction(
    library_id: Uuid,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<LibraryGcReport, GcStaleChunksError> {
    let stale_evidence = count_stale_evidence_candidates_transaction(library_id, tx).await?;
    let (orphan_evidence, zombie_edges, zombie_nodes) =
        count_runtime_graph_gc_candidates_transaction(library_id, tx).await?;
    Ok(LibraryGcReport {
        runtime_graph_nodes_removed: zombie_nodes,
        runtime_graph_edges_removed: zombie_edges,
        runtime_graph_evidence_removed: stale_evidence.total_rows_removed() + orphan_evidence,
        ..LibraryGcReport::default()
    })
}

#[derive(Debug, Clone, Copy, Default)]
struct DocumentGcCounts {
    chunks_removed: i64,
    vectors_removed: i64,
}

async fn gc_document(
    state: &AppState,
    library_id: Uuid,
    document: &crate::infra::arangodb::document_store::KnowledgeDocumentRow,
    options: GcStaleChunksOptions,
) -> anyhow::Result<DocumentGcCounts> {
    let canonical_revision_ids: Vec<Uuid> =
        [document.readable_revision_id, document.active_revision_id]
            .into_iter()
            .flatten()
            .collect();

    let revisions = state
        .arango_document_store
        .list_revisions_by_document(document.document_id)
        .await
        .with_context(|| {
            format!("failed to list revisions for document {}", document.document_id)
        })?;
    let canonical_set: HashSet<Uuid> = canonical_revision_ids.iter().copied().collect();
    let stale_revision_ids: Vec<Uuid> = revisions
        .into_iter()
        .map(|revision| revision.revision_id)
        .filter(|revision_id| !canonical_set.contains(revision_id))
        .collect();

    let chunks_removed = arango_scalar_i64(
        &state.arango_client,
        if options.dry_run {
            COUNT_STALE_CHUNKS_FOR_DOC_AQL
        } else {
            DELETE_STALE_CHUNKS_FOR_DOC_AQL
        },
        serde_json::json!({
            "@chunk_collection": KNOWLEDGE_CHUNK_COLLECTION,
            "document_id": document.document_id,
            "canonical_revision_ids": canonical_revision_ids,
        }),
    )
    .await
    .with_context(|| {
        format!("failed to count/delete stale chunks for document {}", document.document_id)
    })?;

    let vectors_removed = if stale_revision_ids.is_empty() {
        0
    } else {
        let arango = &state.arango_client;
        let mut vector_collections: Vec<String> = vec![
            KNOWLEDGE_CHUNK_VECTOR_COLLECTION.to_string(),
            KNOWLEDGE_ENTITY_VECTOR_COLLECTION.to_string(),
        ];
        vector_collections.extend(
            arango
                .list_per_dim_chunk_vector_collections()
                .await
                .context("failed to list per-dim chunk vector shards")?,
        );
        vector_collections.extend(
            arango
                .list_per_dim_entity_vector_collections()
                .await
                .context("failed to list per-dim entity vector shards")?,
        );
        let mut removed: i64 = 0;
        for collection in &vector_collections {
            let count = arango_scalar_i64(
                arango,
                if options.dry_run {
                    COUNT_STALE_VECTORS_FOR_DOC_AQL
                } else {
                    DELETE_STALE_VECTORS_FOR_DOC_AQL
                },
                serde_json::json!({
                    "@vector_collection": collection,
                    "library_id": library_id,
                    "stale_revision_ids": stale_revision_ids,
                }),
            )
            .await
            .with_context(|| {
                format!(
                    "failed to count/delete stale vectors in {collection} for document {}",
                    document.document_id
                )
            })?;
            removed += count;
        }
        removed
    };

    Ok(DocumentGcCounts { chunks_removed, vectors_removed })
}

/// Run `gc.stale-chunks` across every library, returning the rolled-up
/// report. Used both by the CLI all-libraries path and by the scheduler
/// when it walks the library set directly (e.g. on the
/// `gc.orphan-libraries-audit` adjacent class).
pub async fn run_for_all_libraries(
    state: &AppState,
    options: GcStaleChunksOptions,
) -> anyhow::Result<LibraryGcReport> {
    let libraries = catalog_repository::list_libraries(&state.persistence.postgres, None).await?;
    let mut totals = LibraryGcReport::default();
    for library in libraries {
        match run_for_library(state, library.workspace_id, library.id, options).await {
            Ok(report) => {
                totals = totals.merge(report);
            }
            Err(GcStaleChunksError::ActiveIngest { active_jobs, .. }) => {
                warn!(
                    library_id = %library.id,
                    active_jobs,
                    "gc.stale-chunks skipped: library has active ingest",
                );
            }
            Err(error) => {
                warn!(
                    library_id = %library.id,
                    ?error,
                    "gc.stale-chunks failed; continuing with next library",
                );
            }
        }
    }
    Ok(totals)
}

async fn arango_scalar_i64(
    client: &ArangoClient,
    query: &str,
    bind_vars: serde_json::Value,
) -> anyhow::Result<i64> {
    arango_single_row(client, query, bind_vars).await
}

async fn arango_single_row<T: DeserializeOwned>(
    client: &ArangoClient,
    query: &str,
    bind_vars: serde_json::Value,
) -> anyhow::Result<T> {
    let cursor = client.query_json(query, bind_vars).await.with_context(|| {
        format!("arango query failed: {}", query.chars().take(96).collect::<String>())
    })?;
    let rows =
        cursor.get("result").cloned().context("arango cursor payload missing result field")?;
    let mut rows: Vec<T> =
        serde_json::from_value(rows).context("failed to deserialise arango query result")?;
    rows.pop().context("expected one arango result row")
}

/// Outcome of a `gc.orphan-libraries --purge` pass.
#[derive(Debug, Default, Clone, Serialize)]
pub struct OrphanLibrariesPurgeReport {
    pub orphan_libraries_total: usize,
    pub purged: usize,
    pub failed: usize,
}

/// Wipe every ArangoDB row whose `library_id` points at a PostgreSQL
/// `catalog_library` row that no longer exists.
///
/// Reuses the canonical snapshot-restore replace sweep
/// (`clear_library_arango_footprint`) so the cleanup binary travels the
/// same path snapshot-restore uses. The Arango orphan inventory comes
/// from [`crate::services::maintenance::audit::orphan_libraries`], so
/// the destructive variant cannot drift out of sync with the
/// read-only audit.
pub async fn purge_orphan_libraries(
    state: &AppState,
    audit: &OrphanLibrariesAudit,
) -> anyhow::Result<OrphanLibrariesPurgeReport> {
    let orphans = orphan_library_ids(audit);
    let mut report = OrphanLibrariesPurgeReport {
        orphan_libraries_total: orphans.len(),
        ..OrphanLibrariesPurgeReport::default()
    };
    for library_id in &orphans {
        match clear_library_arango_footprint(state, *library_id).await {
            Ok(()) => report.purged += 1,
            Err(error) => {
                report.failed += 1;
                warn!(
                    library_id = %library_id,
                    ?error,
                    "failed to purge orphan library footprint; continuing with next",
                );
            }
        }
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
    /// `content_chunk` (e.g. the chunk was swept by `gc.stale-chunks`
    /// but the matching evidence row stayed behind).
    pub phantom_chunk_rows: i64,
}

impl StaleEvidenceReport {
    /// Total rows the sweeper removed, summed across lanes. Used by
    /// the scheduler so it can report a single `rows_removed` metric
    /// without needing to know the report shape.
    #[must_use]
    pub fn total_rows_removed(self) -> i64 {
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
///    chunk has been deleted (e.g. `gc.stale-chunks` already swept
///    it). Such rows are unreachable from any live retrieval path.
///
/// Both lanes skip rows whose document has an active ingest job
/// (`queue_state IN ('queued','leased','paused') AND completed_at IS
/// NULL`). The acquired graph advisory lock plus the active-ingest
/// guard mirror the safety contract of `gc.stale-chunks` so the two
/// sweepers can run on the same library concurrently with confidence.
pub async fn run_stale_evidence(
    state: &AppState,
    library_id: Uuid,
) -> Result<StaleEvidenceReport, GcStaleChunksError> {
    let pool = &state.persistence.postgres;
    let lock = repositories::acquire_runtime_library_graph_lock(pool, library_id).await?;
    let outcome = async {
        ensure_no_active_ingest(pool, library_id).await?;
        run_stale_evidence_under_lock(pool, library_id).await
    }
    .await;
    let release = repositories::release_runtime_library_graph_lock(lock, library_id).await;
    match (outcome, release) {
        (Ok(report), Ok(())) => Ok(report),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(release_error)) => Err(GcStaleChunksError::Sqlx(release_error)),
        (Err(error), Err(release_error)) => {
            tracing::error!(
                %library_id,
                ?error,
                ?release_error,
                "gc.stale-evidence failed and advisory lock release also failed",
            );
            Err(error)
        }
    }
}

async fn run_stale_evidence_under_lock(
    pool: &PgPool,
    library_id: Uuid,
) -> Result<StaleEvidenceReport, GcStaleChunksError> {
    let mut tx = pool.begin().await?;
    let report = run_stale_evidence_transaction(library_id, &mut tx).await?;
    tx.commit().await?;
    Ok(report)
}

async fn run_stale_evidence_transaction(
    library_id: Uuid,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<StaleEvidenceReport, GcStaleChunksError> {
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

async fn count_stale_evidence_candidates_transaction(
    library_id: Uuid,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<StaleEvidenceReport, GcStaleChunksError> {
    let stale_revision_rows = sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint \
         from runtime_graph_evidence ev \
         join content_document_head h on h.document_id = ev.document_id \
         where ev.library_id = $1 \
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
           )",
    )
    .bind(library_id)
    .fetch_one(&mut **tx)
    .await?;

    let phantom_chunk_rows = sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint \
         from runtime_graph_evidence ev \
         where ev.library_id = $1 \
           and ev.chunk_id is not null \
           and not exists (select 1 from content_chunk c where c.id = ev.chunk_id)",
    )
    .bind(library_id)
    .fetch_one(&mut **tx)
    .await?;

    Ok(StaleEvidenceReport { stale_revision_rows, phantom_chunk_rows })
}

async fn count_runtime_graph_gc_candidates_transaction(
    library_id: Uuid,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<(i64, i64, i64), GcStaleChunksError> {
    sqlx::query_as::<_, (i64, i64, i64)>(
        "with orphan_evidence as ( \
             select evidence.id \
             from runtime_graph_evidence evidence \
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
         ), \
         zombie_nodes as ( \
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
         ) \
         select \
             (select count(*)::bigint from orphan_evidence), \
             (select count(*)::bigint from zombie_edges), \
             (select count(*)::bigint from zombie_nodes)",
    )
    .bind(library_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(GcStaleChunksError::Sqlx)
}

fn graph_gc_error_to_stale_chunks_error(error: graph_gc::GraphGcError) -> GcStaleChunksError {
    match error {
        graph_gc::GraphGcError::Postgres { source, .. } => GcStaleChunksError::Sqlx(source),
        graph_gc::GraphGcError::Arango { library_id, source } => {
            GcStaleChunksError::Arango { library_id, source }
        }
        other => GcStaleChunksError::Arango { library_id: Uuid::nil(), source: anyhow!(other) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_merge_is_additive() {
        let a = LibraryGcReport {
            documents_visited: 10,
            documents_with_stale: 3,
            stale_chunks_removed: 100,
            stale_vectors_removed: 50,
            null_head_docs_total: 7,
            null_head_docs_processed: 0,
            null_head_chunks_removed: 0,
            null_head_vectors_removed: 0,
            runtime_graph_nodes_removed: 0,
            runtime_graph_edges_removed: 0,
            runtime_graph_evidence_removed: 0,
        };
        let b = LibraryGcReport {
            documents_visited: 5,
            documents_with_stale: 2,
            stale_chunks_removed: 20,
            stale_vectors_removed: 7,
            null_head_docs_total: 3,
            null_head_docs_processed: 3,
            null_head_chunks_removed: 18,
            null_head_vectors_removed: 9,
            runtime_graph_nodes_removed: 4,
            runtime_graph_edges_removed: 6,
            runtime_graph_evidence_removed: 8,
        };
        let merged = a.merge(b);
        assert_eq!(merged.documents_visited, 15);
        assert_eq!(merged.documents_with_stale, 5);
        assert_eq!(merged.stale_chunks_removed, 120);
        assert_eq!(merged.stale_vectors_removed, 57);
        assert_eq!(merged.null_head_docs_total, 10);
        assert_eq!(merged.null_head_docs_processed, 3);
        assert_eq!(merged.null_head_chunks_removed, 18);
        assert_eq!(merged.null_head_vectors_removed, 9);
        assert_eq!(merged.runtime_graph_nodes_removed, 4);
        assert_eq!(merged.runtime_graph_edges_removed, 6);
        assert_eq!(merged.runtime_graph_evidence_removed, 8);
    }

    #[test]
    fn total_rows_removed_sums_all_lanes() {
        let report = LibraryGcReport {
            stale_chunks_removed: 10,
            stale_vectors_removed: 20,
            null_head_chunks_removed: 30,
            null_head_vectors_removed: 40,
            runtime_graph_nodes_removed: 5,
            runtime_graph_edges_removed: 6,
            runtime_graph_evidence_removed: 7,
            ..LibraryGcReport::default()
        };
        assert_eq!(report.total_rows_removed(), 118);
    }

    #[test]
    fn error_codes_are_stable() {
        assert_eq!(
            GcStaleChunksError::ActiveIngest { library_id: Uuid::nil(), active_jobs: 2 }.code(),
            "active_ingest"
        );
        assert_eq!(
            GcStaleChunksError::Arango { library_id: Uuid::nil(), source: anyhow!("boom") }.code(),
            "arango"
        );
    }
}
