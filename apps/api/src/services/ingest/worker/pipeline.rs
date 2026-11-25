use std::time::Instant;

use anyhow::Context;
use chrono::Utc;
use tracing::{Instrument as _, info, warn};

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
    let Some(revision) = prepare_and_publish_base(&context).await? else {
        return Ok(ContentPipelineOutcome::LifecyclePublished);
    };

    // The requested plan is resolved from the persisted mutation identity,
    // never from the live request, and strictly before the first
    // provider-capable stage: a plan that cannot be read or parsed fails the
    // attempt before any embedding request, reservation or charge exists.
    let enrichment_scope =
        crate::services::ingest::service::recorded_enrichment_scope(context.state, context.job)
            .await?;

    let embedding = Box::pin(embed_ingest_content(&context, &revision))
        .instrument(tracing::info_span!("ingest.embed", "ingest.stage" = "embed"))
        .await?;

    // Summary generation and execution-profile validation may need the main
    // pool/provider stack, so both must finish before graph reconcile acquires
    // its long heartbeat-pool publication guard. The summary itself remains
    // uncommitted until the exact attempt publication below.
    //
    // A bounded plan performs no summary and no graph work at all: the base
    // text publication stays the readable result, the graph state keeps the
    // value base publication wrote, and no provider is called beyond the
    // embedding stage the plan names.
    let (document_summary, graph) =
        run_enrichment_stages(&context, &revision, &embedding, enrichment_scope).await?;

    finalize_content_enrichment(&context, embedding, document_summary, graph, enrichment_scope)
        .instrument(tracing::info_span!("ingest.finalize", "ingest.stage" = "finalize"))
        .await
}

/// Prepares the revision and publishes its permitted base text. `None` means the
/// visibility decision withheld the revision, so nothing else will ever run for
/// it.
async fn prepare_and_publish_base(
    context: &IngestStageContext<'_>,
) -> anyhow::Result<Option<crate::infra::knowledge_rows::KnowledgeRevisionRow>> {
    let PreparedContent { revision } = Box::pin(prepare_ingest_content(context))
        .instrument(tracing::info_span!("ingest.prepare", "ingest.stage" = "prepare"))
        .await?;
    // The permitted prepared text becomes readable here, before any optional
    // enrichment. A reader may wait for a lexical/text answer; it may not wait for
    // an embedding provider, a summary model or graph extraction.
    let base_publication = publish_content_base(context)
        .instrument(tracing::info_span!("ingest.publish_base", "ingest.stage" = "publish_base"))
        .await?;
    // The visibility-intent decision withheld this revision, so it can never be
    // exposed and has no enrichment to do. Base publication already completed
    // its attempt and job.
    Ok(base_publication.publicly_visible.then_some(revision))
}

/// Runs the provider-capable enrichment stages the recorded plan allows, in the
/// order the plan requires.
async fn run_enrichment_stages(
    context: &IngestStageContext<'_>,
    revision: &crate::infra::knowledge_rows::KnowledgeRevisionRow,
    embedding: &EmbeddingStageOutcome,
    enrichment_scope: crate::domains::content::EnrichmentScope,
) -> anyhow::Result<(Option<String>, GraphExtractionOutcome)> {
    match enrichment_scope {
        crate::domains::content::EnrichmentScope::Full => {
            let document_summary = prepare_document_summary(context)
                .instrument(tracing::info_span!("ingest.summary", "ingest.stage" = "summary"))
                .await;
            validate_embedding_execution_profile(
                context,
                revision.library_id,
                embedding.embedding_profile_key.as_deref(),
            )
            .await?;
            let graph = extract_revision_graph(context, revision)
                .instrument(tracing::info_span!("ingest.graph", "ingest.stage" = "graph"))
                .await?;
            Ok((document_summary, graph))
        }
        crate::domains::content::EnrichmentScope::TextAndEmbedding => {
            validate_embedding_execution_profile(
                context,
                revision.library_id,
                embedding.embedding_profile_key.as_deref(),
            )
            .await?;
            Ok((None, GraphExtractionOutcome::not_requested()))
        }
    }
}

/// The graph state base publication already wrote for this revision.
///
/// A bounded plan must not relabel the revision: the value stays exactly what
/// the publication stage stored, so an unrun graph stage is not reported as a
/// degraded one. The row is the owner of that value, so it is read, not guessed.
async fn published_graph_state(
    context: &IngestStageContext<'_>,
    revision_id: uuid::Uuid,
) -> anyhow::Result<String> {
    sqlx::query_scalar::<_, String>(
        "select graph_state from knowledge_revision where revision_id = $1 and library_id = $2",
    )
    .bind(revision_id)
    .bind(context.job.library_id)
    .fetch_optional(&context.state.persistence.postgres)
    .await?
    .ok_or_else(|| anyhow::anyhow!("published revision readiness row is missing"))
}

/// The base-publication outcome the pipeline still needs: whether the revision
/// may be enriched at all.
struct BasePublication {
    publicly_visible: bool,
}

/// Publishes the prepared revision's allowed text, structure and lexical
/// material on the same readable head the enrichment finalizer later decorates.
async fn publish_content_base(context: &IngestStageContext<'_>) -> anyhow::Result<BasePublication> {
    let IngestStageContext { state, job, attempt_id, document_id, revision_id, .. } = *context;
    let mutation_id = job.mutation_id.context("content ingest job is missing mutation_id")?;
    let mutation_item_id =
        job.mutation_item_id.context("content ingest job is missing mutation_item_id")?;
    let outcome = ingest_repository::publish_content_ingest_base(
        &state.persistence.postgres,
        &ingest_repository::PublishContentIngestBase {
            workspace_id: job.workspace_id,
            library_id: job.library_id,
            document_id,
            revision_id,
            mutation_id,
            mutation_item_id,
            attempt_id,
            completed_at: Utc::now(),
        },
    )
    .await
    .context("failed to publish the prepared content base")?;
    match outcome {
        ingest_repository::PublishContentIngestSuccessOutcome::Applied {
            publicly_visible, ..
        } => {
            info!(
                attempt_id = %attempt_id,
                revision_id = %revision_id,
                publicly_visible,
                "prepared content base published",
            );
            Ok(BasePublication { publicly_visible })
        }
        ingest_repository::PublishContentIngestSuccessOutcome::AuthorityLost { .. } => {
            Err(anyhow::Error::new(JobLeaseLost { job_id: job.id }))
        }
    }
}

async fn finalize_content_enrichment(
    context: &IngestStageContext<'_>,
    embedding: EmbeddingStageOutcome,
    document_summary: Option<String>,
    graph: GraphExtractionOutcome,
    enrichment_scope: crate::domains::content::EnrichmentScope,
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

    // --- Stage: finalize enrichment ------------------------------------------
    let finalizing_start = Instant::now();

    let mutation_id = job.mutation_id.context("content ingest job is missing mutation_id")?;
    let mutation_item_id =
        job.mutation_item_id.context("content ingest job is missing mutation_item_id")?;
    let now = Utc::now();
    // A bounded plan says nothing about the graph: the published base value
    // stays, and no readiness or degradation vocabulary is invented for it.
    let graph_state = match enrichment_scope {
        crate::domains::content::EnrichmentScope::Full => {
            if graph_degraded {
                GRAPH_STATE_DEGRADED.to_owned()
            } else {
                graph_state_after_successful_extract(graph_ready).to_owned()
            }
        }
        crate::domains::content::EnrichmentScope::TextAndEmbedding => {
            published_graph_state(context, revision_id).await?
        }
    };
    let enrichment_input = ingest_repository::FinalizeContentIngestEnrichment {
        workspace_id: job.workspace_id,
        library_id: job.library_id,
        document_id,
        revision_id,
        mutation_id,
        mutation_item_id,
        attempt_id,
        expected_source_truth_version: vector_write_source_truth_version,
        embedding_profile_key: embed_chunk_profile_key,
        graph_state,
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
        details_json: serde_json::json!({
            "enrichmentScope": match enrichment_scope {
                crate::domains::content::EnrichmentScope::Full => "full",
                crate::domains::content::EnrichmentScope::TextAndEmbedding => "text_and_embedding",
            },
            "enrichmentOutcome": match enrichment_scope {
                crate::domains::content::EnrichmentScope::Full => "full",
                crate::domains::content::EnrichmentScope::TextAndEmbedding => "not_requested",
            },
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
        // The published base text and the truthful pending channel states stay
        // exactly as they are: a cancel stops enrichment, it does not revoke a
        // readable document.
        return Err(cancellation_error);
    }

    let publication = if let Some(publication_guard) = publication_guard {
        ingest_repository::finalize_content_ingest_enrichment_with_graph_guard(
            publication_guard,
            &enrichment_input,
            &effects,
        )
        .await
    } else {
        ingest_repository::finalize_content_ingest_enrichment(
            &state.persistence.postgres,
            &enrichment_input,
            &effects,
            state,
        )
        .await
    }
    .context("failed to atomically finalize content ingest enrichment")?;
    let publicly_visible = match publication {
        ingest_repository::PublishContentIngestSuccessOutcome::Applied {
            publicly_visible, ..
        } => publicly_visible,
        ingest_repository::PublishContentIngestSuccessOutcome::AuthorityLost { .. } => {
            // The guarded finalization already rolled its staged graph writes back
            // when authority moved; there is nothing left to publish or undo.
            return Err(anyhow::Error::new(JobLeaseLost { job_id: job.id }));
        }
    };

    refresh_published_content_summary(
        context,
        job.library_id,
        publication,
        pending_summary_refresh,
    )
    .await;

    invalidate_library_embedding_profile_inventory(job.library_id);
    settle_published_content_page(context, mutation_item_id).await;
    converge_published_technical_facts(context).await;

    log_content_enrichment(context, publicly_visible);
    Ok(ContentPipelineOutcome::LifecyclePublished)
}

/// Runs the one-transaction publication and its deferred effects over already
/// retained extraction/vector results. No preparation or provider stage is
/// invoked.
///
/// This test seam keeps the pre-split single-transaction contract (including the
/// staged-graph discard when the visibility decision withholds a revision) so
/// graph-atomicity cases can drive reconcile and publication together. The
/// product pipelines publish the base revision first and finalize enrichment
/// afterwards instead.
#[cfg(feature = "test-support")]
pub async fn finalize_content_readiness_for_test(
    state: &crate::app::state::AppState,
    job: &ingest_repository::IngestJobRow,
    input: &ingest_repository::PublishContentIngestSuccess,
    effects: ingest_repository::ContentIngestPublicationEffects,
    authorized: crate::services::graph::service::AuthorizedRevisionGraphReconcile,
) -> anyhow::Result<()> {
    let cancellation_token = tokio_util::sync::CancellationToken::new();
    let requested = std::sync::atomic::AtomicBool::new(false);
    let context = IngestStageContext {
        state,
        worker_id: "prepared-content-completion-test",
        job,
        attempt_id: input.attempt_id,
        document_id: input.document_id,
        revision_id: input.revision_id,
        cancellation: super::errors::PipelineCancellation {
            token: &cancellation_token,
            user_requested: &requested,
            operator_pause_requested: &requested,
            lease_lost_requested: &requested,
        },
    };
    let outcome = authorized.outcome?;
    let pending_summary_refresh = outcome.pending_summary_refresh.clone();
    let publication =
        ingest_repository::publish_content_ingest_success_revision_fenced_with_graph_guard(
            authorized.publication_guard,
            input,
            &effects,
        )
        .await
        .context("failed to atomically publish retained content ingest success")?;
    refresh_published_content_summary(
        &context,
        job.library_id,
        publication,
        pending_summary_refresh,
    )
    .await;
    invalidate_library_embedding_profile_inventory(job.library_id);
    settle_published_content_page(&context, input.mutation_item_id).await;
    converge_published_technical_facts(&context).await;
    Ok(())
}

fn log_content_enrichment(context: &IngestStageContext<'_>, publicly_visible: bool) {
    info!(
        attempt_id = %context.attempt_id,
        revision_id = %context.revision_id,
        publicly_visible,
        "content ingest enrichment finalized",
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
    publication: ingest_repository::PublishContentIngestSuccessOutcome,
    pending_summary_refresh: Option<crate::services::graph::summary::PendingGraphSummaryRefresh>,
) {
    let Some(source_truth_version) = publication.published_source_truth_version() else {
        return;
    };
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
