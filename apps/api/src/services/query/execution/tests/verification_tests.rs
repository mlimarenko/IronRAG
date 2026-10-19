use super::*;

mod authoritative_evidence;
mod broad_procedure;
mod conflicts;
mod diagnostics_and_persistence;
mod enumeration_coverage;
mod exact_values;
mod runtime_literals;
mod structural_literals;

fn runtime_chunk(source_text: &str) -> RuntimeMatchedChunk {
    let document_id = Uuid::now_v7();
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id,
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: Some("paragraph".to_string()),
        document_label: "fixture.md".to_string(),
        excerpt: source_text.to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(1.0),
        source_text: source_text.to_string(),
    }
}

fn runtime_chunk_for_document(
    document_id: Uuid,
    document_label: &str,
    source_text: &str,
) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id,
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: Some("paragraph".to_string()),
        document_label: document_label.to_string(),
        excerpt: source_text.to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(1.0),
        source_text: source_text.to_string(),
    }
}

/// Empty authoritative evidence for verifier fixtures that exercise runtime
/// chunks and assistant grounding only.
fn empty_authoritative_evidence() -> AuthoritativeAnswerEvidence {
    AuthoritativeAnswerEvidence {
        bundle: None,
        chunk_rows: Vec::new(),
        structured_blocks: Vec::new(),
        technical_facts: Vec::new(),
    }
}

fn verify_broad_procedure(
    answer: &str,
    chunks: &[RuntimeMatchedChunk],
) -> RuntimeAnswerVerification {
    verify_answer_against_authoritative_evidence(AnswerVerificationRequest {
        question: "How do I configure Sample Connector?",
        answer,
        intent_profile: &QueryIntentProfile {
            act: Some(crate::domains::query_ir::QueryAct::ConfigureHow),
            broad_procedure_variant_coverage: true,
            ..QueryIntentProfile::default()
        },
        evidence: &empty_authoritative_evidence(),
        chunks,
        prompt_context: answer,
        assistant_grounding: &AssistantGroundingEvidence::default(),
    })
}
