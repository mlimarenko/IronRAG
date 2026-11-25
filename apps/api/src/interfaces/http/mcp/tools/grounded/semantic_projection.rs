//! A bounded MCP projection, never a partially serialized canonical report.

use ironrag_contracts::assistant::semantic_verification::{
    SemanticEvidenceLink, SemanticVerificationOutcome, SemanticVerificationReport,
};
use serde_json::{Value, json};

use super::payload::{GROUNDED_ANSWER_COMPACT_TEXT_MAX_CHARS, grounded_answer_compact_text};

const FINDING_LIMIT: usize = 8;

pub(super) fn compact_semantic_verification(
    report: &SemanticVerificationReport,
    source_limit: usize,
) -> Value {
    let sources = report
        .sources
        .iter()
        .take(source_limit)
        .map(|source| {
            json!({
                "sourceId": source.source_id,
                "documentId": source.document_id,
                "documentRevisionId": source.document_revision_id,
                "locator": source.locator,
                "scope": source.scope,
                "textSha256": source.text_sha256,
            })
        })
        .collect::<Vec<_>>();
    let outcome = match &report.outcome {
        SemanticVerificationOutcome::Completed { assessment } => {
            let units = assessment.units.iter().take(FINDING_LIMIT).map(|unit| {
                let text = report.answer_units.iter().find(|value| value.unit_id == unit.unit_id)
                    .map(|value| value.text.as_str()).unwrap_or_default();
                json!({
                    "unitId": unit.unit_id,
                    "support": unit.support,
                    "assertsDocumentAbsence": unit.asserts_document_absence,
                    "checkedText": grounded_answer_compact_text(text),
                    "checkedTextTruncated": text.chars().count() > GROUNDED_ANSWER_COMPACT_TEXT_MAX_CHARS,
                    "sourceIds": linked_source_ids(&unit.evidence),
                })
            }).collect::<Vec<_>>();
            let caveats = assessment.missing_caveats.iter().take(FINDING_LIMIT).map(|caveat| json!({
                "description": grounded_answer_compact_text(&caveat.description),
                "descriptionTruncated": caveat.description.chars().count() > GROUNDED_ANSWER_COMPACT_TEXT_MAX_CHARS,
                "sourceIds": linked_source_ids(&caveat.evidence),
            })).collect::<Vec<_>>();
            json!({
                "status": "completed",
                "claimSupport": assessment.claim_support,
                "completeness": assessment.completeness,
                "responseClassification": assessment.response_classification,
                "unitSummary": {
                    "totalCount": assessment.units.len(), "returnedCount": units.len(),
                    "truncated": assessment.units.len() > units.len(), "units": units,
                },
                "missingCaveatSummary": {
                    "totalCount": assessment.missing_caveats.len(), "returnedCount": caveats.len(),
                    "truncated": assessment.missing_caveats.len() > caveats.len(), "caveats": caveats,
                },
            })
        }
        outcome => json!(outcome),
    };
    let mut projected = json!({
        "projection": "summary",
        "protocolVersion": report.protocol_version,
        "questionSha256": report.question_sha256,
        "checkedAnswerSha256": report.checked_answer_sha256,
        "visibleAnswerSha256": report.visible_answer_sha256,
        "evidenceManifestSha256": report.evidence_manifest_sha256,
        "elapsedMs": report.elapsed_ms,
        "providerCallId": report.provider_call_id,
        "outcome": outcome,
        "sourceSummary": {
            "totalCount": report.sources.len(), "returnedCount": sources.len(),
            "truncated": report.sources.len() > sources.len(), "sources": sources,
            "sourceTextIncluded": false, "evidenceQuotesIncluded": false,
        },
    });
    // Typed preparation refusal is reported verbatim and only when present.
    // The safe status stays in `outcome`; this projection never recomputes it.
    if let Some(limit) = &report.preparation_limit {
        projected["preparationLimit"] = json!({
            "bound": limit.bound,
            "actual": limit.actual,
            "limit": limit.limit,
        });
    }
    projected
}

fn linked_source_ids(evidence: &[SemanticEvidenceLink]) -> Vec<&str> {
    let mut ids = evidence.iter().map(|link| link.source_id.as_str()).collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::query::execution::semantic_verification::{
        SemanticPreparationBound, SemanticPreparationError, preparation_failure_report,
        preparation_limit,
    };
    use ironrag_contracts::assistant::semantic_verification::{
        SEMANTIC_VERIFICATION_PROTOCOL_VERSION, SemanticAssessment, SemanticClaimSupport,
        SemanticCompleteness, SemanticResponseClassification, SemanticVerificationOutcome,
    };

    fn completed_report() -> SemanticVerificationReport {
        SemanticVerificationReport {
            protocol_version: SEMANTIC_VERIFICATION_PROTOCOL_VERSION,
            question_sha256: "a".repeat(64),
            checked_answer_sha256: "b".repeat(64),
            visible_answer_sha256: None,
            evidence_manifest_sha256: "c".repeat(64),
            answer_units: Vec::new(),
            sources: Vec::new(),
            elapsed_ms: 4,
            provider_call_id: None,
            preparation_limit: None,
            outcome: SemanticVerificationOutcome::Completed {
                assessment: SemanticAssessment {
                    claim_support: SemanticClaimSupport::NoFactualClaims,
                    units: Vec::new(),
                    completeness: SemanticCompleteness::Undetermined,
                    missing_caveats: Vec::new(),
                    response_classification: SemanticResponseClassification::FalseRefusal,
                },
            },
        }
    }

    #[test]
    fn compact_projection_includes_typed_preparation_limit_only_when_present() {
        let report = preparation_failure_report(
            "Q",
            "A",
            SemanticPreparationError::EvidenceTooLarge(preparation_limit(
                SemanticPreparationBound::EvidenceUtf8Bytes,
                22_001,
            )),
            7,
        );
        let compact = compact_semantic_verification(&report, 8);
        assert_eq!(compact["preparationLimit"]["bound"], "evidence_utf8_bytes");
        assert_eq!(compact["preparationLimit"]["actual"], json!(22_001u64));
        assert_eq!(compact["preparationLimit"]["limit"], json!(22_000u64));
        assert_eq!(compact["outcome"]["status"], "not_run");
        assert_eq!(compact["outcome"]["reason"], "input_limit_exceeded");
    }

    #[test]
    fn compact_projection_omits_preparation_limit_for_ordinary_and_completed_reports() {
        let ordinary =
            preparation_failure_report("Q", "A", SemanticPreparationError::NoEvidence, 1);
        let compact = compact_semantic_verification(&ordinary, 8);
        assert!(compact.get("preparationLimit").is_none());
        assert_eq!(compact["outcome"]["status"], "not_run");
        assert_eq!(compact["outcome"]["reason"], "no_evidence");

        let compact = compact_semantic_verification(&completed_report(), 8);
        assert!(compact.get("preparationLimit").is_none());
        assert_eq!(compact["outcome"]["status"], "completed");
    }
}
