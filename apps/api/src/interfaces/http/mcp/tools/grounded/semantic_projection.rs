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
    json!({
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
    })
}

fn linked_source_ids(evidence: &[SemanticEvidenceLink]) -> Vec<&str> {
    let mut ids = evidence.iter().map(|link| link.source_id.as_str()).collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    ids
}
