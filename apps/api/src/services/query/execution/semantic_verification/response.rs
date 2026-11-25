use std::collections::HashSet;

use ironrag_contracts::assistant::semantic_verification::{
    SEMANTIC_VERIFICATION_PROTOCOL_VERSION, SemanticAssessment, SemanticClaimSupport,
    SemanticCompleteness, SemanticEvidenceLink, SemanticEvidenceScope, SemanticFailureCode,
    SemanticMissingCaveat, SemanticResponseClassification, SemanticUnitVerdict,
    SemanticVerificationOutcome, SemanticVerificationReport,
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{
    MAX_CAVEAT_UTF8_BYTES, MAX_EVIDENCE_UTF8_BYTES, MAX_LINKS_PER_UNIT, MAX_MISSING_CAVEATS,
    MAX_OUTPUT_TOKENS, MAX_OUTPUT_UTF8_BYTES, PreparedSemanticVerification,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SemanticOutputStatus {
    Complete,
    Truncated,
}

/// Transport completion metadata is supplied by the gateway, never parsed from
/// model-authored JSON. Unknown token usage still has the independent byte cap.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StructuredSemanticOutput<'a> {
    pub(crate) json: &'a str,
    pub(crate) status: SemanticOutputStatus,
    pub(crate) output_tokens: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum SemanticValidationError {
    #[error("structured output was truncated")]
    Truncated,
    #[error("structured output exceeds a byte or token bound")]
    OutputTooLarge,
    #[error("response is not a strict semantic JSON object")]
    InvalidJson,
    #[error("response protocol or checked-input identity does not match")]
    IdentityMismatch,
    #[error("unit coverage is missing, duplicated or unknown")]
    InvalidUnitCoverage,
    #[error("an evidence passage refers to an unknown handle")]
    UnknownPassage,
    #[error("an evidence link does not match exact source bytes")]
    InvalidEvidenceLink,
    #[error("the same evidence span is repeated within one assessment")]
    DuplicateEvidence,
    #[error("a support or completeness assessment lacks required source links")]
    MissingEvidence,
    #[error("supported document absence requires proven complete-document scope")]
    IncompleteDocumentScope,
    #[error("response classification, claims and completeness are inconsistent")]
    IncoherentAssessment,
    #[error("assessment collections or text exceed protocol limits")]
    AssessmentTooLarge,
}

impl SemanticValidationError {
    pub(crate) const fn failure_code(self) -> SemanticFailureCode {
        match self {
            Self::Truncated
            | Self::OutputTooLarge
            | Self::InvalidJson
            | Self::AssessmentTooLarge => SemanticFailureCode::InvalidResponse,
            _ => SemanticFailureCode::InvalidProvenance,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderAssessment {
    protocol_version: u32,
    question_sha256: String,
    checked_answer_sha256: String,
    evidence_manifest_sha256: String,
    units: Vec<ProviderUnitVerdict>,
    completeness: SemanticCompleteness,
    missing_caveats: Vec<ProviderMissingCaveat>,
    response_classification: SemanticResponseClassification,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderUnitVerdict {
    unit_id: String,
    support: SemanticClaimSupport,
    asserts_document_absence: bool,
    evidence: Vec<ProviderEvidenceLink>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderMissingCaveat {
    description: String,
    evidence: Vec<ProviderEvidenceLink>,
}

/// Only an opaque passage handle crosses the provider boundary. Authoritative
/// identities, exact quotes and byte positions are resolved by the server.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderEvidenceLink {
    passage_id: String,
}

/// Strict schema for the existing structured gateway. Parsing also enforces
/// semantic cross-field invariants and provenance that JSON Schema cannot prove.
pub(crate) fn response_json_schema(prepared: &PreparedSemanticVerification) -> Value {
    let unit_ids: Vec<_> = prepared.units().iter().map(|unit| &unit.unit_id).collect();
    let passage_ids: Vec<_> = prepared.passages().iter().map(|passage| &passage.handle).collect();
    let link = json!({
        "type":"object", "additionalProperties":false,
        "properties":{
            "passageId":{"type":"string","enum":passage_ids}
        }, "required":["passageId"]
    });
    let links = |minimum: usize| json!({"type":"array","minItems":minimum,"maxItems":MAX_LINKS_PER_UNIT,"items":link});
    json!({
        "type":"object", "additionalProperties":false,
        "properties":{
            "protocolVersion":{"type":"integer","enum":[SEMANTIC_VERIFICATION_PROTOCOL_VERSION]},
            "questionSha256":{"type":"string","enum":[prepared.question_sha256()]},
            "checkedAnswerSha256":{"type":"string","enum":[prepared.answer_sha256()]},
            "evidenceManifestSha256":{"type":"string","enum":[prepared.manifest_sha256()]},
            "units":{"type":"array","minItems":prepared.units().len(),"maxItems":prepared.units().len(),"items":{
                "type":"object","additionalProperties":false,
                "properties":{
                    "unitId":{"type":"string","enum":unit_ids},
                    "support":{"type":"string","enum":["supported","contradicted","insufficient_evidence","mixed","no_factual_claims"]},
                    "assertsDocumentAbsence":{"type":"boolean"},
                    "evidence":links(0)
                },"required":["unitId","support","assertsDocumentAbsence","evidence"]
            }},
            "completeness":{"type":"string","enum":["complete","missing_essential_caveats","undetermined"]},
            "missingCaveats":{"type":"array","maxItems":MAX_MISSING_CAVEATS,"items":{
                "type":"object","additionalProperties":false,
                "properties":{
                    "description":{"type":"string","minLength":1,"maxLength":MAX_CAVEAT_UTF8_BYTES},
                    "evidence":links(1)
                },"required":["description","evidence"]
            }},
            "responseClassification":{"type":"string","enum":["factual_answer","correct_source_bounded_abstention","false_refusal","unresolved_refusal"]}
        },
        "required":["protocolVersion","questionSha256","checkedAnswerSha256","evidenceManifestSha256","units","completeness","missingCaveats","responseClassification"]
    })
}

impl PreparedSemanticVerification {
    /// Parse a complete structured response and bind it to this immutable input.
    /// No model-supplied UUID, span, aggregate support or formal verdict is accepted.
    pub(crate) fn parse_response(
        &self,
        output: StructuredSemanticOutput<'_>,
        elapsed_ms: u64,
        provider_call_id: Option<Uuid>,
    ) -> Result<SemanticVerificationReport, SemanticValidationError> {
        if output.status != SemanticOutputStatus::Complete {
            return Err(SemanticValidationError::Truncated);
        }
        if output.json.len() > MAX_OUTPUT_UTF8_BYTES
            || output.output_tokens.is_some_and(|count| count > MAX_OUTPUT_TOKENS)
        {
            return Err(SemanticValidationError::OutputTooLarge);
        }
        let parsed: ProviderAssessment =
            serde_json::from_str(output.json).map_err(|_| SemanticValidationError::InvalidJson)?;
        if parsed.protocol_version != SEMANTIC_VERIFICATION_PROTOCOL_VERSION
            || parsed.question_sha256 != self.question_sha256
            || parsed.checked_answer_sha256 != self.answer_sha256
            || parsed.evidence_manifest_sha256 != self.manifest_sha256
        {
            return Err(SemanticValidationError::IdentityMismatch);
        }
        let mut assessment = self.resolve_assessment(parsed)?;
        // Return server order even when the model reordered its exhaustive list.
        assessment.units.sort_by_key(|verdict| {
            self.units.iter().position(|unit| unit.unit_id == verdict.unit_id)
        });
        self.validate_assessment(&mut assessment)?;
        Ok(self.report(
            SemanticVerificationOutcome::Completed { assessment },
            elapsed_ms,
            provider_call_id,
        ))
    }

    /// One owner for every resolved assessment, whether it came from this
    /// provider's handles or from an already server-resolved recorded report.
    /// Links are checked against immutable source bytes; handles are never
    /// required to match newly generated passage boundaries.
    pub(crate) fn validate_assessment(
        &self,
        assessment: &mut SemanticAssessment,
    ) -> Result<(), SemanticValidationError> {
        if assessment.units.len() != self.units.len()
            || assessment
                .units
                .iter()
                .zip(&self.units)
                .any(|(verdict, unit)| verdict.unit_id != unit.unit_id)
        {
            return Err(SemanticValidationError::InvalidUnitCoverage);
        }
        if assessment.claim_support != aggregate_support(&assessment.units) {
            return Err(SemanticValidationError::IncoherentAssessment);
        }
        self.validate_units(&assessment.units)?;
        self.validate_completeness(assessment)?;
        if matches!(
            assessment.response_classification,
            SemanticResponseClassification::FalseRefusal
                | SemanticResponseClassification::UnresolvedRefusal
        ) && assessment.completeness == SemanticCompleteness::Complete
        {
            // A refusal supplies no requested fact. The server owns that
            // completeness decision instead of turning a coherent refusal into
            // an operational provenance failure.
            assessment.completeness = SemanticCompleteness::Undetermined;
        }
        let claim_support = assessment.claim_support;
        validate_classification(assessment, claim_support)
    }

    fn resolve_assessment(
        &self,
        parsed: ProviderAssessment,
    ) -> Result<SemanticAssessment, SemanticValidationError> {
        if parsed.units.len() != self.units.len() {
            return Err(SemanticValidationError::InvalidUnitCoverage);
        }
        if parsed.missing_caveats.len() > MAX_MISSING_CAVEATS {
            return Err(SemanticValidationError::AssessmentTooLarge);
        }
        let units = parsed
            .units
            .into_iter()
            .map(|verdict| {
                Ok(SemanticUnitVerdict {
                    unit_id: verdict.unit_id,
                    support: verdict.support,
                    asserts_document_absence: verdict.asserts_document_absence,
                    evidence: self.resolve_links(&verdict.evidence)?,
                })
            })
            .collect::<Result<Vec<_>, SemanticValidationError>>()?;
        let missing_caveats = parsed
            .missing_caveats
            .into_iter()
            .map(|caveat| {
                Ok(SemanticMissingCaveat {
                    description: caveat.description,
                    evidence: self.resolve_links(&caveat.evidence)?,
                })
            })
            .collect::<Result<Vec<_>, SemanticValidationError>>()?;
        Ok(SemanticAssessment {
            claim_support: aggregate_support(&units),
            units,
            missing_caveats,
            completeness: parsed.completeness,
            response_classification: parsed.response_classification,
        })
    }

    fn validate_units(&self, units: &[SemanticUnitVerdict]) -> Result<(), SemanticValidationError> {
        if units.len() != self.units.len() {
            return Err(SemanticValidationError::InvalidUnitCoverage);
        }
        let mut seen = HashSet::new();
        for verdict in units {
            if !self.units.iter().any(|unit| unit.unit_id == verdict.unit_id)
                || !seen.insert(&verdict.unit_id)
            {
                return Err(SemanticValidationError::InvalidUnitCoverage);
            }
            self.validate_verdict(verdict)?;
        }
        Ok(())
    }

    fn validate_verdict(
        &self,
        verdict: &SemanticUnitVerdict,
    ) -> Result<(), SemanticValidationError> {
        self.validate_links(&verdict.evidence)?;
        match verdict.support {
            SemanticClaimSupport::Supported
            | SemanticClaimSupport::Contradicted
            | SemanticClaimSupport::Mixed
                if verdict.evidence.is_empty() =>
            {
                return Err(SemanticValidationError::MissingEvidence);
            }
            SemanticClaimSupport::NoFactualClaims
                if !verdict.evidence.is_empty() || verdict.asserts_document_absence =>
            {
                return Err(SemanticValidationError::IncoherentAssessment);
            }
            _ => {}
        }
        // An excerpt can disprove an absence assertion by showing the fact.
        // Only a *supported* absence assertion requires whole-document proof.
        if verdict.asserts_document_absence
            && matches!(
                verdict.support,
                SemanticClaimSupport::Supported | SemanticClaimSupport::Mixed
            )
            && !verdict.evidence.iter().all(|link| {
                self.sources.iter().any(|source| {
                    source.source_id == link.source_id
                        && source.scope == SemanticEvidenceScope::CompleteDocument
                })
            })
        {
            return Err(SemanticValidationError::IncompleteDocumentScope);
        }
        Ok(())
    }

    fn validate_completeness(
        &self,
        parsed: &SemanticAssessment,
    ) -> Result<(), SemanticValidationError> {
        if (parsed.completeness == SemanticCompleteness::MissingEssentialCaveats)
            == parsed.missing_caveats.is_empty()
        {
            return Err(SemanticValidationError::IncoherentAssessment);
        }
        for caveat in &parsed.missing_caveats {
            if caveat.description.len() > MAX_CAVEAT_UTF8_BYTES {
                return Err(SemanticValidationError::AssessmentTooLarge);
            }
            if caveat.description.trim().is_empty() {
                return Err(SemanticValidationError::IncoherentAssessment);
            }
            if caveat.evidence.is_empty() {
                return Err(SemanticValidationError::MissingEvidence);
            }
            self.validate_links(&caveat.evidence)?;
        }
        Ok(())
    }

    /// Validate an already resolved link list against immutable source bytes.
    /// This is the only evidence-exactness owner: provider handles resolve into
    /// these links, and recorded reports arrive in this shape unchanged.
    fn validate_links(
        &self,
        links: &[SemanticEvidenceLink],
    ) -> Result<(), SemanticValidationError> {
        if links.len() > MAX_LINKS_PER_UNIT {
            return Err(SemanticValidationError::AssessmentTooLarge);
        }
        let mut seen = HashSet::new();
        for link in links {
            let source = self
                .sources
                .iter()
                .find(|source| source.source_id == link.source_id)
                .ok_or(SemanticValidationError::InvalidEvidenceLink)?;
            let exact = link.span.start < link.span.end
                && link.quote.len() <= MAX_EVIDENCE_UTF8_BYTES
                && source.text.get(link.span.start..link.span.end) == Some(link.quote.as_str());
            if !exact {
                return Err(SemanticValidationError::InvalidEvidenceLink);
            }
            if !seen.insert((link.source_id.as_str(), link.span.start, link.span.end)) {
                return Err(SemanticValidationError::DuplicateEvidence);
            }
        }
        Ok(())
    }

    fn resolve_links(
        &self,
        links: &[ProviderEvidenceLink],
    ) -> Result<Vec<SemanticEvidenceLink>, SemanticValidationError> {
        if links.len() > MAX_LINKS_PER_UNIT {
            return Err(SemanticValidationError::AssessmentTooLarge);
        }
        let mut resolved = Vec::with_capacity(links.len());
        for link in links {
            let passage = self
                .passages
                .iter()
                .find(|passage| passage.handle == link.passage_id)
                .ok_or(SemanticValidationError::UnknownPassage)?;
            let source = self
                .sources
                .iter()
                .find(|source| source.source_id == passage.source_id)
                .ok_or(SemanticValidationError::UnknownPassage)?;
            let quote = source
                .text
                .get(passage.span.start..passage.span.end)
                .ok_or(SemanticValidationError::UnknownPassage)?;
            resolved.push(SemanticEvidenceLink {
                source_id: passage.source_id.clone(),
                quote: quote.to_owned(),
                span: passage.span.clone(),
            });
        }
        self.validate_links(&resolved)?;
        Ok(resolved)
    }
}

fn validate_classification(
    parsed: &SemanticAssessment,
    claim_support: SemanticClaimSupport,
) -> Result<(), SemanticValidationError> {
    if parsed.completeness == SemanticCompleteness::Complete
        && (claim_support == SemanticClaimSupport::NoFactualClaims
            || matches!(
                parsed.response_classification,
                SemanticResponseClassification::FalseRefusal
                    | SemanticResponseClassification::UnresolvedRefusal
            ))
    {
        return Err(SemanticValidationError::IncoherentAssessment);
    }
    match parsed.response_classification {
        SemanticResponseClassification::FactualAnswer
            if claim_support == SemanticClaimSupport::NoFactualClaims =>
        {
            return Err(SemanticValidationError::IncoherentAssessment);
        }
        SemanticResponseClassification::CorrectSourceBoundedAbstention => {
            let coherent = claim_support == SemanticClaimSupport::Supported
                && parsed.completeness == SemanticCompleteness::Complete
                && parsed.units.iter().any(|unit| unit.asserts_document_absence);
            if !coherent {
                return Err(SemanticValidationError::IncoherentAssessment);
            }
        }
        _ => {}
    }
    Ok(())
}

fn aggregate_support(units: &[SemanticUnitVerdict]) -> SemanticClaimSupport {
    let mut supported = false;
    let mut insufficient = false;
    for unit in units {
        match unit.support {
            SemanticClaimSupport::Contradicted => return SemanticClaimSupport::Contradicted,
            SemanticClaimSupport::Supported => supported = true,
            SemanticClaimSupport::InsufficientEvidence => insufficient = true,
            SemanticClaimSupport::Mixed => {
                supported = true;
                insufficient = true;
            }
            SemanticClaimSupport::NoFactualClaims => {}
        }
    }
    match (supported, insufficient) {
        (true, true) => SemanticClaimSupport::Mixed,
        (true, false) => SemanticClaimSupport::Supported,
        (false, true) => SemanticClaimSupport::InsufficientEvidence,
        (false, false) => SemanticClaimSupport::NoFactualClaims,
    }
}
