use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::{
    domains::{
        ingest::{is_retry_budget_exhausted, retry_backoff_with_deferral},
        ops::OpsFailureDisposition,
        webhook::{WebhookEvent, revision_ready_event_id},
    },
    infra::{
        knowledge_plane::ActiveEmbeddingProfileSource,
        postgres::pg_search_store::{
            advance_vector_source_truth_version, delete_ingest_revision_chunk_vectors,
            lock_exclusive_vector_mutation_source, validate_ingest_revision_vector_coverage,
        },
        repositories::{
            catalog_repository,
            content_document_head_repository::{self, NewContentDocumentHead},
            content_knowledge_projection_repository::{self, MaterializeKnowledgeDocumentOutcome},
            ingest_repository::NewIngestStageEvent,
            runtime_graph_repository::IngestRevisionGraphPublicationGuard,
            webhook_outbox_repository,
        },
    },
};

#[derive(Debug, FromRow)]
struct PublicationJobAuthorityRow {
    id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    mutation_id: Option<Uuid>,
    mutation_item_id: Option<Uuid>,
    async_operation_id: Option<Uuid>,
    knowledge_document_id: Option<Uuid>,
    knowledge_revision_id: Option<Uuid>,
    job_kind: String,
}

#[derive(Debug, FromRow)]
struct PublicationAttemptAuthorityRow {
    id: Uuid,
    attempt_number: i32,
    failure_message: Option<String>,
}

/// Proves that the exact revision's structured artifact is complete before a
/// lifecycle transaction makes it readable. Locking the revision and metadata
/// keeps direct/operator replacement from racing the family counts through the
/// publication commit.
pub(super) async fn validate_prepared_structured_revision_for_publication(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
) -> anyhow::Result<()> {
    let metadata = sqlx::query_as::<_, (String, i32, i32, i32)>(
        "select structured.preparation_state,
                structured.block_count::int4,
                structured.chunk_count::int4,
                structured.typed_fact_count::int4
         from knowledge_revision as revision
         join knowledge_structured_revision as structured
           on structured.revision_id = revision.revision_id
          and structured.workspace_id = revision.workspace_id
          and structured.library_id = revision.library_id
          and structured.document_id = revision.document_id
         where revision.revision_id = $1
           and revision.workspace_id = $2
           and revision.library_id = $3
           and revision.document_id = $4
         for update of revision, structured",
    )
    .bind(revision_id)
    .bind(workspace_id)
    .bind(library_id)
    .bind(document_id)
    .fetch_optional(&mut **transaction)
    .await
    .context("lock exact structured artifact for publication")?
    .context("publication structured artifact is missing or outside revision scope")?;
    anyhow::ensure!(
        metadata.0 == "prepared",
        "publication requires a prepared structured artifact"
    );
    anyhow::ensure!(
        metadata.1 >= 0 && metadata.2 >= 0 && metadata.3 >= 0,
        "publication structured artifact declares negative family counts"
    );

    let actual_counts = sqlx::query_as::<_, (i64, i64, i64, i64)>(
        "select
            (select count(*) from knowledge_structured_block where revision_id = $1),
            (select count(*) from content_chunk where revision_id = $1),
            (select count(*) from knowledge_chunk where revision_id = $1),
            (select count(*) from knowledge_technical_fact where revision_id = $1)",
    )
    .bind(revision_id)
    .fetch_one(&mut **transaction)
    .await
    .context("count structured artifact families for publication")?;
    anyhow::ensure!(
        actual_counts.0 == i64::from(metadata.1)
            && actual_counts.1 == i64::from(metadata.2)
            && actual_counts.2 == i64::from(metadata.2)
            && actual_counts.3 == i64::from(metadata.3),
        "publication structured artifact family counts do not match metadata"
    );
    Ok(())
}

/// Exact identities and source/profile observation required to atomically
/// publish one successful content-ingest attempt.
///
/// The repository revalidates every identity while holding the library, job,
/// and attempt locks. Revision-scoped publication treats
/// `expected_source_truth_version` as a monotonic lower bound because unrelated
/// revisions may advance the library-global generation, and validates
/// `embedding_profile_key` through [`ActiveEmbeddingProfileSource`] under the
/// library lock. Callers must treat
/// [`PublishContentIngestSuccessOutcome::AuthorityLost`] as a successful stale-worker no-op and
/// must not run a second lifecycle finalizer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishContentIngestSuccess {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub revision_id: Uuid,
    pub mutation_id: Uuid,
    pub mutation_item_id: Uuid,
    pub attempt_id: Uuid,
    pub expected_source_truth_version: i64,
    pub embedding_profile_key: Option<String>,
    pub text_state: String,
    pub graph_state: String,
    pub text_readable_at: Option<DateTime<Utc>>,
    pub graph_ready_at: Option<DateTime<Utc>>,
    pub completed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishContentIngestSuccessOutcome {
    Applied { source_truth_version: i64, mutation_completed: bool },
    AuthorityLost { source_truth_version: i64 },
}

/// Optional durable side effects that must share the exact content lifecycle
/// commit. An absent summary preserves the previously published summary; stage
/// events are appended only after exact attempt authority is revalidated.
#[derive(Debug, Clone, Default)]
pub struct ContentIngestPublicationEffects {
    pub document_summary: Option<String>,
    pub terminal_stage_events: Vec<NewIngestStageEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailContentIngestAttempt {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub revision_id: Uuid,
    pub mutation_id: Uuid,
    pub mutation_item_id: Uuid,
    pub attempt_id: Uuid,
    pub current_stage: Option<String>,
    pub failure_class: Option<String>,
    pub failure_code: Option<String>,
    /// Server-authoritative disposition to store on the operation when this
    /// failure terminates it, carried from the caller's already-computed
    /// classification. Ignored while a retry is still scheduled.
    pub failure_disposition: Option<OpsFailureDisposition>,
    pub failure_message: Option<String>,
    pub retryable: bool,
    /// Operational provider cooldown (a 429 `Retry-After` or rate-limit/quota
    /// cooldown). When set, the failed attempt is requeued after the deferral
    /// without consuming the attempt budget.
    pub operational_defer: Option<chrono::Duration>,
    pub delete_vectors: bool,
    pub failed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailContentIngestAttemptOutcome {
    Applied {
        deleted: u64,
        source_truth_version: i64,
        retry_scheduled: bool,
        mutation_failed: bool,
    },
    AuthorityLost {
        source_truth_version: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishGraphRefreshSuccess {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub revision_id: Uuid,
    pub attempt_id: Uuid,
    pub graph_state: String,
    pub graph_ready_at: Option<DateTime<Utc>>,
    pub completed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishGraphRefreshSuccessOutcome {
    Applied { source_truth_version: i64 },
    Superseded { source_truth_version: i64 },
    AuthorityLost { source_truth_version: i64 },
}

async fn refresh_runtime_graph_provenance_coverage_for_publication(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
) -> anyhow::Result<()> {
    let projection_version = sqlx::query_scalar::<_, i64>(
        "select projection_version
         from runtime_graph_snapshot
         where library_id = $1
         for update",
    )
    .bind(library_id)
    .fetch_optional(&mut **transaction)
    .await
    .context("lock graph snapshot for provenance publication")?;
    let Some(projection_version) = projection_version else {
        return Ok(());
    };
    let (ready_count, projected_count) =
        crate::infra::repositories::content_document_list_repository::count_library_graph_coverage_for_publication_with_executor(
            &mut **transaction,
            library_id,
            projection_version,
            document_id,
            revision_id,
        )
        .await
        .context("count graph provenance coverage for lifecycle publication")?;
    let coverage = if ready_count <= 0 {
        0.0
    } else {
        projected_count.clamp(0, ready_count) as f64 * 100.0 / ready_count as f64
    };
    let updated = sqlx::query(
        "update runtime_graph_snapshot
         set provenance_coverage_percent = $2,
             updated_at = now()
         where library_id = $1
           and projection_version = $3",
    )
    .bind(library_id)
    .bind(coverage)
    .bind(projection_version)
    .execute(&mut **transaction)
    .await
    .context("publish graph provenance coverage")?;
    anyhow::ensure!(updated.rows_affected() == 1, "graph snapshot changed during publication");
    Ok(())
}

/// Terminal graph telemetry committed with the graph-refresh projection and
/// lifecycle state. The vector permits future terminal sub-stages without
/// reopening the reconcile/publication split.
#[derive(Debug, Clone, Default)]
pub struct GraphRefreshPublicationEffects {
    pub terminal_stage_events: Vec<NewIngestStageEvent>,
}

mod content_failure;
mod content_success;
mod graph_refresh;

pub use self::{content_failure::*, content_success::*, graph_refresh::*};
