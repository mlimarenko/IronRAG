use super::*;

#[test]
fn structural_source_profile_marker_is_recognized_without_kind() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let document_index = HashMap::from([(document_id, document_row(document_id, revision_id))]);
    let row =
        chunk_row(document_id, revision_id, 0, "metadata_block", "[source_profile unit_count=3]");

    let mapped = map_companion_chunk(row, 1.0, &document_index, &[]).unwrap();

    assert_eq!(mapped.chunk_kind.as_deref(), Some("source_profile"));
    assert_eq!(mapped.excerpt, "[source_profile unit_count=3]");
}

#[test]
fn map_companion_chunk_preserves_paths_lost_from_window_text() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let document_index = HashMap::from([(document_id, document_row(document_id, revision_id))]);
    let mut row = chunk_row(
        document_id,
        revision_id,
        1,
        "code_block",
        "sample-install alpha-connector\n\
         sample-configure alpha-connector\n\
         module configuration: /opt/alpha/modules/connector/connector.conf",
    );
    row.window_text = Some(
        "sample-install alpha-connector\n\
         sample-configure alpha-connector\n\
         module configuration:"
            .to_string(),
    );

    let mapped = map_companion_chunk(row, 1.0, &document_index, &[]).unwrap();

    assert!(mapped.source_text.contains("/opt/alpha/modules/connector/connector.conf"));
    assert!(mapped.excerpt.contains("/opt/alpha/modules/connector/connector.conf"));
}

#[test]
fn map_companion_chunk_merges_structured_literals_from_window_and_content() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let document_index = HashMap::from([(document_id, document_row(document_id, revision_id))]);
    let mut row = chunk_row(
        document_id,
        revision_id,
        2,
        "paragraph",
        "[Main]\nalphaMode = strict\nconfiguration path: /etc/sample/service.conf",
    );
    row.window_text = Some(
        "Run the reload command after editing the configuration:\n\
         samplectl reload alpha-plugin --config /etc/sample/service.conf"
            .to_string(),
    );

    let mapped = map_companion_chunk(row, 1.0, &document_index, &[]).unwrap();

    assert!(mapped.source_text.contains("[Main]"));
    assert!(mapped.source_text.contains("alphaMode = strict"));
    assert!(mapped.source_text.contains("samplectl reload alpha-plugin"));
    assert!(mapped.excerpt.contains("[Main]"));
    assert!(mapped.excerpt.contains("alphaMode = strict"));
    assert!(mapped.excerpt.contains("samplectl reload alpha-plugin"));
}

#[test]
fn neighbor_companion_does_not_sort_above_anchor() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut chunks = vec![RuntimeMatchedChunk {
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(42.0),
        ..runtime_chunk(document_id, revision_id, 10)
    }];
    let neighbor = RuntimeMatchedChunk {
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(source_context_neighbor_score(42.0, 1.0, 11)),
        ..runtime_chunk(document_id, revision_id, 11)
    };

    apply_structured_source_companions(
        &mut chunks,
        vec![StructuredSourceCompanion {
            chunk: neighbor,
            kind: StructuredSourceCompanionKind::Neighbor,
        }],
    );

    assert_eq!(chunks[0].chunk_index, 10);
    assert_eq!(chunks[1].chunk_index, 11);
}

#[test]
fn companions_promote_profile_and_dedupe_neighbor() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let existing = runtime_chunk(document_id, revision_id, 10);
    let duplicate_neighbor = RuntimeMatchedChunk {
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(2.0),
        excerpt: "expanded unit 10".to_string(),
        source_text: "expanded unit 10".to_string(),
        ..existing.clone()
    };
    let profile = companion_chunk(document_id, revision_id, 0, "source_profile", 3.0);
    let mut chunks = vec![existing.clone()];

    let diagnostics = apply_structured_source_companions(
        &mut chunks,
        vec![
            StructuredSourceCompanion {
                chunk: profile.clone(),
                kind: StructuredSourceCompanionKind::SourceProfile,
            },
            StructuredSourceCompanion {
                chunk: duplicate_neighbor,
                kind: StructuredSourceCompanionKind::Neighbor,
            },
        ],
    );

    assert_eq!(diagnostics.source_profile_count, 1);
    assert_eq!(diagnostics.neighbor_count, 1);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].chunk_kind.as_deref(), Some("source_profile"));
    let expanded = chunks.iter().find(|chunk| chunk.chunk_id == existing.chunk_id).unwrap();
    assert_eq!(expanded.excerpt, "expanded unit 10");
}

#[test]
fn inventory_profile_score_can_outrank_generic_context() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let unrelated_document_id = Uuid::now_v7();
    let mut chunks = vec![
        RuntimeMatchedChunk {
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(20.0),
            ..runtime_chunk(document_id, revision_id, 12)
        },
        RuntimeMatchedChunk {
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(22.0),
            ..runtime_chunk(unrelated_document_id, revision_id, 7)
        },
    ];
    let profile = companion_chunk(
        document_id,
        revision_id,
        0,
        "source_profile",
        structured_inventory_source_profile_score(20.0, 0),
    );

    apply_structured_source_companions(
        &mut chunks,
        vec![StructuredSourceCompanion {
            chunk: profile,
            kind: StructuredSourceCompanionKind::SourceProfile,
        }],
    );

    assert_eq!(chunks[0].chunk_kind.as_deref(), Some("source_profile"));
    assert_eq!(chunks[0].document_id, document_id);
}

#[test]
fn inventory_profile_score_caps_artificial_anchor_scores() {
    let capped = structured_inventory_source_profile_score(1_000_000.0, 0);

    assert!(capped < 100.0);
    assert_eq!(
        capped,
        SOURCE_CONTEXT_PROFILE_ANCHOR_SCORE_CAP + SOURCE_CONTEXT_STRUCTURED_PROFILE_SCORE_BONUS
    );
}

#[test]
fn companions_enrich_duplicate_chunks_with_missing_paths() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let existing = RuntimeMatchedChunk {
        score: Some(20.0),
        source_text: "install alpha-connector and configure the module".to_string(),
        excerpt: "install alpha-connector".to_string(),
        ..runtime_chunk(document_id, revision_id, 10)
    };
    let enriched = RuntimeMatchedChunk {
        score: Some(10.0),
        source_text:
            "install alpha-connector and configure /opt/alpha/modules/connector/connector.conf"
                .to_string(),
        excerpt: "configure /opt/alpha/modules/connector/connector.conf".to_string(),
        ..existing.clone()
    };
    let mut chunks = vec![existing.clone()];

    apply_structured_source_companions(
        &mut chunks,
        vec![StructuredSourceCompanion {
            chunk: enriched,
            kind: StructuredSourceCompanionKind::Neighbor,
        }],
    );

    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].score, Some(20.0));
    assert!(chunks[0].source_text.contains("/opt/alpha/modules/connector/connector.conf"));
    assert!(chunks[0].excerpt.contains("/opt/alpha/modules/connector/connector.conf"));
}

#[test]
fn map_companion_chunk_drops_orphan_documents_without_heads() {
    // Contract update mirrors `map_chunk_hit`: companion chunks are
    // no longer dropped on plain revision-id mismatch — only when
    // the owning document has both heads null (orphan).
    let document_id = Uuid::now_v7();
    let stale_revision_id = Uuid::now_v7();
    let mut orphan = document_row(document_id, Uuid::now_v7());
    orphan.active_revision_id = None;
    orphan.readable_revision_id = None;
    let document_index = HashMap::from([(document_id, orphan)]);
    let row = chunk_row(
        document_id,
        stale_revision_id,
        0,
        "source_profile",
        "[source_profile unit_count=3]",
    );

    let mapped = map_companion_chunk(row, 1.0, &document_index, &[]);

    assert!(mapped.is_none());
}
