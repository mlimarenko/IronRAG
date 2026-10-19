use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use super::{
    GraphTargetEntityCoverageField, GraphTargetEntityCoverageFieldKind,
    associative_edges_for_entities, entities_from_relationships, graph_relevance_profile,
    graph_target_entity_coverage_score, graph_target_entity_profiles, lexical_entity_hits,
    lexical_relationship_hits, merge_entity_retrieval_lane_slices, merge_entity_retrieval_lanes,
    merge_primary_then_expanded_entities, query_relevant_entity_hits, score_value,
};
use crate::{
    domains::query_ir::{
        ComparisonSpec, DocumentHint, EntityMention, EntityRole, QueryAct, QueryIR, QueryLanguage,
        QueryScope, QueryTargetKind,
    },
    infra::repositories::{RuntimeGraphQueryEdgeRow, RuntimeGraphQueryNodeRow},
    services::{
        knowledge::runtime_read::ActiveRuntimeGraphProjection,
        query::{
            execution::associative_graph_retrieval::associative_seed_score_for_rank,
            execution::{QueryGraphIndex, RuntimeMatchedEntity, RuntimeMatchedRelationship},
            planner::{RuntimeQueryPlan, build_query_plan},
        },
    },
};

mod relevance_tests;
mod retrieval_tests;
mod target_profiles_tests;

fn graph_index_with_nodes(nodes: Vec<RuntimeGraphQueryNodeRow>) -> QueryGraphIndex {
    let positions = nodes.iter().enumerate().map(|(position, node)| (node.id, position)).collect();
    QueryGraphIndex::new(
        std::sync::Arc::new(ActiveRuntimeGraphProjection { nodes, edges: Vec::new() }),
        positions,
        Default::default(),
    )
}

fn graph_index_with_projection(
    nodes: Vec<RuntimeGraphQueryNodeRow>,
    edges: Vec<RuntimeGraphQueryEdgeRow>,
) -> QueryGraphIndex {
    let node_positions =
        nodes.iter().enumerate().rev().map(|(position, node)| (node.id, position)).collect();
    let edge_positions =
        edges.iter().enumerate().rev().map(|(position, edge)| (edge.id, position)).collect();
    QueryGraphIndex::new(
        std::sync::Arc::new(ActiveRuntimeGraphProjection { nodes, edges }),
        node_positions,
        edge_positions,
    )
}

fn node(label: &str, node_type: &str, summary: Option<&str>) -> RuntimeGraphQueryNodeRow {
    RuntimeGraphQueryNodeRow {
        id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        identity_key: format!("{node_type}:{}", label.to_lowercase()),
        label: label.to_string(),
        node_type: node_type.to_string(),
        aliases_json: json!([]),
        summary: summary.map(str::to_string),
        support_count: 1,
        projection_version: 1,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn edge(
    from_node_id: Uuid,
    to_node_id: Uuid,
    relation_type: &str,
    summary: Option<&str>,
) -> RuntimeGraphQueryEdgeRow {
    RuntimeGraphQueryEdgeRow {
        id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        from_node_id,
        to_node_id,
        relation_type: relation_type.to_string(),
        summary: summary.map(str::to_string),
        weight: None,
        support_count: 1,
        projection_version: 1,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn matched_entity(label: &str) -> RuntimeMatchedEntity {
    RuntimeMatchedEntity {
        node_id: Uuid::now_v7(),
        label: label.to_string(),
        node_type: "artifact".to_string(),
        summary: None,
        score: Some(1.0),
    }
}

fn inventory_ir(target_types: &[&str]) -> QueryIR {
    QueryIR {
        act: QueryAct::Enumerate,
        scope: QueryScope::LibraryMeta,
        language: QueryLanguage::Auto,
        target_types: target_types
            .iter()
            .map(|value| {
                QueryTargetKind::from_wire(value).expect("test target type must be normalized")
            })
            .collect(),
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

fn wildcard_inventory_ir(target_label: &str) -> QueryIR {
    QueryIR {
        target_entities: vec![EntityMention {
            label: target_label.to_string(),
            role: EntityRole::Subject,
        }],
        ..inventory_ir(&[])
    }
}

fn configure_ir(target_label: &str, focus_hint: &str) -> QueryIR {
    QueryIR {
        act: QueryAct::ConfigureHow,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![QueryTargetKind::Path, QueryTargetKind::Procedure],
        target_entities: vec![EntityMention {
            label: target_label.to_string(),
            role: EntityRole::Subject,
        }],
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: Some(DocumentHint { hint: focus_hint.to_string() }),
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.9,
    }
}

fn configure_ir_with_object_facets(focus_hint: &str, object_labels: &[&str]) -> QueryIR {
    QueryIR {
        target_entities: object_labels
            .iter()
            .map(|label| EntityMention { label: (*label).to_string(), role: EntityRole::Object })
            .collect(),
        document_focus: Some(DocumentHint { hint: focus_hint.to_string() }),
        ..configure_ir(focus_hint, focus_hint)
    }
}

fn focused_compare_ir(
    focus_hint: &str,
    facet_labels: &[&str],
    second_operand: Option<&str>,
) -> QueryIR {
    QueryIR {
        act: QueryAct::Compare,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![QueryTargetKind::Document, QueryTargetKind::Concept],
        target_entities: facet_labels
            .iter()
            .map(|label| EntityMention { label: (*label).to_string(), role: EntityRole::Subject })
            .collect(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: Some(ComparisonSpec {
            a: Some("available variants".to_string()),
            b: second_operand.map(str::to_string),
            dimension: "facet coverage".to_string(),
        }),
        document_focus: Some(DocumentHint { hint: focus_hint.to_string() }),
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.86,
    }
}

fn describe_ir(target_label: &str) -> QueryIR {
    describe_ir_with_targets(&[target_label])
}

fn describe_ir_with_targets(target_labels: &[&str]) -> QueryIR {
    QueryIR {
        act: QueryAct::Describe,
        scope: QueryScope::LibraryMeta,
        language: QueryLanguage::Auto,
        target_types: Vec::new(),
        target_entities: target_labels
            .iter()
            .map(|label| EntityMention { label: (*label).to_string(), role: EntityRole::Subject })
            .collect(),
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
