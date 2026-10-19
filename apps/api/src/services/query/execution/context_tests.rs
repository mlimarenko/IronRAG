use chrono::Utc;
use uuid::Uuid;

use crate::domains::query_ir::{
    EntityMention, EntityRole, QueryAct, QueryIR, QueryLanguage, QueryScope, QueryTargetKind,
    SourceSliceDirection, SourceSliceFilter, SourceSliceSpec,
};
use crate::{
    infra::repositories::RuntimeGraphQueryNodeRow,
    services::knowledge::runtime_read::ActiveRuntimeGraphProjection,
};

use super::super::{
    retrieve::excerpt_for,
    types::{QueryGraphIndex, RuntimeChunkScoreKind, RuntimeMatchedChunk, RuntimeMatchedEntity},
};
use super::*;
use super::{
    answer_context::focused_preview_from_bundle_chunks,
    assembly::{
        DECLARATION_INVENTORY_CONTEXT_CHARS, DECLARATION_INVENTORY_ENTRY_LIMIT,
        declaration_inventory_context_block, interleave_bounded_context_lines,
    },
    chunk_ranking::{ProcedureContextModel, order_bounded_context_chunks},
    entity_context::TARGET_ENTITY_CONTEXT_LINE_LIMIT,
};

fn source_slice_ir(direction: SourceSliceDirection, count: u16) -> QueryIR {
    QueryIR {
        act: QueryAct::Enumerate,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![QueryTargetKind::Record],
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: Some(SourceSliceSpec {
            direction,
            count: Some(count),
            filter: SourceSliceFilter::None,
        }),
        retrieval_query: None,
        confidence: 0.9,
    }
}

fn latest_version_slice_ir(count: u16) -> QueryIR {
    let mut ir = source_slice_ir(SourceSliceDirection::Tail, count);
    ir.scope = QueryScope::LibraryMeta;
    ir.target_types = vec![QueryTargetKind::Release, QueryTargetKind::Version];
    if let Some(slice) = ir.source_slice.as_mut() {
        slice.filter = SourceSliceFilter::ReleaseMarker;
    }
    ir
}

fn general_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::Enumerate,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![QueryTargetKind::Record],
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.9,
    }
}

fn entity_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::Describe,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![QueryTargetKind::Person],
        target_entities: vec![EntityMention {
            label: "Project Omega".to_string(),
            role: EntityRole::Subject,
        }],
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.9,
    }
}

fn inventory_entity_ir(target_label: &str) -> QueryIR {
    QueryIR {
        act: QueryAct::Enumerate,
        scope: QueryScope::LibraryMeta,
        language: QueryLanguage::Auto,
        target_types: vec![QueryTargetKind::Artifact],
        target_entities: vec![EntityMention {
            label: target_label.to_string(),
            role: EntityRole::Subject,
        }],
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.9,
    }
}

fn library_inventory_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::Enumerate,
        scope: QueryScope::LibraryMeta,
        language: QueryLanguage::Auto,
        target_types: Vec::new(),
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.9,
    }
}

fn source_slice_unit(ordinal: i32, source_text: &str) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: ordinal,
        chunk_kind: Some("metadata_block".to_string()),
        document_label: "records.jsonl".to_string(),
        excerpt: source_text.to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(3.0),
        source_text: source_text.to_string(),
    }
}

fn latest_version_chunk(label: &str, chunk_index: i32, score: f32) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index,
        chunk_kind: Some("paragraph".to_string()),
        document_label: label.to_string(),
        excerpt: format!("{label} excerpt {chunk_index}"),
        score_kind: RuntimeChunkScoreKind::DocumentIdentity,
        score: Some(score),
        source_text: format!("{label} body {chunk_index}"),
    }
}

fn source_profile(source_text: &str) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: Some("source_profile".to_string()),
        document_label: "records.jsonl".to_string(),
        excerpt: "[source_profile source_format=record_jsonl unit_count=2]".to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(4.0),
        source_text: source_text.to_string(),
    }
}

fn ordinary_chunk(excerpt: &str, source_text: &str) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 1,
        chunk_kind: Some("paragraph".to_string()),
        document_label: "guide.md".to_string(),
        excerpt: excerpt.to_string(),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(1.0),
        source_text: source_text.to_string(),
    }
}
fn runtime_graph_node(
    label: &str,
    node_type: &str,
    summary: Option<&str>,
) -> RuntimeGraphQueryNodeRow {
    RuntimeGraphQueryNodeRow {
        id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        identity_key: format!("{node_type}:{label}"),
        label: label.to_string(),
        node_type: node_type.to_string(),
        aliases_json: serde_json::json!([]),
        summary: summary.map(str::to_string),
        support_count: 1,
        projection_version: 1,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn graph_index_with_nodes(nodes: Vec<RuntimeGraphQueryNodeRow>) -> QueryGraphIndex {
    let node_positions =
        nodes.iter().enumerate().map(|(position, node)| (node.id, position)).collect();
    QueryGraphIndex::new(
        std::sync::Arc::new(ActiveRuntimeGraphProjection { nodes, edges: Vec::new() }),
        node_positions,
        Default::default(),
    )
}

#[path = "context_tests/bounded_context.rs"]
mod bounded_context;
#[path = "context_tests/declaration_inventory.rs"]
mod declaration_inventory;
#[path = "context_tests/entity_graph.rs"]
mod entity_graph;
