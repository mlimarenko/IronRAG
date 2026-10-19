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
        &query_ir_with_scope_and_target_types(QueryScope::SingleDocument, ["secondary_heading"]),
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
fn build_focused_document_answer_prefers_pdf_for_format_marker_when_stem_is_ambiguous() {
    let pdf_id = Uuid::now_v7();
    let docx_id = Uuid::now_v7();
    let pptx_id = Uuid::now_v7();
    let answer = build_focused_document_answer(
        "What report name appears in the runtime PDF upload check?",
        &query_ir_with_scope_and_target_types(QueryScope::SingleDocument, ["secondary_heading"]),
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: pdf_id,
                document_label: "runtime_upload_check.pdf".to_string(),
                excerpt: "Runtime PDF upload check".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.7),
                source_text: "Runtime PDF upload check\n\nQuarterly graph report".to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: docx_id,
                document_label: "runtime_upload_check.docx".to_string(),
                excerpt: "Runtime PDF upload check".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.9),
                source_text: "Runtime PDF upload check\n\nLegacy upload report".to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: pptx_id,
                document_label: "runtime_upload_check.pptx".to_string(),
                excerpt: "Runtime PDF upload check".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.8),
                source_text: "Runtime PDF upload check\n\nOperations deck".to_string(),
            },
        ],
    );
    assert_eq!(answer.as_deref(), Some("Quarterly graph report"));
}

#[test]
fn build_focused_document_answer_extracts_report_name_from_pdf_single_line_text() {
    let pdf_id = Uuid::now_v7();
    let docx_id = Uuid::now_v7();
    let answer = build_focused_document_answer(
        "What report name appears in the runtime PDF upload check?",
        &query_ir_with_scope_and_target_types(QueryScope::SingleDocument, ["secondary_heading"]),
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: pdf_id,
                document_label: "runtime_upload_check.pdf".to_string(),
                excerpt: "Runtime PDF upload check".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.7),
                source_text: "Runtime PDF upload check Quarterly graph report".to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id: docx_id,
                document_label: "runtime_upload_check.docx".to_string(),
                excerpt: "Runtime DOCX upload check".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(1_000_000.0),
                source_text: "## Runtime DOCX upload check\n\nPrimary pipeline validation"
                    .to_string(),
            },
        ],
    );

    assert_eq!(answer.as_deref(), Some("Quarterly graph report"));
}

#[test]
fn build_focused_document_answer_abstains_for_procedural_config_ir() {
    let document_id = Uuid::now_v7();
    let answer = build_focused_document_answer(
        "Where should the connector config be written and which parameters are required?",
        &query_ir_with_act_scope_and_target_types(
            QueryAct::ConfigureHow,
            QueryScope::SingleDocument,
            ["secondary_heading", "config_key"],
        ),
        &[RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id,
            document_label: "payment_connector_setup.md".to_string(),
            excerpt: "Payment Connector\n\n[Main]".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(1.0),
            source_text: "Payment Connector\n\n[Main]\nendpoint = https://example.invalid/api\nprintReceipt = true\n\n[Check]\nprintReceiptSlip = false".to_string(),
        }],
    );

    assert!(answer.is_none());
}

#[test]
fn build_focused_document_answer_abstains_from_formats_without_typed_evidence() {
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
    assert!(answer.is_none());
}

#[test]
fn build_focused_document_answer_does_not_classify_untyped_headings_by_english_prefix() {
    for heading in ["Source: visible heading", "Source type: visible heading"] {
        let document_id = Uuid::now_v7();
        let answer = build_focused_document_answer(
            "What is the primary heading?",
            &query_ir_with_scope_and_target_types(QueryScope::SingleDocument, ["primary_heading"]),
            &[RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id,
                document_label: "neutral-document.md".to_string(),
                excerpt: heading.to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(1.0),
                source_text: format!("{heading}\nFallback heading"),
            }],
        );

        assert_eq!(answer.as_deref(), Some(heading));
    }
}

#[test]
fn build_focused_document_answer_excludes_typed_metadata_blocks_from_headings() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let answer = build_focused_document_answer(
        "What is the primary heading?",
        &query_ir_with_scope_and_target_types(QueryScope::SingleDocument, ["primary_heading"]),
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id,
                chunk_index: 0,
                chunk_kind: Some("metadata_block".to_string()),
                document_id,
                document_label: "neutral-document.md".to_string(),
                excerpt: "Opaque provenance record".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(1.0),
                source_text: "Opaque provenance record".to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id,
                chunk_index: 1,
                chunk_kind: Some("heading".to_string()),
                document_id,
                document_label: "neutral-document.md".to_string(),
                excerpt: "Primary heading".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.9),
                source_text: "# Primary heading".to_string(),
            },
        ],
    );

    assert_eq!(answer.as_deref(), Some("Primary heading"));
}

#[test]
fn build_focused_document_answer_does_not_answer_semantic_vectorized_modalities_question() {
    let document_id = Uuid::now_v7();
    let answer = build_focused_document_answer(
            "According to the vector database article, what kinds of data can all be vectorized?",
            &generic_query_ir(),
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
fn build_focused_document_answer_does_not_answer_semantic_ocr_sources_question() {
    let document_id = Uuid::now_v7();
    let answer = build_focused_document_answer(
            "Which kinds of source material are explicitly listed as OCR inputs in the OCR article?",
            &generic_query_ir(),
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
            &generic_query_ir(),
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
