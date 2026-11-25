use anyhow::Context;
use ironrag_contracts::assistant::semantic_verification::{
    SEMANTIC_VERIFICATION_PROTOCOL_VERSION, SemanticNotRunReason, SemanticVerificationOutcome,
    SemanticVerificationReport,
};
use serde_json::Value;

const SUMMARY_KEY: &str = "semanticVerification";

pub(crate) fn persisted_semantic_verification(
    candidate_summary: &Value,
) -> anyhow::Result<Option<SemanticVerificationReport>> {
    let summary =
        candidate_summary.as_object().context("persisted candidate summary must be an object")?;
    let Some(recorded) = summary.get(SUMMARY_KEY) else {
        return Ok(None);
    };
    let report: SemanticVerificationReport = serde_json::from_value(recorded.clone())
        .context("malformed persisted semantic verification report")?;
    anyhow::ensure!(
        report.protocol_version == SEMANTIC_VERIFICATION_PROTOCOL_VERSION,
        "unsupported semantic verification protocol"
    );
    for hash in
        [&report.question_sha256, &report.checked_answer_sha256, &report.evidence_manifest_sha256]
            .into_iter()
            .chain(report.visible_answer_sha256.iter())
    {
        anyhow::ensure!(
            hash.len() == 64
                && hash.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "malformed persisted semantic identity"
        );
    }
    validate_recorded_snapshot(&report)?;
    Ok(Some(report))
}

/// Internal integrity only, not renewed authorization. Read-side source guards
/// and passthrough admission must still check the actual current source owners.
fn validate_recorded_snapshot(report: &SemanticVerificationReport) -> anyhow::Result<()> {
    use super::{
        AuthorizedSemanticSource, MAX_ANSWER_UNITS, MAX_ANSWER_UTF8_BYTES, MAX_EVIDENCE_UTF8_BYTES,
        MAX_SOURCE_ENTRIES, PreparedSemanticVerification, configured_preparation_limit,
        preparation,
    };
    if let Some(limit) = &report.preparation_limit {
        anyhow::ensure!(
            matches!(
                &report.outcome,
                SemanticVerificationOutcome::NotRun {
                    reason: SemanticNotRunReason::InputLimitExceeded
                }
            ),
            "recorded preparation limit is not a genuine limit refusal"
        );
        anyhow::ensure!(
            limit.actual > limit.limit,
            "recorded preparation limit did not exceed its configured limit"
        );
        anyhow::ensure!(
            limit.limit == configured_preparation_limit(limit.bound),
            "recorded preparation limit does not match the configured bound"
        );
    }
    anyhow::ensure!(
        report.answer_units.len() <= MAX_ANSWER_UNITS && report.sources.len() <= MAX_SOURCE_ENTRIES,
        "recorded semantic input exceeds entry bounds"
    );
    anyhow::ensure!(
        report.sources.windows(2).all(|pair| pair[0].source_id < pair[1].source_id),
        "recorded semantic manifest is not canonically ordered"
    );
    let mut seen = std::collections::HashSet::new();
    let mut bytes = 0usize;
    for source in &report.sources {
        bytes = bytes
            .checked_add(source.text.len())
            .context("recorded semantic evidence byte overflow")?;
        anyhow::ensure!(
            bytes <= MAX_EVIDENCE_UTF8_BYTES,
            "recorded semantic evidence exceeds byte bound"
        );
        let identity = AuthorizedSemanticSource {
            document_id: source.document_id,
            document_revision_id: source.document_revision_id,
            locator: source.locator.clone(),
            text: source.text.clone(),
            complete_document: None,
        };
        anyhow::ensure!(
            !source.document_id.is_nil()
                && !source.document_revision_id.is_nil()
                && !source.text.trim().is_empty(),
            "invalid recorded semantic source identity"
        );
        validate_recorded_locator(source)?;
        anyhow::ensure!(
            source.text_sha256 == preparation::sha256(source.text.as_bytes())
                && source.source_id
                    == format!("s_{}", preparation::source_identity_hash(&identity, source.scope))
                && seen.insert(&source.source_id),
            "recorded semantic source identity/hash mismatch"
        );
    }
    anyhow::ensure!(
        report.evidence_manifest_sha256 == preparation::manifest_sha256(&report.sources),
        "recorded semantic manifest hash mismatch"
    );
    if report.answer_units.is_empty() {
        anyhow::ensure!(report.sources.is_empty() && !matches!(report.outcome, ironrag_contracts::assistant::semantic_verification::SemanticVerificationOutcome::Completed { .. }), "completed or sourced report lacks checked answer units");
        return Ok(());
    }
    let raw = report.answer_units.iter().map(|unit| unit.text.as_str()).collect::<String>();
    anyhow::ensure!(
        raw.len() <= MAX_ANSWER_UTF8_BYTES && !raw.trim().is_empty() && !report.sources.is_empty(),
        "invalid recorded semantic answer input"
    );
    anyhow::ensure!(
        report.checked_answer_sha256 == preparation::sha256(raw.as_bytes())
            && report.answer_units
                == preparation::prepare_units(&raw, &report.checked_answer_sha256)?,
        "recorded checked answer/unit identity mismatch"
    );
    if let ironrag_contracts::assistant::semantic_verification::SemanticVerificationOutcome::Completed { assessment } = &report.outcome {
        // Recorded links are already server-resolved. Validate them against the
        // immutable recorded source bytes with the same owner as live provider
        // output; do not reconstruct or re-resolve provider handles here.
        let prepared = PreparedSemanticVerification { question: String::new(), question_sha256: report.question_sha256.clone(), answer_sha256: report.checked_answer_sha256.clone(), manifest_sha256: report.evidence_manifest_sha256.clone(), units: report.answer_units.clone(), sources: report.sources.clone(), passages: Vec::new() };
        let mut validated = assessment.clone();
        prepared.validate_assessment(&mut validated)?;
        anyhow::ensure!(validated == *assessment, "recorded semantic assessment disagrees with canonical validation");
    }
    Ok(())
}

fn validate_recorded_locator(
    source: &ironrag_contracts::assistant::semantic_verification::SemanticSourceEntry,
) -> anyhow::Result<()> {
    use ironrag_contracts::assistant::semantic_verification::{
        SemanticEvidenceScope as Scope, SemanticSourceLocator as Locator,
    };
    let valid = match &source.locator {
        Locator::Chunk { chunk_id } => !chunk_id.is_nil(),
        Locator::Segment { segment_id } => !segment_id.is_nil(),
        Locator::DocumentSlice { start, end } => end.checked_sub(*start) == Some(source.text.len()),
    };
    anyhow::ensure!(valid, "recorded semantic source locator is invalid");
    if source.scope == Scope::CompleteDocument {
        anyhow::ensure!(
            source.locator == (Locator::DocumentSlice { start: 0, end: source.text.len() }),
            "recorded complete scope is not a whole-document locator"
        );
    }
    Ok(())
}

pub(crate) fn attach_semantic_verification(
    mut summary: Value,
    report: Option<&SemanticVerificationReport>,
) -> anyhow::Result<Value> {
    let fields = summary.as_object_mut().context("candidate summary must be an object")?;
    if let Some(report) = report {
        fields.insert(SUMMARY_KEY.into(), serde_json::to_value(report)?);
    } else {
        fields.remove(SUMMARY_KEY);
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::query::execution::semantic_verification::{
        MAX_EVIDENCE_UTF8_BYTES, SemanticPreparationBound, SemanticPreparationError,
        configured_preparation_limit, preparation_failure_report, preparation_limit,
    };
    use ironrag_contracts::assistant::semantic_verification::{
        SemanticAssessment, SemanticClaimSupport, SemanticCompleteness, SemanticFailureCode,
        SemanticPreparationLimit, SemanticResponseClassification,
    };

    fn limit_report() -> SemanticVerificationReport {
        preparation_failure_report(
            "Q",
            "A",
            SemanticPreparationError::EvidenceTooLarge(preparation_limit(
                SemanticPreparationBound::EvidenceUtf8Bytes,
                MAX_EVIDENCE_UTF8_BYTES + 1,
            )),
            5,
        )
    }

    fn validate(report: &SemanticVerificationReport) -> anyhow::Result<()> {
        validate_recorded_snapshot(report)
    }

    fn error_message(report: &SemanticVerificationReport) -> String {
        validate(report).expect_err("report must be rejected").to_string()
    }

    #[test]
    fn typed_limit_report_survives_persistence_with_exact_measurement() -> anyhow::Result<()> {
        let report = limit_report();
        assert_eq!(
            report.preparation_limit,
            Some(SemanticPreparationLimit {
                bound: SemanticPreparationBound::EvidenceUtf8Bytes,
                actual: (MAX_EVIDENCE_UTF8_BYTES + 1) as u64,
                limit: MAX_EVIDENCE_UTF8_BYTES as u64,
            })
        );
        let summary = attach_semantic_verification(serde_json::json!({}), Some(&report))?;
        assert_eq!(persisted_semantic_verification(&summary)?, Some(report));
        Ok(())
    }

    #[test]
    fn absent_limit_field_keeps_stored_reports_readable() {
        let mut report = limit_report();
        report.preparation_limit = None;
        assert!(validate(&report).is_ok());
        let json = serde_json::to_value(&report).expect("report serializes");
        assert!(json.get("preparationLimit").is_none());
    }

    #[test]
    fn completed_report_cannot_carry_a_false_preparation_limit() {
        let mut report = limit_report();
        report.outcome = SemanticVerificationOutcome::Completed {
            assessment: SemanticAssessment {
                claim_support: SemanticClaimSupport::NoFactualClaims,
                units: Vec::new(),
                completeness: SemanticCompleteness::Undetermined,
                missing_caveats: Vec::new(),
                response_classification: SemanticResponseClassification::FalseRefusal,
            },
        };
        assert!(error_message(&report).contains("not a genuine limit refusal"));
    }

    #[test]
    fn failed_or_non_limit_outcome_cannot_carry_a_preparation_limit() {
        let mut report = limit_report();
        report.outcome =
            SemanticVerificationOutcome::Failed { code: SemanticFailureCode::ProviderError };
        assert!(error_message(&report).contains("not a genuine limit refusal"));

        let mut report = limit_report();
        report.outcome =
            SemanticVerificationOutcome::NotRun { reason: SemanticNotRunReason::NoEvidence };
        assert!(error_message(&report).contains("not a genuine limit refusal"));
    }

    #[test]
    fn preparation_limit_must_measure_a_real_excess() {
        let mut report = limit_report();
        let limit = report.preparation_limit.expect("limit present");
        report.preparation_limit = Some(SemanticPreparationLimit { actual: limit.limit, ..limit });
        assert!(error_message(&report).contains("did not exceed"));
    }

    #[test]
    fn preparation_limit_must_match_the_configured_bound() {
        for bound in [
            SemanticPreparationBound::AnswerUtf8Bytes,
            SemanticPreparationBound::QuestionUtf8Bytes,
            SemanticPreparationBound::AnswerUnits,
            SemanticPreparationBound::SourceEntries,
            SemanticPreparationBound::EvidenceUtf8Bytes,
        ] {
            let configured = configured_preparation_limit(bound);
            let mut report = limit_report();
            report.preparation_limit =
                Some(SemanticPreparationLimit { bound, actual: configured + 1, limit: configured });
            assert!(validate(&report).is_ok(), "{bound} must accept its exact configured limit");

            report.preparation_limit = Some(SemanticPreparationLimit {
                bound,
                actual: configured + 2,
                limit: configured + 1,
            });
            assert!(error_message(&report).contains("does not match"));
        }
    }
}
