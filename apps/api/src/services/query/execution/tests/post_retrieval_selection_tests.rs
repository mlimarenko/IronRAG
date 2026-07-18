use super::*;

fn acronym_how_to_ir(question: &str, acronym: &str) -> QueryIR {
    QueryIR {
        act: QueryAct::ConfigureHow,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![
            crate::domains::query_ir::QueryTargetKind::Concept,
            crate::domains::query_ir::QueryTargetKind::Procedure,
        ],
        target_entities: vec![EntityMention {
            label: acronym.to_string(),
            role: EntityRole::Subject,
        }],
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: Some(question.to_string()),
        confidence: 0.52,
    }
}

fn ranked_chunk(rank: i32, label: &str, text: &str) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: rank,
        chunk_kind: Some("paragraph".to_string()),
        document_label: label.to_string(),
        excerpt: text.to_string(),
        score_kind: RuntimeChunkScoreKind::Relevance,
        score: Some(100.0 - rank as f32),
        source_text: text.to_string(),
    }
}

#[test]
fn raw_acronym_how_to_does_not_select_commands_from_an_unbound_sibling() {
    let question = "Как подключить ZX?";
    let relevant = ranked_chunk(
        1,
        "Подключение устройства ZX",
        "Параметры протокола ZX перечислены в таблице устройства.",
    );
    let distractor = ranked_chunk(
        21,
        "Подключение клавиатуры SampleBoard",
        "# Подключение клавиатуры SampleBoard\n\
         apt-get purge sample-keyboard-utils\n\
         apt-get install sample-keyboard-utils",
    );

    let answer = super::super::answer::build_update_procedure_sequence_answer(
        question,
        &acronym_how_to_ir(question, "ZX"),
        &[relevant, distractor],
    );

    assert!(
        answer.is_none(),
        "an unbound sibling procedure must yield to grounded synthesis: {answer:?}"
    );
}

#[test]
fn lowercase_short_structured_identity_still_requires_bound_evidence() {
    let question = "How do I connect zx?";
    let relevant = ranked_chunk(
        1,
        "ZX connector reference",
        "The ZX connector exposes protocol settings in its device table.",
    );
    let distractor = ranked_chunk(
        21,
        "Connect SampleBoard keyboard",
        "Connect SampleBoard keyboard:\n\
         1. Remove sample-keyboard-utils.\n\
         2. Install sample-keyboard-utils.",
    );

    let answer = super::super::answer::build_update_procedure_sequence_answer(
        question,
        &acronym_how_to_ir(question, "zx"),
        &[relevant, distractor],
    );

    assert!(
        answer.is_none(),
        "compiler-preserved lowercase identity must not authorize sibling commands: {answer:?}"
    );
}

#[test]
fn literal_provenance_uses_the_same_direct_acronym_binding_guard() {
    let question = "How do I connect ZX?";
    let mut query_ir = acronym_how_to_ir(question, "ZX");
    query_ir.target_entities.clear();
    query_ir.literal_constraints =
        vec![LiteralSpan { kind: LiteralKind::Identifier, text: "ZX".to_string() }];
    let distractor = ranked_chunk(
        21,
        "Connect SampleBoard keyboard",
        "Connect SampleBoard keyboard:\n\
         1. Remove sample-keyboard-utils.\n\
         2. Install sample-keyboard-utils.",
    );
    let evidence = CanonicalAnswerEvidence {
        bundle: None,
        chunk_rows: Vec::new(),
        structured_blocks: Vec::new(),
        technical_facts: Vec::new(),
    };

    let answer = super::super::answer::build_deterministic_grounded_answer(
        question,
        &query_ir,
        &evidence,
        &[distractor],
    );

    assert!(
        answer.as_deref().is_none_or(|text| !text.contains("sample-keyboard-utils")),
        "literal-derived acronym identity must retain the binding guard: {answer:?}"
    );
}

#[test]
fn deterministic_answer_keeps_a_bound_direct_acronym_procedure() {
    let question = "How do I connect ZX?";
    let bound = ranked_chunk(
        1,
        "ZX connect guide",
        "ZX connect procedure:\n\
         1. Stop the ZX worker.\n\
         2. Attach the ZX adapter.\n\
         3. Restart the ZX worker.",
    );
    let evidence = CanonicalAnswerEvidence {
        bundle: None,
        chunk_rows: Vec::new(),
        structured_blocks: Vec::new(),
        technical_facts: Vec::new(),
    };

    let answer = super::super::answer::build_deterministic_grounded_answer(
        question,
        &acronym_how_to_ir(question, "ZX"),
        &evidence,
        &[bound],
    )
    .expect("an action-bound direct acronym procedure should stay available");

    assert!(answer.contains("Attach the ZX adapter"), "{answer}");
}

#[test]
fn direct_acronym_does_not_match_a_lowercase_ordinary_word() {
    let question = "how to update AS?";
    let unrelated = ranked_chunk(
        1,
        "Update as administrator",
        "Update as administrator:\n\
         1. Stop the unrelated workers.\n\
         2. Install the unrelated package.\n\
         3. Restart the unrelated workers.",
    );

    let answer = super::super::answer::build_update_procedure_sequence_answer(
        question,
        &acronym_how_to_ir(question, "AS"),
        &[unrelated],
    );

    assert!(
        answer.is_none(),
        "a lowercase ordinary word must not bind an uppercase acronym identity: {answer:?}"
    );
}

#[test]
fn full_subject_identity_beats_a_colliding_generated_acronym() {
    let question = "how to update Alpha Service?";
    let mut colliding = ranked_chunk(
        1,
        "AS update guide",
        "AS update:\n\
         1. Stop AS workers.\n\
         2. Install AS package version 9.0.0.\n\
         3. Restart AS workers.",
    );
    colliding.score = Some(10_000.0);
    let mut exact = ranked_chunk(
        2,
        "Alpha Service update guide",
        "Alpha Service update:\n\
         1. Stop Alpha Service workers.\n\
         2. Install Alpha Service package version 2.0.0.\n\
         3. Restart Alpha Service workers.",
    );
    exact.score = Some(1.0);

    let answer = super::super::answer::build_update_procedure_sequence_answer(
        question,
        &acronym_how_to_ir(question, "Alpha Service"),
        &[colliding, exact],
    )
    .expect("full subject identity must beat a colliding generated acronym");

    assert!(answer.contains("Alpha Service update guide"), "{answer}");
    assert!(answer.contains("version 2.0.0"), "{answer}");
    assert!(!answer.contains("version 9.0.0"), "{answer}");
}

#[test]
fn generic_target_runbook_beats_a_repeated_incidental_step_in_a_specialized_runbook() {
    let question = "How do I update Sample Server?";
    let specialized_document_id = Uuid::now_v7();
    let specialized_revision_id = Uuid::now_v7();
    let mut specialized = ranked_chunk(
        1,
        "Partner Integration migration guide",
        "Partner Integration migration:\n\
         1. Back up Partner Integration.\n\
         2. Update Partner Integration configuration.\n\
         3. Update Sample Server to version 7.0.\n\
         4. Update Partner Integration credentials.\n\
         5. Update Partner Integration routing.\n\
         6. Restart Partner Integration.",
    );
    specialized.document_id = specialized_document_id;
    specialized.revision_id = specialized_revision_id;
    let mut duplicated_projection = ranked_chunk(
        2,
        "Partner Integration migration guide",
        "3. Update Sample Server to version 7.0.",
    );
    duplicated_projection.document_id = specialized_document_id;
    duplicated_projection.revision_id = specialized_revision_id;
    let generic = ranked_chunk(
        20,
        "Shared operations handbook",
        "Updating Sample Server\n\
         1. Back up the current state.\n\
         2. Apply the release bundle.\n\
         3. Restart the service and validate its version.",
    );

    let mut query_ir = acronym_how_to_ir(question, "Sample Server");
    query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Version];

    let answer = super::super::answer::build_update_procedure_sequence_answer(
        question,
        &query_ir,
        &[specialized, duplicated_projection, generic],
    )
    .expect("the generic target runbook is directly actionable");

    assert!(answer.contains("Shared operations handbook"), "{answer}");
    assert!(answer.contains("Apply the release bundle"), "{answer}");
    assert!(!answer.contains("Partner Integration"), "{answer}");
}
