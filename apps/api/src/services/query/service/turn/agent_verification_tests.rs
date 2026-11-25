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

    let targets = literal_inventory_coverage_revision_targets(
        answer,
        &history,
        &AgentEvidenceManifest::default(),
        &grounding,
    );

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
            &AgentEvidenceManifest::default(),
            &grounding,
        )
        .is_empty()
    );
}

#[test]
fn literal_inventory_coverage_requires_current_grounding() {
    use crate::interfaces::http::mcp::{
        McpToolResult, tools::source_evidence::admitted_source_texts,
    };
    use crate::services::query::assistant_grounding::AdmittedAgentSource;
    let history = vec![ExternalConversationTurn {
        turn_kind: QueryTurnKind::Assistant,
        content_text:
            "ir.memory.literals.v1: `alphaPackage`, `endpointUrl`, `secretKey`, `searchMode`, `retryTimeout`, `draftKey`"
                .to_string(),
    }];
    let search = McpToolResult {
        content: vec![],
        is_error: false,
        structured_content: serde_json::json!({
            "query": "alphaPackage endpointUrl secretKey", "limit": 1, "libraries": ["w/l"],
            "hits": [{"documentId": Uuid::from_u128(4), "libraryId": Uuid::from_u128(5),
                "workspaceId": Uuid::from_u128(6), "documentTitle": "secretKey", "score": 1.0,
                "readabilityState": "readable", "readinessKind": "ready", "graphCoverageKind": "complete",
                "excerpt": "The `alphaPackage` accepts `endpointUrl` and `searchMode`."}]
        }),
    };
    let mut grounding = AssistantGroundingEvidence {
        verification_corpus: admitted_source_texts("search_documents", &search),
        ..Default::default()
    };
    grounding.verification_corpus.push(
        "[MCP tool result: grounded_answer]\n`alphaPackage`, `endpointUrl`, and `draftKey`.".into(),
    );
    let manifest = AgentEvidenceManifest {
        sources: vec![AdmittedAgentSource {
            chunk_id: Uuid::from_u128(1),
            document_id: Uuid::from_u128(2),
            document_revision_id: Uuid::from_u128(3),
            text: "Set `retryTimeout` alongside `alphaPackage` and `endpointUrl`.".into(),
        }],
        ..Default::default()
    };
    let answer = "Configure `endpointUrl` and `alphaPackage`.";

    let targets =
        literal_inventory_coverage_revision_targets(answer, &history, &manifest, &grounding);

    assert_eq!(targets, ["searchMode", "retryTimeout"]);
    let context = manifest.source_context(&grounding);
    assert!(context.contains("Set `retryTimeout` alongside"));
    assert!(context.contains("`endpointUrl` and `searchMode`"));
    assert!(!context.contains("secretKey"));
    assert!(!context.contains("draftKey"));
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
