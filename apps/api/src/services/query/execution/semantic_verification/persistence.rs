use anyhow::Context;
use ironrag_contracts::assistant::semantic_verification::{
    SEMANTIC_VERIFICATION_PROTOCOL_VERSION, SemanticVerificationReport,
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
        MAX_SOURCE_ENTRIES, PreparedSemanticVerification, preparation,
    };
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
        let prepared = PreparedSemanticVerification { question: String::new(), question_sha256: report.question_sha256.clone(), answer_sha256: report.checked_answer_sha256.clone(), manifest_sha256: report.evidence_manifest_sha256.clone(), units: report.answer_units.clone(), sources: report.sources.clone() };
        let link = |link: &ironrag_contracts::assistant::semantic_verification::SemanticEvidenceLink| serde_json::json!({"sourceId":link.source_id,"quote":link.quote});
        let units = assessment.units.iter().map(|unit| serde_json::json!({"unitId":unit.unit_id,"support":unit.support,"assertsDocumentAbsence":unit.asserts_document_absence,"evidence":unit.evidence.iter().map(&link).collect::<Vec<_>>()})).collect::<Vec<_>>();
        let caveats = assessment.missing_caveats.iter().map(|caveat| serde_json::json!({"description":caveat.description,"evidence":caveat.evidence.iter().map(&link).collect::<Vec<_>>()})).collect::<Vec<_>>();
        let raw = serde_json::json!({"protocolVersion":report.protocol_version,"questionSha256":report.question_sha256,"checkedAnswerSha256":report.checked_answer_sha256,"evidenceManifestSha256":report.evidence_manifest_sha256,"units":units,"completeness":assessment.completeness,"missingCaveats":caveats,"responseClassification":assessment.response_classification}).to_string();
        let validated = prepared.parse_response(super::StructuredSemanticOutput { json: &raw, status: super::SemanticOutputStatus::Complete, output_tokens: None }, report.elapsed_ms, report.provider_call_id)?;
        anyhow::ensure!(validated.outcome == report.outcome, "recorded semantic assessment/quote spans disagree with canonical validation");
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
