use super::test_prelude::*;

fn readable_identity(
    is_catching_up: bool,
    has_readable_content: bool,
) -> crate::infra::repositories::content_fingerprint_repository::LibraryReadableContentFingerprint {
    crate::infra::repositories::content_fingerprint_repository::LibraryReadableContentFingerprint {
        value: "41".to_string(),
        source_truth_version: 41,
        is_catching_up,
        has_readable_content,
    }
}

#[test]
fn converging_readable_snapshot_keeps_the_answer_and_marks_catching_up() {
    let identity = readable_identity(true, true);
    let coverage = resolve_query_readable_content_coverage(&identity)
        .expect("a readable snapshot must remain queryable");
    let mut verification_state = QueryVerificationState::Verified;
    let mut warnings = Vec::new();

    apply_query_readable_content_coverage(coverage, &mut verification_state, &mut warnings);

    assert_eq!(coverage, QueryReadableContentCoverage::CatchingUp);
    assert_eq!(verification_state, QueryVerificationState::PartiallySupported);
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].code, READABLE_SNAPSHOT_WARNING_CODE);
    let visibility = super::finalize_answer_visibility(
        VerificationLevel::Moderate,
        verification_state,
        &warnings,
        QueryLanguage::Auto,
        "Readable answer",
        super::AnswerVisibilityKind::FactualCandidate,
    );
    assert_eq!(visibility.disposition, QueryAnswerDisposition::FactualReady);
}

#[test]
fn catching_up_snapshot_bypasses_answer_cache() {
    assert!(super::query_result_cache_is_allowed(&readable_identity(false, true)));
    assert!(
        !super::query_result_cache_is_allowed(&readable_identity(true, true)),
        "cached answers must not replay or be stored while readable content is catching up",
    );
}

#[test]
fn snapshot_without_readable_content_routes_to_not_ready() {
    let error = resolve_query_readable_content_coverage(&readable_identity(true, false))
        .expect_err("a first ingest without readable content must not run a query");

    assert!(matches!(error, ApiError::KnowledgeNotReady(_)));
    assert_eq!(error.kind(), "knowledge_not_ready");
}

#[test]
fn current_snapshot_keeps_typed_coverage_byte_identical() {
    let mut verification_state = QueryVerificationState::Verified;
    let mut warnings = vec![QueryVerificationWarning {
        code: "existing_warning".to_string(),
        message: "Existing warning".to_string(),
        related_segment_id: None,
        related_fact_id: None,
    }];
    let before = serde_json::to_vec(&(verification_state, &warnings))
        .expect("serialize typed coverage before applying current state");

    apply_query_readable_content_coverage(
        QueryReadableContentCoverage::Current,
        &mut verification_state,
        &mut warnings,
    );

    let after = serde_json::to_vec(&(verification_state, &warnings))
        .expect("serialize typed coverage after applying current state");
    assert_eq!(after, before);
}
