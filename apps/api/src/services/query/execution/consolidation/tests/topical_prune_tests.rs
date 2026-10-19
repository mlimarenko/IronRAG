use super::*;

#[test]
fn topical_prune_keeps_title_family_and_drops_generic_tail() {
    let workflow_a = Uuid::now_v7();
    let workflow_b = Uuid::now_v7();
    let generic = Uuid::now_v7();
    let rev = Uuid::now_v7();
    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            sample_chunk(workflow_a, rev, 0, "Workflow Alpha Admin Guide", 0.9),
            sample_chunk(workflow_b, rev, 0, "Workflow Beta Admin Guide", 0.8),
            sample_chunk(generic, rev, 0, "Generic Installation HOWTO", 0.7),
        ],
    };

    let diagnostics =
        prune_non_topical_document_tail(&mut bundle, "how to configure workflow", None, false);

    assert_eq!(diagnostics.removed_chunk_count, 1);
    assert_eq!(diagnostics.kept_chunk_count, 2);
    assert!(bundle.chunks.iter().all(|chunk| chunk.document_label.contains("Workflow")));
}

#[test]
fn topical_prune_preserves_additive_evidence_chunk_without_title_match() {
    let topic_a = Uuid::now_v7();
    let topic_b = Uuid::now_v7();
    let rare_evidence = Uuid::now_v7();
    let generic = Uuid::now_v7();
    let rev = Uuid::now_v7();
    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            sample_chunk(topic_a, rev, 0, "Scanner Alpha Guide", 0.02),
            sample_chunk(topic_b, rev, 0, "Scanner Beta Guide", 0.02),
            sample_chunk(rare_evidence, rev, 0, "Shared Device Manual", 1.5),
            sample_chunk(generic, rev, 0, "Generic Installation HOWTO", 0.02),
        ],
    };

    let diagnostics =
        prune_non_topical_document_tail(&mut bundle, "how to configure scanner", None, false);

    assert_eq!(diagnostics.removed_chunk_count, 1);
    assert_eq!(diagnostics.kept_chunk_count, 3);
    assert!(bundle.chunks.iter().any(|chunk| chunk.document_id == rare_evidence));
    assert!(!bundle.chunks.iter().any(|chunk| chunk.document_id == generic));
}

#[test]
fn topical_prune_preserves_versioned_update_instruction_title_anchor() {
    let topical_a = Uuid::now_v7();
    let topical_b = Uuid::now_v7();
    let instruction_doc = Uuid::now_v7();
    let generic = Uuid::now_v7();
    let rev = Uuid::now_v7();
    let mut instruction = sample_chunk(
        instruction_doc,
        rev,
        0,
        "Instruction for updating Alpha Control Service",
        0.02,
    );
    instruction.score_kind = RuntimeChunkScoreKind::FocusedDocument;
    instruction.source_text = concat!(
        "Alpha Control Service update procedure\n",
        "1. sudo sample-runner update\n",
        "2. sudo sample-runner upgrade\n",
        "3. sudo sample-configure alpha-control\n",
        "4. sudo service alpha-control restart"
    )
    .to_string();
    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            sample_chunk(topical_a, rev, 0, "Workflow Alpha Guide", 0.02),
            sample_chunk(topical_b, rev, 0, "Workflow Beta Guide", 0.02),
            instruction,
            sample_chunk(generic, rev, 0, "Generic Installation HOWTO", 0.02),
        ],
    };
    let mut query_ir = ir(QueryScope::SingleDocument);
    query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Version];
    query_ir.target_entities = vec![EntityMention {
        label: "Alpha Control Service".to_string(),
        role: EntityRole::Subject,
    }];
    query_ir.document_focus = Some(DocumentHint { hint: "Alpha Control Service".to_string() });
    query_ir.retrieval_query = Some("how to update Alpha Control Service version?".to_string());

    let diagnostics = prune_non_topical_document_tail(
        &mut bundle,
        "how to configure workflow",
        Some(&query_ir),
        false,
    );

    assert_eq!(diagnostics.removed_chunk_count, 1);
    assert!(bundle.chunks.iter().any(|chunk| chunk.document_id == instruction_doc));
    assert!(!bundle.chunks.iter().any(|chunk| chunk.document_id == generic));
}

#[test]
fn topical_prune_skips_latest_version_multi_document_queries() {
    let release_a = Uuid::now_v7();
    let release_b = Uuid::now_v7();
    let generic = Uuid::now_v7();
    let rev = Uuid::now_v7();
    let mut bundle = RetrievalBundle {
        entities: Vec::new(),
        relationships: Vec::new(),
        chunks: vec![
            sample_chunk(release_a, rev, 0, "Version 9.8.765 Admin Guide", 0.9),
            sample_chunk(release_b, rev, 0, "Version 9.8.764 Admin Guide", 0.8),
            sample_chunk(generic, rev, 0, "Admin Guide", 0.7),
        ],
    };

    let diagnostics = prune_non_topical_document_tail(&mut bundle, "latest releases", None, true);

    assert_eq!(diagnostics.removed_chunk_count, 0);
    assert_eq!(bundle.chunks.len(), 3);
}

#[test]
fn topical_prune_ignores_library_wide_title_tokens() {
    let doc_a = Uuid::now_v7();
    let doc_b = Uuid::now_v7();
    let doc_c = Uuid::now_v7();
    let rev = Uuid::now_v7();
    let original = vec![
        sample_chunk(doc_a, rev, 0, "Platform Alpha Guide", 0.9),
        sample_chunk(doc_b, rev, 0, "Platform Beta Guide", 0.8),
        sample_chunk(doc_c, rev, 0, "Platform Gamma Guide", 0.7),
    ];
    let mut bundle =
        RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks: original };

    let diagnostics = prune_non_topical_document_tail(&mut bundle, "platform setup", None, false);

    assert_eq!(diagnostics.removed_chunk_count, 0);
    assert_eq!(bundle.chunks.len(), 3);
}

#[test]
fn significant_tokens_language_agnostic() {
    // Hint-matching must not rely on domain vocabulary — any
    // 3+ char alphanumeric token counts, regardless of language.
    let tokens = significant_tokens("SubjectA WorkflowModule Guide v2.0");
    assert!(tokens.contains("subjecta"));
    assert!(tokens.contains("workflowmodule"));
}
