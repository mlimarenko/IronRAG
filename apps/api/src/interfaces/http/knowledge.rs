use std::collections::{BTreeMap, HashSet};

mod library;
mod search;

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, State},
    http::header,
    response::Response,
    routing::get,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::knowledge::{KnowledgeLibraryGeneration, TypedTechnicalFact},
    infra::arangodb::{
        collections::KNOWLEDGE_CHUNK_COLLECTION, document_store::KnowledgeChunkRow,
        graph_store::KnowledgeEvidenceRow,
    },
    infra::repositories,
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_KNOWLEDGE_READ, load_library_and_authorize},
        router_support::ApiError,
    },
    services::knowledge::graph_stream::build_graph_topology_bytes,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/knowledge/context-bundles/{bundle_id}", get(library::get_context_bundle))
        .route(
            "/knowledge/libraries/{library_id}/context-bundles",
            get(library::list_context_bundles),
        )
        .route("/knowledge/libraries/{library_id}/documents", get(library::list_documents))
        .route(
            "/knowledge/libraries/{library_id}/documents/{document_id}",
            get(library::get_document),
        )
        .route("/knowledge/libraries/{library_id}/summary", get(library::get_library_summary))
        .route("/knowledge/libraries/{library_id}/graph", get(get_graph))
        .route("/knowledge/libraries/{library_id}/entities/{entity_id}", get(get_entity))
        .route("/knowledge/libraries/{library_id}/relations/{relation_id}", get(get_relation))
        .route(
            "/knowledge/libraries/{library_id}/generations",
            get(library::list_library_generations),
        )
        .route("/knowledge/libraries/{library_id}/search/documents", get(search::search_documents))
        .route("/search/documents", get(search::search_documents_by_library_query))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeEntityDetailResponse {
    entity: RuntimeKnowledgeEntityRow,
    mention_edges: Vec<KnowledgeEntityMentionEdgeRow>,
    mentioned_chunks: Vec<KnowledgeChunkRow>,
    supporting_evidence_edges: Vec<KnowledgeEvidenceSupportEntityEdgeRow>,
    supporting_evidence: Vec<RuntimeKnowledgeEvidenceRow>,
    supporting_typed_facts: Vec<TypedTechnicalFact>,
    graph_evidence_summary: KnowledgeGraphEvidenceSummary,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeRelationDetailResponse {
    relation: RuntimeKnowledgeRelationRow,
    supporting_evidence_edges: Vec<KnowledgeEvidenceSupportRelationEdgeRow>,
    supporting_evidence: Vec<RuntimeKnowledgeEvidenceRow>,
    supporting_typed_facts: Vec<TypedTechnicalFact>,
    graph_evidence_summary: KnowledgeGraphEvidenceSummary,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeLibrarySummaryResponse {
    library_id: Uuid,
    document_counts_by_readiness: BTreeMap<String, i64>,
    node_count: i64,
    edge_count: i64,
    graph_ready_document_count: i64,
    graph_sparse_document_count: i64,
    typed_fact_document_count: i64,
    updated_at: DateTime<Utc>,
    latest_generation: Option<KnowledgeLibraryGeneration>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeDocumentProvenanceSummary {
    supporting_evidence_count: usize,
    lexical_chunk_count: usize,
    vector_chunk_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeTechnicalFactProvenanceSummary {
    typed_fact_count: usize,
    fact_kind_counts: BTreeMap<String, usize>,
    conflict_group_count: usize,
    support_block_count: usize,
    support_chunk_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeGraphEvidenceSummary {
    evidence_count: usize,
    chunk_backed_count: usize,
    block_backed_count: usize,
    fact_backed_count: usize,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeEntityMentionEdgeRow {
    key: String,
    entity_id: Uuid,
    chunk_id: Uuid,
    rank: Option<i32>,
    score: Option<f64>,
    inclusion_reason: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeEvidenceSupportEntityEdgeRow {
    key: String,
    evidence_id: Uuid,
    entity_id: Uuid,
    rank: Option<i32>,
    score: Option<f64>,
    inclusion_reason: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeEvidenceSupportRelationEdgeRow {
    key: String,
    evidence_id: Uuid,
    relation_id: Uuid,
    rank: Option<i32>,
    score: Option<f64>,
    inclusion_reason: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeKnowledgeEntityRow {
    key: String,
    entity_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    canonical_label: String,
    aliases: Vec<String>,
    entity_type: String,
    entity_sub_type: Option<String>,
    summary: Option<String>,
    confidence: Option<f64>,
    support_count: i32,
    freshness_generation: i64,
    entity_state: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeKnowledgeRelationRow {
    key: String,
    relation_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    relation_type: String,
    normalized_assertion: String,
    confidence: Option<f64>,
    support_count: i32,
    contradiction_state: String,
    freshness_generation: i64,
    relation_state: String,
    subject_entity_id: Uuid,
    object_entity_id: Uuid,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeKnowledgeEvidenceRow {
    key: String,
    evidence_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    chunk_id: Option<Uuid>,
    span_start: Option<i32>,
    span_end: Option<i32>,
    excerpt: String,
    support_kind: String,
    extraction_method: String,
    confidence: Option<f64>,
    evidence_state: String,
    freshness_generation: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

// Removed in the 2026-04-15 perf audit: `list_entities`, `list_relations`
// and `get_graph_workbench` were huge endpoints (17–27 MiB payloads, no
// pagination) that were never called from the web UI. The only graph
// surface that survives is the compact NDJSON stream at
// `/v1/knowledge/libraries/{id}/graph`, which the React graph page has
// used exclusively since the B4 release. Single-entity and
// single-relation detail endpoints are still wired up below.

#[tracing::instrument(
    level = "info",
    name = "http.get_graph",
    skip_all,
    fields(%library_id)
)]
async fn get_graph(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;

    let bytes = build_graph_topology_bytes(&state, library_id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(bytes))
        .map_err(|error| ApiError::internal_with_log(error, "internal"))
}

#[tracing::instrument(
    level = "info",
    name = "http.get_entity",
    skip_all,
    fields(library_id = %library_id, entity_id = %entity_id)
)]
async fn get_entity(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((library_id, entity_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<KnowledgeEntityDetailResponse>, ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let entity = repositories::get_runtime_graph_node_by_id(
        &state.persistence.postgres,
        library_id,
        entity_id,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    .ok_or_else(|| ApiError::resource_not_found("runtime_graph_node", entity_id))?;
    if entity.library_id != library_id {
        return Err(ApiError::resource_not_found("knowledge_entity", entity_id));
    }

    let supporting_evidence =
        load_runtime_graph_supporting_evidence(&state, library_id, "node", entity_id).await?;
    let mention_edges = build_runtime_entity_mention_edges(entity_id, &supporting_evidence);
    let mention_chunk_ids: Vec<Uuid> = mention_edges.iter().map(|edge| edge.chunk_id).collect();
    let mentioned_chunks = load_chunks_by_ids(&state, &mention_chunk_ids).await?;
    let supporting_evidence_edges =
        build_runtime_entity_evidence_support_edges(entity_id, &supporting_evidence);
    let supporting_typed_facts = Vec::new();
    let graph_evidence_summary = summarize_runtime_graph_evidence(&supporting_evidence, 0);

    Ok(Json(KnowledgeEntityDetailResponse {
        entity: map_runtime_graph_node_to_entity_row(entity, library.workspace_id, library_id),
        mention_edges,
        mentioned_chunks,
        supporting_evidence_edges,
        supporting_evidence,
        supporting_typed_facts,
        graph_evidence_summary,
    }))
}

#[tracing::instrument(
    level = "info",
    name = "http.get_relation",
    skip_all,
    fields(library_id = %library_id, relation_id = %relation_id)
)]
async fn get_relation(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((library_id, relation_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<KnowledgeRelationDetailResponse>, ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let relation = repositories::get_runtime_graph_edge_by_id(
        &state.persistence.postgres,
        library_id,
        relation_id,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    .ok_or_else(|| ApiError::resource_not_found("runtime_graph_edge", relation_id))?;
    if relation.library_id != library_id {
        return Err(ApiError::resource_not_found("knowledge_relation", relation_id));
    }

    let supporting_evidence =
        load_runtime_graph_supporting_evidence(&state, library_id, "edge", relation_id).await?;
    let supporting_evidence_edges =
        build_runtime_relation_evidence_support_edges(relation_id, &supporting_evidence);
    let supporting_typed_facts = Vec::new();
    let graph_evidence_summary = summarize_runtime_graph_evidence(&supporting_evidence, 0);

    Ok(Json(KnowledgeRelationDetailResponse {
        relation: map_runtime_graph_edge_to_relation_row(
            library.workspace_id,
            library_id,
            relation,
        ),
        supporting_evidence_edges,
        supporting_evidence,
        supporting_typed_facts,
        graph_evidence_summary,
    }))
}

async fn load_chunks_by_ids(
    state: &AppState,
    chunk_ids: &[Uuid],
) -> Result<Vec<KnowledgeChunkRow>, ApiError> {
    if chunk_ids.is_empty() {
        return Ok(Vec::new());
    }
    let cursor = state
        .arango_document_store
        .client()
        .query_json(
            "FOR chunk IN @@collection
             FILTER chunk.chunk_id IN @chunk_ids
             SORT chunk.chunk_id ASC
             RETURN chunk",
            serde_json::json!({
                "@collection": KNOWLEDGE_CHUNK_COLLECTION,
                "chunk_ids": chunk_ids,
            }),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    decode_many_results(cursor).map_err(|e| ApiError::internal_with_log(e, "internal"))
}

fn summarize_typed_technical_facts(
    typed_facts: &[TypedTechnicalFact],
) -> KnowledgeTechnicalFactProvenanceSummary {
    let mut fact_kind_counts = BTreeMap::<String, usize>::new();
    let mut conflict_group_ids = HashSet::<String>::new();
    let mut support_block_ids = HashSet::<Uuid>::new();
    let mut support_chunk_ids = HashSet::<Uuid>::new();
    for fact in typed_facts {
        *fact_kind_counts.entry(fact.fact_kind.as_str().to_string()).or_default() += 1;
        if let Some(conflict_group_id) = fact.conflict_group_id.as_ref() {
            conflict_group_ids.insert(conflict_group_id.clone());
        }
        support_block_ids.extend(fact.support_block_ids.iter().copied());
        support_chunk_ids.extend(fact.support_chunk_ids.iter().copied());
    }
    KnowledgeTechnicalFactProvenanceSummary {
        typed_fact_count: typed_facts.len(),
        fact_kind_counts,
        conflict_group_count: conflict_group_ids.len(),
        support_block_count: support_block_ids.len(),
        support_chunk_count: support_chunk_ids.len(),
    }
}

fn summarize_graph_evidence(
    evidence_rows: &[KnowledgeEvidenceRow],
) -> KnowledgeGraphEvidenceSummary {
    KnowledgeGraphEvidenceSummary {
        evidence_count: evidence_rows.len(),
        chunk_backed_count: evidence_rows
            .iter()
            .filter(|evidence| evidence.chunk_id.is_some())
            .count(),
        block_backed_count: evidence_rows
            .iter()
            .filter(|evidence| evidence.block_id.is_some())
            .count(),
        fact_backed_count: evidence_rows
            .iter()
            .filter(|evidence| evidence.fact_id.is_some())
            .count(),
    }
}

fn summarize_runtime_graph_evidence(
    evidence_rows: &[RuntimeKnowledgeEvidenceRow],
    typed_fact_count: usize,
) -> KnowledgeGraphEvidenceSummary {
    let chunk_backed_count =
        evidence_rows.iter().filter(|evidence| evidence.chunk_id.is_some()).count();
    KnowledgeGraphEvidenceSummary {
        evidence_count: evidence_rows.len(),
        chunk_backed_count,
        block_backed_count: 0,
        fact_backed_count: typed_fact_count,
    }
}

fn runtime_graph_aliases(metadata: &serde_json::Value) -> Vec<String> {
    metadata
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .collect()
}

fn runtime_graph_confidence(metadata: &serde_json::Value) -> Option<f64> {
    metadata.get("confidence").and_then(serde_json::Value::as_f64)
}

fn runtime_graph_state(metadata: &serde_json::Value, fallback: &str) -> String {
    // `extraction_recovery_status` describes HOW extraction produced the
    // node (clean / partial / recovered / failed) — it is NOT the
    // admittance state. The admittance state lives in `entity_state` /
    // `relation_state`, which default to "active" when admitted.
    metadata
        .get("entity_state")
        .and_then(serde_json::Value::as_str)
        .or_else(|| metadata.get("relation_state").and_then(serde_json::Value::as_str))
        .unwrap_or(fallback)
        .to_string()
}

fn runtime_graph_contradiction_state(metadata: &serde_json::Value) -> String {
    metadata
        .get("contradiction_state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("clean")
        .to_string()
}

fn map_runtime_graph_node_to_entity_row(
    row: repositories::RuntimeGraphNodeRow,
    workspace_id: Uuid,
    library_id: Uuid,
) -> RuntimeKnowledgeEntityRow {
    RuntimeKnowledgeEntityRow {
        key: row.canonical_key.clone(),
        entity_id: row.id,
        workspace_id,
        library_id,
        canonical_label: row.label,
        aliases: runtime_graph_aliases(&row.aliases_json),
        entity_type: row.node_type,
        entity_sub_type: row
            .metadata_json
            .get("sub_type")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string),
        summary: row.summary,
        confidence: runtime_graph_confidence(&row.metadata_json),
        support_count: row.support_count,
        freshness_generation: row.projection_version,
        entity_state: runtime_graph_state(&row.metadata_json, "active"),
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn map_runtime_graph_edge_to_relation_row(
    workspace_id: Uuid,
    library_id: Uuid,
    row: repositories::RuntimeGraphEdgeRow,
) -> RuntimeKnowledgeRelationRow {
    RuntimeKnowledgeRelationRow {
        key: row.canonical_key.clone(),
        relation_id: row.id,
        workspace_id,
        library_id,
        relation_type: row.relation_type.clone(),
        normalized_assertion: row.summary.clone().unwrap_or_else(|| row.canonical_key.clone()),
        confidence: row.weight,
        support_count: row.support_count,
        contradiction_state: runtime_graph_contradiction_state(&row.metadata_json),
        freshness_generation: row.projection_version,
        relation_state: runtime_graph_state(&row.metadata_json, "active"),
        subject_entity_id: row.from_node_id,
        object_entity_id: row.to_node_id,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

async fn load_runtime_graph_supporting_evidence(
    state: &AppState,
    library_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
) -> Result<Vec<RuntimeKnowledgeEvidenceRow>, ApiError> {
    let workspace_id = state
        .canonical_services
        .catalog
        .get_library(state, library_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .workspace_id;
    let evidence_rows = repositories::list_active_runtime_graph_evidence_lifecycle_by_target(
        &state.persistence.postgres,
        library_id,
        target_kind,
        target_id,
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

    Ok(evidence_rows
        .into_iter()
        .filter_map(|row| {
            let document_id = row.document_id?;
            Some(RuntimeKnowledgeEvidenceRow {
                key: row.id.to_string(),
                evidence_id: row.id,
                workspace_id,
                library_id,
                document_id,
                revision_id: row.revision_id.unwrap_or_else(Uuid::nil),
                chunk_id: row.chunk_id,
                span_start: None,
                span_end: None,
                excerpt: row.evidence_text,
                support_kind: target_kind.to_string(),
                extraction_method: "runtime_graph".to_string(),
                confidence: row.confidence_score,
                evidence_state: "active".to_string(),
                freshness_generation: 0,
                created_at: row.created_at,
                updated_at: row.created_at,
            })
        })
        .collect())
}

fn build_runtime_entity_mention_edges(
    entity_id: Uuid,
    evidence_rows: &[RuntimeKnowledgeEvidenceRow],
) -> Vec<KnowledgeEntityMentionEdgeRow> {
    let mut seen = HashSet::<Uuid>::new();
    let mut edges = Vec::new();
    for row in evidence_rows {
        let Some(chunk_id) = row.chunk_id else {
            continue;
        };
        if !seen.insert(chunk_id) {
            continue;
        }
        edges.push(KnowledgeEntityMentionEdgeRow {
            key: format!("{}:{chunk_id}", row.evidence_id),
            entity_id,
            chunk_id,
            rank: None,
            score: row.confidence,
            inclusion_reason: Some("runtime_graph_evidence".to_string()),
            created_at: row.created_at,
        });
    }
    edges
}

fn build_runtime_entity_evidence_support_edges(
    entity_id: Uuid,
    evidence_rows: &[RuntimeKnowledgeEvidenceRow],
) -> Vec<KnowledgeEvidenceSupportEntityEdgeRow> {
    evidence_rows
        .iter()
        .map(|row| KnowledgeEvidenceSupportEntityEdgeRow {
            key: row.key.clone(),
            evidence_id: row.evidence_id,
            entity_id,
            rank: None,
            score: row.confidence,
            inclusion_reason: Some("runtime_graph_evidence".to_string()),
            created_at: row.created_at,
        })
        .collect()
}

fn build_runtime_relation_evidence_support_edges(
    relation_id: Uuid,
    evidence_rows: &[RuntimeKnowledgeEvidenceRow],
) -> Vec<KnowledgeEvidenceSupportRelationEdgeRow> {
    evidence_rows
        .iter()
        .map(|row| KnowledgeEvidenceSupportRelationEdgeRow {
            key: row.key.clone(),
            evidence_id: row.evidence_id,
            relation_id,
            rank: None,
            score: row.confidence,
            inclusion_reason: Some("runtime_graph_evidence".to_string()),
            created_at: row.created_at,
        })
        .collect()
}

fn decode_many_results<T>(cursor: serde_json::Value) -> Result<Vec<T>, ApiError>
where
    T: for<'de> serde::Deserialize<'de>,
{
    let result = cursor.get("result").cloned().ok_or(ApiError::Internal)?;
    serde_json::from_value(result).map_err(|e| ApiError::internal_with_log(e, "internal"))
}
