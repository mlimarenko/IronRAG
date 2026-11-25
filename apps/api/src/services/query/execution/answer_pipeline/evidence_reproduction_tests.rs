//! Behaviour of the answer boundary over a document whose body is a long
//! comment/activity feed: the answer stage must synthesize from bounded
//! evidence, and an unverified echo of that evidence must never be published as
//! the answer.

use serde_json::json;

use super::super::types::{
    AnswerGenerationStage, AnswerVerificationStage, AuthoritativeAnswerEvidence,
    RuntimeAnswerVerification, RuntimeChunkScoreKind, RuntimeMatchedChunk,
};
use super::super::verification_policy::{AnswerVisibilityKind, finalize_answer_visibility};
use super::coverage::single_shot_answer_is_acceptable;
use super::evidence_reproduction::{
    ANSWER_REPRODUCED_EVIDENCE, evidence_reproduction_warning, measure_evidence_reproduction,
};
use crate::domains::query::{QueryAnswerDisposition, QueryVerificationState};
use crate::domains::query_ir::{
    QueryAct, QueryIR, QueryLanguage, QueryScope, QueryTargetKind, VerificationLevel,
};
use crate::services::query::assistant_grounding::AssistantGroundingEvidence;
use crate::services::query::i18n::deterministic_query_messages;
use crate::services::query::planner::QueryIntentProfile;

/// A tracker-style record whose whole body is comments: timestamps, authors and
/// status updates with no summary fields. Deliberately synthetic and
/// domain-neutral; the defect it reproduces is structural, not corpus-specific.
fn synthetic_comment_feed() -> String {
    (1..=12)
        .map(|step| {
            format!(
                "- Author: synthetic-operator\n\
                 - Created: 2026-01-05T10:{step:02}:00Z\n\
                 - Progress: step {step} finished; artifacts retained; removed objects absent; surviving objects and storage bound; independent readback recorded before the next step started.\n"
            )
        })
        .collect()
}

/// The evidence block the grounded-answer prompt receives.
fn synthetic_grounded_context() -> String {
    format!(
        "Retrieved document briefs\n- Rollout record: synthetic tracker export\n\n\
         EVIDENCE_CHUNK document=\"rollout-record\" chunk=\"2\"\n{}\n\
         EVIDENCE_CHUNK document=\"rollout-record\" chunk=\"3\"\n{}\n",
        synthetic_comment_feed(),
        synthetic_comment_feed()
    )
}

/// A real answer to a synthesis question over that feed.
fn synthesized_answer() -> String {
    "Продукт v1 — это один активный контур: сервис, его хранилище и права доступа, которые перечислены в записи. \
     Из активного контура выводятся прежние имена и прежняя реализация: старые приложения, их сервисы и подтверждающие их контроллеры. \
     Доказанность означает не объявление, а проверяемый набор: удалённые объекты не находятся в кластере, сохраняемые объекты и хранилища остаются на месте, \
     а приёмка подтверждена отдельным протоколом `independent-removal-resume-2026-01-05.terminal.json`."
        .to_string()
}

fn synthesis_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::Describe,
        scope: QueryScope::MultiDocument,
        language: QueryLanguage::Ru,
        target_types: vec![QueryTargetKind::Concept],
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.9,
    }
}

fn evidence_chunk(text: &str) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: uuid::Uuid::now_v7(),
        document_id: uuid::Uuid::now_v7(),
        revision_id: uuid::Uuid::now_v7(),
        chunk_index: 2,
        chunk_kind: Some("comment".to_string()),
        document_label: "Rollout record".to_string(),
        excerpt: text.to_string(),
        score_kind: RuntimeChunkScoreKind::Relevance,
        score: Some(0.8),
        source_text: text.to_string(),
    }
}

fn verification_stage(answer: String, prompt_context: String) -> AnswerVerificationStage {
    AnswerVerificationStage {
        generation: AnswerGenerationStage {
            intent_profile: QueryIntentProfile::default(),
            primary_answer_chunks: vec![evidence_chunk(&synthetic_comment_feed())],
            authoritative_evidence: AuthoritativeAnswerEvidence {
                bundle: None,
                chunk_rows: Vec::new(),
                structured_blocks: Vec::new(),
                technical_facts: Vec::new(),
            },
            assistant_grounding: AssistantGroundingEvidence::default(),
            answer,
            usage_json: json!({}),
            prompt_context,
            query_ir: synthesis_ir(),
        },
        verification: RuntimeAnswerVerification {
            semantic_verification: None,
            state: QueryVerificationState::Failed,
            warnings: Vec::new(),
            unsupported_literals: Vec::new(),
        },
    }
}

#[test]
fn comment_feed_returned_as_the_answer_is_a_wholesale_reproduction() {
    let evidence = synthetic_grounded_context();
    let answer = synthetic_comment_feed();

    let reproduction = measure_evidence_reproduction(&answer, [evidence.as_str()]);

    assert!(reproduction.is_wholesale(), "a copied comment feed must be measured as evidence");
}

#[test]
fn synthesis_over_the_same_feed_is_not_a_reproduction() {
    let evidence = synthetic_grounded_context();

    let reproduction = measure_evidence_reproduction(&synthesized_answer(), [evidence.as_str()]);

    assert!(!reproduction.is_wholesale(), "a synthesized answer quotes no sentence-long run");
}

#[test]
fn short_exact_quote_keeps_a_short_answer_an_answer() {
    let evidence = synthetic_grounded_context();
    let answer =
        "Удаление подтверждено протоколом `independent-removal-resume-2026-01-05.terminal.json`.";

    assert!(!measure_evidence_reproduction(answer, [evidence.as_str()]).is_wholesale());
}

#[test]
fn verified_body_is_not_flagged_even_when_it_quotes_the_requested_section() {
    let evidence = synthetic_grounded_context();
    let echo = synthetic_comment_feed();

    assert!(
        evidence_reproduction_warning(&echo, [evidence.as_str()], QueryVerificationState::Verified)
            .is_none(),
        "a body the verifier established stays the answer"
    );
}

#[test]
fn unverified_comment_feed_body_gets_a_typed_warning() {
    let evidence = synthetic_grounded_context();
    let echo = synthetic_comment_feed();

    for state in [
        QueryVerificationState::Failed,
        QueryVerificationState::PartiallySupported,
        QueryVerificationState::NotRun,
    ] {
        let warning = evidence_reproduction_warning(&echo, [evidence.as_str()], state)
            .expect("an unverified evidence echo must be reported");
        assert_eq!(warning.code, ANSWER_REPRODUCED_EVIDENCE);
    }
}

#[test]
fn unverified_synthesis_is_not_flagged() {
    let evidence = synthetic_grounded_context();

    assert!(
        evidence_reproduction_warning(
            &synthesized_answer(),
            [evidence.as_str()],
            QueryVerificationState::Failed
        )
        .is_none()
    );
}

#[test]
fn comment_feed_echo_is_not_accepted_as_a_single_shot_answer() {
    let evidence = synthetic_grounded_context();
    let echo = synthetic_comment_feed();
    let stage = verification_stage(echo.clone(), evidence.clone());

    assert!(
        !single_shot_answer_is_acceptable(&echo, &stage, 1, &synthesis_ir(), &evidence),
        "the pipeline must escalate instead of accepting the evidence as its answer"
    );
}

#[test]
fn synthesis_over_the_feed_is_accepted_as_a_single_shot_answer() {
    let evidence = synthetic_grounded_context();
    let answer = synthesized_answer();
    let stage = verification_stage(answer.clone(), evidence.clone());

    assert!(single_shot_answer_is_acceptable(&answer, &stage, 1, &synthesis_ir(), &evidence));
}

#[test]
fn unverified_comment_feed_body_visible_answer_is_a_bounded_refusal() {
    let evidence = synthetic_grounded_context();
    let echo = synthetic_comment_feed();
    let state = QueryVerificationState::Failed;
    let warning = evidence_reproduction_warning(&echo, [evidence.as_str()], state)
        .expect("an unverified evidence echo must be reported");

    let finalized = finalize_answer_visibility(
        VerificationLevel::Moderate,
        state,
        &[warning],
        QueryLanguage::Ru,
        &echo,
        AnswerVisibilityKind::FactualCandidate,
    );

    assert_eq!(finalized.disposition, QueryAnswerDisposition::SafeFallback);
    assert_eq!(
        &*finalized.visible_answer,
        deterministic_query_messages(QueryLanguage::Ru).evidence_reproduction_refusal
    );
    assert!(!finalized.visible_answer.contains("Created:"), "no evidence fragment is published");
}
