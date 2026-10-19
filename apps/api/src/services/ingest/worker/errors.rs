use std::sync::atomic::{AtomicBool, Ordering};

use thiserror::Error;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::limits::HeavyRevisionPipelinePermitTimeout;
use crate::{
    domains::ops::OpsFailureDisposition,
    integrations::{
        provider_budget::ProviderBudgetError,
        retry::{ProviderCallError, ProviderRateLimit},
    },
    services::content::error::ContentServiceError,
    services::ingest::cancellation::anyhow_is_cancelled,
    shared::extraction::file_extract::UploadAdmissionError,
};

#[derive(Debug)]
pub(super) struct ContentExtractionError {
    pub(super) failure_code: String,
    /// The one classification of this failure. The worker requeues exactly the
    /// `RetryExhausted` outcome; retryability and content-ness are read from
    /// here, never from `failure_code` text (which no longer separates a
    /// rejected payload from an operational fault).
    pub(super) failure_disposition: OpsFailureDisposition,
    message: String,
}

#[derive(Debug, Error)]
#[error("document {document_id} was deleted before ingest could run")]
pub(super) struct DeletedDocumentJobSkipped {
    pub(super) document_id: Uuid,
}

/// Raised from inside the pipeline when the heartbeat observer notices that
/// the job has been transitioned to `queue_state='canceled'` by the cancel
/// endpoint (`cancel_jobs_for_document`). The cancel path has already marked
/// the job and leased attempt as canceled, so the worker only stops cleanly
/// and must not rewrite the terminal cancellation state.
#[derive(Debug, Error)]
#[error("ingest job {job_id} was canceled by user request")]
pub(super) struct JobCanceledByRequest {
    pub(super) job_id: Uuid,
}

#[derive(Debug, Error)]
#[error("ingest job {job_id} was paused by operator request")]
pub(super) struct JobPausedByOperator {
    pub(super) job_id: Uuid,
}

#[derive(Debug, Error)]
#[error("ingest job {job_id} stopped because worker shutdown was requested")]
pub(super) struct JobCanceledByShutdown {
    pub(super) job_id: Uuid,
}

#[derive(Debug, Error)]
#[error("ingest job {job_id} lost its active attempt lease")]
pub(super) struct JobLeaseLost {
    pub(super) job_id: Uuid,
}

/// Carries structural cleanup intent across the pipeline boundary without
/// inferring it from provider text. Only lifecycle invalidation after a
/// successful vector write requests revision-wide deletion.
#[derive(Debug, Error)]
#[error("{source:#}")]
pub(super) struct ContentPipelineFailure {
    pub(super) delete_vectors: bool,
    #[source]
    pub(super) source: anyhow::Error,
}

/// Raised when the pipeline cannot proceed because required operator
/// configuration is absent (for example no active `EmbedChunk` binding).
///
/// This is deliberately distinct from a transient provider fault: retrying
/// cannot succeed until an operator changes the configuration, so the attempt
/// is finalized terminally with an actionable failure code instead of being
/// requeued forever behind a silently repeating warning.
#[derive(Debug, Error)]
#[error("{message}")]
pub(super) struct ContentConfigurationError {
    failure_code: String,
    message: String,
}

impl ContentConfigurationError {
    const FAILURE_CLASS: &'static str = "content_configuration";

    pub(super) fn embedding_binding_not_configured(message: impl std::fmt::Display) -> Self {
        Self {
            failure_code: "embedding_binding_not_configured".to_string(),
            message: message.to_string(),
        }
    }
}

/// Raised when the provider formally reports exhausted billing quota
/// (`insufficient_quota`). Not terminal: the failure is operational and
/// self-healing once an operator tops up billing, so the owning job is parked
/// for a long cooldown and requeued without consuming its attempt budget.
/// Retrying on the ordinary backoff would only hammer the provider.
#[derive(Debug, Error)]
#[error("{message}")]
pub(super) struct ContentProviderQuotaError {
    pub(super) message: String,
}

impl ContentProviderQuotaError {
    const FAILURE_CLASS: &'static str = "provider";
    const FAILURE_CODE: &'static str = "provider_quota_exhausted";
}

/// How a failed content-ingest attempt is recorded: which class and code the
/// operator sees, whether the job may be requeued, and the disposition the
/// operation carries if this failure terminates it.
///
/// The disposition is computed here, next to the retryability that determines
/// it, and is then carried unchanged to the operation finalisation. It is never
/// recomputed from `failure_code` text downstream, so the stored disposition
/// and the retry decision cannot disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ContentIngestFailureClassification {
    pub(super) failure_class: &'static str,
    pub(super) failure_code: String,
    /// Disposition the operation carries once this failure is terminal:
    /// `RetryExhausted` for a retryable failure (applied after its attempt
    /// budget is exhausted, or never when the failure carries an
    /// `operational_defer`), `ContentUnsupported` for a deterministic content
    /// rejection, `OperationalTerminal` for every other terminal failure.
    pub(super) failure_disposition: OpsFailureDisposition,
    /// When set, the failure is an operational provider rate-limit (HTTP 429 or
    /// exhausted quota). The worker requeues the job after this delay without
    /// consuming its attempt budget, so a temporary provider outage can never
    /// permanently fail a revision. `None` for every ordinary failure.
    pub(super) operational_defer: Option<chrono::Duration>,
}

/// Cooldown applied to a provider rate-limit (HTTP 429) that carried no
/// `Retry-After`: long enough to leave the provider's current window.
const DEFAULT_PROVIDER_RATE_LIMIT_COOLDOWN: std::time::Duration =
    std::time::Duration::from_secs(60);

/// Cooldown applied to an exhausted provider quota. Quota heals only when an
/// operator tops up billing, so the job is parked well past the request-window
/// timescale instead of being terminally failed.
const PROVIDER_QUOTA_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(900);

/// Converts a provider cooldown to the domain's `chrono::Duration` at the one
/// boundary where the two clocks meet, bounding a value that cannot be
/// represented exactly (only reachable far beyond any sane cooldown).
fn operational_defer_to_chrono(delay: std::time::Duration) -> chrono::Duration {
    chrono::Duration::from_std(delay).unwrap_or_else(|_| chrono::Duration::days(1))
}

impl ContentIngestFailureClassification {
    /// The one source of truth for retryability. `RetryExhausted` is exactly
    /// the disposition of a failure the worker may requeue.
    #[must_use]
    pub(super) const fn is_retryable(&self) -> bool {
        matches!(self.failure_disposition, OpsFailureDisposition::RetryExhausted)
    }
}

/// Decides the terminal-vs-retryable outcome for a content-ingest failure.
///
/// Classified extraction faults carry their own retry decision. Missing
/// operator configuration is always terminal, because no number of retries can
/// supply a binding. Everything else stays retryable so transient provider and
/// infrastructure faults still recover on their own.
pub(super) fn classify_content_ingest_failure(
    error: &anyhow::Error,
) -> ContentIngestFailureClassification {
    // A provider 429 (rate limit or exhausted quota) is operational and
    // self-healing. It is neither a content rejection nor a terminal fault: run
    // it again after the provider's cooldown and it succeeds. It must not
    // consume the attempt budget, so the classification carries an explicit
    // deferral and the worker requeues the job without ever exhausting it.
    if let Some(rate_limit) = provider_rate_limit_error(error) {
        return ContentIngestFailureClassification {
            failure_class: "provider",
            failure_code: "provider_rate_limited".to_string(),
            failure_disposition: OpsFailureDisposition::RetryExhausted,
            operational_defer: Some(operational_defer_to_chrono(
                rate_limit
                    .retry_after
                    .filter(|delay| !delay.is_zero())
                    .unwrap_or(DEFAULT_PROVIDER_RATE_LIMIT_COOLDOWN),
            )),
        };
    }
    if provider_admission_error(error).is_some() {
        return ContentIngestFailureClassification {
            failure_class: "provider",
            failure_code: "provider_concurrency_unavailable".to_string(),
            failure_disposition: OpsFailureDisposition::RetryExhausted,
            operational_defer: None,
        };
    }
    if error.downcast_ref::<HeavyRevisionPipelinePermitTimeout>().is_some() {
        return ContentIngestFailureClassification {
            failure_class: "ingest_admission",
            failure_code: "heavy_revision_permit_timeout".to_string(),
            failure_disposition: OpsFailureDisposition::RetryExhausted,
            operational_defer: None,
        };
    }
    if let Some(failure) = error.downcast_ref::<ContentExtractionError>() {
        return ContentIngestFailureClassification {
            failure_class: "content_extract",
            failure_code: failure.failure_code.clone(),
            failure_disposition: failure.failure_disposition,
            operational_defer: None,
        };
    }
    if let Some(failure) = error.downcast_ref::<ContentConfigurationError>() {
        return ContentIngestFailureClassification {
            failure_class: ContentConfigurationError::FAILURE_CLASS,
            failure_code: failure.failure_code.clone(),
            failure_disposition: OpsFailureDisposition::OperationalTerminal,
            operational_defer: None,
        };
    }
    if error.downcast_ref::<ContentProviderQuotaError>().is_some() {
        // Quota does not replenish on a retry timescale, but it also must not
        // permanently fail the revision: park the job for a long cooldown so it
        // resumes by itself once an operator tops up billing.
        return ContentIngestFailureClassification {
            failure_class: ContentProviderQuotaError::FAILURE_CLASS,
            failure_code: ContentProviderQuotaError::FAILURE_CODE.to_string(),
            failure_disposition: OpsFailureDisposition::RetryExhausted,
            operational_defer: Some(operational_defer_to_chrono(PROVIDER_QUOTA_COOLDOWN)),
        };
    }
    ContentIngestFailureClassification {
        failure_class: "worker_error",
        failure_code: "\u{63}anonical_pipeline_failed".to_string(),
        failure_disposition: OpsFailureDisposition::RetryExhausted,
        operational_defer: None,
    }
}

/// Finds the typed operational rate-limit signal in an error chain, unwrapping
/// the content-service boundary the graph-extraction path crosses.
pub(super) fn provider_rate_limit_error(error: &anyhow::Error) -> Option<ProviderRateLimit> {
    if let Some(rate_limit) = error.downcast_ref::<ProviderRateLimit>() {
        return Some(rate_limit.clone());
    }
    if let Some(ContentServiceError::Internal(source)) = error.downcast_ref::<ContentServiceError>()
    {
        return provider_rate_limit_error(source);
    }
    None
}

fn provider_admission_error(error: &anyhow::Error) -> Option<&ProviderBudgetError> {
    if let Some(error) = error.downcast_ref::<ProviderBudgetError>() {
        return Some(error);
    }
    if let Some(error) = error.downcast_ref::<ProviderCallError>() {
        return error.admission_error();
    }
    if let Some(ContentServiceError::Internal(source)) = error.downcast_ref::<ContentServiceError>()
    {
        return provider_admission_error(source);
    }
    None
}

pub(super) fn is_provider_lane_acquisition_failure(error: &ContentServiceError) -> bool {
    match error {
        ContentServiceError::Internal(source) => provider_admission_error(source).is_some(),
        _ => false,
    }
}

pub(super) fn job_cancellation_error(
    job_id: Uuid,
    user_cancel_requested: &AtomicBool,
    operator_pause_requested: &AtomicBool,
    lease_lost_requested: &AtomicBool,
) -> anyhow::Error {
    if user_cancel_requested.load(Ordering::Relaxed) {
        anyhow::Error::new(JobCanceledByRequest { job_id })
    } else if operator_pause_requested.load(Ordering::Relaxed) {
        anyhow::Error::new(JobPausedByOperator { job_id })
    } else if lease_lost_requested.load(Ordering::Relaxed) {
        anyhow::Error::new(JobLeaseLost { job_id })
    } else {
        anyhow::Error::new(JobCanceledByShutdown { job_id })
    }
}

pub(super) fn check_job_cancellation(
    cancellation_token: &CancellationToken,
    user_cancel_requested: &AtomicBool,
    operator_pause_requested: &AtomicBool,
    lease_lost_requested: &AtomicBool,
    job_id: Uuid,
) -> anyhow::Result<()> {
    if cancellation_token.is_cancelled() {
        Err(job_cancellation_error(
            job_id,
            user_cancel_requested,
            operator_pause_requested,
            lease_lost_requested,
        ))
    } else {
        Ok(())
    }
}

pub(super) fn map_stage_error(
    error: anyhow::Error,
    user_cancel_requested: &AtomicBool,
    operator_pause_requested: &AtomicBool,
    lease_lost_requested: &AtomicBool,
    job_id: Uuid,
    context: &'static str,
) -> anyhow::Error {
    if anyhow_is_cancelled(&error) {
        job_cancellation_error(
            job_id,
            user_cancel_requested,
            operator_pause_requested,
            lease_lost_requested,
        )
    } else {
        error.context(context)
    }
}

impl ContentExtractionError {
    pub(super) fn missing_stored_source(job_id: Uuid, revision_id: Uuid) -> Self {
        Self {
            failure_code: "missing_stored_source".to_string(),
            failure_disposition: OpsFailureDisposition::OperationalTerminal,
            message: format!(
                "ingest job {job_id}: revision {revision_id} has no normalized_text and no stored source bytes",
            ),
        }
    }

    pub(super) fn stored_source_read(storage_ref: &str, error: impl std::fmt::Display) -> Self {
        Self {
            failure_code: "stored_source_unavailable".to_string(),
            failure_disposition: OpsFailureDisposition::OperationalTerminal,
            message: format!("failed to read stored source {storage_ref}: {error}"),
        }
    }

    /// Maps a rejected extraction plan to its disposition from the typed class
    /// the producer assigned. Only a deterministic content rejection becomes
    /// `content_unsupported`; an operational fault becomes retryable or
    /// operational-terminal and is never published as a content rejection. A
    /// request-level rejection (`None`: malformed/truncated multipart body,
    /// oversized upload) carries no content verdict and is operational-terminal.
    /// The `failure_code` stays the admission `error_kind`, so one code (for
    /// example `upload_extraction_failed`) may now carry two dispositions.
    pub(super) fn from_upload_rejection(rejection: &UploadAdmissionError) -> Self {
        Self {
            failure_code: rejection.error_kind().to_string(),
            failure_disposition: OpsFailureDisposition::from_extraction_failure_class(
                rejection.extraction_failure_class(),
            ),
            message: rejection.message().to_string(),
        }
    }

    pub(super) fn extraction_failed(failure_code: &str, message: impl std::fmt::Display) -> Self {
        Self {
            failure_code: failure_code.to_string(),
            failure_disposition: OpsFailureDisposition::RetryExhausted,
            message: message.to_string(),
        }
    }

    /// A terminal extraction fault whose cause is storage or worker resources
    /// (for example a docling OOM code), not a rejected content admission.
    pub(super) fn extraction_failed_terminal(
        failure_code: &str,
        message: impl std::fmt::Display,
    ) -> Self {
        Self {
            failure_code: failure_code.to_string(),
            failure_disposition: OpsFailureDisposition::OperationalTerminal,
            message: message.to_string(),
        }
    }
}

impl std::fmt::Display for ContentExtractionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ContentExtractionError {}

#[derive(Clone, Copy)]
pub(super) struct PipelineCancellation<'a> {
    pub(super) token: &'a CancellationToken,
    pub(super) user_requested: &'a AtomicBool,
    pub(super) operator_pause_requested: &'a AtomicBool,
    pub(super) lease_lost_requested: &'a AtomicBool,
}

impl PipelineCancellation<'_> {
    pub(super) fn check(&self, job_id: Uuid) -> anyhow::Result<()> {
        check_job_cancellation(
            self.token,
            self.user_requested,
            self.operator_pause_requested,
            self.lease_lost_requested,
            job_id,
        )
    }

    pub(super) fn error(&self, job_id: Uuid) -> anyhow::Error {
        job_cancellation_error(
            job_id,
            self.user_requested,
            self.operator_pause_requested,
            self.lease_lost_requested,
        )
    }
}
