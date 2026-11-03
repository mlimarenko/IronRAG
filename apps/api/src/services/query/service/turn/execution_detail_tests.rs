use super::execution_detail::clear_references_for_system_statement;

#[test]
fn semantic_reload_binds_the_actual_visible_body_without_relabelling_checked_candidate() {
    use super::execution_detail::validate_semantic_visible_body;
    use crate::services::query::execution::semantic_verification::{
        SemanticPreparationError, bind_visible_answer, preparation_failure_report,
    };
    let mut report = preparation_failure_report(
        "Question",
        "Raw candidate",
        SemanticPreparationError::NoEvidence,
        0,
    );
    assert!(validate_semantic_visible_body(&report, "Visible fallback").is_err());
    bind_visible_answer(&mut report, "Visible fallback");
    let checked_identity = report.checked_answer_sha256.clone();
    assert!(validate_semantic_visible_body(&report, "Visible fallback").is_ok());
    assert!(validate_semantic_visible_body(&report, "Raw candidate").is_err());
    assert!(validate_semantic_visible_body(&report, "Changed visible fallback").is_err());
    assert_eq!(report.checked_answer_sha256, checked_identity);
}

#[test]
fn system_statement_clears_every_reference_vector() {
    let mut chunks = vec![1_u8, 2];
    let mut segments = vec!["segment"];
    let mut facts = vec![3_u16];
    let mut nodes = vec![4_u32];
    let mut edges = vec![5_u64];

    clear_references_for_system_statement(
        true,
        &mut chunks,
        &mut segments,
        &mut facts,
        &mut nodes,
        &mut edges,
    );

    assert!(chunks.is_empty());
    assert!(segments.is_empty());
    assert!(facts.is_empty());
    assert!(nodes.is_empty());
    assert!(edges.is_empty());
}

#[test]
fn grounded_answer_keeps_every_reference_vector() {
    let mut chunks = vec![1_u8];
    let mut segments = vec![2_u8];
    let mut facts = vec![3_u8];
    let mut nodes = vec![4_u8];
    let mut edges = vec![5_u8];

    clear_references_for_system_statement(
        false,
        &mut chunks,
        &mut segments,
        &mut facts,
        &mut nodes,
        &mut edges,
    );

    assert_eq!(chunks, vec![1_u8]);
    assert_eq!(segments, vec![2_u8]);
    assert_eq!(facts, vec![3_u8]);
    assert_eq!(nodes, vec![4_u8]);
    assert_eq!(edges, vec![5_u8]);
}
