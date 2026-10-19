use super::*;

#[test]
fn literal_revision_rejects_internal_query_block_replacement() {
    let draft = "- `alpha-one` — supported item.\n- `alpha-two` — supported item.";
    let revision = "scope: prior answer\nentities: alpha-one, alpha-two\nquestion: describe each";

    assert!(!literal_revision_can_replace_answer(draft, revision));
    assert!(literal_revision_can_replace_answer(
        draft,
        "- `alpha-one` — supported item.\n- `alpha-two` — supported item."
    ));
}

#[test]
fn literal_revision_allows_removing_unsupported_synthetic_code_blocks() {
    let draft = format!(
        "{}\n\n```ini\n[Generated]\nalpha = <value>\nbeta = <value>\ngamma = <value>\n```\n",
        "This setup uses the Alpha connector. ".repeat(24)
    );
    let revision = "This setup uses the Alpha connector. The evidence provides the parameter names but not a complete ready configuration file.";

    assert!(literal_revision_can_replace_answer(&draft, revision));
}
#[test]
fn literal_revision_targets_include_fenced_block_for_unsupported_fenced_line() {
    let answer = "Supported facts:\n\n```ini\n[Alpha]\nendpoint = https://alpha.example\nsecretKey = <secret>\n```\n\nDone.";
    let targets = literal_revision_targets(answer, &["secretKey = <secret>".to_string()]);

    assert_eq!(targets[0], "secretKey = <secret>");
    assert!(targets.iter().any(|target| {
        target == "[Alpha]\nendpoint = https://alpha.example\nsecretKey = <secret>"
    }));
}

#[test]
fn literal_revision_targets_do_not_duplicate_existing_targets() {
    let block = "[Alpha]\nsecretKey = <secret>";
    let answer = format!("```ini\n{block}\n```");
    let targets = literal_revision_targets(&answer, &[block.to_string()]);

    assert_eq!(targets, vec![block.to_string()]);
}

#[test]
fn declared_subject_literal_revision_surfaces_evidence_spelling() {
    let mut query_ir = sample_ir_with_act(QueryAct::Describe, 0.9, None);
    query_ir.target_entities = vec![EntityMention {
        label: "request guard pattern".to_string(),
        role: EntityRole::Subject,
    }];
    let evidence = "class RequestGuard:\n    state: GuardState\n    failure_count: int";

    assert_eq!(
        declared_subject_literal_revision_targets(
            "The guard tracks failures and changes state.",
            &query_ir,
            evidence,
        ),
        vec!["RequestGuard".to_string()],
    );
}

#[test]
fn declared_subject_literal_revision_uses_typed_identifier_literals_only() {
    let mut query_ir = sample_ir_with_act(QueryAct::Describe, 0.9, None);
    query_ir.literal_constraints =
        vec![LiteralSpan { text: "WorkerPool".to_string(), kind: LiteralKind::Identifier }];
    let evidence = concat!(
        "class WorkerPool:\n",
        "    workers: list\n",
        "class UnrelatedPool:\n",
        "    workers: list",
    );

    assert_eq!(
        declared_subject_literal_revision_targets(
            "The pool owns a bounded worker set.",
            &query_ir,
            evidence,
        ),
        vec!["WorkerPool".to_string()],
    );
}

#[test]
fn declared_subject_literal_revision_requires_a_declaration_and_total_omission() {
    let mut query_ir = sample_ir_with_act(QueryAct::Describe, 0.9, None);
    query_ir.target_entities =
        vec![EntityMention { label: "RequestGuard".to_string(), role: EntityRole::Subject }];

    assert!(
        declared_subject_literal_revision_targets(
            "RequestGuard tracks failures.",
            &query_ir,
            "class RequestGuard:\n    failure_count: int",
        )
        .is_empty()
    );
    assert!(
        declared_subject_literal_revision_targets(
            "The guard tracks failures.",
            &query_ir,
            "RequestGuard is referenced here without a declaration body.",
        )
        .is_empty()
    );
}

#[test]
fn declared_subject_literal_revision_is_bounded_per_answer() {
    let mut query_ir = sample_ir_with_act(QueryAct::Describe, 0.9, None);
    query_ir.target_entities = (0..12)
        .map(|index| EntityMention {
            label: format!("SubjectHandler{index}"),
            role: EntityRole::Subject,
        })
        .collect();
    let evidence = (0..12)
        .map(|index| format!("class SubjectHandler{index}:\n    state: int"))
        .collect::<Vec<_>>()
        .join("\n");

    assert_eq!(
        declared_subject_literal_revision_targets(
            "The handlers track state.",
            &query_ir,
            &evidence
        )
        .len(),
        8,
    );
}

/// The restoration stage triggers a SECOND full-answer generation, so it must
/// stay rare. It once also targeted any compound literal from evidence whose
/// bare value reached the answer (`MODE=active` when the answer wrote
/// `active`) — ordinary paraphrasing, which made the "conditional" stage fire
/// on 100 % of live executions and 62 % of golden answers for no measurable
/// quality gain (paired golden runs: 104/109 strict with it, 108/108 without,
/// p95 ~55 s vs ~31 s). Only the question's own declared subject earns the
/// second generation.
#[test]
fn restoration_targets_only_the_questions_declared_subject() {
    let mut query_ir = QueryIR::test_with_act_scope(
        crate::domains::query_ir::QueryAct::RetrieveValue,
        crate::domains::query_ir::QueryScope::SingleDocument,
    );
    query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
        label: "alpha_gateway".to_string(),
        role: crate::domains::query_ir::EntityRole::Subject,
    }];
    // A declaration needs a body: a bare `name: value` line is not one.
    let evidence = "alpha_gateway:\n  mode: active\n  cache: 4812";

    // A paraphrased compound literal is not a restoration target: the answer
    // carries the subject identifier, so nothing is missing.
    let paraphrasing_answer = "The alpha_gateway runs in active mode with cache 4812.";
    assert!(
        declared_subject_literal_revision_targets(paraphrasing_answer, &query_ir, evidence)
            .is_empty()
    );

    // Dropping the subject's own declared identifier still is.
    let subjectless_answer = "It runs in active mode with cache 4812.";
    assert_eq!(
        declared_subject_literal_revision_targets(subjectless_answer, &query_ir, evidence),
        vec!["alpha_gateway".to_string()]
    );
}
