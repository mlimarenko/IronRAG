use uuid::Uuid;

use crate::{app::state::AppState, domains::query_ir::QueryIR};

use super::{
    AssistantGroundingEvidence, QueryAnswerDisposition, QueryClarification,
    QueryVerificationPersistence, QueryVerificationState, RuntimeAnswerVerification,
};

/// Finalize the deterministic missing-document statement as a terminal
/// non-factual answer.
///
/// The body is the product's own statement about the library, so verification
/// is `NotRun` with no warnings: there is no corpus-grounded claim to verify
/// and no semantic warning to attach. The persisted answer kind is what tells
/// read-side consumers to hide retrieval references for this execution.
pub(super) async fn finalize_deterministic_document_statement(
    state: &AppState,
    execution_id: Uuid,
    answer: &str,
    query_ir: &QueryIR,
) -> anyhow::Result<()> {
    let verification = RuntimeAnswerVerification {
        state: QueryVerificationState::NotRun,
        warnings: Vec::new(),
        unsupported_literals: Vec::new(),
    };
    let finalized = super::super::verification_policy::finalize_answer_visibility(
        query_ir.verification_level(),
        verification.state,
        &verification.warnings,
        query_ir.language,
        answer,
        super::super::verification_policy::AnswerVisibilityKind::DeterministicStatement,
    );
    anyhow::ensure!(
        matches!(finalized.disposition, QueryAnswerDisposition::SafeFallback),
        "deterministic document statement finalizer rejected its own body"
    );
    super::super::persist_query_verification(QueryVerificationPersistence {
        state,
        execution_id,
        verification: &verification,
        answer_disposition: finalized.disposition,
        clarification: &QueryClarification::default(),
        authoritative_evidence: &super::super::AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        assistant_grounding: &AssistantGroundingEvidence::default(),
        answer_kind: Some(super::super::answer_kind::AnswerKind::MissingExplicitDocument),
    })
    .await
}
