use super::*;

fn configure_how_ir(subject: &str) -> QueryIR {
    QueryIR {
        act: QueryAct::ConfigureHow,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![
            crate::domains::query_ir::QueryTargetKind::ConfigurationFile,
            crate::domains::query_ir::QueryTargetKind::Package,
            crate::domains::query_ir::QueryTargetKind::Procedure,
        ],
        target_entities: vec![EntityMention {
            label: subject.to_string(),
            role: EntityRole::Subject,
        }],
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: Some(format!("how to configure {subject}")),
        confidence: 0.52,
    }
}

fn setup_variant_chunk(rank: i32, provider: &str, package: &str) -> RuntimeMatchedChunk {
    let text = format!(
        "Sample Connector {provider} configuration\n\
         sample-install {package}\n\
         Settings are defined in /etc/sample/connector.conf in section [{provider}Provider].\n\
         providerName = {provider}"
    );
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: rank,
        chunk_kind: Some("paragraph".to_string()),
        document_label: format!("Sample Connector {provider} provider setup"),
        excerpt: text.clone(),
        score_kind: RuntimeChunkScoreKind::Relevance,
        score: Some(100.0 - rank as f32),
        source_text: text,
    }
}

#[test]
fn setup_variants_with_shared_path_yield_broad_single_document_request_to_synthesis() {
    let query_ir = configure_how_ir("Sample Connector");
    let chunks = vec![
        setup_variant_chunk(1, "Atlas", "atlas-connector"),
        setup_variant_chunk(2, "Boreal", "boreal-connector"),
    ];

    let candidate = super::super::answer::build_setup_configuration_anchor_candidate(
        "How do I configure Sample Connector?",
        &query_ir,
        &chunks,
    )
    .expect("both provider variants are actionable setup evidence");

    assert!(candidate.is_multi_variant(), "shared generic paths must not collapse providers");
    assert!(
        !candidate.should_use_as_preflight_answer(&query_ir, &chunks),
        "a broad request must yield distinct provider variants to clarification or synthesis"
    );
    assert!(
        !candidate.should_use_as_direct_answer(&query_ir, &chunks),
        "the direct renderer must not concatenate distinct provider procedures"
    );
}

#[test]
fn shared_path_variants_stay_non_direct_for_broad_concept_procedure_ir() {
    let mut query_ir = configure_how_ir("Sample Connector");
    query_ir.target_types = vec![
        crate::domains::query_ir::QueryTargetKind::Concept,
        crate::domains::query_ir::QueryTargetKind::Procedure,
    ];
    let chunks = vec![
        setup_variant_chunk(1, "Atlas", "atlas-connector"),
        setup_variant_chunk(2, "Boreal", "boreal-connector"),
    ];

    let candidate = super::super::answer::build_setup_configuration_anchor_candidate(
        "How do I configure Sample Connector?",
        &query_ir,
        &chunks,
    )
    .expect("the evidence still contains two structural setup variants");

    assert!(candidate.is_multi_variant(), "the compiler target shape must not collapse variants");
    assert!(!candidate.should_use_as_preflight_answer(&query_ir, &chunks));
    assert!(!candidate.should_use_as_direct_answer(&query_ir, &chunks));
}

#[test]
fn shared_package_does_not_collapse_distinct_provider_sections() {
    let query_ir = configure_how_ir("Sample Connector");
    let chunks = vec![
        setup_variant_chunk(1, "Atlas", "shared-connector"),
        setup_variant_chunk(2, "Boreal", "shared-connector"),
    ];

    let candidate = super::super::answer::build_setup_configuration_anchor_candidate(
        "How do I configure Sample Connector?",
        &query_ir,
        &chunks,
    )
    .expect("both provider sections are actionable setup evidence");

    assert!(
        candidate.is_multi_variant(),
        "a shared package must not erase two mutually unique provider section anchors"
    );
    assert!(!candidate.should_use_as_direct_answer(&query_ir, &chunks));
}

#[test]
fn focused_single_setup_variant_remains_directly_answerable() {
    let query_ir = configure_how_ir("Sample Connector Atlas");
    let chunks = vec![setup_variant_chunk(1, "Atlas", "atlas-connector")];

    let candidate = super::super::answer::build_setup_configuration_anchor_candidate(
        "How do I configure Sample Connector Atlas?",
        &query_ir,
        &chunks,
    )
    .expect("the focused provider variant is actionable setup evidence");

    assert!(!candidate.is_multi_variant());
    assert!(candidate.should_use_as_direct_answer(&query_ir, &chunks));
    let answer = candidate.into_answer();
    assert!(answer.contains("atlas-connector"), "{answer}");
    assert!(answer.contains("[AtlasProvider]"), "{answer}");
    assert!(!answer.contains("Boreal"), "{answer}");
}
