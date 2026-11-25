use std::time::Instant;

use crate::shared::provider_failure::ProviderFailureKind;
use anyhow::Context;
use tracing::warn;

use crate::{
    infra::repositories::catalog_repository,
    services::ingest::service::{INGEST_STAGE_EMBED_CHUNK, RecordStageEventCommand},
    services::query::error::QueryServiceError,
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
    let embed_chunk_outcome =
        Box::pin(state.application_services.search.embed_chunks_for_revision(
            crate::services::query::search::EmbedRevisionChunksInput {
                state,
                library_id: revision.library_id,
                revision_id,
                attempt_id,
                vector_write_source_truth_version,
                cancellation_token: cancellation.token,
            },
        ))
        .await;
    let embed_chunk_elapsed_ms = Some(embed_chunk_start.elapsed().as_millis() as i64);
    let mut embed_chunk_failure: Option<anyhow::Error> = None;
    match &embed_chunk_outcome {
        Ok(outcome) => {
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
                details_json: serde_json::json!({
                    "error": format!("{error:#}"),
                    "providerCallId": error.provider_call_id(),
                    "providerDetailRedacted": error.provider_failure()
                        .and_then(|failure| failure.detail_redacted()),
                }),
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
    if let Some(failure) = error.provider_failure() {
        match failure.kind() {
            ProviderFailureKind::QuotaExhausted | ProviderFailureKind::AllowanceExhausted => {
                return anyhow::Error::new(ContentProviderQuotaError {
                    message: format!("chunk embedding failed: {}", failure.message()),
                });
            }
            ProviderFailureKind::RequestRejected | ProviderFailureKind::AdmissionRefused => {
                return anyhow::Error::new(failure.clone());
            }
            ProviderFailureKind::Unavailable => {}
        }
    }
    match error {
        QueryServiceError::BindingNotConfigured { message } => {
            anyhow::Error::new(ContentConfigurationError::embedding_binding_not_configured(
                format!("chunk embedding failed: {message}"),
            ))
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
        ingest::service::RecordStageEventCommand, query::search::EmbedChunksStageOutcome,
    };
    use crate::shared::provider_failure::ProviderFailure;
    use uuid::Uuid;

    #[test]
    fn rejected_provider_requests_are_terminal_and_metadata_is_safe() {
        use crate::domains::ops::OpsFailureDisposition;
        use crate::integrations::retry::{ProviderCallError, provider_http_status_error};
        use crate::services::ingest::worker::errors::classify_content_ingest_failure;
        use reqwest::{StatusCode, header::HeaderMap};

        let body = r#"{"error":{"type":"invalid_request_error","code":"context_length_exceeded","param":"input","message":"secret-canary"}}"#;
        let remote =
            provider_http_status_error("openai", StatusCode::BAD_REQUEST, &HeaderMap::new(), body);
        let detail = remote.redacted_detail();
        assert!(
            detail.contains("type=invalid_request_error code=context_length_exceeded param=input")
        );
        assert!(!format!("{remote:?} {remote} {detail}").contains("secret-canary"));
        let query = QueryServiceError::from(remote);
        let worker_error = embedding_failure(&query);
        let disposition = classify_content_ingest_failure(&worker_error);
        assert_eq!(disposition.failure_disposition, OpsFailureDisposition::OperationalTerminal);
        assert!(!disposition.is_retryable());
        assert!(disposition.operational_defer.is_none());

        let local = ProviderCallError::local_request_rejected();
        assert_eq!(local.redacted_detail(), "origin=local request_rejected");
        assert!(
            !classify_content_ingest_failure(&embedding_failure(&QueryServiceError::from(local)))
                .is_retryable()
        );

        let unknown = provider_http_status_error(
            "openai",
            StatusCode::BAD_REQUEST,
            &HeaderMap::new(),
            r#"{"error":{"type":"secret-canary","code":"secret-canary","param":"secret-canary"}}"#,
        );
        assert!(unknown.redacted_detail().contains("type=unknown code=unknown param=unknown"));
        assert!(!format!("{unknown:?} {unknown}").contains("secret-canary"));
        let rate = provider_http_status_error(
            "openai",
            StatusCode::TOO_MANY_REQUESTS,
            &HeaderMap::new(),
            "{}",
        );
        assert_eq!(
            ProviderFailure::from(rate).kind(),
            crate::shared::provider_failure::ProviderFailureKind::Unavailable
        );
        let transport = ProviderCallError::timeout("synthetic");
        assert_eq!(
            ProviderFailure::from(transport).kind(),
            crate::shared::provider_failure::ProviderFailureKind::Unavailable
        );
    }

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
    fn embedding_completed_event_preserves_usage_and_scope_metadata() {
        let attempt_id = Uuid::now_v7();
        let outcome = embedding_outcome();
        for elapsed_ms in [None, Some(0), Some(42)] {
            assert_embedding_completed_event(attempt_id, &outcome, elapsed_ms);
        }
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

        let reserved_quota = embedding_failure(&QueryServiceError::EmbeddingProvider {
            provider_call_id: Uuid::from_u128(42),
            failure: ProviderFailure::quota_exhausted("budget exhausted"),
        });
        assert!(reserved_quota.downcast_ref::<ContentProviderQuotaError>().is_some());
        assert_eq!(reserved_quota.to_string(), "chunk embedding failed: budget exhausted");
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
