use std::collections::{BTreeSet, HashMap, HashSet};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_QUERY_READ, load_project_and_authorize},
        router_support::ApiError,
    },
};

const DEFAULT_SEARCH_LIMIT: usize = 8;
const MAX_SEARCH_LIMIT: usize = 50;
const DEFAULT_GRAPH_WARNING: &str = "Graph runtime is live on persisted entity/relation rows, but extraction run tracking and provenance depth are still partial.";
const ENTITY_ONLY_GRAPH_WARNING: &str = "Graph runtime has persisted entity rows, but relation coverage is still empty and extraction provenance remains partial.";
const EMPTY_GRAPH_WARNING: &str = "Graph runtime is wired, but this project does not have persisted graph rows yet. Ingestion-time extraction remains the blocker.";

#[derive(Debug, Clone, FromRow)]
struct EntityRow {
    id: Uuid,
    project_id: Uuid,
    canonical_name: String,
    entity_type: Option<String>,
    metadata_json: Value,
}

#[derive(Debug, Clone, FromRow)]
struct RelationRow {
    id: Uuid,
    project_id: Uuid,
    from_entity_id: Uuid,
    to_entity_id: Uuid,
    relation_type: String,
    provenance_json: Value,
}

#[derive(Serialize)]
pub struct GraphCoverageSummary {
    pub project_id: Uuid,
    pub entity_count: usize,
    pub relation_count: usize,
    pub extraction_runs: usize,
    pub status: String,
    pub warning: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct GraphEntitySummary {
    pub id: Uuid,
    pub project_id: Uuid,
    pub canonical_name: String,
    pub entity_type: Option<String>,
    pub source_chunk_count: usize,
}

#[derive(Serialize, Clone)]
pub struct GraphRelationSummary {
    pub id: Uuid,
    pub project_id: Uuid,
    pub relation_type: String,
    pub from_entity_id: Uuid,
    pub to_entity_id: Uuid,
    pub source_chunk_count: usize,
}

#[derive(Serialize)]
pub struct GraphProductSnapshot {
    pub project_id: Uuid,
    pub coverage: GraphCoverageSummary,
    pub entities: Vec<GraphEntitySummary>,
    pub relations: Vec<GraphRelationSummary>,
    pub generated_at: DateTime<Utc>,
}

#[derive(Serialize)]
pub struct GraphProductResponse {
    pub snapshot: GraphProductSnapshot,
}

#[derive(Serialize)]
pub struct GraphKindCount {
    pub name: String,
    pub count: usize,
}

#[derive(Serialize)]
pub struct GraphProjectSummaryResponse {
    pub project_id: Uuid,
    pub coverage: GraphCoverageSummary,
    pub entity_kinds: Vec<GraphKindCount>,
    pub relation_kinds: Vec<GraphKindCount>,
    pub top_entities: Vec<GraphEntitySummary>,
    pub sample_relations: Vec<GraphRelationSummary>,
    pub generated_at: DateTime<Utc>,
}

#[derive(Serialize)]
pub struct GraphEntitySearchHit {
    pub entity: GraphEntitySummary,
    pub match_reasons: Vec<String>,
}

#[derive(Serialize)]
pub struct GraphRelationSearchHit {
    pub relation: GraphRelationSummary,
    pub from_entity_name: String,
    pub to_entity_name: String,
    pub match_reasons: Vec<String>,
}

#[derive(Serialize)]
pub struct GraphSearchResponse {
    pub project_id: Uuid,
    pub query: String,
    pub searched_fields: Vec<String>,
    pub result_count: usize,
    pub entity_results: Vec<GraphEntitySearchHit>,
    pub relation_results: Vec<GraphRelationSearchHit>,
    pub generated_at: DateTime<Utc>,
    pub warning: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct GraphRelationDetail {
    pub relation: GraphRelationSummary,
    pub from_entity_name: String,
    pub to_entity_name: String,
}

#[derive(Serialize)]
pub struct GraphEntityDetailResponse {
    pub project_id: Uuid,
    pub entity: GraphEntitySummary,
    pub aliases: Vec<String>,
    pub source_document_ids: Vec<Uuid>,
    pub source_chunk_ids: Vec<Uuid>,
    pub observed_relation_count: usize,
    pub incoming_relations: Vec<GraphRelationDetail>,
    pub outgoing_relations: Vec<GraphRelationDetail>,
    pub generated_at: DateTime<Utc>,
    pub warning: Option<String>,
}

#[derive(Serialize)]
pub struct GraphSubgraphResponse {
    pub project_id: Uuid,
    pub focus_entity_id: Uuid,
    pub depth: u8,
    pub entity_count: usize,
    pub relation_count: usize,
    pub entities: Vec<GraphEntitySummary>,
    pub relations: Vec<GraphRelationDetail>,
    pub generated_at: DateTime<Utc>,
    pub warning: Option<String>,
}

#[derive(Serialize)]
pub struct GraphContentSummary {
    pub persisted_document_count: usize,
    pub persisted_chunk_count: usize,
    pub embedded_chunk_count: usize,
    pub retrieval_run_count: usize,
    pub referenced_document_count: usize,
    pub referenced_chunk_count: usize,
}

#[derive(Serialize)]
pub struct GraphProvenanceSummary {
    pub entities_with_document_refs: usize,
    pub entities_with_chunk_refs: usize,
    pub entities_without_chunk_refs: usize,
    pub relations_with_document_refs: usize,
    pub relations_with_chunk_refs: usize,
    pub relations_without_chunk_refs: usize,
}

#[derive(Serialize)]
pub struct GraphReadinessSummary {
    pub status: String,
    pub blockers: Vec<String>,
    pub next_steps: Vec<String>,
}

#[derive(Serialize)]
pub struct GraphProjectDiagnosticsResponse {
    pub project_id: Uuid,
    pub coverage: GraphCoverageSummary,
    pub content: GraphContentSummary,
    pub provenance: GraphProvenanceSummary,
    pub readiness: GraphReadinessSummary,
    pub generated_at: DateTime<Utc>,
}

#[derive(Deserialize)]
pub struct GraphSearchQuery {
    pub q: String,
    pub limit: Option<usize>,
}

#[derive(Deserialize)]
pub struct GraphSubgraphQuery {
    pub depth: Option<u8>,
}

struct GraphProjectCounts {
    document_count: usize,
    chunk_count: usize,
    embedded_chunk_count: usize,
    retrieval_run_count: usize,
}

pub fn router() -> Router<crate::app::state::AppState> {
    Router::new()
        .route("/graph-products/{project_id}", get(get_graph_product))
        .route("/graph-products/{project_id}/summary", get(get_graph_summary))
        .route("/graph-products/{project_id}/diagnostics", get(get_graph_diagnostics))
        .route("/graph-products/{project_id}/search", get(search_graph))
        .route("/graph-products/{project_id}/entities/{entity_id}", get(get_graph_entity_detail))
        .route(
            "/graph-products/{project_id}/entities/{entity_id}/subgraph",
            get(get_graph_subgraph),
        )
}

async fn get_graph_product(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<GraphProductResponse>, ApiError> {
    let project = load_project_and_authorize(&auth, &state, project_id, POLICY_QUERY_READ).await?;

    let snapshot = load_graph_snapshot(&state.persistence.postgres, project_id).await?;

    info!(
        auth_token_id = %auth.token_id,
        workspace_id = %project.workspace_id,
        project_id = %project_id,
        entity_count = snapshot.coverage.entity_count,
        relation_count = snapshot.coverage.relation_count,
        status = %snapshot.coverage.status,
        "loaded graph product snapshot",
    );

    Ok(Json(GraphProductResponse { snapshot }))
}

async fn get_graph_summary(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<GraphProjectSummaryResponse>, ApiError> {
    let project = load_project_and_authorize(&auth, &state, project_id, POLICY_QUERY_READ).await?;

    let snapshot = load_graph_snapshot(&state.persistence.postgres, project_id).await?;
    let entities = load_entities(&state.persistence.postgres, project_id).await?;
    let relations = load_relations(&state.persistence.postgres, project_id).await?;

    info!(
        auth_token_id = %auth.token_id,
        workspace_id = %project.workspace_id,
        project_id = %project_id,
        entity_count = entities.len(),
        relation_count = relations.len(),
        status = %snapshot.coverage.status,
        "loaded graph summary",
    );

    Ok(Json(GraphProjectSummaryResponse {
        project_id,
        coverage: snapshot.coverage,
        entity_kinds: summarize_entity_kinds(&entities),
        relation_kinds: summarize_relation_kinds(&relations),
        top_entities: sort_entity_summaries(
            entities.iter().map(entity_summary).collect::<Vec<_>>(),
        )
        .into_iter()
        .take(8)
        .collect(),
        sample_relations: sort_relation_summaries(
            relations.iter().map(relation_summary).collect::<Vec<_>>(),
        )
        .into_iter()
        .take(8)
        .collect(),
        generated_at: Utc::now(),
    }))
}

async fn get_graph_diagnostics(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
) -> Result<Json<GraphProjectDiagnosticsResponse>, ApiError> {
    let project = load_project_and_authorize(&auth, &state, project_id, POLICY_QUERY_READ).await?;

    let entities = load_entities(&state.persistence.postgres, project_id).await?;
    let relations = load_relations(&state.persistence.postgres, project_id).await?;
    let counts = load_graph_project_counts(&state.persistence.postgres, project_id).await?;
    let diagnostics = build_graph_diagnostics(project_id, &counts, &entities, &relations);

    info!(
        auth_token_id = %auth.token_id,
        workspace_id = %project.workspace_id,
        project_id = %project_id,
        entity_count = diagnostics.coverage.entity_count,
        relation_count = diagnostics.coverage.relation_count,
        document_count = diagnostics.content.persisted_document_count,
        chunk_count = diagnostics.content.persisted_chunk_count,
        readiness_status = %diagnostics.readiness.status,
        "loaded graph diagnostics",
    );

    Ok(Json(diagnostics))
}

async fn search_graph(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(project_id): Path<Uuid>,
    Query(query): Query<GraphSearchQuery>,
) -> Result<Json<GraphSearchResponse>, ApiError> {
    let project = load_project_and_authorize(&auth, &state, project_id, POLICY_QUERY_READ).await?;

    let needle = query.q.trim();
    if needle.is_empty() {
        warn!(
            auth_token_id = %auth.token_id,
            workspace_id = %project.workspace_id,
            project_id = %project_id,
            "rejecting graph search with empty query text",
        );
        return Err(ApiError::BadRequest("query parameter q must not be empty".into()));
    }

    let limit = query.limit.unwrap_or(DEFAULT_SEARCH_LIMIT).clamp(1, MAX_SEARCH_LIMIT);
    let entities = load_entities(&state.persistence.postgres, project_id).await?;
    let relations = load_relations(&state.persistence.postgres, project_id).await?;
    let entity_names = entity_name_map(&entities);
    let lower_needle = needle.to_lowercase();

    let entity_results = entities
        .iter()
        .filter_map(|row| {
            let reasons = entity_match_reasons(row, &lower_needle);
            if reasons.is_empty() {
                None
            } else {
                Some(GraphEntitySearchHit { entity: entity_summary(row), match_reasons: reasons })
            }
        })
        .take(limit)
        .collect::<Vec<_>>();

    let relation_results = relations
        .iter()
        .filter_map(|row| {
            let from_name = entity_names
                .get(&row.from_entity_id)
                .cloned()
                .unwrap_or_else(|| row.from_entity_id.to_string());
            let to_name = entity_names
                .get(&row.to_entity_id)
                .cloned()
                .unwrap_or_else(|| row.to_entity_id.to_string());
            let reasons = relation_match_reasons(row, &from_name, &to_name, &lower_needle);
            if reasons.is_empty() {
                None
            } else {
                Some(GraphRelationSearchHit {
                    relation: relation_summary(row),
                    from_entity_name: from_name,
                    to_entity_name: to_name,
                    match_reasons: reasons,
                })
            }
        })
        .take(limit)
        .collect::<Vec<_>>();

    let result_count = entity_results.len() + relation_results.len();

    info!(
        auth_token_id = %auth.token_id,
        workspace_id = %project.workspace_id,
        project_id = %project_id,
        query_len = needle.chars().count(),
        limit,
        entity_count = entities.len(),
        relation_count = relations.len(),
        result_count,
        "completed graph search",
    );

    Ok(Json(GraphSearchResponse {
        project_id,
        query: needle.into(),
        searched_fields: vec![
            "entity.canonical_name".into(),
            "entity.entity_type".into(),
            "entity.aliases".into(),
            "relation.relation_type".into(),
            "relation.endpoints".into(),
        ],
        result_count,
        entity_results,
        relation_results,
        generated_at: Utc::now(),
        warning: Some(graph_warning(entities.len(), relations.len())),
    }))
}

async fn get_graph_entity_detail(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((project_id, entity_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<GraphEntityDetailResponse>, ApiError> {
    let project = load_project_and_authorize(&auth, &state, project_id, POLICY_QUERY_READ).await?;

    let entities = load_entities(&state.persistence.postgres, project_id).await?;
    let relations = load_relations(&state.persistence.postgres, project_id).await?;
    let entity = entities
        .iter()
        .find(|row| row.id == entity_id)
        .ok_or_else(|| ApiError::NotFound(format!("entity {entity_id} not found")))?;
    let entity_names = entity_name_map(&entities);

    let incoming_relations = relations
        .iter()
        .filter(|row| row.to_entity_id == entity_id)
        .map(|row| relation_detail(row, &entity_names))
        .collect::<Vec<_>>();
    let outgoing_relations = relations
        .iter()
        .filter(|row| row.from_entity_id == entity_id)
        .map(|row| relation_detail(row, &entity_names))
        .collect::<Vec<_>>();

    let mut source_document_ids =
        collect_uuid_values(&entity.metadata_json, &["source_document_ids", "document_ids"]);
    let mut source_chunk_ids =
        collect_uuid_values(&entity.metadata_json, &["source_chunk_ids", "chunk_ids"]);

    for relation in relations
        .iter()
        .filter(|row| row.from_entity_id == entity_id || row.to_entity_id == entity_id)
    {
        source_document_ids.extend(collect_uuid_values(
            &relation.provenance_json,
            &["source_document_ids", "document_ids"],
        ));
        source_chunk_ids.extend(collect_uuid_values(
            &relation.provenance_json,
            &["source_chunk_ids", "chunk_ids"],
        ));
    }

    source_document_ids.sort();
    source_document_ids.dedup();
    source_chunk_ids.sort();
    source_chunk_ids.dedup();

    info!(
        auth_token_id = %auth.token_id,
        workspace_id = %project.workspace_id,
        project_id = %project_id,
        entity_id = %entity_id,
        alias_count = collect_string_values(&entity.metadata_json, &["aliases"]).len(),
        source_document_count = source_document_ids.len(),
        source_chunk_count = source_chunk_ids.len(),
        observed_relation_count = incoming_relations.len() + outgoing_relations.len(),
        "loaded graph entity detail",
    );

    Ok(Json(GraphEntityDetailResponse {
        project_id,
        entity: entity_summary(entity),
        aliases: collect_string_values(&entity.metadata_json, &["aliases"]),
        source_document_ids,
        source_chunk_ids,
        observed_relation_count: incoming_relations.len() + outgoing_relations.len(),
        incoming_relations: sort_relation_details(incoming_relations),
        outgoing_relations: sort_relation_details(outgoing_relations),
        generated_at: Utc::now(),
        warning: Some(graph_warning(entities.len(), relations.len())),
    }))
}

async fn get_graph_subgraph(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((project_id, entity_id)): Path<(Uuid, Uuid)>,
    Query(query): Query<GraphSubgraphQuery>,
) -> Result<Json<GraphSubgraphResponse>, ApiError> {
    let project = load_project_and_authorize(&auth, &state, project_id, POLICY_QUERY_READ).await?;

    let depth = query.depth.unwrap_or(1).min(3);
    let entities = load_entities(&state.persistence.postgres, project_id).await?;
    let relations = load_relations(&state.persistence.postgres, project_id).await?;
    let entity_names = entity_name_map(&entities);

    if !entities.iter().any(|row| row.id == entity_id) {
        return Err(ApiError::NotFound(format!("entity {entity_id} not found")));
    }

    let mut included_entities = HashSet::from([entity_id]);
    let mut frontier = HashSet::from([entity_id]);
    let mut included_relations = HashSet::new();

    for _ in 0..depth {
        let mut next_frontier = HashSet::new();
        for relation in &relations {
            let touches_from = frontier.contains(&relation.from_entity_id);
            let touches_to = frontier.contains(&relation.to_entity_id);
            if !touches_from && !touches_to {
                continue;
            }

            included_relations.insert(relation.id);
            included_entities.insert(relation.from_entity_id);
            included_entities.insert(relation.to_entity_id);

            if touches_from {
                next_frontier.insert(relation.to_entity_id);
            }
            if touches_to {
                next_frontier.insert(relation.from_entity_id);
            }
        }
        frontier = next_frontier;
    }

    let entity_items = sort_entity_summaries(
        entities
            .iter()
            .filter(|row| included_entities.contains(&row.id))
            .map(entity_summary)
            .collect(),
    );
    let relation_items = sort_relation_details(
        relations
            .iter()
            .filter(|row| included_relations.contains(&row.id))
            .map(|row| relation_detail(row, &entity_names))
            .collect(),
    );

    info!(
        auth_token_id = %auth.token_id,
        workspace_id = %project.workspace_id,
        project_id = %project_id,
        focus_entity_id = %entity_id,
        depth,
        entity_count = entity_items.len(),
        relation_count = relation_items.len(),
        "loaded graph subgraph",
    );

    Ok(Json(GraphSubgraphResponse {
        project_id,
        focus_entity_id: entity_id,
        depth,
        entity_count: entity_items.len(),
        relation_count: relation_items.len(),
        entities: entity_items,
        relations: relation_items,
        generated_at: Utc::now(),
        warning: Some(graph_warning(entities.len(), relations.len())),
    }))
}

async fn load_graph_snapshot(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<GraphProductSnapshot, ApiError> {
    let entities = load_entities(pool, project_id).await?;
    let relations = load_relations(pool, project_id).await?;
    let coverage = build_graph_coverage(project_id, entities.len(), relations.len());

    Ok(GraphProductSnapshot {
        project_id,
        coverage,
        entities: sort_entity_summaries(entities.iter().map(entity_summary).collect()),
        relations: sort_relation_summaries(relations.iter().map(relation_summary).collect()),
        generated_at: Utc::now(),
    })
}

async fn load_graph_project_counts(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<GraphProjectCounts, ApiError> {
    Ok(GraphProjectCounts {
        document_count: load_project_count(
            pool,
            project_id,
            "select count(*) from document where project_id = $1",
            "documents",
        )
        .await?,
        chunk_count: load_project_count(
            pool,
            project_id,
            "select count(*) from chunk where project_id = $1",
            "chunks",
        )
        .await?,
        embedded_chunk_count: load_project_count(
            pool,
            project_id,
            "select count(*) from chunk_embedding where project_id = $1",
            "chunk embeddings",
        )
        .await?,
        retrieval_run_count: load_project_count(
            pool,
            project_id,
            "select count(*) from retrieval_run where project_id = $1",
            "retrieval runs",
        )
        .await?,
    })
}

async fn load_project_count(
    pool: &PgPool,
    project_id: Uuid,
    query: &str,
    label: &str,
) -> Result<usize, ApiError> {
    sqlx::query_scalar::<_, i64>(query)
        .bind(project_id)
        .fetch_one(pool)
        .await
        .map(|value| usize::try_from(value).unwrap_or_default())
        .map_err(|error| {
            error!(project_id = %project_id, table = label, ?error, "failed to load graph project count");
            ApiError::Internal
        })
}

async fn load_entities(pool: &PgPool, project_id: Uuid) -> Result<Vec<EntityRow>, ApiError> {
    sqlx::query_as::<_, EntityRow>(
        "select id, project_id, canonical_name, entity_type, metadata_json
         from entity
         where project_id = $1
         order by created_at desc, canonical_name asc",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .map_err(|error| {
        error!(project_id = %project_id, ?error, "failed to load graph entities");
        ApiError::Internal
    })
}

async fn load_relations(pool: &PgPool, project_id: Uuid) -> Result<Vec<RelationRow>, ApiError> {
    sqlx::query_as::<_, RelationRow>(
        "select id, project_id, from_entity_id, to_entity_id, relation_type, provenance_json
         from relation
         where project_id = $1
         order by created_at desc, relation_type asc",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .map_err(|error| {
        error!(project_id = %project_id, ?error, "failed to load graph relations");
        ApiError::Internal
    })
}

fn entity_summary(row: &EntityRow) -> GraphEntitySummary {
    GraphEntitySummary {
        id: row.id,
        project_id: row.project_id,
        canonical_name: row.canonical_name.clone(),
        entity_type: row.entity_type.clone(),
        source_chunk_count: collect_string_values(
            &row.metadata_json,
            &["source_chunk_ids", "chunk_ids"],
        )
        .len(),
    }
}

fn relation_summary(row: &RelationRow) -> GraphRelationSummary {
    GraphRelationSummary {
        id: row.id,
        project_id: row.project_id,
        relation_type: row.relation_type.clone(),
        from_entity_id: row.from_entity_id,
        to_entity_id: row.to_entity_id,
        source_chunk_count: collect_string_values(
            &row.provenance_json,
            &["source_chunk_ids", "chunk_ids"],
        )
        .len(),
    }
}

fn relation_detail(row: &RelationRow, entity_names: &HashMap<Uuid, String>) -> GraphRelationDetail {
    GraphRelationDetail {
        relation: relation_summary(row),
        from_entity_name: entity_names
            .get(&row.from_entity_id)
            .cloned()
            .unwrap_or_else(|| row.from_entity_id.to_string()),
        to_entity_name: entity_names
            .get(&row.to_entity_id)
            .cloned()
            .unwrap_or_else(|| row.to_entity_id.to_string()),
    }
}

fn entity_name_map(entities: &[EntityRow]) -> HashMap<Uuid, String> {
    entities.iter().map(|row| (row.id, row.canonical_name.clone())).collect()
}

fn summarize_entity_kinds(entities: &[EntityRow]) -> Vec<GraphKindCount> {
    summarize_kind_counts(
        entities
            .iter()
            .map(|row| row.entity_type.clone().unwrap_or_else(|| "unknown".into()))
            .collect(),
    )
}

fn summarize_relation_kinds(relations: &[RelationRow]) -> Vec<GraphKindCount> {
    summarize_kind_counts(relations.iter().map(|row| row.relation_type.clone()).collect())
}

fn summarize_kind_counts(items: Vec<String>) -> Vec<GraphKindCount> {
    let mut counts = HashMap::<String, usize>::new();
    for item in items {
        *counts.entry(item).or_default() += 1;
    }

    let mut values =
        counts.into_iter().map(|(name, count)| GraphKindCount { name, count }).collect::<Vec<_>>();
    values.sort_by(|left, right| {
        right.count.cmp(&left.count).then_with(|| left.name.cmp(&right.name))
    });
    values
}

fn build_graph_coverage(
    project_id: Uuid,
    entity_count: usize,
    relation_count: usize,
) -> GraphCoverageSummary {
    let status = if entity_count == 0 && relation_count == 0 {
        "waiting_for_extraction"
    } else if relation_count == 0 {
        "entity_only"
    } else {
        "partial"
    };

    GraphCoverageSummary {
        project_id,
        entity_count,
        relation_count,
        extraction_runs: 0,
        status: status.into(),
        warning: Some(graph_warning(entity_count, relation_count)),
    }
}

fn build_graph_diagnostics(
    project_id: Uuid,
    counts: &GraphProjectCounts,
    entities: &[EntityRow],
    relations: &[RelationRow],
) -> GraphProjectDiagnosticsResponse {
    let mut referenced_document_ids = BTreeSet::new();
    let mut referenced_chunk_ids = BTreeSet::new();

    let mut entities_with_document_refs = 0usize;
    let mut entities_with_chunk_refs = 0usize;
    let mut relations_with_document_refs = 0usize;
    let mut relations_with_chunk_refs = 0usize;

    for entity in entities {
        let document_ids =
            collect_uuid_values(&entity.metadata_json, &["source_document_ids", "document_ids"]);
        if !document_ids.is_empty() {
            entities_with_document_refs += 1;
            referenced_document_ids.extend(document_ids);
        }

        let chunk_ids =
            collect_uuid_values(&entity.metadata_json, &["source_chunk_ids", "chunk_ids"]);
        if !chunk_ids.is_empty() {
            entities_with_chunk_refs += 1;
            referenced_chunk_ids.extend(chunk_ids);
        }
    }

    for relation in relations {
        let document_ids = collect_uuid_values(
            &relation.provenance_json,
            &["source_document_ids", "document_ids"],
        );
        if !document_ids.is_empty() {
            relations_with_document_refs += 1;
            referenced_document_ids.extend(document_ids);
        }

        let chunk_ids =
            collect_uuid_values(&relation.provenance_json, &["source_chunk_ids", "chunk_ids"]);
        if !chunk_ids.is_empty() {
            relations_with_chunk_refs += 1;
            referenced_chunk_ids.extend(chunk_ids);
        }
    }

    let content = GraphContentSummary {
        persisted_document_count: counts.document_count,
        persisted_chunk_count: counts.chunk_count,
        embedded_chunk_count: counts.embedded_chunk_count,
        retrieval_run_count: counts.retrieval_run_count,
        referenced_document_count: referenced_document_ids.len(),
        referenced_chunk_count: referenced_chunk_ids.len(),
    };
    let provenance = GraphProvenanceSummary {
        entities_with_document_refs,
        entities_with_chunk_refs,
        entities_without_chunk_refs: entities.len().saturating_sub(entities_with_chunk_refs),
        relations_with_document_refs,
        relations_with_chunk_refs,
        relations_without_chunk_refs: relations.len().saturating_sub(relations_with_chunk_refs),
    };
    let coverage = build_graph_coverage(project_id, entities.len(), relations.len());
    let readiness = build_graph_readiness(&coverage, &content, &provenance);

    GraphProjectDiagnosticsResponse {
        project_id,
        coverage,
        content,
        provenance,
        readiness,
        generated_at: Utc::now(),
    }
}

fn build_graph_readiness(
    coverage: &GraphCoverageSummary,
    content: &GraphContentSummary,
    provenance: &GraphProvenanceSummary,
) -> GraphReadinessSummary {
    let mut blockers = Vec::new();
    let mut next_steps = Vec::new();

    let status = if content.persisted_document_count == 0 && content.persisted_chunk_count == 0 {
        blockers.push(
            "Project has no persisted documents or chunks to extract graph evidence from.".into(),
        );
        next_steps.push(
            "Ingest plain text or upload a supported text-like file into this project before expecting graph rows."
                .into(),
        );
        "no_content"
    } else if coverage.entity_count == 0 && coverage.relation_count == 0 {
        blockers.push(
            "Project content exists, but no entity or relation rows have been persisted yet."
                .into(),
        );
        next_steps.push("Hook entity/relation extraction into the authoritative ingestion execution path so content ingestion emits graph rows.".into());
        "awaiting_graph_rows"
    } else if coverage.relation_count == 0 {
        blockers
            .push("Persisted entities exist, but no relation rows have been extracted yet.".into());
        next_steps.push(
            "Extend extraction to persist relation rows alongside entities so graph navigation becomes connected."
                .into(),
        );
        "entity_only"
    } else if provenance.entities_without_chunk_refs > 0
        || provenance.relations_without_chunk_refs > 0
    {
        if provenance.entities_without_chunk_refs > 0 {
            blockers.push(format!(
                "{} entity rows are missing source chunk references.",
                provenance.entities_without_chunk_refs
            ));
        }
        if provenance.relations_without_chunk_refs > 0 {
            blockers.push(format!(
                "{} relation rows are missing source chunk references.",
                provenance.relations_without_chunk_refs
            ));
        }
        next_steps.push("Persist source chunk ids in `entity.metadata_json` and `relation.provenance_json` for every extracted row.".into());
        if content.referenced_document_count == 0 {
            next_steps.push(
                "Persist source document ids next to chunk refs so graph evidence can link back to operator-facing document diagnostics."
                    .into(),
            );
        }
        "provenance_partial"
    } else {
        if content.persisted_chunk_count > 0 && content.embedded_chunk_count == 0 {
            next_steps.push(
                "Backfill chunk embeddings if retrieval and graph evidence should operate over the same warmed project corpus."
                    .into(),
            );
        }
        "graph_live"
    };

    GraphReadinessSummary { status: status.into(), blockers, next_steps }
}

fn entity_match_reasons(row: &EntityRow, needle: &str) -> Vec<String> {
    let mut reasons = Vec::new();
    if row.canonical_name.to_lowercase().contains(needle) {
        reasons.push("canonical_name".into());
    }
    if row.entity_type.as_ref().is_some_and(|value| value.to_lowercase().contains(needle)) {
        reasons.push("entity_type".into());
    }
    if collect_string_values(&row.metadata_json, &["aliases"])
        .iter()
        .any(|value| value.to_lowercase().contains(needle))
    {
        reasons.push("aliases".into());
    }
    reasons
}

fn relation_match_reasons(
    row: &RelationRow,
    from_name: &str,
    to_name: &str,
    needle: &str,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if row.relation_type.to_lowercase().contains(needle) {
        reasons.push("relation_type".into());
    }
    if from_name.to_lowercase().contains(needle) {
        reasons.push("from_entity".into());
    }
    if to_name.to_lowercase().contains(needle) {
        reasons.push("to_entity".into());
    }
    reasons
}

fn collect_string_values(value: &Value, keys: &[&str]) -> Vec<String> {
    let mut items = BTreeSet::new();
    if let Value::Object(map) = value {
        for key in keys {
            if let Some(entry) = map.get(*key) {
                collect_strings(entry, &mut items);
            }
        }
    }
    items.into_iter().collect()
}

fn collect_uuid_values(value: &Value, keys: &[&str]) -> Vec<Uuid> {
    collect_string_values(value, keys)
        .into_iter()
        .filter_map(|item| Uuid::parse_str(&item).ok())
        .collect()
}

fn collect_strings(value: &Value, output: &mut BTreeSet<String>) {
    match value {
        Value::String(item) => {
            if !item.trim().is_empty() {
                output.insert(item.trim().into());
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_strings(item, output);
            }
        }
        Value::Object(map) => {
            if let Some(item) = map.get("id").and_then(Value::as_str) {
                if !item.trim().is_empty() {
                    output.insert(item.trim().into());
                }
            }
            if let Some(item) = map.get("value").and_then(Value::as_str) {
                if !item.trim().is_empty() {
                    output.insert(item.trim().into());
                }
            }
        }
        _ => {}
    }
}

fn sort_entity_summaries(mut items: Vec<GraphEntitySummary>) -> Vec<GraphEntitySummary> {
    items.sort_by(|left, right| {
        right
            .source_chunk_count
            .cmp(&left.source_chunk_count)
            .then_with(|| left.canonical_name.cmp(&right.canonical_name))
    });
    items
}

fn sort_relation_summaries(mut items: Vec<GraphRelationSummary>) -> Vec<GraphRelationSummary> {
    items.sort_by(|left, right| {
        right
            .source_chunk_count
            .cmp(&left.source_chunk_count)
            .then_with(|| left.relation_type.cmp(&right.relation_type))
    });
    items
}

fn sort_relation_details(mut items: Vec<GraphRelationDetail>) -> Vec<GraphRelationDetail> {
    items.sort_by(|left, right| {
        right
            .relation
            .source_chunk_count
            .cmp(&left.relation.source_chunk_count)
            .then_with(|| left.relation.relation_type.cmp(&right.relation.relation_type))
    });
    items
}

fn graph_warning(entity_count: usize, relation_count: usize) -> String {
    if entity_count == 0 && relation_count == 0 {
        EMPTY_GRAPH_WARNING.into()
    } else if relation_count == 0 {
        ENTITY_ONLY_GRAPH_WARNING.into()
    } else {
        DEFAULT_GRAPH_WARNING.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_string_values_reads_arrays_and_nested_ids() {
        let value = serde_json::json!({
            "aliases": ["Acme", {"value": "Acme Corp"}],
            "source_chunk_ids": [{"id": "a6da273d-c53e-4690-88e7-0ad74d87d6d0"}]
        });

        let aliases = collect_string_values(&value, &["aliases"]);
        let chunk_ids = collect_string_values(&value, &["source_chunk_ids"]);

        assert_eq!(aliases, vec!["Acme", "Acme Corp"]);
        assert_eq!(chunk_ids, vec!["a6da273d-c53e-4690-88e7-0ad74d87d6d0"]);
    }

    #[test]
    fn entity_match_reasons_include_aliases() {
        let row = EntityRow {
            id: Uuid::now_v7(),
            project_id: Uuid::now_v7(),
            canonical_name: "RustRAG".into(),
            entity_type: Some("product".into()),
            metadata_json: serde_json::json!({ "aliases": ["Runtime Graph"] }),
        };

        let reasons = entity_match_reasons(&row, "runtime");

        assert_eq!(reasons, vec!["aliases"]);
    }

    #[test]
    fn build_graph_coverage_marks_entity_only_when_relations_are_missing() {
        let coverage = build_graph_coverage(Uuid::now_v7(), 3, 0);

        assert_eq!(coverage.status, "entity_only");
        assert_eq!(coverage.warning.as_deref(), Some(ENTITY_ONLY_GRAPH_WARNING));
    }

    #[test]
    fn build_graph_diagnostics_marks_waiting_when_content_exists_without_graph_rows() {
        let diagnostics = build_graph_diagnostics(
            Uuid::now_v7(),
            &GraphProjectCounts {
                document_count: 2,
                chunk_count: 8,
                embedded_chunk_count: 0,
                retrieval_run_count: 0,
            },
            &[],
            &[],
        );

        assert_eq!(diagnostics.readiness.status, "awaiting_graph_rows");
        assert_eq!(diagnostics.content.persisted_chunk_count, 8);
        assert_eq!(diagnostics.coverage.status, "waiting_for_extraction");
        assert_eq!(
            diagnostics.readiness.blockers,
            vec!["Project content exists, but no entity or relation rows have been persisted yet."]
        );
    }

    #[test]
    fn build_graph_diagnostics_marks_provenance_partial_when_chunk_refs_are_missing() {
        let entity = EntityRow {
            id: Uuid::now_v7(),
            project_id: Uuid::now_v7(),
            canonical_name: "Acme".into(),
            entity_type: Some("company".into()),
            metadata_json: serde_json::json!({
                "source_document_ids": ["9f7303c8-a2f4-4df7-a4c5-72e130f523e3"]
            }),
        };
        let relation = RelationRow {
            id: Uuid::now_v7(),
            project_id: entity.project_id,
            from_entity_id: entity.id,
            to_entity_id: Uuid::now_v7(),
            relation_type: "supplies".into(),
            provenance_json: serde_json::json!({}),
        };

        let diagnostics = build_graph_diagnostics(
            entity.project_id,
            &GraphProjectCounts {
                document_count: 1,
                chunk_count: 3,
                embedded_chunk_count: 3,
                retrieval_run_count: 1,
            },
            &[entity],
            &[relation],
        );

        assert_eq!(diagnostics.readiness.status, "provenance_partial");
        assert_eq!(diagnostics.provenance.entities_without_chunk_refs, 1);
        assert_eq!(diagnostics.provenance.relations_without_chunk_refs, 1);
        assert!(
            diagnostics
                .readiness
                .next_steps
                .iter()
                .any(|item| item.contains("entity.metadata_json"))
        );
    }
}
