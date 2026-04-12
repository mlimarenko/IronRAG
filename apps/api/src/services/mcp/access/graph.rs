use std::collections::HashSet;

use serde_json::json;
use uuid::Uuid;

use crate::{
    app::state::AppState, infra::repositories, interfaces::http::router_support::ApiError,
};

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
        return Ok(json!({
            "documents": [],
            "entities": [],
            "relations": [],
            "documentLinks": [],
        }));
    };

    if snapshot.graph_status == "empty" || snapshot.projection_version <= 0 {
        return Ok(json!({
            "documents": [],
            "entities": [],
            "relations": [],
            "documentLinks": [],
        }));
    }

    let projection_version = snapshot.projection_version;
    let node_rows = repositories::list_admitted_runtime_graph_nodes_by_library(
        &state.persistence.postgres,
        library_id,
        projection_version,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let edge_rows = repositories::list_admitted_runtime_graph_edges_by_library(
        &state.persistence.postgres,
        library_id,
        projection_version,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let document_link_rows = repositories::list_runtime_graph_document_links_by_library(
        &state.persistence.postgres,
        library_id,
        projection_version,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;

    let document_node_ids: HashSet<Uuid> =
        node_rows.iter().filter(|row| row.node_type == "document").map(|row| row.id).collect();

    let document_ids: Vec<Uuid> = node_rows
        .iter()
        .filter(|row| row.node_type == "document")
        .filter_map(|row| {
            row.metadata_json
                .get("document_id")
                .and_then(serde_json::Value::as_str)
                .and_then(|value| value.parse::<Uuid>().ok())
        })
        .collect();

    let documents = state
        .arango_document_store
        .list_documents_by_ids(&document_ids)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;

    let entities: Vec<serde_json::Value> = node_rows
        .iter()
        .filter(|row| row.node_type != "document")
        .map(|row| {
            json!({
                "entityId": row.id,
                "label": row.label,
                "entityType": row.node_type,
                "summary": row.summary,
                "supportCount": row.support_count,
            })
        })
        .collect();

    let relations: Vec<serde_json::Value> = edge_rows
        .iter()
        .filter(|row| {
            !document_node_ids.contains(&row.from_node_id)
                && !document_node_ids.contains(&row.to_node_id)
        })
        .map(|row| {
            json!({
                "relationId": row.id,
                "sourceEntityId": row.from_node_id,
                "targetEntityId": row.to_node_id,
                "relationType": row.relation_type,
                "summary": row.summary,
                "supportCount": row.support_count,
            })
        })
        .collect();

    let document_links: Vec<serde_json::Value> = document_link_rows
        .iter()
        .map(|row| {
            json!({
                "documentId": row.document_id,
                "targetNodeId": row.target_node_id,
                "targetNodeType": row.target_node_type,
                "relationType": row.relation_type,
                "supportCount": row.support_count,
            })
        })
        .collect();

    let entity_limit = limit.unwrap_or(200).clamp(1, 10000);
    let relation_limit = entity_limit.saturating_mul(5).div_ceil(2).clamp(1, 25000);
    let total_entities = entities.len();
    let total_relations = relations.len();
    let entities_truncated = total_entities > entity_limit;
    let relations_truncated = total_relations > relation_limit;
    let entities: Vec<serde_json::Value> = entities.into_iter().take(entity_limit).collect();
    let relations: Vec<serde_json::Value> = relations.into_iter().take(relation_limit).collect();

    Ok(json!({
        "documents": documents.iter().map(|doc| json!({
            "documentId": doc.document_id,
            "workspaceId": workspace_id,
            "libraryId": library_id,
            "title": doc.title,
        })).collect::<Vec<_>>(),
        "entities": entities,
        "relations": relations,
        "documentLinks": document_links,
        "truncation": {
            "entityLimit": entity_limit,
            "relationLimit": relation_limit,
            "totalEntities": total_entities,
            "totalRelations": total_relations,
            "entitiesTruncated": entities_truncated,
            "relationsTruncated": relations_truncated,
        },
    }))
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
    let node_rows = repositories::list_admitted_runtime_graph_nodes_by_library(
        &state.persistence.postgres,
        library_id,
        projection_version,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let edge_rows = repositories::list_admitted_runtime_graph_edges_by_library(
        &state.persistence.postgres,
        library_id,
        projection_version,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "internal"))?;

    let document_node_ids: HashSet<Uuid> =
        node_rows.iter().filter(|row| row.node_type == "document").map(|row| row.id).collect();

    let node_labels: std::collections::HashMap<Uuid, &str> =
        node_rows.iter().map(|row| (row.id, row.label.as_str())).collect();

    let relations: Vec<serde_json::Value> = edge_rows
        .iter()
        .filter(|row| {
            !document_node_ids.contains(&row.from_node_id)
                && !document_node_ids.contains(&row.to_node_id)
        })
        .take(limit)
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
        .collect();

    Ok(relations)
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
