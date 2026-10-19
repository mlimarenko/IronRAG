use super::{
    PublicHttpUrlError, WebRunFailure, WebRunFilterPatterns, is_direct_image_web_resource,
    web_run_source_identity,
};
use crate::shared::extraction::file_extract::{
    ExtractionFailureClass, FileExtractError, UploadAdmissionError, UploadFileKind,
};

#[test]
fn source_identity_distinguishes_all_four_filter_roles() {
    let empty = serde_json::json!([]);
    let pattern = serde_json::json!(["/guide/*"]);
    let identities = (0..=4)
        .map(|changed_role| {
            let filters = std::array::from_fn::<_, 4, _>(|role| {
                if changed_role == role { &pattern } else { &empty }
            });
            web_run_source_identity(
                "https://docs.example.test/",
                "recursive",
                "same_host",
                2,
                10,
                WebRunFilterPatterns {
                    crawl_allow: filters[0],
                    crawl_block: filters[1],
                    materialization_allow: filters[2],
                    materialization_block: filters[3],
                },
            )
        })
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(identities.len(), 5, "every filter role must contribute separately to identity");
}

#[test]
fn web_fetch_failures_preserve_policy_vs_transient_classification() {
    let policy_failure = WebRunFailure::from_fetch_error(PublicHttpUrlError::CredentialsForbidden);
    assert_eq!(policy_failure.failure_code, "invalid_url");
    assert_eq!(policy_failure.candidate_reason.as_deref(), Some("invalid_url"));

    let transient_failure = WebRunFailure::from_fetch_error(PublicHttpUrlError::ResolveTimedOut);
    assert_eq!(transient_failure.failure_code, "inaccessible");
    assert_eq!(transient_failure.candidate_reason.as_deref(), Some("inaccessible"));
}

#[test]
fn direct_image_web_resources_are_not_document_materialization_targets() {
    assert!(is_direct_image_web_resource(
        "https://docs.example.test/assets/diagram.PNG?cache=1",
        None,
    ));
    assert!(is_direct_image_web_resource(
        "https://docs.example.test/download?id=42",
        Some("image/webp; charset=binary"),
    ));
    assert!(!is_direct_image_web_resource(
        "https://docs.example.test/guide.pdf",
        Some("application/pdf"),
    ));
    assert!(!is_direct_image_web_resource(
        "https://docs.example.test/page",
        Some("text/html; charset=utf-8"),
    ));
}

#[test]
fn web_extraction_rejection_maps_only_content_failures_to_unsupported_content() {
    let content_failure = WebRunFailure::from_extraction_rejection(
        &UploadAdmissionError::from_file_extract_error(
            "guide.bin",
            None,
            4,
            &FileExtractError::UnsupportedBinary,
        ),
        Some("https://docs.example.test/guide.bin".to_string()),
        Some("application/octet-stream".to_string()),
        Some(200),
    );
    assert_eq!(content_failure.failure_code, "unsupported_content");
    assert_eq!(content_failure.candidate_reason.as_deref(), Some("unsupported_content"));

    // A docling timeout is operational: it must not be published as a
    // supported-content verdict about the candidate.
    let operational_failure = WebRunFailure::from_extraction_rejection(
        &UploadAdmissionError::from_file_extract_error(
            "guide.pdf",
            Some("application/pdf"),
            4,
            &FileExtractError::ExtractionFailed {
                file_kind: UploadFileKind::Pdf,
                class: ExtractionFailureClass::OperationalRetryable,
                message: "docling extraction timed out".to_string(),
            },
        ),
        Some("https://docs.example.test/guide.pdf".to_string()),
        Some("application/pdf".to_string()),
        Some(200),
    );
    assert_eq!(operational_failure.failure_code, "web_capture_materialization_failed");
    assert_eq!(operational_failure.candidate_reason, None);

    let terminal_failure = WebRunFailure::from_extraction_rejection(
        &UploadAdmissionError::from_file_extract_error(
            "guide.pdf",
            Some("application/pdf"),
            4,
            &FileExtractError::ExtractionFailed {
                file_kind: UploadFileKind::Pdf,
                class: ExtractionFailureClass::OperationalTerminal,
                message: "docling returned invalid output".to_string(),
            },
        ),
        Some("https://docs.example.test/guide.pdf".to_string()),
        Some("application/pdf".to_string()),
        Some(200),
    );
    assert_eq!(terminal_failure.failure_code, "web_capture_materialization_failed");
    assert_eq!(terminal_failure.candidate_reason, None);

    // A request-level rejection carries no content class and is operational too.
    let request_level_failure = WebRunFailure::from_extraction_rejection(
        &UploadAdmissionError::invalid_multipart_payload(),
        None,
        None,
        None,
    );
    assert_eq!(request_level_failure.failure_code, "web_capture_materialization_failed");
    assert_eq!(request_level_failure.candidate_reason, None);
}
