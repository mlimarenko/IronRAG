//! Shared fixture plumbing for the ingest-authorized graph reconcile suites.
//!
//! `graph_atomicity_fencing` and `incremental_graph_reconcile` both need a
//! readable document head and a leased graph extraction attempt. Keeping one
//! copy here stops the duplication gate from ratcheting on fixture code that is
//! not the subject of either suite.

use anyhow::{Context, Result};
use chrono::Utc;
use ironrag_backend::infra::repositories::{
    content_document_head_repository, content_knowledge_projection_repository, ingest_repository,
};
use sqlx::PgPool;
use uuid::Uuid;

/// Points a document's active and readable revision at `revision_id`.
pub(crate) async fn set_active_revision_head(
    pool: &PgPool,
    document_id: Uuid,
    revision_id: Uuid,
) -> Result<()> {
    content_document_head_repository::upsert_document_head(
        pool,
        &content_document_head_repository::NewContentDocumentHead {
            publishing_mutation_item_id: crate::content_lifecycle_support::publication_item(
                pool,
                revision_id,
                "graph-source.txt",
            )
            .await?,
            document_id,
            active_revision_id: Some(revision_id),
            readable_revision_id: Some(revision_id),
            latest_mutation_id: None,
            latest_successful_attempt_id: None,
        },
    )
    .await?;
    Ok(())
}

/// Publishes a readable head and materializes the knowledge projection, as the
/// content ingest publication does before a graph refresh runs.
pub(crate) async fn publish_readable_head(
    pool: &PgPool,
    document_id: Uuid,
    revision_id: Uuid,
) -> Result<()> {
    set_active_revision_head(pool, document_id, revision_id).await?;
    let materialized =
        content_knowledge_projection_repository::materialize_knowledge_document_from_readable_head(
            pool,
            document_id,
        )
        .await?;
    anyhow::ensure!(
        materialized
            == content_knowledge_projection_repository::MaterializeKnowledgeDocumentOutcome::Materialized,
        "readable knowledge projection was not materialized"
    );
    Ok(())
}

/// Content identity of one leased ingest job tied to a document revision.
pub(crate) struct LeasedIngestJobSeed<'a> {
    pub(crate) workspace_id: Uuid,
    pub(crate) library_id: Uuid,
    pub(crate) document_id: Uuid,
    pub(crate) revision_id: Uuid,
    pub(crate) job_kind: &'a str,
    pub(crate) dedupe_key_prefix: &'a str,
}

/// Creates one leased ingest job tied to a document revision.
pub(crate) async fn create_leased_ingest_job(
    pool: &PgPool,
    seed: LeasedIngestJobSeed<'_>,
) -> Result<ingest_repository::IngestJobRow> {
    let now = Utc::now();
    let owner = if seed.job_kind == "content_mutation" {
        let item_id = crate::content_lifecycle_support::seed_pending_revision_owner(
            pool,
            seed.revision_id,
            "graph-source.txt",
        )
        .await?;
        let mutation_id = sqlx::query_scalar::<_, Uuid>(
            "select mutation_id from content_mutation_item where id=$1",
        )
        .bind(item_id)
        .fetch_one(pool)
        .await?;
        Some((mutation_id, item_id))
    } else {
        None
    };
    ingest_repository::create_ingest_job(
        pool,
        &ingest_repository::NewIngestJob {
            workspace_id: seed.workspace_id,
            library_id: seed.library_id,
            mutation_id: owner.map(|(mutation_id, _)| mutation_id),
            mutation_item_id: owner.map(|(_, item_id)| item_id),
            connector_id: None,
            async_operation_id: None,
            knowledge_document_id: Some(seed.document_id),
            knowledge_revision_id: Some(seed.revision_id),
            job_kind: seed.job_kind.to_string(),
            queue_state: "leased".to_string(),
            priority: 200,
            dedupe_key: Some(format!(
                "{}:{}:{}",
                seed.dedupe_key_prefix,
                seed.revision_id,
                Uuid::now_v7()
            )),
            queued_at: Some(now),
            available_at: Some(now),
            completed_at: None,
        },
    )
    .await
    .context("failed to create leased ingest job")
}

/// Creates one leased attempt for a graph extraction job.
pub(crate) async fn create_leased_graph_attempt(
    pool: &PgPool,
    job_id: Uuid,
    label: &str,
) -> Result<ingest_repository::IngestAttemptRow> {
    let now = Utc::now();
    let attempt = ingest_repository::create_ingest_attempt(
        pool,
        &ingest_repository::NewIngestAttempt {
            job_id,
            attempt_number: 1,
            worker_principal_id: None,
            lease_token: Some(format!("{label}-lease")),
            knowledge_generation_id: None,
            attempt_state: "leased".to_string(),
            current_stage: Some("extract_graph".to_string()),
            started_at: Some(now),
            heartbeat_at: Some(now),
            finished_at: None,
            failure_class: None,
            failure_code: None,
            failure_message: None,
            progress_percent: 90,
            retryable: false,
        },
    )
    .await
    .context("failed to create leased graph extraction attempt")?;
    Ok(attempt)
}
