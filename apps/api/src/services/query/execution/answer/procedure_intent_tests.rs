use super::test_support::*;
use super::*;

#[test]
fn typed_versioned_procedure_renders_exact_identity_formal_steps() {
    let mut chunk = evidence_chunk(
        1,
        Some("paragraph"),
        concat!(
            "Sample Target lifecycle sequence:\n",
            "1. alpha-admin prepare --target=/srv/alpha\n",
            "2. alpha-admin switch --version=2.0.0\n",
            "3. alpha-admin verify --format=json",
        ),
    );
    chunk.document_label = "Sample Target lifecycle guide".to_string();

    let answer = build_update_procedure_sequence_answer(
        &configure_update_focus_ir("Sample Target"),
        &[chunk],
    )
    .expect("typed exact identity and formal steps should produce an answer");

    assert!(answer.contains("alpha-admin prepare --target=/srv/alpha"));
    assert!(answer.contains("alpha-admin switch --version=2.0.0"));
    assert!(answer.contains("alpha-admin verify --format=json"));
}

#[test]
fn typed_versioned_procedure_fails_closed_without_version_or_with_concept() {
    let mut chunk = evidence_chunk(
        1,
        Some("paragraph"),
        "Sample Target:\n1. alpha-admin prepare\n2. alpha-admin verify",
    );
    chunk.document_label = "Sample Target guide".to_string();
    let mut without_version = configure_update_focus_ir("Sample Target");
    without_version.target_types = vec![QueryTargetKind::Procedure];
    let mut concept = configure_update_focus_ir("Sample Target");
    concept.target_types.push(QueryTargetKind::Concept);

    assert!(
        build_update_procedure_sequence_answer(&without_version, std::slice::from_ref(&chunk),)
            .is_none()
    );
    assert!(build_update_procedure_sequence_answer(&concept, &[chunk]).is_none());
}

#[test]
fn typed_versioned_procedure_requires_exact_identity() {
    let mut chunk = evidence_chunk(
        1,
        Some("paragraph"),
        "Sample Targets:\n1. alpha-admin prepare\n2. alpha-admin verify",
    );
    chunk.document_label = "Sample Targets guide".to_string();

    assert!(
        build_update_procedure_sequence_answer(
            &configure_update_focus_ir("Sample Target"),
            &[chunk],
        )
        .is_none()
    );
}

#[test]
fn typed_versioned_procedure_rejects_unbound_materialization_script() {
    let mut chunk = evidence_chunk(
        1,
        Some("paragraph"),
        concat!(
            "Sample Target:\n",
            "1. curl https://example.invalid/runner.sh -o /tmp/runner.sh\n",
            "2. chmod +x /tmp/runner.sh\n",
            "3. /tmp/runner.sh",
        ),
    );
    chunk.document_label = "Sample Target guide".to_string();

    assert!(
        build_update_procedure_sequence_answer(
            &configure_update_focus_ir("Sample Target"),
            &[chunk],
        )
        .is_none()
    );
}

#[test]
fn mechanism_description_does_not_project_assignments_as_procedure_steps() {
    let mut query_ir = configure_how_focus_ir("request guard pattern");
    query_ir.act = QueryAct::Describe;
    query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Concept];
    query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
        label: "request guard pattern".to_string(),
        role: crate::domains::query_ir::EntityRole::Subject,
    }];
    query_ir.retrieval_query = Some("request guard pattern states and threshold".to_string());
    let mut chunk = evidence_chunk(
        1,
        Some("paragraph"),
        concat!(
            "class GuardState:\n",
            "    CLOSED = \"closed\"\n",
            "    OPEN = \"open\"\n",
            "class RequestGuard:\n",
            "    _state: GuardState = GuardState.CLOSED\n",
            "    _failure_count: int = 0\n",
            "    failure_threshold: int = 4\n",
        ),
    );
    chunk.document_label = "request_guard.py".to_string();

    let answer = build_update_procedure_sequence_answer(&query_ir, &[chunk]);

    assert!(
        answer.is_none(),
        "a descriptive act must reach narrative synthesis instead of a Steps projection: {answer:?}",
    );
}

#[test]
fn generic_procedure_target_is_not_a_lifecycle_signal() {
    let mut ir = configure_how_focus_ir("Sample Target");
    ir.target_types = vec![QueryTargetKind::Procedure];

    assert!(!question_requests_lifecycle_update_procedure("", &ir));
}
