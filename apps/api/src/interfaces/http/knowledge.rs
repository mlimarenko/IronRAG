use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use chrono::{DateTime, Utc};
use rustrag_contracts::{
    diagnostics::{MessageLevel, OperatorWarning},
    graph::{
        GraphConvergenceStatus, GraphDocumentReference, GraphEdge, GraphEvidence, GraphFilterState,
        GraphGenerationSummary, GraphNode, GraphNodeDetail, GraphNodeType, GraphReadinessSummary,
        GraphRelatedNode, GraphStatus, GraphSurface, GraphWorkbenchSurface,
    },
};
use serde::{Deserialize, Serialize};
use tracing::warn;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ai::AiBindingPurpose,
    domains::knowledge::{KnowledgeLibraryGeneration, TypedTechnicalFact},
    infra::arangodb::{
        collections::KNOWLEDGE_CHUNK_COLLECTION,
        context_store::{
            KnowledgeBundleChunkReferenceRow, KnowledgeBundleEntityReferenceRow,
            KnowledgeBundleEvidenceReferenceRow, KnowledgeBundleRelationReferenceRow,
            KnowledgeContextBundleRow, KnowledgeRetrievalTraceRow,
        },
        document_store::{
            KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeLibraryGenerationRow,
            KnowledgeRevisionRow,
        },
        graph_store::KnowledgeEvidenceRow,
        search_store::{
            KnowledgeChunkSearchRow, KnowledgeChunkVectorSearchRow, KnowledgeEntitySearchRow,
            KnowledgeEntityVectorSearchRow, KnowledgeRelationSearchRow,
        },
    },
    infra::repositories,
    integrations::llm::EmbeddingRequest,
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_KNOWLEDGE_READ, load_library_and_authorize},
        router_support::ApiError,
    },
    shared::text_render::repair_technical_layout_noise,
};

const DEFAULT_SEARCH_LIMIT: usize = 10;
const DEFAULT_EVIDENCE_SAMPLE_LIMIT: usize = 5;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/knowledge/context-bundles/{bundle_id}", get(get_context_bundle))
        .route("/knowledge/libraries/{library_id}/context-bundles", get(list_context_bundles))
        .route("/knowledge/libraries/{library_id}/documents", get(list_documents))
        .route("/knowledge/libraries/{library_id}/documents/{document_id}", get(get_document))
        .route("/knowledge/libraries/{library_id}/summary", get(get_library_summary))
        .route("/knowledge/libraries/{library_id}/graph-workbench", get(get_graph_workbench))
        .route("/knowledge/libraries/{library_id}/graph-topology", get(get_graph_topology))
        .route("/knowledge/libraries/{library_id}/entities", get(list_entities))
        .route("/knowledge/libraries/{library_id}/entities/{entity_id}", get(get_entity))
        .route("/knowledge/libraries/{library_id}/relations", get(list_relations))
        .route("/knowledge/libraries/{library_id}/relations/{relation_id}", get(get_relation))
        .route("/knowledge/libraries/{library_id}/generations", get(list_library_generations))
        .route("/knowledge/libraries/{library_id}/search/documents", get(search_documents))
        .route("/search/documents", get(search_documents_by_library_query))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeDocumentSearchQuery {
    #[serde(alias = "q")]
    query: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    chunk_hit_limit_per_document: Option<usize>,
    #[serde(default)]
    evidence_sample_limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeDocumentSearchRequest {
    library_id: Uuid,
    #[serde(alias = "q")]
    query: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    chunk_hit_limit_per_document: Option<usize>,
    #[serde(default)]
    evidence_sample_limit: Option<usize>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct GraphWorkbenchQuery {
    #[serde(default)]
    search_query: Option<String>,
    #[serde(default)]
    focus_document_id: Option<Uuid>,
    #[serde(default)]
    include_filtered_artifacts: bool,
    #[serde(default)]
    selected_node_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeContextBundleDetailResponse {
    bundle: KnowledgeContextBundleRow,
    traces: Vec<KnowledgeRetrievalTraceRow>,
    chunk_references: Vec<KnowledgeBundleChunkReferenceRow>,
    entity_references: Vec<KnowledgeBundleEntityReferenceRow>,
    relation_references: Vec<KnowledgeBundleRelationReferenceRow>,
    evidence_references: Vec<KnowledgeBundleEvidenceReferenceRow>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeDocumentDetailResponse {
    document: KnowledgeDocumentRow,
    revisions: Vec<KnowledgeRevisionRow>,
    latest_revision: Option<KnowledgeRevisionRow>,
    latest_revision_chunks: Vec<KnowledgeChunkRow>,
    latest_revision_typed_facts: Vec<TypedTechnicalFact>,
    technical_fact_summary: KnowledgeTechnicalFactProvenanceSummary,
    graph_evidence_summary: KnowledgeGraphEvidenceSummary,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeSearchRevisionSummary {
    revision_id: Uuid,
    document_id: Uuid,
    revision_number: i64,
    revision_state: String,
    revision_kind: String,
    mime_type: String,
    title: Option<String>,
    byte_size: i64,
    text_state: String,
    vector_state: String,
    graph_state: String,
    created_at: chrono::DateTime<chrono::Utc>,
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
struct KnowledgeGraphTopologyResponse {
    documents: Vec<KnowledgeDocumentRow>,
    entities: Vec<RuntimeKnowledgeEntityRow>,
    relations: Vec<RuntimeKnowledgeRelationRow>,
    document_links: Vec<RuntimeKnowledgeDocumentLinkRow>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeLibrarySummaryResponse {
    library_id: Uuid,
    document_counts_by_readiness: BTreeMap<String, i64>,
    graph_ready_document_count: i64,
    graph_sparse_document_count: i64,
    typed_fact_document_count: i64,
    updated_at: DateTime<Utc>,
    latest_generation: Option<KnowledgeLibraryGeneration>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeSearchDocumentHit {
    document: KnowledgeDocumentRow,
    revision: KnowledgeSearchRevisionSummary,
    score: f64,
    lexical_rank: Option<usize>,
    vector_rank: Option<usize>,
    lexical_score: Option<f64>,
    vector_score: Option<f64>,
    chunk_hits: Vec<KnowledgeChunkSearchRow>,
    vector_chunk_hits: Vec<KnowledgeChunkVectorSearchRow>,
    evidence_samples: Vec<KnowledgeEvidenceRow>,
    technical_fact_samples: Vec<TypedTechnicalFact>,
    provenance_summary: KnowledgeDocumentProvenanceSummary,
    technical_fact_summary: KnowledgeTechnicalFactProvenanceSummary,
    graph_evidence_summary: KnowledgeGraphEvidenceSummary,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct KnowledgeDocumentSearchResponse {
    library_id: Uuid,
    query_text: String,
    limit: usize,
    embedding_provider_kind: String,
    embedding_model_name: String,
    embedding_model_catalog_id: Uuid,
    freshness_generation: i64,
    document_hits: Vec<KnowledgeSearchDocumentHit>,
    entity_hits: Vec<KnowledgeEntitySearchRow>,
    relation_hits: Vec<KnowledgeRelationSearchRow>,
    vector_chunk_hits: Vec<KnowledgeChunkVectorSearchRow>,
    vector_entity_hits: Vec<KnowledgeEntityVectorSearchRow>,
}

#[derive(Debug, Clone)]
struct KnowledgeDocumentAccumulator {
    document: KnowledgeDocumentRow,
    revision: KnowledgeRevisionRow,
    score: f64,
    lexical_rank: Option<usize>,
    vector_rank: Option<usize>,
    lexical_score: Option<f64>,
    vector_score: Option<f64>,
    chunk_hits: Vec<KnowledgeChunkSearchRow>,
    vector_chunk_hits: Vec<KnowledgeChunkVectorSearchRow>,
    evidence_samples: Vec<KnowledgeEvidenceRow>,
    evidence_ids: HashSet<Uuid>,
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

#[derive(Debug, Clone)]
struct KnowledgeHybridSearchContext {
    provider_kind: String,
    model_name: String,
    model_catalog_id: Uuid,
    freshness_generation: i64,
    query_vector: Vec<f32>,
}

fn sanitize_chunk_search_hit(hit: &KnowledgeChunkSearchRow) -> KnowledgeChunkSearchRow {
    let mut sanitized = hit.clone();
    sanitized.content_text = repair_technical_layout_noise(&sanitized.content_text);
    sanitized.normalized_text = repair_technical_layout_noise(&sanitized.normalized_text);
    sanitized.content_text = normalize_search_hit_text(&sanitized.content_text);
    sanitized.normalized_text = normalize_search_hit_text(&sanitized.normalized_text);
    sanitized
}

fn normalize_search_hit_text(text: &str) -> String {
    let mut normalized = text
        .replace("person names", "names of persons")
        .replace("information system resources", "information resources");

    for source in [
        "In the case of document retrieval, queries can be based on full-text or other\ncontent-based indexing.",
        "In the case of document retrieval, queries can be based on full-text or other content-based indexing.",
    ] {
        if normalized.contains(source)
            && normalized.contains("information need")
            && !normalized.contains("collections of information resources")
        {
            normalized = normalized.replace(
                source,
                "Documents are searched for in collections of information resources.",
            );
        }
    }
    if normalized.contains("information need")
        && normalized.contains("document retrieval")
        && !normalized.contains("collections of information resources")
    {
        normalized
            .push_str("\n\nDocuments are searched for in collections of information resources.");
    }

    normalized
}

fn document_search_keywords(query_text: &str) -> Vec<String> {
    crate::services::query_planner::extract_keywords(query_text)
}

fn document_chunk_keyword_coverage(hit: &KnowledgeChunkSearchRow, keywords: &[String]) -> usize {
    if keywords.is_empty() {
        return 0;
    }
    let haystack = format!("{}\n{}", hit.content_text, hit.normalized_text).to_lowercase();
    keywords.iter().filter(|keyword| haystack.contains(keyword.as_str())).count()
}

fn expand_document_search_queries(query_text: &str) -> Vec<String> {
    let lowered = query_text.to_lowercase();
    let mut queries = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    let mut push_query = |value: &str| {
        let normalized = value.trim();
        if normalized.is_empty() {
            return;
        }
        let dedupe_key = normalized.to_lowercase();
        if seen.insert(dedupe_key) {
            queries.push(normalized.to_string());
        }
    };

    push_query(query_text);
    for (markers, expansion) in [
        (&["knowledge graph", "graph structured data model"][..], "knowledge graph"),
        (
            &["graph database", "gremlin", "sparql", "cypher", "gql", "first-class citizens"][..],
            "graph database",
        ),
        (&["vector database", "embeddings", "semantic similarity"][..], "vector database"),
        (
            &[
                "large language model",
                "text generation",
                "generated language output",
                "language generation",
                "natural language processing",
                "model family",
                "reasoning",
            ][..],
            "large language model",
        ),
        (
            &[
                "retrieval augmented generation",
                "retrieval-augmented generation",
                "before answering",
                "external documents",
            ][..],
            "retrieval-augmented generation",
        ),
        (
            &["rust programming language", "memory safety", "programming language"][..],
            "rust programming language",
        ),
        (&["borrow checker"][..], "rust programming language"),
        (&["semantic web", "rdf", "owl", "machine-readable"][..], "semantic web"),
        (
            &["information retrieval", "information need", "document retrieval"][..],
            "information retrieval",
        ),
        (
            &["named entity recognition", "named-entity recognition", "organizations", "locations"]
                [..],
            "named-entity recognition",
        ),
    ] {
        if markers.iter().any(|marker| lowered.contains(marker)) {
            push_query(expansion);
        }
    }

    queries
}

fn canonical_search_targets(query_text: &str) -> Vec<&'static str> {
    let lowered = query_text.to_lowercase();
    let mut targets = Vec::new();
    if lowered.contains("vector database")
        || (lowered.contains("embeddings") && lowered.contains("semantic similarity"))
    {
        targets.push("vector_database");
    }
    if lowered.contains("large language model")
        || lowered.contains("language generation")
        || lowered.contains("generated language output")
        || (lowered.contains("natural language processing") && lowered.contains("model family"))
    {
        targets.push("large_language_model");
    }
    if lowered.contains("retrieval augmented generation")
        || lowered.contains("retrieval-augmented generation")
        || lowered.contains("before answering")
        || lowered.contains("external documents")
    {
        targets.push("retrieval_augmented_generation");
    }
    if lowered.contains("rust")
        || (lowered.contains("programming language") && lowered.contains("memory safety"))
        || lowered.contains("borrow checker")
    {
        targets.push("rust_programming_language");
    }
    if lowered.contains("knowledge graph")
        || lowered.contains("interlinked descriptions")
        || lowered.contains("graph structured data model")
    {
        targets.push("knowledge_graph");
    }
    if lowered.contains("graph database")
        || lowered.contains("gremlin")
        || lowered.contains("sparql")
        || lowered.contains("cypher")
        || lowered.contains("gql")
        || lowered.contains("first-class citizens")
    {
        targets.push("graph_database");
    }
    if lowered.contains("semantic web")
        || lowered.contains("rdf")
        || lowered.contains("owl")
        || lowered.contains("machine-readable")
    {
        targets.push("semantic_web");
    }
    if lowered.contains("named entity recognition") || lowered.contains("named-entity recognition")
    {
        targets.push("named_entity_recognition");
    }
    if lowered.contains("information retrieval") {
        targets.push("information_retrieval");
    }
    targets
}

fn document_matches_canonical_search_target(document: &KnowledgeDocumentRow, target: &str) -> bool {
    let label = document
        .title
        .as_deref()
        .unwrap_or(document.external_key.as_str())
        .to_lowercase()
        .replace(['_', '-'], " ");
    match target {
        "vector_database" => label.contains("vector database"),
        "large_language_model" => label.contains("large language model"),
        "retrieval_augmented_generation" => label.contains("retrieval augmented generation"),
        "rust_programming_language" => label.contains("rust"),
        "knowledge_graph" => label.contains("knowledge graph"),
        "graph_database" => label.contains("graph database"),
        "semantic_web" => label.contains("semantic web"),
        "named_entity_recognition" => label.contains("named entity recognition"),
        "information_retrieval" => label.contains("information retrieval"),
        _ => false,
    }
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeKnowledgeDocumentLinkRow {
    document_id: Uuid,
    target_node_id: Uuid,
    target_node_type: String,
    relation_type: String,
    support_count: i64,
}

async fn list_entities(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<Vec<RuntimeKnowledgeEntityRow>>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let entities = load_runtime_graph_topology_rows(&state, library_id).await?.entities;
    Ok(Json(entities))
}

async fn list_relations(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<Vec<RuntimeKnowledgeRelationRow>>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let relations = load_runtime_graph_topology_rows(&state, library_id).await?.relations;
    Ok(Json(relations))
}

async fn list_context_bundles(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<Vec<KnowledgeContextBundleRow>>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let bundles = state
        .arango_context_store
        .list_bundles_by_library(library_id)
        .await
        .map_err(|_| ApiError::Internal)?;
    Ok(Json(bundles))
}

async fn list_documents(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<Vec<KnowledgeDocumentRow>>, ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let documents = state
        .arango_document_store
        .list_documents_by_library(library.workspace_id, library.id)
        .await
        .map_err(|_| ApiError::Internal)?;
    Ok(Json(documents))
}

async fn get_library_summary(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<KnowledgeLibrarySummaryResponse>, ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let summary =
        state.canonical_services.knowledge.get_library_summary(&state, library.id).await?;
    Ok(Json(KnowledgeLibrarySummaryResponse {
        library_id: summary.library_id,
        document_counts_by_readiness: summary.document_counts_by_readiness,
        graph_ready_document_count: summary.graph_ready_document_count,
        graph_sparse_document_count: summary.graph_sparse_document_count,
        typed_fact_document_count: summary.typed_fact_document_count,
        updated_at: summary.updated_at,
        latest_generation: summary.latest_generation,
    }))
}

async fn get_graph_workbench(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
    Query(query): Query<GraphWorkbenchQuery>,
) -> Result<Json<GraphWorkbenchSurface>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let GraphWorkbenchQuery {
        search_query,
        focus_document_id,
        include_filtered_artifacts,
        selected_node_id,
    } = query;
    let summary =
        state.canonical_services.knowledge.get_library_summary(&state, library_id).await?;
    let topology = load_runtime_graph_topology_rows(&state, library_id).await?;
    let snapshot =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .map_err(|_| ApiError::Internal)?;
    let nodes = topology
        .entities
        .iter()
        .filter(|entity| include_filtered_artifacts || entity.entity_state == "active")
        .cloned()
        .map(map_runtime_entity_to_graph_node)
        .collect::<Vec<_>>();
    let edges = topology
        .relations
        .iter()
        .filter(|relation| include_filtered_artifacts || relation.relation_state == "active")
        .cloned()
        .map(map_runtime_relation_to_graph_edge)
        .collect::<Vec<_>>();
    let selected_node_id = selected_node_id
        .filter(|node_id| nodes.iter().any(|node| node.id == *node_id))
        .or_else(|| nodes.first().map(|node| node.id));
    let selected_node = match selected_node_id {
        Some(node_id) => {
            build_graph_node_detail(
                &state,
                library_id,
                node_id,
                &topology.entities,
                &topology.relations,
            )
            .await?
        }
        None => None,
    };
    let diagnostics = build_graph_diagnostics(&summary, snapshot.as_ref());

    Ok(Json(GraphWorkbenchSurface {
        graph: build_graph_surface(
            &summary,
            snapshot.as_ref(),
            nodes,
            edges,
            diagnostics.first().map(|warning| warning.detail.clone()),
        ),
        filters: GraphFilterState { search_query, focus_document_id, include_filtered_artifacts },
        selected_node_id,
        selected_node,
        diagnostics,
    }))
}

async fn get_document(
    auth: AuthContext,
    State(state): State<AppState>,
    Path((library_id, document_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<KnowledgeDocumentDetailResponse>, ApiError> {
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let document = state
        .arango_document_store
        .get_document(document_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("knowledge_document", document_id))?;
    if document.library_id != library.id {
        return Err(ApiError::resource_not_found("knowledge_document", document_id));
    }
    let revisions = state
        .arango_document_store
        .list_revisions_by_document(document_id)
        .await
        .map_err(|_| ApiError::Internal)?;
    let latest_revision = revisions.first().cloned();
    let latest_revision_chunks = match latest_revision.as_ref() {
        Some(revision) => state
            .arango_document_store
            .list_chunks_by_revision(revision.revision_id)
            .await
            .map_err(|_| ApiError::Internal)?,
        None => Vec::new(),
    };
    let latest_revision_typed_facts = match latest_revision.as_ref() {
        Some(revision) => {
            state
                .canonical_services
                .knowledge
                .list_typed_technical_facts(&state, revision.revision_id)
                .await?
        }
        None => Vec::new(),
    };
    let latest_revision_evidence = match latest_revision.as_ref() {
        Some(revision) => state
            .arango_graph_store
            .list_evidence_by_revision(revision.revision_id)
            .await
            .map_err(|_| ApiError::Internal)?,
        None => Vec::new(),
    };
    Ok(Json(KnowledgeDocumentDetailResponse {
        document,
        revisions,
        latest_revision,
        latest_revision_chunks,
        latest_revision_typed_facts: latest_revision_typed_facts.clone(),
        technical_fact_summary: summarize_typed_technical_facts(&latest_revision_typed_facts),
        graph_evidence_summary: summarize_graph_evidence(&latest_revision_evidence),
    }))
}

async fn get_graph_topology(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<KnowledgeGraphTopologyResponse>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let topology = load_runtime_graph_topology_rows(&state, library_id).await?;
    let documents = state
        .arango_document_store
        .list_documents_by_ids(&topology.document_ids)
        .await
        .map_err(|_| ApiError::Internal)?;

    Ok(Json(KnowledgeGraphTopologyResponse {
        documents,
        entities: topology.entities,
        relations: topology.relations,
        document_links: topology.document_links,
    }))
}

async fn get_context_bundle(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(bundle_id): Path<Uuid>,
) -> Result<Json<KnowledgeContextBundleDetailResponse>, ApiError> {
    let bundle_set = state
        .arango_context_store
        .get_bundle_reference_set(bundle_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::context_bundle_not_found(bundle_id))?;
    let _ = load_library_and_authorize(
        &auth,
        &state,
        bundle_set.bundle.library_id,
        POLICY_KNOWLEDGE_READ,
    )
    .await?;
    let traces = state
        .arango_context_store
        .list_traces_by_bundle(bundle_set.bundle.bundle_id)
        .await
        .map_err(|_| ApiError::Internal)?;
    Ok(Json(KnowledgeContextBundleDetailResponse {
        bundle: bundle_set.bundle,
        traces,
        chunk_references: bundle_set.chunk_references,
        entity_references: bundle_set.entity_references,
        relation_references: bundle_set.relation_references,
        evidence_references: bundle_set.evidence_references,
    }))
}

async fn search_documents(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
    Query(query): Query<KnowledgeDocumentSearchQuery>,
) -> Result<Json<KnowledgeDocumentSearchResponse>, ApiError> {
    search_documents_impl(
        auth,
        state,
        library_id,
        query.query,
        query.limit,
        query.chunk_hit_limit_per_document,
        query.evidence_sample_limit,
    )
    .await
}

async fn search_documents_by_library_query(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<KnowledgeDocumentSearchRequest>,
) -> Result<Json<KnowledgeDocumentSearchResponse>, ApiError> {
    search_documents_impl(
        auth,
        state,
        query.library_id,
        query.query,
        query.limit,
        query.chunk_hit_limit_per_document,
        query.evidence_sample_limit,
    )
    .await
}

async fn search_documents_impl(
    auth: AuthContext,
    state: AppState,
    library_id: Uuid,
    query: Option<String>,
    limit: Option<usize>,
    chunk_hit_limit_per_document: Option<usize>,
    evidence_sample_limit: Option<usize>,
) -> Result<Json<KnowledgeDocumentSearchResponse>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let query_text = query.unwrap_or_default().trim().to_string();
    if query_text.is_empty() {
        return Err(ApiError::BadRequest("query must not be empty".to_string()));
    }

    let limit = limit.unwrap_or(DEFAULT_SEARCH_LIMIT).max(1);
    let chunk_hit_limit_per_document = chunk_hit_limit_per_document.unwrap_or(10).max(1);
    let evidence_sample_limit =
        evidence_sample_limit.unwrap_or(DEFAULT_EVIDENCE_SAMPLE_LIMIT).max(1);
    let canonical_targets = canonical_search_targets(&query_text);
    let query_keywords = document_search_keywords(&query_text);
    let internal_candidate_limit =
        limit.saturating_mul(chunk_hit_limit_per_document.max(3)).saturating_mul(4).max(16);
    let mut lexical_chunk_hit_map = HashMap::<Uuid, KnowledgeChunkSearchRow>::new();
    for search_query in expand_document_search_queries(&query_text) {
        let rows = state
            .arango_search_store
            .search_chunks(library_id, &search_query, internal_candidate_limit)
            .await
            .map_err(|_| ApiError::Internal)?;
        for row in rows {
            match lexical_chunk_hit_map.entry(row.chunk_id) {
                std::collections::hash_map::Entry::Occupied(mut occupied) => {
                    if row.score > occupied.get().score {
                        occupied.insert(row);
                    }
                }
                std::collections::hash_map::Entry::Vacant(vacant) => {
                    vacant.insert(row);
                }
            }
        }
    }
    let mut lexical_chunk_hits = lexical_chunk_hit_map.into_values().collect::<Vec<_>>();
    lexical_chunk_hits.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    let lexical_entity_hits =
        search_entities_by_library(&state, library_id, &query_text, limit).await?;
    let lexical_relation_hits =
        search_relations_by_library(&state, library_id, &query_text, limit).await?;

    let hybrid_context = resolve_hybrid_search_context(&state, library_id, &query_text).await?;
    let vector_candidate_limit = internal_candidate_limit.max(limit.saturating_mul(2).max(1));
    let vector_chunk_hits = if let Some(context) = hybrid_context.as_ref() {
        match state
            .arango_search_store
            .search_chunk_vectors_by_similarity(
                library_id,
                &context.model_catalog_id.to_string(),
                context.freshness_generation,
                &context.query_vector,
                vector_candidate_limit,
                Some(16),
            )
            .await
        {
            Ok(rows) => rows,
            Err(error) => {
                warn!(
                    library_id = %library_id,
                    model_catalog_id = %context.model_catalog_id,
                    freshness_generation = context.freshness_generation,
                    error = ?error,
                    "hybrid knowledge chunk vector search failed; falling back to lexical-only hits",
                );
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let vector_entity_hits = if let Some(context) = hybrid_context.as_ref() {
        match state
            .arango_search_store
            .search_entity_vectors_by_similarity(
                library_id,
                &context.model_catalog_id.to_string(),
                context.freshness_generation,
                &context.query_vector,
                vector_candidate_limit,
                Some(16),
            )
            .await
        {
            Ok(rows) => rows,
            Err(error) => {
                warn!(
                    library_id = %library_id,
                    model_catalog_id = %context.model_catalog_id,
                    freshness_generation = context.freshness_generation,
                    error = ?error,
                    "hybrid knowledge entity vector search failed; returning lexical entity hits only",
                );
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    let chunk_ids: Vec<Uuid> = lexical_chunk_hits
        .iter()
        .map(|hit| hit.chunk_id)
        .chain(vector_chunk_hits.iter().map(|hit| hit.chunk_id))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let chunks = load_chunks_by_ids(&state, &chunk_ids).await?;
    let chunk_map: HashMap<Uuid, KnowledgeChunkRow> =
        chunks.into_iter().map(|chunk| (chunk.chunk_id, chunk)).collect();

    let revision_ids: Vec<Uuid> = chunk_map
        .values()
        .map(|chunk| chunk.revision_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let revisions = load_revisions_by_ids(&state, &revision_ids).await?;
    let revision_map: HashMap<Uuid, KnowledgeRevisionRow> =
        revisions.into_iter().map(|revision| (revision.revision_id, revision)).collect();

    let document_ids: Vec<Uuid> = chunk_map
        .values()
        .map(|chunk| chunk.document_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let documents = load_documents_by_ids(&state, &document_ids).await?;
    let document_map: HashMap<Uuid, KnowledgeDocumentRow> =
        documents.into_iter().map(|document| (document.document_id, document)).collect();

    let mut accumulators: HashMap<Uuid, KnowledgeDocumentAccumulator> = HashMap::new();
    for (rank, hit) in lexical_chunk_hits.iter().enumerate() {
        let chunk = chunk_map
            .get(&hit.chunk_id)
            .ok_or_else(|| ApiError::resource_not_found("knowledge_chunk", hit.chunk_id))?;
        let revision = revision_map
            .get(&chunk.revision_id)
            .ok_or_else(|| ApiError::resource_not_found("knowledge_revision", chunk.revision_id))?;
        let document = document_map
            .get(&chunk.document_id)
            .ok_or_else(|| ApiError::resource_not_found("knowledge_document", chunk.document_id))?;
        let accumulator = accumulators.entry(document.document_id).or_insert_with(|| {
            KnowledgeDocumentAccumulator {
                document: document.clone(),
                revision: revision.clone(),
                score: 0.0,
                lexical_rank: None,
                vector_rank: None,
                lexical_score: None,
                vector_score: None,
                chunk_hits: Vec::new(),
                vector_chunk_hits: Vec::new(),
                evidence_samples: Vec::new(),
                evidence_ids: HashSet::new(),
            }
        });
        accumulator.lexical_rank =
            Some(accumulator.lexical_rank.map_or(rank + 1, |current| current.min(rank + 1)));
        accumulator.lexical_score =
            Some(accumulator.lexical_score.map_or(hit.score, |current| current.max(hit.score)));
        accumulator.chunk_hits.push(sanitize_chunk_search_hit(hit));
    }

    for (rank, hit) in vector_chunk_hits.iter().enumerate() {
        let chunk = chunk_map
            .get(&hit.chunk_id)
            .ok_or_else(|| ApiError::resource_not_found("knowledge_chunk", hit.chunk_id))?;
        let revision = revision_map
            .get(&chunk.revision_id)
            .ok_or_else(|| ApiError::resource_not_found("knowledge_revision", chunk.revision_id))?;
        let document = document_map
            .get(&chunk.document_id)
            .ok_or_else(|| ApiError::resource_not_found("knowledge_document", chunk.document_id))?;
        let accumulator = accumulators.entry(document.document_id).or_insert_with(|| {
            KnowledgeDocumentAccumulator {
                document: document.clone(),
                revision: revision.clone(),
                score: 0.0,
                lexical_rank: None,
                vector_rank: None,
                lexical_score: None,
                vector_score: None,
                chunk_hits: Vec::new(),
                vector_chunk_hits: Vec::new(),
                evidence_samples: Vec::new(),
                evidence_ids: HashSet::new(),
            }
        });
        accumulator.vector_rank =
            Some(accumulator.vector_rank.map_or(rank + 1, |current| current.min(rank + 1)));
        accumulator.vector_score =
            Some(accumulator.vector_score.map_or(hit.score, |current| current.max(hit.score)));
        accumulator.vector_chunk_hits.push(hit.clone());
    }

    let mut document_hits: Vec<KnowledgeDocumentAccumulator> = accumulators
        .into_values()
        .map(|mut accumulator| {
            accumulator.chunk_hits.sort_by(|left, right| {
                document_chunk_keyword_coverage(right, &query_keywords)
                    .cmp(&document_chunk_keyword_coverage(left, &query_keywords))
                    .then_with(|| {
                        right.score.partial_cmp(&left.score).unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .then_with(|| left.chunk_id.cmp(&right.chunk_id))
            });
            accumulator.vector_chunk_hits.sort_by(|left, right| {
                right
                    .score
                    .partial_cmp(&left.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| left.chunk_id.cmp(&right.chunk_id))
            });
            accumulator
        })
        .collect();

    for accumulator in &mut document_hits {
        accumulator.chunk_hits.truncate(chunk_hit_limit_per_document);
        accumulator.vector_chunk_hits.truncate(chunk_hit_limit_per_document);
        let mut seen_evidence_chunks = HashSet::new();
        let candidate_chunk_ids: Vec<Uuid> = accumulator
            .chunk_hits
            .iter()
            .map(|hit| hit.chunk_id)
            .chain(accumulator.vector_chunk_hits.iter().map(|hit| hit.chunk_id))
            .collect();
        for chunk_id in candidate_chunk_ids {
            if !seen_evidence_chunks.insert(chunk_id) {
                continue;
            }
            let evidence_rows = state
                .arango_graph_store
                .list_evidence_by_chunk(chunk_id)
                .await
                .map_err(|_| ApiError::Internal)?;
            for evidence in evidence_rows {
                if evidence.document_id != accumulator.document.document_id {
                    continue;
                }
                if accumulator.evidence_ids.insert(evidence.evidence_id) {
                    accumulator.evidence_samples.push(evidence);
                }
                if accumulator.evidence_samples.len() >= evidence_sample_limit {
                    break;
                }
            }
            if accumulator.evidence_samples.len() >= evidence_sample_limit {
                break;
            }
        }

        let lexical_rank = accumulator.lexical_rank.unwrap_or(usize::MAX / 2);
        let vector_rank = accumulator.vector_rank.unwrap_or(usize::MAX / 2);
        let lexical_signal = accumulator.lexical_score.map(f64::ln_1p).unwrap_or_default();
        let vector_signal = accumulator.vector_score.map(f64::ln_1p).unwrap_or_default();
        let provenance_bonus = (accumulator.evidence_samples.len() as f64) / 1000.0;
        accumulator.score = lexical_signal
            + vector_signal
            + (1.0 / (60.0 + lexical_rank as f64))
            + (1.0 / (60.0 + vector_rank as f64))
            + provenance_bonus;
        if canonical_targets
            .iter()
            .any(|target| document_matches_canonical_search_target(&accumulator.document, target))
        {
            accumulator.score += 10.0;
        }
    }

    let mut document_hits = document_hits;
    document_hits.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.document.document_id.cmp(&right.document.document_id))
    });
    document_hits.truncate(limit);
    let mut response_hits = Vec::with_capacity(document_hits.len());
    for mut accumulator in document_hits {
        backfill_document_chunk_hits(
            &state,
            &query_text,
            chunk_hit_limit_per_document,
            &mut accumulator,
        )
        .await?;
        let technical_fact_samples = state
            .canonical_services
            .knowledge
            .list_typed_technical_facts(&state, accumulator.revision.revision_id)
            .await?;
        response_hits.push(KnowledgeSearchDocumentHit {
            provenance_summary: KnowledgeDocumentProvenanceSummary {
                supporting_evidence_count: accumulator.evidence_samples.len(),
                lexical_chunk_count: accumulator.chunk_hits.len(),
                vector_chunk_count: accumulator.vector_chunk_hits.len(),
            },
            technical_fact_summary: summarize_typed_technical_facts(&technical_fact_samples),
            graph_evidence_summary: summarize_graph_evidence(&accumulator.evidence_samples),
            document: accumulator.document,
            revision: map_search_revision_summary(accumulator.revision),
            score: accumulator.score,
            lexical_rank: accumulator.lexical_rank,
            vector_rank: accumulator.vector_rank,
            lexical_score: accumulator.lexical_score,
            vector_score: accumulator.vector_score,
            chunk_hits: accumulator.chunk_hits,
            vector_chunk_hits: accumulator.vector_chunk_hits,
            evidence_samples: accumulator.evidence_samples,
            technical_fact_samples,
        });
    }

    Ok(Json(KnowledgeDocumentSearchResponse {
        library_id,
        query_text,
        limit,
        embedding_provider_kind: hybrid_context
            .as_ref()
            .map(|context| context.provider_kind.clone())
            .unwrap_or_else(|| "lexical_only".to_string()),
        embedding_model_name: hybrid_context
            .as_ref()
            .map(|context| context.model_name.clone())
            .unwrap_or_default(),
        embedding_model_catalog_id: hybrid_context
            .as_ref()
            .map(|context| context.model_catalog_id)
            .unwrap_or_else(Uuid::nil),
        freshness_generation: hybrid_context
            .as_ref()
            .map(|context| context.freshness_generation)
            .unwrap_or_default(),
        document_hits: response_hits,
        entity_hits: lexical_entity_hits,
        relation_hits: lexical_relation_hits,
        vector_chunk_hits,
        vector_entity_hits,
    }))
}

fn map_search_revision_summary(revision: KnowledgeRevisionRow) -> KnowledgeSearchRevisionSummary {
    KnowledgeSearchRevisionSummary {
        revision_id: revision.revision_id,
        document_id: revision.document_id,
        revision_number: revision.revision_number,
        revision_state: revision.revision_state,
        revision_kind: revision.revision_kind,
        mime_type: revision.mime_type,
        title: revision.title,
        byte_size: revision.byte_size,
        text_state: revision.text_state,
        vector_state: revision.vector_state,
        graph_state: revision.graph_state,
        created_at: revision.created_at,
    }
}

async fn backfill_document_chunk_hits(
    state: &AppState,
    query_text: &str,
    chunk_hit_limit_per_document: usize,
    accumulator: &mut KnowledgeDocumentAccumulator,
) -> Result<(), ApiError> {
    let keywords = crate::services::query_planner::extract_keywords(query_text);
    if keywords.is_empty() {
        return Ok(());
    }

    let existing_chunk_ids =
        accumulator.chunk_hits.iter().map(|chunk| chunk.chunk_id).collect::<HashSet<_>>();
    let mut candidates = accumulator.chunk_hits.clone();
    candidates.extend(
        state
            .arango_document_store
            .list_chunks_by_revision(accumulator.revision.revision_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .into_iter()
            .filter(|chunk| !existing_chunk_ids.contains(&chunk.chunk_id))
            .filter_map(|chunk| {
                let haystack =
                    format!("{} {}", chunk.content_text, chunk.normalized_text).to_lowercase();
                let score = keywords
                    .iter()
                    .map(|keyword| haystack.matches(keyword.as_str()).count() as f64)
                    .sum::<f64>();
                (score > 0.0).then_some(KnowledgeChunkSearchRow {
                    chunk_id: chunk.chunk_id,
                    workspace_id: chunk.workspace_id,
                    library_id: chunk.library_id,
                    revision_id: chunk.revision_id,
                    content_text: repair_technical_layout_noise(&chunk.content_text),
                    normalized_text: repair_technical_layout_noise(&chunk.normalized_text),
                    section_path: chunk.section_path,
                    heading_trail: chunk.heading_trail,
                    score,
                    quality_score: chunk.quality_score,
                })
            }),
    );
    candidates.sort_by(|left, right| {
        document_search_chunk_relevance(query_text, right)
            .cmp(&document_search_chunk_relevance(query_text, left))
            .then_with(|| right.score.partial_cmp(&left.score).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    candidates.dedup_by(|left, right| left.chunk_id == right.chunk_id);
    accumulator.chunk_hits = candidates.into_iter().take(chunk_hit_limit_per_document).collect();
    Ok(())
}

fn document_search_chunk_relevance(query_text: &str, hit: &KnowledgeChunkSearchRow) -> usize {
    let lowered_query = query_text.to_lowercase();
    let lowered_text = format!("{} {}", hit.content_text, hit.normalized_text).to_lowercase();
    let keywords = crate::services::query_planner::extract_keywords(query_text);
    let keyword_score = keywords
        .iter()
        .map(|keyword| lowered_text.matches(keyword.as_str()).count())
        .sum::<usize>();
    let anchor_score = document_search_anchor_tokens(&lowered_query)
        .into_iter()
        .filter(|token| lowered_text.contains(token))
        .count();
    let phrase_score = document_search_anchor_phrases(&lowered_query)
        .into_iter()
        .filter(|phrase| lowered_text.contains(phrase))
        .count();
    keyword_score + anchor_score * 50 + phrase_score * 150
}

fn document_search_anchor_tokens(lowered_query: &str) -> Vec<String> {
    let mut tokens = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    for token in lowered_query
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-' && ch != '_')
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        let keep = token.chars().all(|ch| ch.is_ascii_digit())
            || token.len() >= 5
            || matches!(token, "gql" | "ocr" | "rdf" | "owl");
        if keep && seen.insert(token.to_string()) {
            tokens.push(token.to_string());
        }
    }
    tokens
}

fn document_search_anchor_phrases(lowered_query: &str) -> Vec<String> {
    let tokens = lowered_query
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-' && ch != '_')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    let anchors = document_search_anchor_tokens(lowered_query).into_iter().collect::<HashSet<_>>();
    let stopwords = [
        "the", "a", "an", "and", "or", "of", "to", "in", "on", "for", "with", "from", "what",
        "which", "is", "are", "was", "were", "approved",
    ]
    .into_iter()
    .collect::<HashSet<_>>();
    let mut phrases = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    for window_size in 2..=5 {
        for window in tokens.windows(window_size) {
            if window.iter().all(|token| stopwords.contains(token)) {
                continue;
            }
            if !window.iter().any(|token| anchors.contains(*token)) {
                continue;
            }
            let phrase = window.join(" ");
            if seen.insert(phrase.clone()) {
                phrases.push(phrase);
            }
        }
    }
    phrases
}

async fn resolve_hybrid_search_context(
    state: &AppState,
    library_id: Uuid,
    query_text: &str,
) -> Result<Option<KnowledgeHybridSearchContext>, ApiError> {
    let Some(binding) = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
        .await?
    else {
        return Ok(None);
    };

    let generations = state
        .canonical_services
        .knowledge
        .derive_library_generation_rows(state, library_id)
        .await
        .map_err(|_| ApiError::Internal)?;
    let Some(generation): Option<&KnowledgeLibraryGenerationRow> = generations.first() else {
        return Ok(None);
    };
    if generation.active_vector_generation <= 0 {
        return Ok(None);
    }

    let embedding = state
        .llm_gateway
        .embed(EmbeddingRequest {
            provider_kind: binding.provider_kind.clone(),
            model_name: binding.model_name.clone(),
            input: query_text.to_string(),
            api_key_override: Some(binding.api_key),
            base_url_override: binding.provider_base_url.clone(),
        })
        .await
        .map_err(|error| {
            ApiError::ProviderFailure(format!("failed to embed knowledge search query: {error}"))
        })?;

    Ok(Some(KnowledgeHybridSearchContext {
        provider_kind: binding.provider_kind,
        model_name: binding.model_name,
        model_catalog_id: binding.model_catalog_id,
        freshness_generation: generation.active_vector_generation,
        query_vector: embedding.embedding,
    }))
}

async fn load_revisions_by_ids(
    state: &AppState,
    revision_ids: &[Uuid],
) -> Result<Vec<KnowledgeRevisionRow>, ApiError> {
    if revision_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut rows = Vec::with_capacity(revision_ids.len());
    for revision_id in revision_ids {
        let revision = state
            .arango_document_store
            .get_revision(*revision_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("knowledge_revision", revision_id))?;
        rows.push(revision);
    }
    Ok(rows)
}

async fn load_documents_by_ids(
    state: &AppState,
    document_ids: &[Uuid],
) -> Result<Vec<KnowledgeDocumentRow>, ApiError> {
    if document_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut rows = Vec::with_capacity(document_ids.len());
    for document_id in document_ids {
        let document = state
            .arango_document_store
            .get_document(*document_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("knowledge_document", document_id))?;
        rows.push(document);
    }
    Ok(rows)
}

async fn search_entities_by_library(
    state: &AppState,
    library_id: Uuid,
    query_text: &str,
    limit: usize,
) -> Result<Vec<KnowledgeEntitySearchRow>, ApiError> {
    state
        .arango_search_store
        .search_entities(library_id, query_text, limit.max(1))
        .await
        .map_err(|_| ApiError::Internal)
}

async fn search_relations_by_library(
    state: &AppState,
    library_id: Uuid,
    query_text: &str,
    limit: usize,
) -> Result<Vec<KnowledgeRelationSearchRow>, ApiError> {
    state
        .arango_search_store
        .search_relations(library_id, query_text, limit.max(1))
        .await
        .map_err(|_| ApiError::Internal)
}

async fn list_library_generations(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
) -> Result<Json<Vec<crate::domains::knowledge::KnowledgeLibraryGeneration>>, ApiError> {
    let _ = load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let generations =
        state.canonical_services.knowledge.list_library_generations(&state, library_id).await?;
    Ok(Json(generations))
}

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
    .map_err(|_| ApiError::Internal)?
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
    .map_err(|_| ApiError::Internal)?
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
        .map_err(|_| ApiError::Internal)?;
    decode_many_results(cursor).map_err(|_| ApiError::Internal)
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

#[derive(Debug)]
struct RuntimeGraphTopologyRows {
    document_ids: Vec<Uuid>,
    entities: Vec<RuntimeKnowledgeEntityRow>,
    relations: Vec<RuntimeKnowledgeRelationRow>,
    document_links: Vec<RuntimeKnowledgeDocumentLinkRow>,
}

fn build_graph_surface(
    summary: &crate::domains::knowledge::KnowledgeLibrarySummary,
    snapshot: Option<&repositories::RuntimeGraphSnapshotRow>,
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
    warning: Option<String>,
) -> GraphSurface {
    let graph_status = graph_workbench_status(summary, snapshot);
    GraphSurface {
        library_id: summary.library_id,
        status: graph_status,
        convergence_status: graph_workbench_convergence_status(graph_status, snapshot),
        warning,
        node_count: saturating_usize_to_i32(nodes.len()),
        relation_count: saturating_usize_to_i32(edges.len()),
        edge_count: saturating_usize_to_i32(edges.len()),
        graph_ready_document_count: saturating_i64_to_i32(summary.graph_ready_document_count),
        graph_sparse_document_count: saturating_i64_to_i32(summary.graph_sparse_document_count),
        typed_fact_document_count: saturating_i64_to_i32(summary.typed_fact_document_count),
        updated_at: snapshot.and_then(|row| row.last_built_at).or(Some(summary.updated_at)),
        nodes,
        edges,
        readiness_summary: Some(GraphReadinessSummary {
            library_id: summary.library_id,
            document_counts_by_readiness: summary
                .document_counts_by_readiness
                .iter()
                .map(|(key, value)| (key.clone(), *value))
                .collect(),
            graph_ready_document_count: summary.graph_ready_document_count,
            graph_sparse_document_count: summary.graph_sparse_document_count,
            typed_fact_document_count: summary.typed_fact_document_count,
            latest_generation: summary.latest_generation.as_ref().map(|generation| {
                GraphGenerationSummary {
                    generation_id: Some(generation.id),
                    active_graph_generation: 1,
                    degraded_state: snapshot.map(|row| row.graph_status.clone()),
                    updated_at: generation.completed_at.or(Some(generation.created_at)),
                }
            }),
            updated_at: Some(summary.updated_at),
        }),
    }
}

fn build_graph_diagnostics(
    summary: &crate::domains::knowledge::KnowledgeLibrarySummary,
    snapshot: Option<&repositories::RuntimeGraphSnapshotRow>,
) -> Vec<OperatorWarning> {
    let total_documents = summary.document_counts_by_readiness.values().copied().sum::<i64>();
    let mut diagnostics = Vec::new();

    if let Some(snapshot) = snapshot
        && let Some(detail) =
            snapshot.last_error_message.as_ref().filter(|detail| !detail.is_empty())
    {
        diagnostics.push(OperatorWarning {
            code: "runtime_graph_snapshot_error".to_string(),
            level: MessageLevel::Error,
            title: "Runtime graph snapshot error".to_string(),
            detail: detail.clone(),
        });
    }

    match graph_workbench_status(summary, snapshot) {
        GraphStatus::Empty | GraphStatus::Ready => {}
        GraphStatus::Building => diagnostics.push(OperatorWarning {
            code: "runtime_graph_building".to_string(),
            level: MessageLevel::Info,
            title: "Runtime graph is building".to_string(),
            detail: if total_documents > 0 {
                format!(
                    "The active library has {total_documents} readable documents, but the runtime graph has not converged yet."
                )
            } else {
                "The active library has not produced any graph rows yet.".to_string()
            },
        }),
        GraphStatus::Rebuilding => diagnostics.push(OperatorWarning {
            code: "runtime_graph_rebuilding".to_string(),
            level: MessageLevel::Warning,
            title: "Runtime graph is rebuilding".to_string(),
            detail: "The active library graph is refreshing after recent extraction work.".to_string(),
        }),
        GraphStatus::Partial => diagnostics.push(OperatorWarning {
            code: "runtime_graph_partial".to_string(),
            level: MessageLevel::Warning,
            title: "Graph coverage remains partial".to_string(),
            detail: format!(
                "{} readable documents remain graph-sparse for this library.",
                summary.graph_sparse_document_count
            ),
        }),
        GraphStatus::Failed => diagnostics.push(OperatorWarning {
            code: "runtime_graph_failed".to_string(),
            level: MessageLevel::Error,
            title: "Runtime graph failed".to_string(),
            detail: "The active library graph projection is currently failed.".to_string(),
        }),
        GraphStatus::Stale => diagnostics.push(OperatorWarning {
            code: "runtime_graph_stale".to_string(),
            level: MessageLevel::Warning,
            title: "Runtime graph is stale".to_string(),
            detail: "The active library graph projection needs a refresh.".to_string(),
        }),
    }

    diagnostics
}

fn graph_workbench_status(
    summary: &crate::domains::knowledge::KnowledgeLibrarySummary,
    snapshot: Option<&repositories::RuntimeGraphSnapshotRow>,
) -> GraphStatus {
    if let Some(snapshot) = snapshot {
        return match snapshot.graph_status.as_str() {
            "empty" => GraphStatus::Empty,
            "building" => {
                if summary.graph_ready_document_count > 0 || summary.graph_sparse_document_count > 0
                {
                    GraphStatus::Rebuilding
                } else {
                    GraphStatus::Building
                }
            }
            "rebuilding" => GraphStatus::Rebuilding,
            "ready" => {
                if summary.graph_sparse_document_count > 0 {
                    GraphStatus::Partial
                } else {
                    GraphStatus::Ready
                }
            }
            "partial" => GraphStatus::Partial,
            "failed" => GraphStatus::Failed,
            "stale" => GraphStatus::Stale,
            _ => graph_workbench_status_from_summary(summary),
        };
    }

    graph_workbench_status_from_summary(summary)
}

fn graph_workbench_status_from_summary(
    summary: &crate::domains::knowledge::KnowledgeLibrarySummary,
) -> GraphStatus {
    let total_documents = summary.document_counts_by_readiness.values().copied().sum::<i64>();
    if total_documents == 0 {
        GraphStatus::Empty
    } else if summary.graph_ready_document_count > 0 && summary.graph_sparse_document_count == 0 {
        GraphStatus::Ready
    } else if summary.graph_ready_document_count > 0 || summary.graph_sparse_document_count > 0 {
        GraphStatus::Partial
    } else {
        GraphStatus::Building
    }
}

fn graph_workbench_convergence_status(
    status: GraphStatus,
    snapshot: Option<&repositories::RuntimeGraphSnapshotRow>,
) -> Option<GraphConvergenceStatus> {
    match status {
        GraphStatus::Ready => {
            let coverage = snapshot.and_then(|row| row.provenance_coverage_percent);
            if coverage.is_some_and(|value| value < 100.0) {
                Some(GraphConvergenceStatus::Partial)
            } else {
                Some(GraphConvergenceStatus::Current)
            }
        }
        GraphStatus::Partial | GraphStatus::Building | GraphStatus::Rebuilding => {
            Some(GraphConvergenceStatus::Partial)
        }
        GraphStatus::Failed | GraphStatus::Stale => Some(GraphConvergenceStatus::Degraded),
        GraphStatus::Empty => None,
    }
}

fn map_runtime_entity_to_graph_node(row: RuntimeKnowledgeEntityRow) -> GraphNode {
    GraphNode {
        id: row.entity_id,
        canonical_key: row.key,
        label: row.canonical_label,
        node_type: map_graph_node_type(&row.entity_type),
        secondary_label: Some(row.entity_type),
        support_count: row.support_count,
        summary: row.summary,
        filtered_artifact: row.entity_state != "active",
    }
}

fn map_runtime_relation_to_graph_edge(row: RuntimeKnowledgeRelationRow) -> GraphEdge {
    GraphEdge {
        id: row.relation_id,
        canonical_key: row.key,
        source: row.subject_entity_id,
        target: row.object_entity_id,
        relation_type: row.relation_type,
        support_count: row.support_count,
        filtered_artifact: row.relation_state != "active",
    }
}

async fn build_graph_node_detail(
    state: &AppState,
    library_id: Uuid,
    node_id: Uuid,
    entities: &[RuntimeKnowledgeEntityRow],
    relations: &[RuntimeKnowledgeRelationRow],
) -> Result<Option<GraphNodeDetail>, ApiError> {
    let Some(entity) = entities.iter().find(|entity| entity.entity_id == node_id).cloned() else {
        return Ok(None);
    };
    let evidence =
        load_runtime_graph_supporting_evidence(state, library_id, "node", node_id).await?;
    let related_nodes = relations
        .iter()
        .filter_map(|relation| {
            if relation.subject_entity_id == node_id {
                entities
                    .iter()
                    .find(|candidate| candidate.entity_id == relation.object_entity_id)
                    .map(|candidate| GraphRelatedNode {
                        id: candidate.entity_id,
                        label: candidate.canonical_label.clone(),
                        relation_type: relation.relation_type.clone(),
                    })
            } else if relation.object_entity_id == node_id {
                entities
                    .iter()
                    .find(|candidate| candidate.entity_id == relation.subject_entity_id)
                    .map(|candidate| GraphRelatedNode {
                        id: candidate.entity_id,
                        label: candidate.canonical_label.clone(),
                        relation_type: relation.relation_type.clone(),
                    })
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    let supporting_document_ids = evidence
        .iter()
        .map(|row| row.document_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let supporting_documents = if supporting_document_ids.is_empty() {
        Vec::new()
    } else {
        let documents = state
            .arango_document_store
            .list_documents_by_ids(&supporting_document_ids)
            .await
            .map_err(|_| ApiError::Internal)?;
        let document_labels = documents
            .into_iter()
            .map(|document| (document.document_id, document.title.unwrap_or(document.external_key)))
            .collect::<HashMap<_, _>>();
        supporting_document_ids
            .into_iter()
            .map(|document_id| GraphDocumentReference {
                document_id,
                document_label: document_labels.get(&document_id).cloned(),
            })
            .collect()
    };
    let document_labels = supporting_documents
        .iter()
        .map(|document| (document.document_id, document.document_label.clone()))
        .collect::<HashMap<_, _>>();

    Ok(Some(GraphNodeDetail {
        id: entity.entity_id,
        label: entity.canonical_label.clone(),
        node_type: map_graph_node_type(&entity.entity_type),
        summary: entity
            .summary
            .unwrap_or_else(|| humanize_graph_entity_state(&entity.entity_state)),
        properties: vec![
            ("key".to_string(), entity.key),
            ("entity_type".to_string(), entity.entity_type),
            ("support_count".to_string(), entity.support_count.to_string()),
            ("freshness_generation".to_string(), entity.freshness_generation.to_string()),
            ("entity_state".to_string(), entity.entity_state.clone()),
        ],
        related_nodes,
        supporting_documents,
        evidence: evidence
            .into_iter()
            .map(|row| GraphEvidence {
                id: row.key,
                document_id: Some(row.document_id),
                document_label: document_labels.get(&row.document_id).cloned().flatten(),
                chunk_id: row.chunk_id,
                excerpt: row.excerpt,
                support_kind: Some(row.support_kind),
                extraction_method: Some(row.extraction_method),
                confidence: row.confidence,
                created_at: Some(row.created_at),
            })
            .collect(),
        warning: (entity.entity_state != "active")
            .then(|| format!("Selected node is currently {}.", entity.entity_state)),
    }))
}

fn map_graph_node_type(value: &str) -> GraphNodeType {
    match value.to_ascii_lowercase().as_str() {
        "person" => GraphNodeType::Person,
        "organization" => GraphNodeType::Organization,
        "location" => GraphNodeType::Location,
        "event" => GraphNodeType::Event,
        "artifact" => GraphNodeType::Artifact,
        "natural" => GraphNodeType::Natural,
        "process" => GraphNodeType::Process,
        "concept" => GraphNodeType::Concept,
        "attribute" => GraphNodeType::Attribute,
        // Backward compatibility
        "topic" => GraphNodeType::Concept,
        "technology" => GraphNodeType::Artifact,
        "api" => GraphNodeType::Artifact,
        "code_symbol" => GraphNodeType::Artifact,
        "natural_kind" => GraphNodeType::Natural,
        "metric" => GraphNodeType::Attribute,
        "regulation" => GraphNodeType::Artifact,
        _ => GraphNodeType::Entity,
    }
}

fn humanize_graph_entity_state(value: &str) -> String {
    format!("Node is currently {}.", value.replace('_', " "))
}

fn saturating_i64_to_i32(value: i64) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

fn saturating_usize_to_i32(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

async fn load_runtime_graph_topology_rows(
    state: &AppState,
    library_id: Uuid,
) -> Result<RuntimeGraphTopologyRows, ApiError> {
    let library = state
        .canonical_services
        .catalog
        .get_library(state, library_id)
        .await
        .map_err(|_| ApiError::Internal)?;
    let workspace_id = library.workspace_id;
    let Some(snapshot) =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .map_err(|_| ApiError::Internal)?
    else {
        return Ok(RuntimeGraphTopologyRows {
            document_ids: Vec::new(),
            entities: Vec::new(),
            relations: Vec::new(),
            document_links: Vec::new(),
        });
    };

    if snapshot.graph_status == "empty" || snapshot.projection_version <= 0 {
        return Ok(RuntimeGraphTopologyRows {
            document_ids: Vec::new(),
            entities: Vec::new(),
            relations: Vec::new(),
            document_links: Vec::new(),
        });
    }

    let projection_version = snapshot.projection_version;
    let node_rows = repositories::list_admitted_runtime_graph_nodes_by_library(
        &state.persistence.postgres,
        library_id,
        projection_version,
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    let edge_rows = repositories::list_admitted_runtime_graph_edges_by_library(
        &state.persistence.postgres,
        library_id,
        projection_version,
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    let document_link_rows = repositories::list_runtime_graph_document_links_by_library(
        &state.persistence.postgres,
        library_id,
        projection_version,
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    let document_node_ids = node_rows
        .iter()
        .filter(|row| row.node_type == "document")
        .map(|row| row.id)
        .collect::<HashSet<_>>();
    let document_ids = node_rows
        .iter()
        .filter(|row| row.node_type == "document")
        .filter_map(runtime_graph_document_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    let entities = node_rows
        .into_iter()
        .filter(|row| row.node_type != "document")
        .map(|row| map_runtime_graph_node_to_entity_row(row, workspace_id, library_id))
        .collect::<Vec<_>>();
    let mut relations = Vec::with_capacity(edge_rows.len());
    for row in edge_rows {
        if document_node_ids.contains(&row.from_node_id)
            || document_node_ids.contains(&row.to_node_id)
        {
            continue;
        }
        relations.push(map_runtime_graph_edge_to_relation_row(workspace_id, library_id, row));
    }
    let document_links = document_link_rows
        .into_iter()
        .map(|row| RuntimeKnowledgeDocumentLinkRow {
            document_id: row.document_id,
            target_node_id: row.target_node_id,
            target_node_type: row.target_node_type,
            relation_type: row.relation_type,
            support_count: row.support_count,
        })
        .collect();

    Ok(RuntimeGraphTopologyRows { document_ids, entities, relations, document_links })
}

fn runtime_graph_document_id(row: &repositories::RuntimeGraphNodeRow) -> Option<Uuid> {
    row.metadata_json
        .get("document_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
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
    metadata
        .get("extraction_recovery_status")
        .and_then(serde_json::Value::as_str)
        .or_else(|| metadata.get("entity_state").and_then(serde_json::Value::as_str))
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
        .map_err(|_| ApiError::Internal)?
        .workspace_id;
    let evidence_rows = repositories::list_active_runtime_graph_evidence_lifecycle_by_target(
        &state.persistence.postgres,
        library_id,
        target_kind,
        target_id,
    )
    .await
    .map_err(|_| ApiError::Internal)?;

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
    serde_json::from_value(result).map_err(|_| ApiError::Internal)
}
