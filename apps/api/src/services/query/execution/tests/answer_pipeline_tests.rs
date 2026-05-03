use super::*;

#[test]
fn build_references_keeps_chunk_node_edge_order_and_ranks() {
    let references = build_references(
        &[RuntimeMatchedEntity {
            node_id: Uuid::now_v7(),
            label: "IronRAG".to_string(),
            node_type: "entity".to_string(),
            score: Some(0.9),
        }],
        &[RuntimeMatchedRelationship {
            edge_id: Uuid::now_v7(),
            relation_type: "links".to_string(),
            from_node_id: Uuid::now_v7(),
            from_label: "spec.md".to_string(),
            to_node_id: Uuid::now_v7(),
            to_label: "IronRAG".to_string(),
            summary: None,
            support_count: 1,
            score: Some(0.7),
        }],
        &[RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id: Uuid::now_v7(),
            document_label: "spec.md".to_string(),
            excerpt: "IronRAG links specs to graph knowledge.".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.8),
            source_text: "IronRAG links specs to graph knowledge.".to_string(),
        }],
        3,
    );

    assert_eq!(references.len(), 3);
    assert_eq!(references[0].kind, "chunk");
    assert_eq!(references[0].rank, 1);
    assert_eq!(references[1].kind, "node");
    assert_eq!(references[1].rank, 2);
    assert_eq!(references[2].kind, "edge");
    assert_eq!(references[2].rank, 3);
}

#[test]
fn grouped_reference_candidates_prefer_document_deduping() {
    let document_id = Uuid::now_v7();
    let candidates = build_grouped_reference_candidates(
        &[],
        &[],
        &[
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id,
                document_label: "spec.md".to_string(),
                excerpt: "First excerpt".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.8),
                source_text: "First excerpt".to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: 0,
                chunk_kind: None,
                document_id,
                document_label: "spec.md".to_string(),
                excerpt: "Second excerpt".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(0.7),
                source_text: "Second excerpt".to_string(),
            },
        ],
        4,
    );

    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].dedupe_key, format!("document:{document_id}"));
    assert_eq!(candidates[1].dedupe_key, format!("document:{document_id}"));
}

#[test]
fn assemble_bounded_context_interleaves_graph_and_document_support() {
    let context = assemble_bounded_context(
        &[RuntimeMatchedEntity {
            node_id: Uuid::now_v7(),
            label: "IronRAG".to_string(),
            node_type: "entity".to_string(),
            score: Some(0.9),
        }],
        &[RuntimeMatchedRelationship {
            edge_id: Uuid::now_v7(),
            relation_type: "uses".to_string(),
            from_node_id: Uuid::now_v7(),
            from_label: "IronRAG".to_string(),
            to_node_id: Uuid::now_v7(),
            to_label: "Arango".to_string(),
            summary: Some("IronRAG stores graph triples in Arango.".to_string()),
            support_count: 2,
            score: Some(0.7),
        }],
        &[RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_id: Uuid::now_v7(),
            document_label: "spec.md".to_string(),
            excerpt: "IronRAG stores graph knowledge.".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.8),
            source_text: "IronRAG stores graph knowledge.".to_string(),
        }],
        2_000,
    );

    assert!(context.starts_with("Context\n"));
    assert!(context.contains("[document] spec.md: IronRAG stores graph knowledge."));
    assert!(context.contains("[graph-node] IronRAG (entity)"));
    assert!(
        context.contains(
            "[graph-edge evidence] evidence: IronRAG stores graph triples in Arango. | relation_hint: IronRAG --uses--> Arango | support_count: 2"
        )
    );
    let document_index = context.find("[document]").unwrap_or_default();
    let graph_node_index = context.find("[graph-node]").unwrap_or_default();
    let graph_edge_index = context.find("[graph-edge").unwrap_or_default();
    assert!(document_index < graph_node_index);
    assert!(graph_node_index < graph_edge_index);
}

#[test]
fn build_answer_prompt_prioritizes_library_context() {
    let prompt = build_answer_prompt(
        "What documents mention IronRAG?",
        "Library summary\n- Documents in library: 12\n\nRecent documents\n- 2026-03-30T22:15:00Z — spec.md (text/markdown; pipeline ready; graph ready)",
        None,
        None,
    );
    assert!(prompt.contains("Treat the active library as the primary source of truth"));
    assert!(prompt.contains("exhaust the provided library context"));
    assert!(prompt.contains("recent document metadata"));
    assert!(prompt.contains("Present the answer directly."));
    assert!(prompt.contains("Do not narrate the retrieval process"));
    assert!(prompt.contains("Do not ask the user to upload"));
    assert!(prompt.contains("Exact technical literals section"));
    assert!(prompt.contains("copy those literals verbatim from Context"));
    assert!(prompt.contains("grouped by document"));
    assert!(prompt.contains("matched excerpt"));
    assert!(prompt.contains("Do not combine parts from different snippets"));
    assert!(prompt.contains("prefer the next distinct item after the excluded one"));
    assert!(prompt.contains("Question: What documents mention IronRAG?"));
    assert!(prompt.contains("Documents in library: 12"));
}

#[test]
fn build_answer_prompt_includes_recent_conversation_history() {
    let prompt = build_answer_prompt(
        "continue",
        "Context\n[dummy] step-by-step instructions",
        Some(
            "User: how do I move items in the product\nAssistant: I can walk you through it step by step.",
        ),
        None,
    );

    assert!(prompt.contains("Use the recent conversation history"));
    assert!(prompt.contains("Recent conversation:"));
    assert!(prompt.contains("Assistant: I can walk you through it step by step."));
    assert!(prompt.contains("Question: continue"));
}

#[test]
fn focused_excerpt_for_prefers_keyword_region_over_chunk_prefix() {
    let content = "\
Header section\n\
Status code example\n\
Unrelated payload\n\
Filler A\n\
Filler B\n\
If a code exists in an active promotion it will be cancelled.\n\
Trailing note";

    let excerpt = focused_excerpt_for(
        content,
        &["exists".to_string(), "promotion".to_string(), "cancelled".to_string()],
        220,
    );

    assert!(excerpt.contains("exists in an active promotion"));
    assert!(excerpt.contains("will be cancelled"));
    assert!(!excerpt.starts_with("Header section"));
}

#[test]
fn assemble_answer_context_excludes_recent_documents_for_mcp_ui_parity() {
    // Constitution §16 — same query + same library must return identical
    // answers across UI and MCP channels. The answer prompt is a
    // deterministic function of (query, retrieved evidence, stable
    // library summary). Live ingest metadata (recent uploads,
    // pipeline_state churn, mutating preview excerpts) MUST NOT enter
    // this prompt — it would drift between back-to-back calls during
    // active ingestion. Diagnostic recent-documents data is still
    // surfaced to the UI via `RuntimeStructuredQueryLibrarySummary`,
    // but it never reaches the LLM answer step.
    let summary = RuntimeQueryLibrarySummary {
        document_count: 12,
        graph_ready_count: 8,
        processing_count: 3,
        failed_count: 1,
        graph_status: "partial",
    };
    let retrieved_documents = vec![RuntimeRetrievedDocumentBrief {
        title: "spec.md".to_string(),
        preview_excerpt: "IronRAG stores graph knowledge.".to_string(),
        source_uri: None,
    }];
    let context = assemble_answer_context(
        &summary,
        &retrieved_documents,
        Some("Exact technical literals\n- URLs: `http://demo.local:8080/wsdl`"),
        "Context\n[document] spec.md: IronRAG",
        false,
    );

    assert!(context.contains("Context\n[document] spec.md: IronRAG"));
    assert!(context.contains("Library summary\n- Documents in library: 12"));
    assert!(context.contains("- Graph-ready documents: 8"));
    assert!(context.contains("- Documents still processing: 3"));
    assert!(context.contains("- Documents failed in pipeline: 1"));
    assert!(context.contains("- Graph coverage status: partial"));
    assert!(context.contains("Retrieved document briefs"));
    assert!(context.contains("Exact technical literals\n- URLs: `http://demo.local:8080/wsdl`"));
    // Anti-regression: the answer prompt must NOT inject the live
    // recent-uploads block, which is the canonical MCP-UI parity
    // violator under active ingestion.
    assert!(!context.contains("Recent documents"));
    assert!(!context.contains("Preview:"));
}

#[test]
fn assemble_answer_context_can_prioritize_graph_context_before_document_briefs() {
    let summary = RuntimeQueryLibrarySummary {
        document_count: 12,
        graph_ready_count: 8,
        processing_count: 0,
        failed_count: 0,
        graph_status: "current",
    };
    let retrieved_documents = vec![RuntimeRetrievedDocumentBrief {
        title: "spec.md".to_string(),
        preview_excerpt: "Long document preview.".to_string(),
        source_uri: None,
    }];
    let context = assemble_answer_context(
        &summary,
        &retrieved_documents,
        None,
        "Context\n[graph-node] Project Omega (person)\n[document] spec.md: Project Omega",
        true,
    );

    let graph_index = context.find("[graph-node]").unwrap_or_default();
    let briefs_index = context.find("Retrieved document briefs").unwrap_or_default();
    assert!(graph_index < briefs_index);
}
