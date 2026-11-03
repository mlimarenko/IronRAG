//! Semantic judgments are separate from formal verification and answer readiness.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Protocol version used by the server-defined units and evidence manifest.
pub const SEMANTIC_VERIFICATION_PROTOCOL_VERSION: u32 = 1;

/// Exact half-open UTF-8 byte range, never character offsets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SemanticByteSpan {
    /// Inclusive byte offset.
    pub start: usize,
    /// Exclusive byte offset.
    pub end: usize,
}

/// Exhaustive answer block assigned by the server, not by the verifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SemanticAnswerUnit {
    /// Stable opaque identifier within this checked answer.
    pub unit_id: String,
    /// Position in the exact checked answer.
    pub span: SemanticByteSpan,
    /// Exact text at that position.
    pub text: String,
}

/// Provenance locator supplied only by the authorized server-side evidence owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SemanticSourceLocator {
    /// A selected indexed chunk.
    Chunk {
        /// Authoritative chunk identity.
        chunk_id: Uuid,
    },
    /// A selected prepared document segment.
    Segment {
        /// Authoritative segment identity.
        segment_id: Uuid,
    },
    /// An exact byte slice of the canonical document text.
    DocumentSlice {
        /// Inclusive document byte offset.
        start: usize,
        /// Exclusive document byte offset.
        end: usize,
    },
}

/// Evidence authority established upstream; selected excerpts cannot prove absence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SemanticEvidenceScope {
    /// Only the selected text is available.
    SelectedExcerpt,
    /// The complete canonical document text was hash- and length-checked.
    CompleteDocument,
}

/// Server-retained source manifest entry. Never accept these identities from a model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SemanticSourceEntry {
    /// Opaque reference exposed to the verifier.
    pub source_id: String,
    /// Authorized document identity.
    pub document_id: Uuid,
    /// Immutable document revision used by the evidence owner.
    pub document_revision_id: Uuid,
    /// Typed position of the selected source.
    pub locator: SemanticSourceLocator,
    /// Extent of the proven source coverage.
    pub scope: SemanticEvidenceScope,
    /// SHA-256 of exactly `text`, encoded as lowercase hexadecimal.
    pub text_sha256: String,
    /// Only canonical source text, without metadata or tool wrappers.
    pub text: String,
}

/// Support of all assertions within a unit, or the server-derived aggregate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SemanticClaimSupport {
    /// All factual assertions are supported.
    Supported,
    /// At least one assertion is contradicted, regardless of other support.
    Contradicted,
    /// No assertion was supported and evidence is insufficient.
    InsufficientEvidence,
    /// Supported and insufficiently evidenced assertions coexist, without contradiction.
    Mixed,
    /// There are no substantive factual assertions; never factual success.
    NoFactualClaims,
}

/// A verifier-proposed quote validated against an authorized source entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SemanticEvidenceLink {
    /// Opaque identifier of a selected source, not a document UUID.
    pub source_id: String,
    /// Server-derived half-open UTF-8 byte range of a unique exact quote within
    /// the selected text. Provider-supplied spans are not accepted.
    pub span: SemanticByteSpan,
    /// Exact text at the quoted range.
    pub quote: String,
}

/// One verdict covers every assertion in the corresponding server-defined block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SemanticUnitVerdict {
    /// Identifier of exactly one prepared answer unit.
    pub unit_id: String,
    /// Independent factual support assessment.
    pub support: SemanticClaimSupport,
    /// Whether this unit asserts document-wide absence of a fact.
    pub asserts_document_absence: bool,
    /// Supporting or contradicting exact source quotes.
    pub evidence: Vec<SemanticEvidenceLink>,
}

/// Completeness for the question, independent of claim support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SemanticCompleteness {
    /// No essential qualification or requested fact was omitted.
    Complete,
    /// At least one essential qualification or requested fact was omitted.
    MissingEssentialCaveats,
    /// Evidence cannot establish completeness.
    Undetermined,
}

/// An omitted essential qualification with its source basis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SemanticMissingCaveat {
    /// Bounded explanation of the omission, not executable instructions.
    pub description: String,
    /// Exact source basis for the qualification.
    pub evidence: Vec<SemanticEvidenceLink>,
}

/// Candidate-answer classification, not the operational status of the verifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SemanticResponseClassification {
    /// A substantive factual answer.
    FactualAnswer,
    /// A supported statement of unavailability in the proven complete source.
    CorrectSourceBoundedAbstention,
    /// An answerable question was refused, with or without factual assertions.
    FalseRefusal,
    /// A refusal whose correctness cannot be established from this evidence.
    UnresolvedRefusal,
}

/// Validated semantic result; it does not encode formal guards or readiness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SemanticAssessment {
    /// Aggregate derived by the server from all unit verdicts.
    pub claim_support: SemanticClaimSupport,
    /// Exhaustive, server-ordered unit verdicts.
    pub units: Vec<SemanticUnitVerdict>,
    /// Completeness independent of the support aggregate.
    pub completeness: SemanticCompleteness,
    /// Source-grounded omissions; nonempty exactly when essential caveats are missing.
    pub missing_caveats: Vec<SemanticMissingCaveat>,
    /// Candidate response/refusal classification.
    pub response_classification: SemanticResponseClassification,
}

/// Why no semantic verifier invocation was attempted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SemanticNotRunReason {
    /// A typed clarification or deterministic system statement is not a factual candidate.
    NonFactualResponse,
    /// The feature or its binding is disabled.
    Disabled,
    /// No binding was configured.
    BindingUnavailable,
    /// No authorized source text was selected.
    NoEvidence,
    /// No nonempty answer was available.
    EmptyAnswer,
    /// A preparation bound was exceeded; no input was silently truncated.
    InputLimitExceeded,
    /// The gateway cannot enforce the structured transport contract.
    UnsupportedStructuredTransport,
}

/// Operational failures never imply semantic support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SemanticFailureCode {
    /// The verifier exceeded its deadline.
    Timeout,
    /// The provider refused the verifier request.
    ProviderRefusal,
    /// Provider transport or execution failed.
    ProviderError,
    /// The response was malformed, truncated, oversized or structurally invalid.
    InvalidResponse,
    /// Response identities, unit coverage or evidence provenance failed validation.
    InvalidProvenance,
}

/// Mutually exclusive semantic and operational outcomes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum SemanticVerificationOutcome {
    /// A completed semantic judgment, not necessarily a supported answer.
    Completed {
        /// Validated judgment.
        assessment: SemanticAssessment,
    },
    /// No verifier call was attempted.
    NotRun {
        /// Explicit reason for not running.
        reason: SemanticNotRunReason,
    },
    /// The verifier did not produce a valid semantic judgment.
    Failed {
        /// Operational failure category.
        code: SemanticFailureCode,
    },
}

/// A report bound to the exact answer and authorized evidence used by the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SemanticVerificationReport {
    /// Version of the preparation and validation protocol.
    pub protocol_version: u32,
    /// SHA-256 of the exact question bytes used to judge completeness and refusal.
    pub question_sha256: String,
    /// SHA-256 of the exact checked answer bytes.
    pub checked_answer_sha256: String,
    /// Hash of the final visible body, assigned only by the server finalizer.
    /// This never replaces the identity of the checked raw candidate.
    pub visible_answer_sha256: Option<String>,
    /// Canonical SHA-256 including source identities, scope and exact text.
    pub evidence_manifest_sha256: String,
    /// Exhaustive server-defined blocks of the raw checked candidate, never a
    /// later visible fallback. Verdict IDs resolve only against these units.
    pub answer_units: Vec<SemanticAnswerUnit>,
    /// Exact authorized manifest. Opaque quote references resolve only against
    /// these server-owned identities, scopes and source texts.
    pub sources: Vec<SemanticSourceEntry>,
    /// Server-measured elapsed time in milliseconds.
    pub elapsed_ms: u64,
    /// Server-owned provider-call identity, if a call was made.
    pub provider_call_id: Option<Uuid>,
    /// Semantic or operational result.
    pub outcome: SemanticVerificationOutcome,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_contract_operational_outcomes_round_trip() -> Result<(), serde_json::Error> {
        for outcome in [
            SemanticVerificationOutcome::NotRun { reason: SemanticNotRunReason::NoEvidence },
            SemanticVerificationOutcome::Failed { code: SemanticFailureCode::ProviderRefusal },
        ] {
            let encoded = serde_json::to_value(&outcome)?;
            assert!(encoded.get("status").is_some());
            assert_eq!(serde_json::from_value::<SemanticVerificationOutcome>(encoded)?, outcome);
        }
        Ok(())
    }

    #[test]
    fn semantic_contract_rejects_mixed_operational_and_semantic_outcomes() {
        for value in [
            serde_json::json!({"status":"failed","code":"timeout","assessment":{}}),
            serde_json::json!({"status":"not_run","reason":"no_evidence","code":"timeout"}),
            serde_json::json!({"status":"completed","code":"timeout"}),
            serde_json::json!({"status":"verified"}),
        ] {
            assert!(serde_json::from_value::<SemanticVerificationOutcome>(value).is_err());
        }
    }

    #[test]
    fn semantic_contract_keeps_support_completeness_and_refusal_independent()
    -> Result<(), serde_json::Error> {
        let assessment = SemanticAssessment {
            claim_support: SemanticClaimSupport::NoFactualClaims,
            units: vec![SemanticUnitVerdict {
                unit_id: "opaque".into(),
                support: SemanticClaimSupport::NoFactualClaims,
                asserts_document_absence: false,
                evidence: vec![],
            }],
            completeness: SemanticCompleteness::Undetermined,
            missing_caveats: vec![],
            response_classification: SemanticResponseClassification::FalseRefusal,
        };
        let report = SemanticVerificationReport {
            protocol_version: SEMANTIC_VERIFICATION_PROTOCOL_VERSION,
            question_sha256: "c".repeat(64),
            checked_answer_sha256: "a".repeat(64),
            visible_answer_sha256: None,
            evidence_manifest_sha256: "b".repeat(64),
            answer_units: vec![],
            sources: vec![],
            elapsed_ms: 7,
            provider_call_id: None,
            outcome: SemanticVerificationOutcome::Completed { assessment },
        };
        assert_eq!(
            serde_json::from_str::<SemanticVerificationReport>(&serde_json::to_string(&report)?)?,
            report
        );
        let mut without_question_identity = serde_json::to_value(&report)?;
        if let Some(fields) = without_question_identity.as_object_mut() {
            fields.remove("questionSha256");
        }
        assert!(
            serde_json::from_value::<SemanticVerificationReport>(without_question_identity)
                .is_err()
        );
        Ok(())
    }
}
