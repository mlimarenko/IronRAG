use super::test_prelude::*;

#[test]
fn literal_inventory_coverage_targets_missing_identifier_literals() {
    let history = vec![ExternalConversationTurn {
        turn_kind: QueryTurnKind::Assistant,
        content_text: "ir.memory.literals.v1: `alphaPackage`, `endpointUrl`, `partnerId`, `secretKey`, `retryTimeout`, `sendDetails`\nSample Subject setup.".to_string(),
    }];
    let grounding = AssistantGroundingEvidence {
        verification_corpus: vec![
            "Current evidence supports `alphaPackage`, `endpointUrl`, `partnerId`, `secretKey`, `retryTimeout`, and `sendDetails`.".to_string(),
        ],
        document_references: Vec::new(),
    };
    let answer = "Configure `endpointUrl`, `partnerId`, `secretKey`, and `sendDetails`.";

    let targets = literal_inventory_coverage_revision_targets(answer, &history, &grounding);

    assert_eq!(targets, vec!["alphaPackage".to_string(), "retryTimeout".to_string()]);
}

#[test]
fn literal_inventory_coverage_waits_until_answer_enumerates_inventory() {
    let history = vec![ExternalConversationTurn {
        turn_kind: QueryTurnKind::Assistant,
        content_text: "ir.memory.literals.v1: `alphaPackage`, `endpointUrl`, `partnerId`, `secretKey`, `retryTimeout`, `sendDetails`".to_string(),
    }];
    let grounding = AssistantGroundingEvidence {
        verification_corpus: vec![
            "Current evidence supports `alphaPackage`, `endpointUrl`, `partnerId`, `secretKey`, `retryTimeout`, and `sendDetails`.".to_string(),
        ],
        document_references: Vec::new(),
    };

    assert!(
        literal_inventory_coverage_revision_targets(
            "Use `endpointUrl` for this one setting.",
            &history,
            &grounding,
        )
        .is_empty()
    );
}

#[test]
fn literal_inventory_coverage_requires_current_grounding() {
    let history = vec![ExternalConversationTurn {
        turn_kind: QueryTurnKind::Assistant,
        content_text:
            "ir.memory.literals.v1: `alphaPackage`, `endpointUrl`, `secretKey`, `/opt/alpha.conf`, `retryTimeout`"
                .to_string(),
    }];
    let grounding = AssistantGroundingEvidence {
        verification_corpus: vec![
            "Current evidence supports `alphaPackage`, `endpointUrl`, and `retryTimeout`."
                .to_string(),
        ],
        document_references: Vec::new(),
    };
    let answer = "Configure `endpointUrl` and `alphaPackage`.";

    let targets = literal_inventory_coverage_revision_targets(answer, &history, &grounding);

    assert_eq!(targets, vec!["retryTimeout".to_string()]);
}

#[test]
fn literal_fidelity_revision_detects_history_anchor_loss() {
    let history = vec![ExternalConversationTurn {
        turn_kind: QueryTurnKind::Assistant,
        content_text:
            "ir.memory.literals.v1: `alphaPackage`, `/opt/alpha.ini`, `[Main]`, `retryTimeout`, `visible`, `false`"
                .to_string(),
    }];
    let draft =
        "Install `alphaPackage`, edit `/opt/alpha.ini`, use `[Main]`, and set `retryTimeout`.";
    let revised = "Set `retryTimeout` after installation.";

    assert_eq!(
        literal_revision_history_literal_coverage(draft, revised, &history),
        Some((4, 1, 4))
    );
}

#[test]
fn literal_fidelity_revision_ignores_plain_boolean_memory_values() {
    let history = vec![ExternalConversationTurn {
        turn_kind: QueryTurnKind::Assistant,
        content_text: "ir.memory.literals.v1: `true`, `false`, `enabled`, `disabled`".to_string(),
    }];

    assert_eq!(
        literal_revision_history_literal_coverage(
            "Set `true` and `false`.",
            "Set the supported values.",
            &history,
        ),
        None
    );
}

#[test]
fn literal_inventory_revision_requires_target_coverage() {
    let required = vec!["alphaPackage".to_string(), "retryTimeout".to_string()];

    assert!(literal_revision_covers_required_literals(
        "Use `alphaPackage` and set `retryTimeout`.",
        &required,
    ));
    assert!(!literal_revision_covers_required_literals("Use `alphaPackage`.", &required,));
}

#[test]
fn literal_inventory_presence_uses_identifier_boundaries() {
    assert!(text_contains_literal("Set `retryTimeout`.", "retryTimeout"));
    assert!(!text_contains_literal("Set `retryTimeoutMs`.", "retryTimeout"));
    assert!(!text_contains_literal("Set `alpha.retryTimeout`.", "retryTimeout"));
    assert!(!text_contains_literal("Set `retry-timeout`.", "timeout"));
}

#[test]
fn literal_inventory_dedup_preserves_case_sensitive_literals() {
    let history = vec![ExternalConversationTurn {
        turn_kind: QueryTurnKind::Assistant,
        content_text:
            "ir.memory.literals.v1: `retryTimeout`, `RetryTimeout`, `endpointUrl`, `secretKey`"
                .to_string(),
    }];

    assert_eq!(
        latest_dense_history_identifier_literals(&history),
        vec![
            "retryTimeout".to_string(),
            "RetryTimeout".to_string(),
            "endpointUrl".to_string(),
            "secretKey".to_string()
        ]
    );
}
