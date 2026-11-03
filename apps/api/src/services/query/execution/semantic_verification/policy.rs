use super::super::{
    types::RuntimeAnswerVerification,
    verification_policy::verification_warning_blocks_factual_readiness,
};
use crate::domains::query::{QueryVerificationState as State, QueryVerificationWarning};
use ironrag_contracts::assistant::semantic_verification::{
    SemanticClaimSupport as Support, SemanticCompleteness as Completeness, SemanticNotRunReason,
    SemanticResponseClassification as Response, SemanticVerificationOutcome as Outcome,
    SemanticVerificationReport,
};

/// Semantic support may close the formal verifier's semantic limitation, never
/// its literal, conflicting-source, intent or coverage blockers.
pub(crate) fn apply_semantic_report(
    verification: &mut RuntimeAnswerVerification,
    report: SemanticVerificationReport,
) {
    let formal_blocked = has_formal_blocker(verification);
    match &report.outcome {
        Outcome::Completed { assessment } => {
            verification.warnings.retain(|warning| {
                !matches!(
                    warning.code.as_str(),
                    "semantic_not_run"
                        | "semantic_verification_not_run"
                        | "semantic_verification_partial"
                )
            });
            let (state, warning) = assessment_state(assessment);
            verification.state = preserve_formal_floor(verification, state, formal_blocked);
            if let Some(code) = warning {
                append_warning(verification, code);
            }
            if assessment.response_classification == Response::FalseRefusal {
                append_warning(verification, "semantic_false_refusal");
            }
        }
        Outcome::NotRun { .. } => {
            verification.state = preserve_formal_floor(verification, State::NotRun, formal_blocked);
            append_warning(verification, "semantic_not_run");
        }
        Outcome::Failed { .. } => {
            verification.state = State::Failed;
            append_warning(verification, "semantic_operational_failure");
        }
    }
    verification.semantic_verification = Some(report);
}

fn assessment_state(
    assessment: &ironrag_contracts::assistant::semantic_verification::SemanticAssessment,
) -> (State, Option<&'static str>) {
    match assessment.claim_support {
        Support::Contradicted => (State::Conflicting, Some("semantic_contradiction")),
        Support::InsufficientEvidence => {
            (State::InsufficientEvidence, Some("semantic_unsupported"))
        }
        Support::Mixed => (State::PartiallySupported, Some("semantic_mixed_support")),
        Support::NoFactualClaims => (State::NotRun, Some("semantic_nonfactual_response")),
        Support::Supported if assessment.completeness == Completeness::MissingEssentialCaveats => {
            (State::PartiallySupported, Some("semantic_missing_caveat"))
        }
        Support::Supported if assessment.completeness == Completeness::Undetermined => {
            (State::PartiallySupported, Some("semantic_completeness_undetermined"))
        }
        Support::Supported if assessment.response_classification != Response::FactualAnswer => {
            (State::NotRun, Some("semantic_nonfactual_response"))
        }
        Support::Supported => (State::Verified, None),
    }
}

fn preserve_formal_floor(
    verification: &RuntimeAnswerVerification,
    proposed: State,
    formal_blocked: bool,
) -> State {
    if !formal_blocked || proposed == State::Conflicting {
        return proposed;
    }
    if verification.state == State::Verified {
        if !verification.unsupported_literals.is_empty()
            || verification.warnings.iter().any(|warning| {
                matches!(
                    warning.code.as_str(),
                    "unsupported_literal" | "unsupported_canonical_claim"
                )
            })
        {
            State::InsufficientEvidence
        } else if verification.warnings.iter().any(|warning| warning.code == "conflicting_evidence")
        {
            State::Conflicting
        } else {
            State::PartiallySupported
        }
    } else {
        verification.state
    }
}

pub(crate) fn has_formal_blocker(verification: &RuntimeAnswerVerification) -> bool {
    matches!(verification.state, State::Conflicting | State::Failed | State::InsufficientEvidence)
        || !verification.unsupported_literals.is_empty()
        || verification.warnings.iter().any(|warning| {
            !warning.code.starts_with("semantic_")
                && verification_warning_blocks_factual_readiness(&warning.code)
        })
}

fn append_warning(verification: &mut RuntimeAnswerVerification, code: &str) {
    if !verification.warnings.iter().any(|warning| warning.code == code) {
        verification.warnings.push(QueryVerificationWarning {
            code: code.into(),
            message: "Semantic verification did not establish a complete factual answer.".into(),
            related_segment_id: None,
            related_fact_id: None,
        });
    }
}

pub(crate) fn is_bounded_abstention(report: &SemanticVerificationReport) -> bool {
    matches!(&report.outcome, Outcome::Completed { assessment } if assessment.response_classification == Response::CorrectSourceBoundedAbstention)
}

pub(crate) fn bind_visible_answer(report: &mut SemanticVerificationReport, visible: &str) {
    report.visible_answer_sha256 = Some(super::preparation::sha256(visible.as_bytes()));
}

pub(crate) fn nonfactual_report(question: &str, answer: &str) -> SemanticVerificationReport {
    let mut report = super::preparation_failure_report(
        question,
        answer,
        super::SemanticPreparationError::NoEvidence,
        0,
    );
    report.outcome = Outcome::NotRun { reason: SemanticNotRunReason::NonFactualResponse };
    bind_visible_answer(&mut report, answer);
    report
}

pub(crate) fn receipt_matches(
    prepared: &super::PreparedSemanticVerification,
    report: &SemanticVerificationReport,
) -> bool {
    report.protocol_version == ironrag_contracts::assistant::semantic_verification::SEMANTIC_VERIFICATION_PROTOCOL_VERSION
        && report.question_sha256 == prepared.question_sha256()
        && report.checked_answer_sha256 == prepared.answer_sha256()
        && report.evidence_manifest_sha256 == prepared.manifest_sha256()
        && report.answer_units == prepared.units()
        && report.sources == prepared.sources()
}
