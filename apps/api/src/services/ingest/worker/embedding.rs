use std::time::Instant;

use crate::shared::provider_failure::ProviderFailureKind;
use anyhow::Context;
use tracing::warn;

use crate::{
    infra::repositories::catalog_repository,
    services::ingest::service::{INGEST_STAGE_EMBED_CHUNK, RecordStageEventCommand},
    services::query::{error::QueryServiceError, search::EmbedChunksStageOutcome},
};

use super::{
    attempt::IngestStageContext,
    errors::{ContentConfigurationError, ContentProviderQuotaError},
};

pub(super) struct EmbeddingStageOutcome {
    pub(super) source_truth_version: i64,
    pub(super) embedding_profile_key: Option<String>,
}

pub(super) async fn embed_ingest_content(
    context: &IngestStageContext<'_>,
    revision: &crate::infra::knowledge_rows::KnowledgeRevisionRow,
) -> anyhow::Result<EmbeddingStageOutcome> {
    let state = context.state;
    let job = context.job;
    let attempt_id = context.attempt_id;
    let revision_id = context.revision_id;
    let cancellation = &context.cancellation;
    // --- Stage: embed_chunk ---------------------------------------------------
    // Chunk embedding is required for a readable revision. Failure marks
    // vector/graph readiness failed and aborts this attempt; the worker does
    // not continue into graph extraction with a partial vector inventory.
    cancellation.check(job.id)?;
    state
        .application_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand {
                attempt_id,
                stage_name: INGEST_STAGE_EMBED_CHUNK.to_string(),
                stage_state: "started".to_string(),
                message: Some("embedding chunks".to_string()),
                details_json: serde_json::json!({
                    "revisionId": revision_id,
                }),
                provider_kind: None,
                model_name: None,
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
                cached_tokens: None,
                estimated_cost: None,
                currency_code: None,
                elapsed_ms: None,
            },
        )
        .await
        .context("failed to record embed_chunk started stage event")?;

    let vector_write_source_truth_version = catalog_repository::get_library_source_truth_version(
        &state.persistence.postgres,
        revision.library_id,
    )
    .await
    .context("failed to capture embed_chunk source-truth fence")?;
    let embed_chunk_start = Instant::now();
    let embed_chunk_outcome = state
        .application_services
        .search
        .embed_chunks_for_revision(crate::services::query::search::EmbedRevisionChunksInput {
            state,
            library_id: revision.library_id,
            revision_id,
            attempt_id,
            vector_write_source_truth_version,
            cancellation_token: cancellation.token,
        })
        .await;
    let embed_chunk_elapsed_ms = Some(embed_chunk_start.elapsed().as_millis() as i64);
    let mut embed_chunk_failure: Option<anyhow::Error> = None;
    match &embed_chunk_outcome {
        Ok(outcome) => {
            capture_embedding_billing(context, outcome).await;
            state
                .application_services
                .ingest
                .record_stage_event(
                    state,
                    RecordStageEventCommand::for_chunk_embedding(
                        attempt_id,
                        outcome,
                        embed_chunk_elapsed_ms,
                    ),
                )
                .await
                .context("failed to record embed_chunk stage event")?;
        }
        Err(error) => {
            if matches!(error, crate::services::query::error::QueryServiceError::Cancelled) {
                return Err(cancellation.error(job.id));
            }
            // A missing binding is operator configuration, not a transient
            // provider fault: classify it terminally so the document surfaces
            // an actionable reason instead of requeueing forever.
            embed_chunk_failure =
                Some(record_embedding_failure(context, error, embed_chunk_elapsed_ms).await?);
        }
    };
    let embed_chunk_profile_key =
        embed_chunk_outcome.as_ref().ok().and_then(|outcome| outcome.embedding_profile_key.clone());
    drop(embed_chunk_outcome);
    if let Some(failure) = embed_chunk_failure {
        // The embedding service owns exact attempt-ID cleanup. The outer
        // lifecycle boundary atomically publishes failed readiness together
        // with the attempt/job transition. The typed error is propagated as-is
        // so the failure classifier can keep configuration faults terminal.
        return Err(failure);
    }
    Ok(EmbeddingStageOutcome {
        source_truth_version: vector_write_source_truth_version,
        embedding_profile_key: embed_chunk_profile_key,
    })
}

async fn capture_embedding_billing(
    context: &IngestStageContext<'_>,
    outcome: &EmbedChunksStageOutcome,
) {
    let state = context.state;
    let worker_id = context.worker_id;
    let job = context.job;
    if let Some(command) =
        crate::services::ops::billing::CaptureIngestAttemptBillingCommand::for_chunk_embedding(
            job.workspace_id,
            job.library_id,
            context.attempt_id,
            outcome,
        )
        && let Err(e) =
            state.application_services.billing.capture_ingest_attempt(state, command).await
    {
        warn!(%worker_id, job_id = %job.id, ?e, "embed_chunk billing capture failed");
    }
}

async fn record_embedding_failure(
    context: &IngestStageContext<'_>,
    error: &QueryServiceError,
    elapsed_ms: Option<i64>,
) -> anyhow::Result<anyhow::Error> {
    let failure = embedding_failure(error);
    let state = context.state;
    let worker_id = context.worker_id;
    let job = context.job;
    let revision_id = context.revision_id;
    warn!(
        %worker_id,
        job_id = %job.id,
        %revision_id,
        ?error,
        "chunk embedding failed; readiness remains count-gated for retry",
    );
    state
        .application_services
        .ingest
        .record_stage_event(
            state,
            RecordStageEventCommand {
                attempt_id: context.attempt_id,
                stage_name: INGEST_STAGE_EMBED_CHUNK.to_string(),
                stage_state: "failed".to_string(),
                message: Some("chunk embedding failed".to_string()),
                details_json: serde_json::json!({ "error": format!("{error:#}") }),
                provider_kind: None,
                model_name: None,
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
                cached_tokens: None,
                estimated_cost: None,
                currency_code: None,
                elapsed_ms,
            },
        )
        .await
        .context("failed to record embed_chunk failed stage event")?;
    Ok(failure)
}

fn embedding_failure(error: &QueryServiceError) -> anyhow::Error {
    match error {
        QueryServiceError::BindingNotConfigured { message } => {
            anyhow::Error::new(ContentConfigurationError::embedding_binding_not_configured(
                format!("chunk embedding failed: {message}"),
            ))
        }
        QueryServiceError::Provider(failure)
            if failure.kind() == ProviderFailureKind::QuotaExhausted =>
        {
            anyhow::Error::new(ContentProviderQuotaError {
                message: format!("chunk embedding failed: {}", failure.message()),
            })
        }
        _ => anyhow::anyhow!(format!("chunk embedding failed: {error:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ContentConfigurationError, ContentProviderQuotaError, QueryServiceError, embedding_failure,
    };
    use crate::services::{
        ingest::service::RecordStageEventCommand, ops::billing::CaptureIngestAttemptBillingCommand,
        query::search::EmbedChunksStageOutcome,
    };
    use crate::shared::provider_failure::ProviderFailure;
    use uuid::Uuid;

    fn embedding_outcome() -> EmbedChunksStageOutcome {
        EmbedChunksStageOutcome {
            embedding_profile_key: Some("profile".to_string()),
            chunks_embedded: 2,
            chunks_reused: 3,
            usage_json: Some(serde_json::json!({ "tokens": 9 })),
            provider_kind: Some("provider".to_string()),
            model_name: Some("model".to_string()),
            prompt_tokens: Some(7),
            completion_tokens: Some(2),
            total_tokens: Some(9),
        }
    }

    #[test]
    fn embedding_commands_preserve_scope_usage_and_completed_event_metadata() {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let attempt_id = Uuid::now_v7();
        let outcome = embedding_outcome();
        let billing = CaptureIngestAttemptBillingCommand::for_chunk_embedding(
            workspace_id,
            library_id,
            attempt_id,
            &outcome,
        )
        .expect("complete embedding usage must produce billing attribution");
        assert_embedding_billing_attribution(&billing, workspace_id, library_id, attempt_id);

        for elapsed_ms in [None, Some(0), Some(42)] {
            assert_embedding_completed_event(attempt_id, &outcome, elapsed_ms);
        }
    }

    fn assert_embedding_billing_attribution(
        billing: &CaptureIngestAttemptBillingCommand,
        workspace_id: Uuid,
        library_id: Uuid,
        attempt_id: Uuid,
    ) {
        assert_eq!(
            (billing.workspace_id, billing.library_id, billing.attempt_id),
            (workspace_id, library_id, attempt_id)
        );
        assert_eq!(billing.binding_id, None);
        assert_eq!(billing.provider_kind, "provider");
        assert_eq!(billing.model_name, "model");
        assert_eq!(billing.call_kind, "embed_chunk");
        assert_eq!(billing.usage_json, serde_json::json!({ "tokens": 9 }));
    }

    fn assert_embedding_completed_event(
        attempt_id: Uuid,
        outcome: &EmbedChunksStageOutcome,
        elapsed_ms: Option<i64>,
    ) {
        let event = RecordStageEventCommand::for_chunk_embedding(attempt_id, outcome, elapsed_ms);
        assert_eq!(event.attempt_id, attempt_id);
        assert_eq!(event.stage_name, "embed_chunk");
        assert_eq!(event.stage_state, "completed");
        assert_eq!(event.message.as_deref(), Some("chunk embeddings persisted"));
        assert_eq!(
            event.details_json,
            serde_json::json!({
                "chunksEmbedded": 2, "chunksReused": 3,
                "providerKind": "provider", "modelName": "model",
            })
        );
        assert_eq!(event.provider_kind.as_deref(), Some("provider"));
        assert_eq!(event.model_name.as_deref(), Some("model"));
        assert_eq!(
            (event.prompt_tokens, event.completion_tokens, event.total_tokens),
            (Some(7), Some(2), Some(9))
        );
        assert_eq!(
            (event.cached_tokens, event.estimated_cost, event.currency_code),
            (None, None, None)
        );
        assert_eq!(event.elapsed_ms, elapsed_ms);
    }

    #[test]
    fn embedding_billing_requires_all_three_optional_usage_fields() {
        let scope = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        for missing in 0..3 {
            let mut outcome = embedding_outcome();
            match missing {
                0 => outcome.provider_kind = None,
                1 => outcome.model_name = None,
                _ => outcome.usage_json = None,
            }
            assert!(
                CaptureIngestAttemptBillingCommand::for_chunk_embedding(
                    scope.0, scope.1, scope.2, &outcome,
                )
                .is_none()
            );
        }
    }

    #[test]
    fn embedding_failure_preserves_terminal_configuration_and_quota_types() {
        let configuration = embedding_failure(&QueryServiceError::BindingNotConfigured {
            message: "missing binding".to_string(),
        });
        assert!(configuration.downcast_ref::<ContentConfigurationError>().is_some());
        assert_eq!(configuration.to_string(), "chunk embedding failed: missing binding");

        let quota = embedding_failure(&QueryServiceError::Provider(
            ProviderFailure::quota_exhausted("budget exhausted"),
        ));
        assert!(quota.downcast_ref::<ContentProviderQuotaError>().is_some());
        assert_eq!(quota.to_string(), "chunk embedding failed: budget exhausted");
    }

    #[test]
    fn embedding_failure_keeps_unavailable_provider_errors_unclassified() {
        let error = embedding_failure(&QueryServiceError::Provider(ProviderFailure::unavailable(
            "provider offline",
        )));
        assert!(error.downcast_ref::<ContentConfigurationError>().is_none());
        assert!(error.downcast_ref::<ContentProviderQuotaError>().is_none());
        assert_eq!(
            error.to_string(),
            "chunk embedding failed: query provider unavailable: provider offline"
        );
    }
}
