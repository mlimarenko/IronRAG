use super::errors::{
    ContentConfigurationError, ContentExtractionError, ContentProviderQuotaError,
    classify_content_ingest_failure,
};
use super::limits::HeavyRevisionPipelinePermitTimeout;
use crate::{
    domains::ops::OpsFailureDisposition,
    integrations::{provider_budget::ProviderBudgetError, retry::ProviderRateLimit},
    services::content::error::ContentServiceError,
    shared::extraction::file_extract::{
        ExtractionFailureClass, FileExtractError, UploadAdmissionError, UploadFileKind,
    },
};
use uuid::Uuid;

#[test]
fn missing_embedding_binding_is_terminal_and_actionable() {
    let error = anyhow::Error::new(ContentConfigurationError::embedding_binding_not_configured(
        "chunk embedding failed: active embed_chunk binding is not configured",
    ));

    let classification = classify_content_ingest_failure(&error);

    assert_eq!(classification.failure_class, "content_configuration");
    assert_eq!(classification.failure_code, "embedding_binding_not_configured");
    assert_eq!(
        classification.failure_disposition,
        OpsFailureDisposition::OperationalTerminal,
        "absent configuration is terminal, and it is not a content rejection"
    );
    assert!(!classification.is_retryable());
}

#[test]
fn provider_quota_exhaustion_defers_without_consuming_the_attempt_budget() {
    let error = anyhow::Error::new(ContentProviderQuotaError {
        message: "chunk embedding failed: provider quota exhausted".to_string(),
    });

    let classification = classify_content_ingest_failure(&error);

    assert_eq!(classification.failure_class, "provider");
    assert_eq!(classification.failure_code, "provider_quota_exhausted");
    assert_eq!(
        classification.failure_disposition,
        OpsFailureDisposition::RetryExhausted,
        "billing quota is operational and self-healing: the job is parked, not terminally failed"
    );
    assert!(classification.is_retryable());
    assert_eq!(classification.operational_defer, Some(chrono::Duration::seconds(900)));
}

#[test]
fn provider_rate_limit_defers_for_the_advertised_retry_after() {
    let error = anyhow::Error::new(ProviderRateLimit::new(
        "provider-alpha",
        Some(std::time::Duration::from_secs(42)),
        "provider rate limited; response details were redacted",
    ));

    let classification = classify_content_ingest_failure(&error);

    assert_eq!(classification.failure_class, "provider");
    assert_eq!(classification.failure_code, "provider_rate_limited");
    assert_eq!(classification.failure_disposition, OpsFailureDisposition::RetryExhausted);
    assert!(classification.is_retryable());
    assert_eq!(classification.operational_defer, Some(chrono::Duration::seconds(42)));
}

#[test]
fn provider_rate_limit_without_retry_after_uses_the_default_cooldown() {
    let error = anyhow::Error::new(ProviderRateLimit::new(
        "provider-alpha",
        None,
        "provider rate limited; response details were redacted",
    ));

    let classification = classify_content_ingest_failure(&error);

    assert_eq!(classification.failure_code, "provider_rate_limited");
    assert!(classification.is_retryable());
    assert_eq!(classification.operational_defer, Some(chrono::Duration::seconds(60)));
}

#[test]
fn provider_rate_limit_is_found_across_the_content_service_boundary() {
    let error = anyhow::Error::new(ContentServiceError::Internal(anyhow::Error::new(
        ProviderRateLimit::new(
            "provider-alpha",
            Some(std::time::Duration::from_secs(5)),
            "provider rate limited; response details were redacted",
        ),
    )))
    .context("graph materialization boundary");

    let classification = classify_content_ingest_failure(&error);

    assert_eq!(classification.failure_code, "provider_rate_limited");
    assert!(classification.is_retryable());
    assert_eq!(classification.operational_defer, Some(chrono::Duration::seconds(5)));
}

#[test]
fn ordinary_failures_carry_no_operational_deferral() {
    let error = anyhow::anyhow!("provider connection reset");

    let classification = classify_content_ingest_failure(&error);

    assert_eq!(classification.operational_defer, None);
    assert!(classification.is_retryable());
}

#[test]
fn provider_lane_acquisition_failure_is_retryable_and_actionable() {
    let error = anyhow::Error::new(ContentServiceError::Internal(anyhow::Error::new(
        ProviderBudgetError::LimiterClosed,
    )))
    .context("graph materialization boundary");

    let classification = classify_content_ingest_failure(&error);

    assert_eq!(classification.failure_class, "provider");
    assert_eq!(classification.failure_code, "provider_concurrency_unavailable");
    assert_eq!(classification.failure_disposition, OpsFailureDisposition::RetryExhausted);
    assert!(classification.is_retryable());
}

#[test]
fn heavy_revision_permit_timeout_is_retryable_and_actionable() {
    let error = anyhow::Error::new(HeavyRevisionPipelinePermitTimeout { timeout_ms: 20 });

    let classification = classify_content_ingest_failure(&error);

    assert_eq!(classification.failure_class, "ingest_admission");
    assert_eq!(classification.failure_code, "heavy_revision_permit_timeout");
    assert_eq!(classification.failure_disposition, OpsFailureDisposition::RetryExhausted);
    assert!(classification.is_retryable(), "the slot frees when a running heavy job finishes");
}

#[test]
fn unclassified_pipeline_failures_stay_retryable() {
    let error = anyhow::anyhow!("provider connection reset");

    let classification = classify_content_ingest_failure(&error);

    assert_eq!(classification.failure_class, "worker_error");
    assert_eq!(classification.failure_code, "\u{63}anonical_pipeline_failed");
    assert_eq!(classification.failure_disposition, OpsFailureDisposition::RetryExhausted);
    assert!(classification.is_retryable());
}

#[test]
fn extraction_failures_keep_their_own_retry_decision() {
    let retryable = anyhow::Error::new(ContentExtractionError::extraction_failed(
        "extraction_timeout",
        "extractor timed out",
    ));
    let worker_terminal = anyhow::Error::new(ContentExtractionError::extraction_failed_terminal(
        "docling_process_oom",
        "document did not fit the extraction worker's memory budget",
    ));

    let retryable = classify_content_ingest_failure(&retryable);
    let worker_terminal = classify_content_ingest_failure(&worker_terminal);

    assert_eq!(retryable.failure_class, "content_extract");
    assert_eq!(retryable.failure_code, "extraction_timeout");
    assert_eq!(retryable.failure_disposition, OpsFailureDisposition::RetryExhausted);
    assert!(retryable.is_retryable());

    assert_eq!(worker_terminal.failure_class, "content_extract");
    assert_eq!(worker_terminal.failure_code, "docling_process_oom");
    // A terminal extraction fault caused by worker resources is operational;
    // only a rejected content admission is `content_unsupported`.
    assert_eq!(worker_terminal.failure_disposition, OpsFailureDisposition::OperationalTerminal);
    assert!(!worker_terminal.is_retryable());
}

#[test]
fn content_admission_rejection_is_the_only_content_unsupported_outcome() {
    let rejection = UploadAdmissionError::from_file_extract_error(
        "broken.pdf",
        None,
        1024,
        &FileExtractError::ExtractionFailed {
            file_kind: UploadFileKind::Pdf,
            class: ExtractionFailureClass::ContentInvalid,
            message: "pdf could not be parsed".to_string(),
        },
    );
    let error = anyhow::Error::new(ContentExtractionError::from_upload_rejection(&rejection));

    let classification = classify_content_ingest_failure(&error);

    assert_eq!(classification.failure_class, "content_extract");
    // The real non-retryable path the connector sees: an upload whose bytes the
    // server rejected at admission.
    assert_eq!(classification.failure_code, "upload_extraction_failed");
    assert_eq!(classification.failure_disposition, OpsFailureDisposition::ContentUnsupported);
    assert!(!classification.is_retryable());
}

#[test]
fn missing_stored_source_is_operational_not_content_unsupported() {
    let error = anyhow::Error::new(ContentExtractionError::missing_stored_source(
        Uuid::now_v7(),
        Uuid::now_v7(),
    ));

    let classification = classify_content_ingest_failure(&error);

    assert_eq!(classification.failure_code, "missing_stored_source");
    assert_eq!(classification.failure_disposition, OpsFailureDisposition::OperationalTerminal);
    assert!(!classification.is_retryable());
}

#[test]
fn request_level_multipart_rejection_is_operational_not_content_unsupported() {
    // A truncated multipart stream is a request-level fault: the bytes may be
    // incomplete, so the server holds no content verdict and must never publish
    // `content_unsupported`.
    let rejection = UploadAdmissionError::invalid_multipart_payload();
    assert_eq!(
        rejection.extraction_failure_class(),
        None,
        "a transport-level rejection carries no content class"
    );
    let error = anyhow::Error::new(ContentExtractionError::from_upload_rejection(&rejection));

    let classification = classify_content_ingest_failure(&error);

    assert_eq!(classification.failure_class, "content_extract");
    assert_eq!(classification.failure_code, "multipart_stream_failure");
    assert_eq!(classification.failure_disposition, OpsFailureDisposition::OperationalTerminal);
    assert!(!classification.is_retryable());
}

#[test]
fn malformed_local_bytes_are_the_content_unsupported_outcome() {
    // A byte-level rejection (malformed text bytes) is deterministic and stays
    // the only `content_unsupported` extraction outcome.
    let rejection = UploadAdmissionError::from_file_extract_error(
        "notes.txt",
        Some("text/plain"),
        2,
        &FileExtractError::InvalidUtf8,
    );
    assert_eq!(
        rejection.extraction_failure_class(),
        Some(ExtractionFailureClass::ContentInvalid),
        "a byte-level rejection carries the content class"
    );
    let error = anyhow::Error::new(ContentExtractionError::from_upload_rejection(&rejection));

    let classification = classify_content_ingest_failure(&error);

    assert_eq!(classification.failure_class, "content_extract");
    assert_eq!(classification.failure_code, "invalid_text_encoding");
    assert_eq!(classification.failure_disposition, OpsFailureDisposition::ContentUnsupported);
    assert!(!classification.is_retryable());
}

#[test]
fn operational_extraction_rejection_is_retryable_not_content_unsupported() {
    // A docling timeout is operational: the same payload may extract on retry.
    let rejection = UploadAdmissionError::from_file_extract_error(
        "manual.pdf",
        Some("application/pdf"),
        1024,
        &FileExtractError::ExtractionFailed {
            file_kind: UploadFileKind::Pdf,
            class: ExtractionFailureClass::OperationalRetryable,
            message: "docling extraction timed out".to_string(),
        },
    );
    let error = anyhow::Error::new(ContentExtractionError::from_upload_rejection(&rejection));

    let classification = classify_content_ingest_failure(&error);

    assert_eq!(classification.failure_code, "upload_extraction_failed");
    assert_eq!(classification.failure_disposition, OpsFailureDisposition::RetryExhausted);
    assert!(classification.is_retryable());
}

#[test]
fn terminal_extractor_fault_is_operational_not_content_unsupported() {
    let rejection = UploadAdmissionError::from_file_extract_error(
        "manual.pdf",
        Some("application/pdf"),
        1024,
        &FileExtractError::ExtractionFailed {
            file_kind: UploadFileKind::Pdf,
            class: ExtractionFailureClass::OperationalTerminal,
            message: "docling returned invalid output".to_string(),
        },
    );
    let error = anyhow::Error::new(ContentExtractionError::from_upload_rejection(&rejection));

    let classification = classify_content_ingest_failure(&error);

    assert_eq!(classification.failure_code, "upload_extraction_failed");
    assert_eq!(classification.failure_disposition, OpsFailureDisposition::OperationalTerminal);
    assert!(!classification.is_retryable());
}
