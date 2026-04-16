use std::collections::{BTreeSet, HashMap, HashSet};

use serde_json::json;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{
        self, RuntimeGraphDocumentLinkRow, RuntimeGraphEdgeRow, RuntimeGraphNodeRow,
    },
    interfaces::http::router_support::ApiError,
};

const DEFAULT_GRAPH_ENTITY_LIMIT: usize = 200;
const MAX_GRAPH_ENTITY_LIMIT: usize = 10_000;
const MAX_GRAPH_RELATION_LIMIT: usize = 25_000;
const MAX_ENTITY_SEARCH_LIMIT: usize = 200;

struct SelectedGraphTopology {
    entities: Vec<RuntimeGraphNodeRow>,
    relations: Vec<RuntimeGraphEdgeRow>,
    document_links: Vec<RuntimeGraphDocumentLinkRow>,
    visible_document_ids: Vec<Uuid>,
    relation_limit: usize,
}

struct RankedSubgraph {
    entities: Vec<RuntimeGraphNodeRow>,
    relations: Vec<RuntimeGraphEdgeRow>,
    relation_limit: usize,
}

fn empty_graph_payload() -> serde_json::Value {
    json!({
        "documents": [],
        "entities": [],
        "relations": [],
        "documentLinks": [],
    })
}

fn compare_entity_quality(
    left: &RuntimeGraphNodeRow,
    right: &RuntimeGraphNodeRow,
) -> std::cmp::Ordering {
    right
        .support_count
        .cmp(&left.support_count)
        .then_with(|| left.label.cmp(&right.label))
        .then_with(|| left.created_at.cmp(&right.created_at))
}

fn compare_relation_quality(
    left: &RuntimeGraphEdgeRow,
    right: &RuntimeGraphEdgeRow,
) -> std::cmp::Ordering {
    right
        .support_count
        .cmp(&left.support_count)
        .then_with(|| left.relation_type.cmp(&right.relation_type))
        .then_with(|| left.created_at.cmp(&right.created_at))
}

fn select_graph_topology_slice(
    entity_rows: Vec<RuntimeGraphNodeRow>,
    relation_rows: Vec<RuntimeGraphEdgeRow>,
    mut document_link_rows: Vec<RuntimeGraphDocumentLinkRow>,
    entity_limit: usize,
) -> SelectedGraphTopology {
    let ranked = select_ranked_subgraph(entity_rows, relation_rows, entity_limit);
    let selected_entity_ids: HashSet<Uuid> = ranked.entities.iter().map(|row| row.id).collect();
    let selected_relation_ids: HashSet<Uuid> = ranked.relations.iter().map(|row| row.id).collect();
    document_link_rows.retain(|row| {
        selected_entity_ids.contains(&row.target_node_id)
            || selected_relation_ids.contains(&row.target_node_id)
    });
    document_link_rows.sort_by(|left, right| {
        right
            .support_count
            .cmp(&left.support_count)
            .then_with(|| left.document_id.cmp(&right.document_id))
            .then_with(|| left.target_node_id.cmp(&right.target_node_id))
    });

    let visible_document_ids = document_link_rows
        .iter()
        .map(|row| row.document_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    SelectedGraphTopology {
        entities: ranked.entities,
        relations: ranked.relations,
        document_links: document_link_rows,
        visible_document_ids,
        relation_limit: ranked.relation_limit,
    }
}

fn select_ranked_subgraph(
    mut entity_rows: Vec<RuntimeGraphNodeRow>,
    mut relation_rows: Vec<RuntimeGraphEdgeRow>,
    entity_limit: usize,
) -> RankedSubgraph {
    entity_rows.sort_by(compare_entity_quality);
    entity_rows.truncate(entity_limit);

    let selected_entity_ids: HashSet<Uuid> = entity_rows.iter().map(|row| row.id).collect();
    let relation_limit =
        entity_limit.saturating_mul(5).div_ceil(2).clamp(1, MAX_GRAPH_RELATION_LIMIT);
    relation_rows.retain(|row| {
        selected_entity_ids.contains(&row.from_node_id)
            && selected_entity_ids.contains(&row.to_node_id)
    });
    relation_rows.sort_by(compare_relation_quality);
    relation_rows.truncate(relation_limit);

    RankedSubgraph { entities: entity_rows, relations: relation_rows, relation_limit }
}

pub async fn get_graph_topology(
    state: &AppState,
    library_id: Uuid,
    limit: Option<usize>,
) -> Result<serde_json::Value, ApiError> {
    let library = state
        .canonical_services
        .catalog
        .get_library(state, library_id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let workspace_id = library.workspace_id;

    let Some(snapshot) =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?
    else {
        return Ok(empty_graph_payload());
    };

    if snapshot.graph_status == "empty" || snapshot.projection_version <= 0 {
        return Ok(empty_graph_payload());
    }

    let projection_version = snapshot.projection_version;
    let entity_limit = limit.unwrap_or(DEFAULT_GRAPH_ENTITY_LIMIT).clamp(1, MAX_GRAPH_ENTITY_LIMIT);
    let total_entities = repositories::count_admitted_runtime_graph_entities_by_library(
        &state.persistence.postgres,
        library_id,
        projection_version,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let total_relations = repositories::count_admitted_runtime_graph_relations_by_library(
        &state.persistence.postgres,
        library_id,
        projection_version,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let entity_rows = repositories::list_top_admitted_runtime_graph_entities_by_library(
        &state.persistence.postgres,
        library_id,
        projection_version,
        entity_limit,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let selected_entity_ids: Vec<Uuid> = entity_rows.iter().map(|row| row.id).collect();
    let edge_rows = repositories::list_admitted_runtime_graph_edges_by_node_ids(
        &state.persistence.postgres,
        library_id,
        projection_version,
        &selected_entity_ids,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;

    let ranked = select_ranked_subgraph(entity_rows, edge_rows, entity_limit);
    let mut visible_target_ids: Vec<Uuid> = ranked.entities.iter().map(|row| row.id).collect();
    visible_target_ids.extend(ranked.relations.iter().map(|row| row.id));
    let document_link_rows = repositories::list_runtime_graph_document_links_by_target_ids(
        &state.persistence.postgres,
        library_id,
        projection_version,
        &visible_target_ids,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let selected = select_graph_topology_slice(
        ranked.entities,
        ranked.relations,
        document_link_rows,
        entity_limit,
    );

    let mut documents = state
        .arango_document_store
        .list_documents_by_ids(&selected.visible_document_ids)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let document_support_counts =
        selected.document_links.iter().fold(HashMap::<Uuid, i64>::new(), |mut counts, row| {
            *counts.entry(row.document_id).or_default() += row.support_count;
            counts
        });
    documents.sort_by(|left, right| {
        let left_support = document_support_counts.get(&left.document_id).copied().unwrap_or(0);
        let right_support = document_support_counts.get(&right.document_id).copied().unwrap_or(0);
        right_support
            .cmp(&left_support)
            .then_with(|| {
                left.title.as_deref().unwrap_or("").cmp(right.title.as_deref().unwrap_or(""))
            })
            .then_with(|| left.external_key.cmp(&right.external_key))
    });

    Ok(json!({
        "documents": documents.iter().map(|doc| json!({
            "documentId": doc.document_id,
            "workspaceId": workspace_id,
            "libraryId": library_id,
            "title": doc.title,
        })).collect::<Vec<_>>(),
        "entities": selected.entities.iter().map(|row| json!({
            "entityId": row.id,
            "label": row.label,
            "entityType": row.node_type,
            "summary": row.summary,
            "supportCount": row.support_count,
        })).collect::<Vec<_>>(),
        "relations": selected.relations.iter().map(|row| json!({
            "relationId": row.id,
            "sourceEntityId": row.from_node_id,
            "targetEntityId": row.to_node_id,
            "relationType": row.relation_type,
            "summary": row.summary,
            "supportCount": row.support_count,
        })).collect::<Vec<_>>(),
        "documentLinks": selected.document_links.iter().map(|row| json!({
            "documentId": row.document_id,
            "targetNodeId": row.target_node_id,
            "targetNodeType": row.target_node_type,
            "relationType": row.relation_type,
            "supportCount": row.support_count,
        })).collect::<Vec<_>>(),
        "truncation": {
            "entityLimit": entity_limit,
            "relationLimit": selected.relation_limit,
            "totalEntities": total_entities,
            "totalRelations": total_relations,
            "entitiesTruncated": (total_entities as usize) > entity_limit,
            "relationsTruncated": (total_relations as usize) > selected.relation_limit,
        },
    }))
}

pub async fn search_entities(
    state: &AppState,
    library_id: Uuid,
    query: &str,
    limit: usize,
) -> Result<Vec<serde_json::Value>, ApiError> {
    let Some(snapshot) =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?
    else {
        return Ok(Vec::new());
    };

    if snapshot.graph_status == "empty" || snapshot.projection_version <= 0 {
        return Ok(Vec::new());
    }

    let rows = repositories::search_admitted_runtime_graph_entities_by_query_text(
        &state.persistence.postgres,
        library_id,
        snapshot.projection_version,
        query,
        limit.clamp(1, MAX_ENTITY_SEARCH_LIMIT) as i64,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;

    Ok(rows
        .into_iter()
        .map(|row| {
            json!({
                "entityId": row.id,
                "label": row.label,
                "entityType": row.node_type,
                "summary": row.summary,
                "score": f64::from(row.support_count),
            })
        })
        .collect())
}

pub async fn list_relations(
    state: &AppState,
    library_id: Uuid,
    limit: usize,
) -> Result<Vec<serde_json::Value>, ApiError> {
    let Some(snapshot) =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?
    else {
        return Ok(Vec::new());
    };

    if snapshot.graph_status == "empty" || snapshot.projection_version <= 0 {
        return Ok(Vec::new());
    }

    let projection_version = snapshot.projection_version;
    let relation_rows = repositories::list_top_admitted_runtime_graph_relations_by_library(
        &state.persistence.postgres,
        library_id,
        projection_version,
        limit,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let node_ids: Vec<Uuid> = relation_rows
        .iter()
        .flat_map(|row| [row.from_node_id, row.to_node_id])
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let node_rows = repositories::list_admitted_runtime_graph_nodes_by_ids(
        &state.persistence.postgres,
        library_id,
        projection_version,
        &node_ids,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let node_labels: HashMap<Uuid, &str> =
        node_rows.iter().map(|row| (row.id, row.label.as_str())).collect();

    Ok(relation_rows
        .iter()
        .map(|row| {
            let source_label = node_labels.get(&row.from_node_id).copied().unwrap_or("unknown");
            let target_label = node_labels.get(&row.to_node_id).copied().unwrap_or("unknown");
            json!({
                "relationId": row.id,
                "sourceLabel": source_label,
                "targetLabel": target_label,
                "relationType": row.relation_type,
                "summary": row.summary,
            })
        })
        .collect())
}

pub async fn get_communities(
    state: &AppState,
    library_id: Uuid,
    limit: usize,
) -> Result<Vec<serde_json::Value>, ApiError> {
    let communities = sqlx::query_as::<_, (i32, Option<String>, Vec<String>, i32, i32)>(
        "SELECT community_id, summary, top_entities, node_count, edge_count
         FROM runtime_graph_community
         WHERE library_id = $1
         ORDER BY node_count DESC
         LIMIT $2",
    )
    .bind(library_id)
    .bind(limit as i64)
    .fetch_all(&state.persistence.postgres)
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;

    Ok(communities
        .into_iter()
        .map(|(community_id, summary, top_entities, node_count, edge_count)| {
            json!({
                "communityId": community_id,
                "summary": summary,
                "topEntities": top_entities,
                "nodeCount": node_count,
                "edgeCount": edge_count,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;

    use super::*;

    fn entity(label: &str, support_count: i32) -> RuntimeGraphNodeRow {
        RuntimeGraphNodeRow {
            id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            canonical_key: format!("entity:{label}"),
            label: label.to_string(),
            node_type: "entity".to_string(),
            aliases_json: json!([]),
            summary: Some(format!("{label} summary")),
            metadata_json: json!({}),
            support_count,
            projection_version: 7,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn relation(
        from_node_id: Uuid,
        to_node_id: Uuid,
        relation_type: &str,
        support_count: i32,
    ) -> RuntimeGraphEdgeRow {
        RuntimeGraphEdgeRow {
            id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            from_node_id,
            to_node_id,
            relation_type: relation_type.to_string(),
            canonical_key: format!("{from_node_id}:{relation_type}:{to_node_id}"),
            summary: Some(format!("{relation_type} summary")),
            weight: None,
            support_count,
            metadata_json: json!({}),
            projection_version: 7,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn document_link(
        document_id: Uuid,
        target_node_id: Uuid,
        support_count: i64,
    ) -> RuntimeGraphDocumentLinkRow {
        RuntimeGraphDocumentLinkRow {
            document_id,
            target_node_id,
            target_node_type: "entity".to_string(),
            relation_type: "supports".to_string(),
            support_count,
        }
    }

    #[test]
    fn selects_high_support_subgraph_and_filters_orphaned_links() {
        let top = entity("Orion", 10);
        let second = entity("Atlas", 8);
        let hidden = entity("Noise", 1);

        let visible_relation = relation(top.id, second.id, "depends_on", 9);
        let hidden_relation = relation(top.id, hidden.id, "mentions", 1);
        let visible_doc = Uuid::now_v7();
        let hidden_doc = Uuid::now_v7();

        let selected = select_graph_topology_slice(
            vec![hidden.clone(), second.clone(), top.clone()],
            vec![hidden_relation.clone(), visible_relation.clone()],
            vec![
                document_link(visible_doc, top.id, 3),
                document_link(visible_doc, visible_relation.id, 2),
                document_link(hidden_doc, hidden.id, 5),
            ],
            2,
        );

        assert_eq!(
            selected.entities.iter().map(|row| row.label.as_str()).collect::<Vec<_>>(),
            vec!["Orion", "Atlas"]
        );
        assert_eq!(selected.relations.len(), 1);
        assert_eq!(selected.relations[0].id, visible_relation.id);
        assert_eq!(selected.document_links.len(), 2);
        assert!(selected.document_links.iter().all(|row| row.document_id == visible_doc));
        assert_eq!(selected.visible_document_ids, vec![visible_doc]);
    }

    #[test]
    fn relation_limit_scales_with_entity_limit() {
        let first = entity("First", 5);
        let second = entity("Second", 4);
        let third = entity("Third", 3);

        let selected = select_graph_topology_slice(
            vec![first.clone(), second.clone(), third.clone()],
            vec![
                relation(first.id, second.id, "a", 5),
                relation(first.id, third.id, "b", 4),
                relation(second.id, third.id, "c", 3),
            ],
            Vec::new(),
            1,
        );

        assert_eq!(selected.relation_limit, 3);
        assert!(selected.relations.is_empty());
    }

    #[test]
    fn ranked_subgraph_prefers_support_over_input_order() {
        let top = entity("Orion", 10);
        let second = entity("Atlas", 8);
        let hidden = entity("Noise", 1);
        let strong_relation = relation(top.id, second.id, "depends_on", 9);
        let hidden_relation = relation(top.id, hidden.id, "mentions", 1);

        let selected = select_ranked_subgraph(
            vec![hidden.clone(), second.clone(), top.clone()],
            vec![hidden_relation, strong_relation.clone()],
            2,
        );

        assert_eq!(
            selected.entities.iter().map(|row| row.label.as_str()).collect::<Vec<_>>(),
            vec!["Orion", "Atlas"]
        );
        assert_eq!(selected.relations.len(), 1);
        assert_eq!(selected.relations[0].id, strong_relation.id);
    }
}
