use super::*;

fn base_ir(act: QueryAct) -> QueryIR {
    QueryIR {
        act,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::En,
        target_types: Vec::new(),
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

#[test]
fn valid_descriptive_ir_passes() {
    assert!(validate_ir(&base_ir(QueryAct::Describe)).is_ok());
}

#[test]
fn compare_without_comparison_fails() {
    assert_eq!(
        validate_ir(&base_ir(QueryAct::Compare)),
        Err(QueryIrValidationError::CompareWithoutComparison)
    );
}

#[test]
fn follow_up_without_refs_and_confident_fails() {
    let mut ir = base_ir(QueryAct::FollowUp);
    ir.confidence = 0.9;
    assert_eq!(validate_ir(&ir), Err(QueryIrValidationError::FollowUpWithoutRefs));
}

#[test]
fn follow_up_without_refs_but_low_confidence_passes() {
    let mut ir = base_ir(QueryAct::FollowUp);
    ir.confidence = 0.3;
    assert!(validate_ir(&ir).is_ok());
}

#[test]
fn confidence_out_of_range_fails() {
    let mut ir = base_ir(QueryAct::Describe);
    ir.confidence = 1.5;
    assert!(matches!(validate_ir(&ir), Err(QueryIrValidationError::ConfidenceOutOfRange(_))));
}
