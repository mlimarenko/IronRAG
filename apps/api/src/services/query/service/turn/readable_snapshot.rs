use super::*;

use crate::infra::knowledge_plane::BundleStateUpdate;

pub(super) const READABLE_SNAPSHOT_WARNING_CODE: &str = "content_snapshot_catching_up";
const READABLE_SNAPSHOT_WARNING_MESSAGE: &str = "This answer covers the currently readable library snapshot while newer content is still being indexed.";
const LIBRARY_INDEXING_MESSAGE: &str =
    "library content is still being indexed; wait for readable content before querying";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QueryReadableContentCoverage {
    Current,
    CatchingUp,
}

pub(super) fn resolve_query_readable_content_coverage(
    identity: &crate::infra::repositories::content_fingerprint_repository::LibraryReadableContentFingerprint,
) -> Result<QueryReadableContentCoverage, ApiError> {
    if !identity.has_readable_content {
        return Err(ApiError::knowledge_not_ready(LIBRARY_INDEXING_MESSAGE));
    }
    Ok(if identity.is_catching_up {
        QueryReadableContentCoverage::CatchingUp
    } else {
        QueryReadableContentCoverage::Current
    })
}

pub(super) fn apply_query_readable_content_coverage(
    coverage: QueryReadableContentCoverage,
    verification_state: &mut QueryVerificationState,
    verification_warnings: &mut Vec<QueryVerificationWarning>,
) {
    if coverage == QueryReadableContentCoverage::Current {
        return;
    }
    if *verification_state == QueryVerificationState::Verified {
        *verification_state = QueryVerificationState::PartiallySupported;
    }
    if verification_warnings.iter().any(|warning| warning.code == READABLE_SNAPSHOT_WARNING_CODE) {
        return;
    }
    verification_warnings.push(QueryVerificationWarning {
        code: READABLE_SNAPSHOT_WARNING_CODE.to_string(),
        message: READABLE_SNAPSHOT_WARNING_MESSAGE.to_string(),
        related_segment_id: None,
        related_fact_id: None,
    });
}

pub(super) async fn persist_query_readable_content_coverage(
    state: &AppState,
    execution_id: Uuid,
    coverage: QueryReadableContentCoverage,
) -> Result<(), ApiError> {
    if coverage == QueryReadableContentCoverage::Current {
        return Ok(());
    }
    let bundle = state
        .context_store
        .get_bundle_by_query_execution(execution_id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?
        .ok_or(ApiError::Internal)?;
    let mut verification_state = parse_query_verification_state(&bundle.verification_state);
    let mut verification_warnings =
        parse_query_verification_warnings(&bundle.verification_warnings);
    apply_query_readable_content_coverage(
        coverage,
        &mut verification_state,
        &mut verification_warnings,
    );
    let verification_warnings = serde_json::to_value(verification_warnings)
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let updated = state
        .context_store
        .update_bundle_state(BundleStateUpdate {
            bundle_id: bundle.bundle_id,
            bundle_state: &bundle.bundle_state,
            selected_fact_ids: &bundle.selected_fact_ids,
            verification_state: verification_state_storage_label(verification_state),
            verification_warnings,
            freshness_snapshot: bundle.freshness_snapshot,
            candidate_summary: bundle.candidate_summary,
            assembly_diagnostics: bundle.assembly_diagnostics,
        })
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    if updated.is_none() {
        return Err(ApiError::Internal);
    }
    Ok(())
}
