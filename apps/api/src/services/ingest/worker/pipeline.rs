use std::time::Instant;

use anyhow::Context;
use chrono::Utc;
use tracing::{info, warn};

use crate::{
    infra::repositories::ingest_repository,
    services::{
        content::service::{GRAPH_STATE_DEGRADED, graph_state_after_successful_extract},
        ingest::service::INGEST_STAGE_FINALIZING,
        query::vector_dimensions::{
            ensure_active_embedding_profile_key, invalidate_library_embedding_profile_inventory,
        },
    },
};

use super::{
    attempt::IngestStageContext,
    embedding::{EmbeddingStageOutcome, embed_ingest_content},
    errors::{ContentPipelineFailure, JobLeaseLost},
    extraction::generate_document_summary_from_blocks,
    graph::{GraphExtractionOutcome, extract_revision_graph},
    preparation::{PreparedContent, prepare_ingest_content},
};

pub(super) enum ContentPipelineOutcome {
    LifecyclePublished,
}

pub(super) async fn run_ingest_pipeline(
    context: IngestStageContext<'_>,
) -> anyhow::Result<ContentPipelineOutcome> {
    let PreparedContent { revision } = Box::pin(prepare_ingest_content(&context)).await?;
    let embedding = embed_ingest_content(&context, &revision).await?;

    // Summary generation and execution-profile validation may need the main
    // pool/provider stack, so both must finish before graph reconcile acquires
    // its long heartbeat-pool publication guard. The summary itself remains
    // uncommitted until the exact attempt publication below.
    let document_summary = prepare_document_summary(&context).await;
    validate_embedding_execution_profile(
        &context,
        revision.library_id,
        embedding.embedding_profile_key.as_deref(),
    )
    .await?;

    let graph = extract_revision_graph(&context, &revision).await?;
    finalize_content_readiness(&context, &revision, embedding, document_summary, graph).await
}

async fn finalize_content_readiness(
    context: &IngestStageContext<'_>,
    revision: &crate::infra::knowledge_rows::KnowledgeRevisionRow,
    embedding: EmbeddingStageOutcome,
    document_summary: Option<String>,
    graph: GraphExtractionOutcome,
) -> anyhow::Result<ContentPipelineOutcome> {
    let IngestStageContext {
        state, job, attempt_id, document_id, revision_id, cancellation, ..
    } = *context;
    let EmbeddingStageOutcome {
        source_truth_version: vector_write_source_truth_version,
        embedding_profile_key: embed_chunk_profile_key,
    } = embedding;

    let GraphExtractionOutcome {
        graph_ready,
        graph_degraded,
        pending_summary_refresh,
        publication_guard,
        terminal_stage_event,
    } = graph;

    // --- Stage: finalize readiness --------------------------------------------
    let finalizing_start = Instant::now();

    let mutation_id = job.mutation_id.context("content ingest job is missing mutation_id")?;
    let mutation_item_id =
        job.mutation_item_id.context("content ingest job is missing mutation_item_id")?;
    let now = Utc::now();
    let graph_state = if graph_degraded {
        GRAPH_STATE_DEGRADED
    } else {
        graph_state_after_successful_extract(graph_ready)
    };
    let publication_input = ingest_repository::PublishContentIngestSuccess {
        workspace_id: job.workspace_id,
        library_id: job.library_id,
        document_id,
        revision_id,
        mutation_id,
        mutation_item_id,
        attempt_id,
        expected_source_truth_version: vector_write_source_truth_version,
        embedding_profile_key: embed_chunk_profile_key,
        text_state: "text_readable".to_string(),
        graph_state: graph_state.to_string(),
        text_readable_at: Some(now),
        graph_ready_at: graph_ready.then_some(now),
        completed_at: now,
    };
    let mut terminal_stage_events = terminal_stage_event.into_iter().collect::<Vec<_>>();
    terminal_stage_events.push(ingest_repository::NewIngestStageEvent {
        attempt_id,
        stage_name: INGEST_STAGE_FINALIZING.to_string(),
        stage_state: "started".to_string(),
        ordinal: 0,
        message: None,
        details_json: serde_json::json!({}),
        recorded_at: None,
        provider_kind: None,
        model_name: None,
        prompt_tokens: None,
        completion_tokens: None,
        total_tokens: None,
        cached_tokens: None,
        estimated_cost: None,
        currency_code: None,
        elapsed_ms: None,
    });
    terminal_stage_events.push(ingest_repository::NewIngestStageEvent {
        attempt_id,
        stage_name: INGEST_STAGE_FINALIZING.to_string(),
        stage_state: "completed".to_string(),
        ordinal: 0,
        message: Some("ingest pipeline completed".to_string()),
        details_json: serde_json::json!({
            "revisionId": revision_id,
            "documentId": document_id,
        }),
        recorded_at: None,
        provider_kind: None,
        model_name: None,
        prompt_tokens: None,
        completion_tokens: None,
        total_tokens: None,
        cached_tokens: None,
        estimated_cost: None,
        currency_code: None,
        elapsed_ms: Some(finalizing_start.elapsed().as_millis() as i64),
    });
    let effects = ingest_repository::ContentIngestPublicationEffects {
        document_summary,
        terminal_stage_events,
    };

    if let Err(cancellation_error) = cancellation.check(job.id) {
        if let Some(publication_guard) = publication_guard {
            publication_guard
                .rollback()
                .await
                .context("roll back canceled content graph reconcile")?;
        }
        return Err(cancellation_error);
    }

    let publication = if let Some(publication_guard) = publication_guard {
        ingest_repository::publish_content_ingest_success_revision_fenced_with_graph_guard(
            publication_guard,
            &publication_input,
            &effects,
        )
        .await
    } else {
        ingest_repository::publish_content_ingest_success_revision_fenced_with_effects(
            &state.persistence.postgres,
            &publication_input,
            &effects,
            state,
        )
        .await
    }
    .context("failed to atomically publish content ingest success")?;
    let (mutation_completed, source_truth_version) = match publication {
        ingest_repository::PublishContentIngestSuccessOutcome::Applied {
            mutation_completed,
            source_truth_version,
        } => (mutation_completed, source_truth_version),
        ingest_repository::PublishContentIngestSuccessOutcome::AuthorityLost { .. } => {
            return Err(anyhow::Error::new(JobLeaseLost { job_id: job.id }));
        }
    };

    refresh_published_content_summary(
        context,
        revision.library_id,
        source_truth_version,
        pending_summary_refresh,
    )
    .await;

    invalidate_library_embedding_profile_inventory(revision.library_id);
    settle_published_content_page(context, mutation_item_id).await;
    converge_published_technical_facts(context).await;

    log_content_publication(context, mutation_completed);
    Ok(ContentPipelineOutcome::LifecyclePublished)
}

fn log_content_publication(context: &IngestStageContext<'_>, mutation_completed: bool) {
    info!(
        attempt_id = %context.attempt_id,
        revision_id = %context.revision_id,
        mutation_completed,
        "content ingest publication committed",
    );
}

async fn prepare_document_summary(context: &IngestStageContext<'_>) -> Option<String> {
    match generate_document_summary_from_blocks(context.state, context.revision_id).await {
        Ok(summary) if !summary.trim().is_empty() => Some(summary),
        Err(error) => {
            warn!(document_id = %context.document_id, ?error,
                "failed to generate document summary; preserving the previously published summary");
            None
        }
        _ => None,
    }
}

async fn validate_embedding_execution_profile(
    context: &IngestStageContext<'_>,
    library_id: uuid::Uuid,
    embedding_profile_key: Option<&str>,
) -> anyhow::Result<()> {
    if let Some(embedding_profile_key) = embedding_profile_key
        && let Err(error) =
            ensure_active_embedding_profile_key(context.state, library_id, embedding_profile_key)
                .await
    {
        let reason =
            format!("embedding execution profile changed before revision readiness: {error:#}");
        return Err(anyhow::Error::new(ContentPipelineFailure {
            delete_vectors: true,
            source: error.context(reason),
        }));
    }
    Ok(())
}

async fn refresh_published_content_summary(
    context: &IngestStageContext<'_>,
    library_id: uuid::Uuid,
    source_truth_version: i64,
    pending_summary_refresh: Option<crate::services::graph::summary::PendingGraphSummaryRefresh>,
) {
    if let Some(pending_summary_refresh) = pending_summary_refresh
        && let Err(error) = context
            .state
            .application_services
            .graph
            .apply_published_summary_refresh(
                context.state,
                library_id,
                source_truth_version,
                &pending_summary_refresh,
            )
            .await
    {
        warn!(worker_id = %context.worker_id, job_id = %context.job.id,
            attempt_id = %context.attempt_id, revision_id = %context.revision_id,
            source_truth_version, ?error,
            "content lifecycle committed but active graph summary refresh failed");
    }
}

async fn settle_published_content_page(
    context: &IngestStageContext<'_>,
    mutation_item_id: uuid::Uuid,
) {
    if let Err(error) = context
        .state
        .application_services
        .web_ingest
        .settle_materialized_page_for_mutation_item(context.state, mutation_item_id)
        .await
    {
        warn!(job_id = %context.job.id, attempt_id = %context.attempt_id, %mutation_item_id, ?error,
            "content publication committed but web page settlement failed; read-side reconciliation will retry");
    }
}

async fn converge_published_technical_facts(context: &IngestStageContext<'_>) {
    if let Err(error) = context
        .state
        .application_services
        .content
        .converge_document_technical_facts(
            context.state,
            context.document_id,
            Some(context.revision_id),
        )
        .await
    {
        warn!(document_id = %context.document_id, revision_id = %context.revision_id, ?error,
            "post-publication technical fact convergence failed");
    }
}
