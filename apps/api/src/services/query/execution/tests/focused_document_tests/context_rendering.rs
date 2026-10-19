use super::*;

#[test]
fn render_prepared_segment_section_preserves_bounded_code_blocks() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let code = format!(
        "[Main]\n{}\nlateConfigKey = verified-value",
        (0..40)
            .map(|index| format!("fillerKey{index} = filler-value-{index}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let block = KnowledgeStructuredBlockRow {
        block_kind: "code_block".to_string(),
        normalized_text: code.clone(),
        text: code,
        heading_trail: vec!["Configuration".to_string(), "Example".to_string()],
        section_path: vec!["configuration".to_string(), "example".to_string()],
        ..sample_structured_block_row(Uuid::now_v7(), document_id, revision_id)
    };

    let section = render_prepared_segment_section(
        "Show the lateConfigKey configuration example",
        Some(&generic_query_ir()),
        &[block],
        false,
    );

    assert!(section.contains("code_block > Configuration > Example (coverage=full):"));
    assert!(section.contains("```text"));
    assert!(section.contains("lateConfigKey = verified-value"));
}

#[test]
fn render_prepared_segment_section_focuses_long_code_blocks_on_question_literals() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let code = format!(
        "[Main]\n{}\nrareTargetKey = expected-value\n{}",
        (0..140)
            .map(|index| format!("earlyFillerKey{index} = early-filler-value-{index}"))
            .collect::<Vec<_>>()
            .join("\n"),
        (0..140)
            .map(|index| format!("lateFillerKey{index} = late-filler-value-{index}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let block = KnowledgeStructuredBlockRow {
        block_kind: "code_block".to_string(),
        normalized_text: code.clone(),
        text: code,
        heading_trail: vec!["Configuration".to_string(), "Example".to_string()],
        section_path: vec!["configuration".to_string(), "example".to_string()],
        ..sample_structured_block_row(Uuid::now_v7(), document_id, revision_id)
    };

    let section = render_prepared_segment_section(
        "Which value should rareTargetKey use?",
        Some(&generic_query_ir()),
        &[block],
        false,
    );

    assert!(section.contains("coverage=excerpt"));
    assert!(section.contains("rareTargetKey = expected-value"));
}

#[test]
fn render_primary_chunk_section_preserves_bounded_code_block_chunks() {
    let source_text = format!(
        "[Main]\n{}\nlateConfigKey = verified-value",
        (0..48)
            .map(|index| format!("fillerKey{index} = filler-value-{index}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let chunk = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 5,
        chunk_kind: Some("code_block".to_string()),
        document_id: Uuid::now_v7(),
        document_label: "settings.md".to_string(),
        excerpt: "short preview".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::SourceContext,
        score: Some(0.9),
        source_text,
    };

    let section = render_primary_chunk_section(
        "Show the lateConfigKey configuration example",
        &generic_query_ir(),
        &[chunk],
        false,
    );

    assert!(section.contains("scope=code_block"));
    assert!(section.contains("lateConfigKey = verified-value"));
}

#[test]
fn render_primary_chunk_section_keeps_full_graph_evidence_text() {
    let chunk = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 3,
        chunk_kind: None,
        document_id: Uuid::now_v7(),
        document_label: "rare-node.md".to_string(),
        excerpt: "Nearby heading only.".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::GraphEvidence,
        score: Some(0.9),
        source_text: "RareNode evidence line one.\n\nExact low-frequency setting: alpha.port=7191."
            .to_string(),
    };

    let section = render_primary_chunk_section(
        "Which exact low-frequency setting is attached to RareNode?",
        &generic_query_ir(),
        &[chunk],
        false,
    );

    assert!(section.contains("scope=graph_evidence"));
    assert!(section.contains("RareNode evidence line one."));
    assert!(section.contains("alpha.port=7191"));
}

#[test]
fn render_primary_chunk_section_uses_longer_question_focused_source_excerpt() {
    let document_id = Uuid::now_v7();
    let section = render_primary_chunk_section(
            "Which search engines and assistants or services are named as examples in the knowledge graph article?",
            &generic_query_ir(),
            &[RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id,
                document_label: "knowledge_graph_wikipedia.md".to_string(),
                excerpt: "Google, Bing, and Yahoo are named as examples.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(1.0),
                source_text: "Knowledge graphs are used by search engines such as Google, Bing, and Yahoo; knowledge engines and question-answering services such as WolframAlpha, Apple's Siri, and Amazon Alexa."
                    .to_string(),
            }],
            false,
        );

    assert!(section.contains("Google, Bing, and Yahoo"));
    assert!(section.contains("WolframAlpha"));
    assert!(section.contains("Siri"));
    assert!(section.contains("Alexa"));
}

#[test]
fn render_primary_chunk_section_expands_single_document_coverage_for_broad_ir() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let chunks = (0..5)
        .map(|index| RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: index,
            chunk_kind: None,
            document_id,
            document_label: "event-stream.jsonl".to_string(),
            excerpt: format!("Segment {index}"),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(1.0 - index as f32 * 0.01),
            source_text: format!("Segment {index}"),
        })
        .collect::<Vec<_>>();

    let section = render_primary_chunk_section(
        "Provide a source overview",
        &generic_query_ir(),
        &chunks,
        false,
    );

    assert!(section.contains("Segment 0"));
    assert!(section.contains("Segment 1"));
    assert!(section.contains("Segment 2"));
    assert!(section.contains("Segment 3"));
    assert!(section.contains("Segment 4"));
}

#[test]
fn render_primary_chunk_section_promotes_source_profile_for_broad_ir() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut chunks = (0..14)
        .map(|index| RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id,
            chunk_index: index,
            chunk_kind: None,
            document_id,
            document_label: "event-stream.jsonl".to_string(),
            excerpt: format!("Sample unit {index}"),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(10.0 - index as f32 * 0.01),
            source_text: format!("Sample unit {index}"),
        })
        .collect::<Vec<_>>();
    chunks.push(RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        revision_id,
        chunk_index: 0,
        chunk_kind: Some("source_profile".to_string()),
        document_id,
        document_label: "event-stream.jsonl".to_string(),
        excerpt: "[source_profile source_format=record_jsonl unit_count=42 time_start=2026-01-01T00:00:00Z time_end=2026-01-02T00:00:00Z]"
            .to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.01),
        source_text: "[source_profile source_format=record_jsonl unit_count=42 time_start=2026-01-01T00:00:00Z time_end=2026-01-02T00:00:00Z]\n[unit_id=first] first unit"
            .to_string(),
    });

    let section = render_primary_chunk_section(
        "Provide a source overview",
        &generic_query_ir(),
        &chunks,
        false,
    );

    assert!(section.contains("AGGREGATE_PROFILE blocks"));
    assert!(section.contains("[AGGREGATE_PROFILE scope=document coverage=full"));
    assert!(section.contains("unit_count=42"));
    assert!(section.contains("time_start=2026-01-01T00:00:00Z"));
    assert!(section.contains("EVIDENCE_CHUNK blocks"));
    assert!(section.contains("[EVIDENCE_CHUNK scope=excerpt coverage=sampled"));
}

#[test]
fn assemble_answer_context_excludes_recent_documents_for_mcp_ui_parity_focused() {
    // This test covers assemble_answer_context from the focused-document path.
    // The primary MCP–UI parity test lives in answer_pipeline_tests.rs;
    // this one verifies the same contract from a different call-site context.
    let summary = RuntimeQueryLibrarySummary {
        document_count: 5,
        graph_ready_count: 5,
        processing_count: 0,
        failed_count: 0,
        graph_status: "ready",
    };
    let retrieved_documents = vec![RuntimeRetrievedDocumentBrief {
        title: "knowledge_graph_wikipedia.md".to_string(),
        preview_excerpt: "A knowledge graph is a structured representation.".to_string(),
        document_hint: None,
    }];
    let context = assemble_answer_context(
        &summary,
        &retrieved_documents,
        None,
        "Context\n[document] knowledge_graph_wikipedia.md: structured representation",
        false,
    );

    assert!(context.contains("Library summary"));
    assert!(context.contains("Documents in library: 5"));
    assert!(!context.contains("Recent documents"));
    assert!(!context.contains("Preview:"));
}
