use super::*;

#[test]
fn technical_focus_probe_terms_prefer_structural_identifiers() {
    let mut ir = sample_ir_with_act(QueryAct::Describe, 0.8, None);
    ir.target_entities = vec![
        EntityMention { label: "OrderStateMachine".to_string(), role: EntityRole::Subject },
        EntityMention { label: "TransitionHooks".to_string(), role: EntityRole::Object },
    ];
    ir.literal_constraints =
        vec![LiteralSpan { text: "APP_DATABASE_URL".to_string(), kind: LiteralKind::Identifier }];

    let terms = super::collect_technical_focus_probe_terms(&ir);

    assert!(terms.iter().any(|term| term == "APP_DATABASE_URL"));
    assert!(terms.iter().any(|term| term == "OrderStateMachine"));
    assert!(terms.iter().any(|term| term == "TransitionHooks"));
    assert!(!terms.iter().any(|term| term == "SAMPLE_LIMIT"));
    assert!(terms.len() <= super::TECHNICAL_FOCUS_PROBE_TERM_LIMIT);
}

#[test]
fn technical_focus_probe_terms_ignore_untyped_raw_question_anchors() {
    let mut ir = sample_ir_with_act(QueryAct::Describe, 0.8, None);
    ir.target_entities.clear();

    let terms = super::collect_technical_focus_probe_terms(&ir);

    assert!(terms.is_empty());
}

#[test]
fn technical_focus_literal_inventory_preserves_exact_identifiers() {
    let mut seen = std::collections::HashSet::new();
    let focus =
        vec!["error".to_string(), "sample".to_string(), "cpu".to_string(), "circuit".to_string()];

    let literals = super::extract_focus_aligned_structural_literals(
        "pub enum OrderError { InvalidTransition } class CircuitBreaker: pass SAMPLE_LIMIT_REQUESTS aws_cloudwatch_metric_alarm.cpu",
        &focus,
        &mut seen,
    );

    assert!(literals.iter().any(|literal| literal == "OrderError"));
    assert!(literals.iter().any(|literal| literal == "CircuitBreaker"));
    assert!(literals.iter().any(|literal| literal == "SAMPLE_LIMIT_REQUESTS"));
    assert!(literals.iter().any(|literal| literal == "aws_cloudwatch_metric_alarm.cpu"));
}

#[test]
fn structural_literal_camel_case_detection_requires_lower_to_upper_transition() {
    assert!(super::token_has_internal_uppercase("sampleValue"));
    assert!(!super::token_has_internal_uppercase("SAMPLE"));
    assert!(!super::token_has_internal_uppercase("Title"));
}
