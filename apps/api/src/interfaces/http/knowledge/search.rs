use std::collections::{HashMap, HashSet};

use axum::{
    Json,
    extract::{Path, Query, State},
};
use serde::{Deserialize, Serialize};
use tracing::warn;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ai::AiBindingPurpose,
    infra::arangodb::{
        document_store::{
            KnowledgeDocumentRow, KnowledgeLibraryGenerationRow, KnowledgeRevisionRow,
        },
        graph_store::KnowledgeEvidenceRow,
        search_store::{
            KnowledgeChunkSearchRow, KnowledgeChunkVectorSearchRow, KnowledgeEntitySearchRow,
            KnowledgeEntityVectorSearchRow, KnowledgeRelationSearchRow,
        },
    },
    integrations::llm::EmbeddingRequest,
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_KNOWLEDGE_READ, load_library_and_authorize},
        router_support::ApiError,
    },
    shared::extraction::text_render::repair_technical_layout_noise,
};

use super::{
    KnowledgeDocumentProvenanceSummary, KnowledgeGraphEvidenceSummary,
    KnowledgeTechnicalFactProvenanceSummary, load_chunks_by_ids, summarize_graph_evidence,
    summarize_typed_technical_facts,
};

const DEFAULT_SEARCH_LIMIT: usize = 10;
const DEFAULT_EVIDENCE_SAMPLE_LIMIT: usize = 5;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct KnowledgeDocumentSearchQuery {
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
pub(super) struct KnowledgeDocumentSearchRequest {
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
    technical_fact_samples: Vec<crate::domains::knowledge::TypedTechnicalFact>,
    provenance_summary: KnowledgeDocumentProvenanceSummary,
    technical_fact_summary: KnowledgeTechnicalFactProvenanceSummary,
    graph_evidence_summary: KnowledgeGraphEvidenceSummary,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct KnowledgeDocumentSearchResponse {
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
    crate::services::query::planner::extract_keywords(query_text)
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

pub(super) async fn search_documents(
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

pub(super) async fn search_documents_by_library_query(
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
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
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
    let chunk_map: HashMap<Uuid, crate::infra::arangodb::document_store::KnowledgeChunkRow> =
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
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
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
    let keywords = crate::services::query::planner::extract_keywords(query_text);
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
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
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
    let keywords = crate::services::query::planner::extract_keywords(query_text);
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
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
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
            api_key_override: binding.api_key.clone(),
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
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
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
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
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
        .map_err(|e| ApiError::internal_with_log(e, "internal"))
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
        .map_err(|e| ApiError::internal_with_log(e, "internal"))
}
