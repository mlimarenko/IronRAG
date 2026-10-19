use super::{
    ClarificationReason, ClarificationSpec, EntityMention, EntityRole, GroupedReference,
    GroupedReferenceKind, QueryAct, QueryIR, QueryLanguage, QueryScope, QueryTargetKind,
    RuntimeChunkScoreKind, RuntimeMatchedChunk, RuntimeRetrievedDocumentBrief, Uuid,
};

pub(super) fn sample_ir(
    confidence: f32,
    needs_clarification: Option<ClarificationReason>,
) -> QueryIR {
    sample_ir_with_act(QueryAct::ConfigureHow, confidence, needs_clarification)
}

pub(super) fn sample_ir_with_act(
    act: QueryAct,
    confidence: f32,
    needs_clarification: Option<ClarificationReason>,
) -> QueryIR {
    QueryIR {
        act,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Ru,
        target_types: vec![QueryTargetKind::Procedure],
        target_entities: vec![EntityMention {
            label: "workflow module".to_string(),
            role: EntityRole::Subject,
        }],
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: needs_clarification
            .map(|reason| ClarificationSpec { reason, suggestion: String::new() }),
        source_slice: None,
        retrieval_query: None,
        confidence,
    }
}

pub(super) fn setup_anchor_chunk(
    document_id: Uuid,
    label: &str,
    package: &str,
    path: &str,
) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::new_v4(),
        document_id,
        revision_id: Uuid::new_v4(),
        chunk_index: 0,
        chunk_kind: Some("paragraph".to_string()),
        document_label: label.to_string(),
        excerpt: format!("sample-install {package}\n{path}"),
        score_kind: RuntimeChunkScoreKind::DocumentIdentity,
        score: Some(42.0),
        source_text: format!("sample-install {package}\n{path}\n[Main]\nurl = http://localhost"),
    }
}

pub(super) fn procedure_chunk(label: &str, text: &str) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::new_v4(),
        document_id: Uuid::new_v4(),
        revision_id: Uuid::new_v4(),
        chunk_index: 1,
        chunk_kind: Some("paragraph".to_string()),
        document_label: label.to_string(),
        excerpt: text.to_string(),
        score_kind: RuntimeChunkScoreKind::FocusedDocument,
        score: Some(42.0),
        source_text: text.to_string(),
    }
}
pub(super) fn sample_ir_with_two_target_entities(
    act: QueryAct,
    confidence: f32,
    needs_clarification: Option<ClarificationReason>,
) -> QueryIR {
    let mut ir = sample_ir_with_act(act, confidence, needs_clarification);
    ir.target_entities = vec![
        EntityMention { label: "Sample Subject".to_string(), role: EntityRole::Subject },
        EntityMention { label: "workflows".to_string(), role: EntityRole::Object },
    ];
    ir
}

pub(super) fn sample_groups() -> Vec<GroupedReference> {
    vec![
        GroupedReference {
            id: "document:1".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 1,
            title: "Provider A configuration".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:1".to_string(), "chunk:2".to_string(), "chunk:3".to_string()],
        },
        GroupedReference {
            id: "document:2".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 2,
            title: "Provider B configuration".to_string(),
            excerpt: None,
            evidence_count: 3,
            support_ids: vec!["chunk:4".to_string(), "chunk:5".to_string(), "chunk:6".to_string()],
        },
        GroupedReference {
            id: "document:3".to_string(),
            kind: GroupedReferenceKind::Document,
            rank: 3,
            title: "Provider C configuration".to_string(),
            excerpt: None,
            evidence_count: 2,
            support_ids: vec!["chunk:7".to_string(), "chunk:8".to_string()],
        },
    ]
}
pub(super) fn retrieved_doc(title: &str, document_hint: &str) -> RuntimeRetrievedDocumentBrief {
    RuntimeRetrievedDocumentBrief {
        title: title.to_string(),
        preview_excerpt: String::new(),
        document_hint: Some(document_hint.to_string()),
    }
}

pub(super) fn prepared_for_single_shot(query_ir: QueryIR) -> super::PreparedAnswerQueryResult {
    use crate::domains::query::{
        ContextAssemblyMetadata, ContextAssemblyStatus, IntentKeywords, QueryIntentCacheStatus,
        QueryPlanningMetadata, RerankMetadata, RerankStatus, RuntimeQueryMode,
    };

    super::PreparedAnswerQueryResult {
        structured: super::super::types::RuntimeStructuredQueryResult {
            planned_mode: RuntimeQueryMode::Hybrid,
            intent_profile: Default::default(),
            context_text: "context-fragment-a".to_string(),
            technical_literals_text: None,
            technical_literal_chunks: Vec::new(),
            diagnostics: super::super::types::RuntimeStructuredQueryDiagnostics {
                requested_mode: RuntimeQueryMode::Hybrid,
                planned_mode: RuntimeQueryMode::Hybrid,
                keywords: Vec::new(),
                high_level_keywords: Vec::new(),
                low_level_keywords: Vec::new(),
                top_k: 8,
                reference_counts: super::super::types::RuntimeStructuredQueryReferenceCounts {
                    entity_count: 0,
                    relationship_count: 0,
                    chunk_count: 1,
                    graph_node_count: 0,
                    graph_edge_count: 0,
                },
                planning: QueryPlanningMetadata {
                    requested_mode: RuntimeQueryMode::Hybrid,
                    planned_mode: RuntimeQueryMode::Hybrid,
                    intent_cache_status: QueryIntentCacheStatus::Miss,
                    keywords: IntentKeywords::default(),
                    warnings: Vec::new(),
                },
                rerank: RerankMetadata {
                    status: RerankStatus::NotApplicable,
                    candidate_count: 1,
                    reordered_count: None,
                    semantic_rerank: None,
                },
                context_assembly: ContextAssemblyMetadata {
                    status: ContextAssemblyStatus::DocumentOnly,
                    warning: None,
                },
                grouped_references: Vec::new(),
                context_text: None,
                warning: None,
                warning_kind: None,
                library_summary: None,
            },
            retrieved_documents: vec![RuntimeRetrievedDocumentBrief {
                title: "document-a".to_string(),
                preview_excerpt: "context-fragment-a".to_string(),
                document_hint: None,
            }],
            retrieved_context_document_titles: vec!["document-a".to_string()],
            chunk_references: Vec::new(),
            context_chunks: Vec::new(),
            ordered_source_units: Vec::new(),
            graph_evidence_context_lines: Vec::new(),
            graph_entity_references: Vec::new(),
            graph_relation_references: Vec::new(),
        },
        answer_context: "context-fragment-a".to_string(),
        query_ir,
        retrieval_spans: Vec::new(),
    }
}
