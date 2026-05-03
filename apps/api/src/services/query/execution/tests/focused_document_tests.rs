use super::*;

#[test]
fn focused_answer_document_id_prefers_dominant_single_document() {
    let primary_document_id = Uuid::now_v7();
    let secondary_document_id = Uuid::now_v7();
    let chunks = vec![
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id: primary_document_id,
            document_label: "vector_database_wikipedia.md".to_string(),
            excerpt:
                "Vector databases typically implement approximate nearest neighbor algorithms."
                    .to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(1.0),
            source_text:
                "Vector databases typically implement approximate nearest neighbor algorithms."
                    .to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id: primary_document_id,
            document_label: "vector_database_wikipedia.md".to_string(),
            excerpt: "Use-cases include multi-modal search and recommendation engines.".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.8),
            source_text: "Use-cases include multi-modal search and recommendation engines."
                .to_string(),
        },
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id: secondary_document_id,
            document_label: "large_language_model_wikipedia.md".to_string(),
            excerpt: "LLMs generate, summarize, translate, and reason over text.".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.25),
            source_text: "LLMs generate, summarize, translate, and reason over text.".to_string(),
        },
    ];

    assert_eq!(
        focused_answer_document_id(
            "Which algorithms do vector databases typically implement, and name one use case mentioned besides semantic search.",
            &chunks,
        ),
        Some(primary_document_id)
    );
}

#[test]
fn question_requests_multi_document_scope_detects_role_pairing_questions() {
    let multi_doc_ir = query_ir_with_scope_and_target_types(QueryScope::MultiDocument, ["concept"]);
    assert!(question_requests_multi_document_scope(
        "If a system needs retrieval from external documents before answering and also semantic similarity over embeddings, which two technologies from this corpus fit those roles?",
        Some(&multi_doc_ir),
    ));
    assert!(question_requests_multi_document_scope(
        "Which technology in this corpus focuses on making Internet data machine-readable through standards like RDF and OWL, and which one stores interlinked descriptions of entities and concepts?",
        Some(&multi_doc_ir),
    ));
    assert!(question_requests_multi_document_scope(
        "How does the REST API for rewards accounts differ from the inventory WSDL transport contract?",
        Some(&multi_doc_ir),
    ));
    assert!(question_requests_multi_document_scope(
        "How does the REST API for rewards accounts differ from the inventory WSDL transport contract?",
        Some(&multi_doc_ir),
    ));
}

#[test]
fn build_focused_document_answer_extracts_report_name_from_focused_document() {
    let document_id = Uuid::now_v7();
    let answer = build_focused_document_answer(
        "What report name appears in the runtime PDF upload check?",
        &query_ir_with_scope_and_target_types(QueryScope::SingleDocument, ["report_name"]),
        &[RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "runtime_upload_check.pdf".to_string(),
            excerpt: "Runtime PDF upload check".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(1.0),
            source_text: "Runtime PDF upload check\n\nQuarterly graph report".to_string(),
        }],
    );
    assert_eq!(answer.as_deref(), Some("Quarterly graph report"));
}

#[test]
fn build_focused_document_answer_extracts_formats_under_test() {
    let document_id = Uuid::now_v7();
    let answer = build_focused_document_answer(
            "Which formats are explicitly listed under test in the PDF smoke fixture?",
            &query_ir_with_scope_and_target_types(QueryScope::SingleDocument, ["formats_under_test"]),
            &[RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id,
                document_label: "upload_smoke_fixture.pdf".to_string(),
                excerpt: "IronRAG PDF smoke fixture".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(1.0),
                source_text: "IronRAG PDF smoke fixture\n\nExpected formats under test: PDF, DOCX, PPTX, PNG, JPG.".to_string(),
            }],
        );
    assert_eq!(answer.as_deref(), Some("PDF, DOCX, PPTX, PNG, JPG."));
}

#[test]
fn build_focused_document_answer_does_not_answer_semantic_vectorized_modalities_question() {
    let document_id = Uuid::now_v7();
    let answer = build_focused_document_answer(
            "According to the vector database article, what kinds of data can all be vectorized?",
            &fallback_query_ir(),
            &[RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id,
                document_label: "vector_database_wikipedia.md".to_string(),
                excerpt:
                    "Words, phrases, or entire documents, as well as images and audio, can all be vectorized."
                        .to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(1.0),
                source_text:
                    "Words, phrases, or entire documents, as well as images and audio, can all be vectorized."
                        .to_string(),
            }],
        );
    assert!(answer.is_none());
}

#[test]
fn build_canonical_answer_context_does_not_filter_on_weak_document_focus() {
    let focused_document_id = Uuid::now_v7();
    let other_document_id = Uuid::now_v7();
    let focused_revision_id = Uuid::now_v7();
    let other_revision_id = Uuid::now_v7();

    let context = build_canonical_answer_context(
        "Which search engines and assistants or services are named as examples in the knowledge graph article?",
        &fallback_query_ir(),
        None,
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: vec![
                KnowledgeStructuredBlockRow {
                    normalized_text:
                        "Google, Bing, Yahoo, WolframAlpha, Siri, and Alexa are named.".to_string(),
                    text: "Google, Bing, Yahoo, WolframAlpha, Siri, and Alexa are named."
                        .to_string(),
                    heading_trail: vec!["Examples".to_string()],
                    ..sample_structured_block_row(
                        Uuid::now_v7(),
                        focused_document_id,
                        focused_revision_id,
                    )
                },
                KnowledgeStructuredBlockRow {
                    normalized_text: "LLMs generate, summarize, translate, and reason over text."
                        .to_string(),
                    text: "LLMs generate, summarize, translate, and reason over text.".to_string(),
                    heading_trail: vec!["Capabilities".to_string()],
                    ..sample_structured_block_row(
                        Uuid::now_v7(),
                        other_document_id,
                        other_revision_id,
                    )
                },
            ],
            technical_facts: vec![
                KnowledgeTechnicalFactRow {
                    display_value: "Google".to_string(),
                    canonical_value_text: "Google".to_string(),
                    canonical_value_exact: "Google".to_string(),
                    canonical_value_json: serde_json::json!("Google"),
                    fact_kind: "example".to_string(),
                    ..sample_technical_fact_row(
                        Uuid::now_v7(),
                        focused_document_id,
                        focused_revision_id,
                    )
                },
                KnowledgeTechnicalFactRow {
                    display_value: "translate".to_string(),
                    canonical_value_text: "translate".to_string(),
                    canonical_value_exact: "translate".to_string(),
                    canonical_value_json: serde_json::json!("translate"),
                    fact_kind: "capability".to_string(),
                    ..sample_technical_fact_row(
                        Uuid::now_v7(),
                        other_document_id,
                        other_revision_id,
                    )
                },
            ],
        },
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: focused_document_id,
                document_label: "knowledge_graph_wikipedia.md".to_string(),
                excerpt: "Google, Bing, Yahoo, WolframAlpha, Siri, and Alexa are named."
                    .to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(1.0),
                source_text: "Google, Bing, Yahoo, WolframAlpha, Siri, and Alexa are named."
                    .to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: other_document_id,
                document_label: "large_language_model_wikipedia.md".to_string(),
                excerpt: "LLMs generate, summarize, translate, and reason over text.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.2),
                source_text: "LLMs generate, summarize, translate, and reason over text."
                    .to_string(),
            },
        ],
        &[],
    );

    assert!(context.contains("Google, Bing, Yahoo, WolframAlpha, Siri, and Alexa"));
    assert!(context.contains("LLMs generate, summarize, translate, and reason over text."));
    assert!(context.contains("capability: `translate`"));
    assert!(!context.contains("Focused grounded document"));
}

#[test]
fn build_canonical_answer_context_filters_only_explicit_document_reference() {
    let focused_document_id = Uuid::now_v7();
    let other_document_id = Uuid::now_v7();
    let context = build_canonical_answer_context(
        "Summarize knowledge_graph_wikipedia.md",
        &fallback_query_ir(),
        None,
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: focused_document_id,
                document_label: "knowledge_graph_wikipedia.md".to_string(),
                excerpt: "Focused document evidence.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.4),
                source_text: "Focused document evidence.".to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: other_document_id,
                document_label: "other_notes.md".to_string(),
                excerpt: "Other document evidence.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(2.0),
                source_text: "Other document evidence.".to_string(),
            },
        ],
        &[],
    );

    assert!(context.contains("Focused grounded document\n- knowledge_graph_wikipedia.md"));
    assert!(context.contains("Focused document evidence."));
    assert!(!context.contains("Other document evidence."));
}

#[test]
fn build_canonical_answer_context_keeps_typed_runtime_graph_evidence() {
    let document_id = Uuid::now_v7();
    let context = build_canonical_answer_context(
        "Which rare graph value belongs to Project Omega?",
        &fallback_query_ir(),
        None,
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &[RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "project_omega.md".to_string(),
            excerpt: "Rare runtime evidence: omega.flag=enabled.".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::GraphEvidence,
            score: Some(0.9),
            source_text: "Rare runtime evidence: omega.flag=enabled.".to_string(),
        }],
        &[],
    );

    assert!(context.contains("scope=graph_evidence"));
    assert!(context.contains("omega.flag=enabled"));
    assert!(!context.contains("[graph-evidence target=\"Project Omega\"]"));
    assert!(!context.contains("[graph-node] Project Omega"));
}

#[test]
fn build_canonical_answer_context_keeps_unhydrated_graph_evidence_lines() {
    let context = build_canonical_answer_context(
        "Which rare graph value belongs to Project Omega?",
        &fallback_query_ir(),
        None,
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        },
        &[],
        &[
            "[graph-evidence target=\"Project Omega\"]".to_string(),
            "Rare runtime evidence: omega.flag=enabled.".to_string(),
        ],
    );

    assert!(context.contains("Retrieved graph evidence"));
    assert!(context.contains("[graph-evidence target=\"Project Omega\"]"));
    assert!(context.contains("omega.flag=enabled"));
}

#[test]
fn apply_runtime_chunk_overlays_preserves_shorter_authoritative_graph_evidence() {
    let chunk_id = Uuid::now_v7();
    let mut chunks = vec![RuntimeMatchedChunk {
        chunk_id,
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id: Uuid::now_v7(),
        document_label: "project_omega.md".to_string(),
        excerpt: "Long generic source excerpt before graph overlay.".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(0.2),
        source_text:
            "Long generic source text that happens to be much longer than the graph evidence."
                .to_string(),
    }];
    let runtime_chunks = vec![RuntimeMatchedChunk {
        chunk_id,
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: None,
        document_id: chunks[0].document_id,
        document_label: chunks[0].document_label.clone(),
        excerpt: "omega.flag=enabled".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::GraphEvidence,
        score: Some(0.9),
        source_text: "omega.flag=enabled".to_string(),
    }];

    apply_runtime_chunk_overlays(&mut chunks, &runtime_chunks);

    assert_eq!(
        chunks[0].score_kind,
        crate::services::query::execution::RuntimeChunkScoreKind::GraphEvidence
    );
    assert_eq!(chunks[0].score, Some(0.9));
    assert_eq!(chunks[0].source_text, "omega.flag=enabled");
    assert_eq!(chunks[0].excerpt, "omega.flag=enabled");
}

#[test]
fn build_canonical_answer_context_promotes_query_matching_prepared_segments_before_truncation() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut structured_blocks = (0..20)
        .map(|ordinal| KnowledgeStructuredBlockRow {
            ordinal,
            normalized_text: format!("Generic filler segment {ordinal} for a broad manual."),
            text: format!("Generic filler segment {ordinal} for a broad manual."),
            heading_trail: vec!["Manual overview".to_string()],
            section_path: vec!["manual".to_string(), "overview".to_string()],
            ..sample_structured_block_row(Uuid::now_v7(), document_id, revision_id)
        })
        .collect::<Vec<_>>();
    structured_blocks.push(KnowledgeStructuredBlockRow {
        ordinal: 30,
        normalized_text:
            "Deferred ticket restoration covers regulated product category checks for controlled footwear."
                .to_string(),
        text:
            "Deferred ticket restoration covers regulated product category checks for controlled footwear."
                .to_string(),
        heading_trail: vec![
            "Deferred ticket".to_string(),
            "Regulated product category".to_string(),
            "Code verification rule".to_string(),
        ],
        section_path: vec![
            "deferred-ticket".to_string(),
            "regulated-product-category".to_string(),
            "code-verification-rule".to_string(),
        ],
        ..sample_structured_block_row(Uuid::now_v7(), document_id, revision_id)
    });

    let mut query_ir = fallback_query_ir();
    query_ir.document_focus = Some(DocumentHint {
        hint: "deferred ticket regulated product code verification".to_string(),
    });
    query_ir.target_entities = vec![
        EntityMention { label: "regulated product category".to_string(), role: EntityRole::Object },
        EntityMention { label: "code verification".to_string(), role: EntityRole::Object },
    ];

    let context = build_canonical_answer_context(
        "For the deferred ticket section, which regulated product category is tied to code verification?",
        &query_ir,
        None,
        &CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks,
            technical_facts: Vec::new(),
        },
        &[],
        &[],
    );

    assert!(
        context.contains("Deferred ticket > Regulated product category > Code verification rule")
    );
    assert!(context.contains("controlled footwear"));
    assert!(!context.contains("Generic filler segment 15"));
}

#[test]
fn render_canonical_chunk_section_keeps_full_graph_evidence_text() {
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

    let section = render_canonical_chunk_section(
        "Which exact low-frequency setting is attached to RareNode?",
        &fallback_query_ir(),
        &[chunk],
        false,
    );

    assert!(section.contains("scope=graph_evidence"));
    assert!(section.contains("RareNode evidence line one."));
    assert!(section.contains("alpha.port=7191"));
}

#[test]
fn render_canonical_chunk_section_uses_longer_question_focused_source_excerpt() {
    let document_id = Uuid::now_v7();
    let section = render_canonical_chunk_section(
            "Which search engines and assistants or services are named as examples in the knowledge graph article?",
            &fallback_query_ir(),
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
fn render_canonical_chunk_section_expands_single_document_coverage_for_broad_ir() {
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

    let section = render_canonical_chunk_section(
        "Provide a source overview",
        &fallback_query_ir(),
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
fn render_canonical_chunk_section_promotes_source_profile_for_broad_ir() {
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

    let section = render_canonical_chunk_section(
        "Provide a source overview",
        &fallback_query_ir(),
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
    // The canonical MCP–UI parity test lives in answer_pipeline_tests.rs;
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
        source_uri: None,
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

#[test]
fn build_focused_document_answer_does_not_answer_semantic_ocr_sources_question() {
    let document_id = Uuid::now_v7();
    let answer = build_focused_document_answer(
            "Which kinds of source material are explicitly listed as OCR inputs in the OCR article?",
            &fallback_query_ir(),
            &[RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id,
                document_label: "optical_character_recognition_wikipedia.md".to_string(),
                excerpt: "machine-encoded text, whether from a scanned document, a photo of a document, a scene photo or from subtitle text.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(1.0),
                source_text: "Optical character recognition converts images into machine-encoded text, whether from a scanned document, a photo of a document, a scene photo (for example the text on signs and billboards in a landscape photo) or from subtitle text superimposed on an image.".to_string(),
            }],
        );

    assert!(answer.is_none());
}

#[test]
fn build_focused_document_answer_does_not_answer_semantic_ocr_conversion_question() {
    let document_id = Uuid::now_v7();
    let answer = build_focused_document_answer(
            "What does OCR convert images of text into, and what kinds of source material are explicitly named?",
            &fallback_query_ir(),
            &[RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id,
                document_label: "optical_character_recognition_wikipedia.md".to_string(),
                excerpt: "machine-encoded text from a scanned document and subtitle text.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(1.0),
                source_text: "Optical character recognition converts images of text into machine-encoded text, whether from a scanned document, a photo of a document, a scene photo (for example the text on signs and billboards in a landscape photo) or from subtitle text superimposed on an image.".to_string(),
            }],
        );

    assert!(answer.is_none());
}
