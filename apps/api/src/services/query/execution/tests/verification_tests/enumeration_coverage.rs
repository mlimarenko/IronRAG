use super::*;

fn enumerate_query_ir() -> crate::domains::query_ir::QueryIR {
    crate::domains::query_ir::QueryIR::test_with_act_scope(
        crate::domains::query_ir::QueryAct::Enumerate,
        crate::domains::query_ir::QueryScope::SingleDocument,
    )
}

fn method_fact(value: &str, document_id: Uuid, revision_id: Uuid) -> KnowledgeTechnicalFactRow {
    KnowledgeTechnicalFactRow {
        fact_kind: "http_method".to_string(),
        normalized_value_text: value.to_string(),
        normalized_value_exact: value.to_string(),
        normalized_value_json: serde_json::json!(value),
        display_value: value.to_string(),
        ..sample_technical_fact_row(Uuid::now_v7(), document_id, revision_id)
    }
}

fn enumeration_evidence(values: &[&str]) -> AuthoritativeAnswerEvidence {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    AuthoritativeAnswerEvidence {
        bundle: None,
        chunk_rows: Vec::new(),
        structured_blocks: Vec::new(),
        technical_facts: values
            .iter()
            .map(|value| method_fact(value, document_id, revision_id))
            .collect(),
    }
}

fn verified_verification() -> RuntimeAnswerVerification {
    RuntimeAnswerVerification {
        state: QueryVerificationState::Verified,
        warnings: Vec::new(),
        unsupported_literals: Vec::new(),
    }
}

#[test]
fn one_item_enumeration_answer_is_demoted_when_evidence_holds_more_values() {
    let mut verification = verified_verification();

    apply_enumeration_coverage_warning(
        "The documented method is `Read`.",
        &enumerate_query_ir(),
        &enumeration_evidence(&["Read", "Create", "Update", "Delete"]),
        &mut verification,
    );

    assert_eq!(verification.state, QueryVerificationState::PartiallySupported);
    assert!(
        verification.warnings.iter().any(|warning| warning.code == "enumeration_coverage_partial"),
        "{:?}",
        verification.warnings
    );
}

#[test]
fn complete_enumeration_answer_stays_verified() {
    let mut verification = verified_verification();

    apply_enumeration_coverage_warning(
        "Documented methods: `Read`, `Create`, `Update`, `Delete`.",
        &enumerate_query_ir(),
        &enumeration_evidence(&["Read", "Create", "Update", "Delete"]),
        &mut verification,
    );

    assert_eq!(verification.state, QueryVerificationState::Verified);
    assert!(verification.warnings.is_empty());
}

#[test]
fn single_documented_value_enumeration_stays_verified() {
    let mut verification = verified_verification();

    apply_enumeration_coverage_warning(
        "The only documented method is `Read`.",
        &enumerate_query_ir(),
        &enumeration_evidence(&["Read"]),
        &mut verification,
    );

    assert_eq!(verification.state, QueryVerificationState::Verified);
    assert!(verification.warnings.is_empty());
}

#[test]
fn non_enumeration_act_is_never_demoted_by_enumeration_coverage() {
    let mut verification = verified_verification();
    let mut ir = enumerate_query_ir();
    ir.act = crate::domains::query_ir::QueryAct::RetrieveValue;

    apply_enumeration_coverage_warning(
        "The documented method is `Read`.",
        &ir,
        &enumeration_evidence(&["Read", "Create", "Update", "Delete"]),
        &mut verification,
    );

    assert_eq!(verification.state, QueryVerificationState::Verified);
    assert!(verification.warnings.is_empty());
}

#[test]
fn enumeration_untouched_when_answer_mentions_no_typed_fact() {
    let mut verification = verified_verification();

    apply_enumeration_coverage_warning(
        "The document describes its request lifecycle.",
        &enumerate_query_ir(),
        &enumeration_evidence(&["Read", "Create", "Update", "Delete"]),
        &mut verification,
    );

    assert_eq!(verification.state, QueryVerificationState::Verified);
    assert!(verification.warnings.is_empty());
}
