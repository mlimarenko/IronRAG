use std::collections::{HashMap, HashSet};

use axum::{
    Json,
    extract::{Path, Query, State},
};
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use tracing::warn;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ai::AiBindingPurpose,
    infra::arangodb::{
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
    integrations::llm::EmbeddingRequest,
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_KNOWLEDGE_READ, load_library_and_authorize},
        router_support::ApiError,
    },
    services::query::vector_dimensions::library_vector_index_dimensions,
    shared::{
        extraction::text_render::repair_technical_layout_noise,
        text_tokens::normalized_alnum_token_sequence_by,
    },
};

use super::{
    KnowledgeDocumentProvenanceSummary, KnowledgeGraphEvidenceSummary,
    KnowledgeTechnicalFactProvenanceSummary, load_chunks_by_ids, summarize_graph_evidence,
    summarize_typed_technical_facts,
};

const DEFAULT_SEARCH_LIMIT: usize = 10;
const DEFAULT_EVIDENCE_SAMPLE_LIMIT: usize = 5;
const SEARCH_LEXICAL_QUERY_PARALLELISM: usize = 4;
const SEARCH_CHUNK_EVIDENCE_PARALLELISM: usize = 4;
const SEARCH_DOCUMENT_ENRICHMENT_PARALLELISM: usize = 4;
const SEARCH_DOCUMENT_BACKFILL_MAX_CANDIDATES: usize = 32;
const DOCUMENT_KEYWORD_COVERAGE_BONUS_WEIGHT: f64 = 0.8;
const DOCUMENT_KEYWORD_AGGREGATE_COVERAGE_BONUS_WEIGHT: f64 = 0.25;
const DOCUMENT_IDENTITY_TOKEN_BONUS_WEIGHT: f64 = 0.25;
const DOCUMENT_IDENTITY_PHRASE_BONUS_WEIGHT: f64 = 0.75;
const DOCUMENT_IDENTITY_EXACT_SOURCE_PHRASE_BONUS_WEIGHT: f64 = 18.0;
const DOCUMENT_ANCHOR_TOKEN_BONUS_WEIGHT: f64 = 1.8;
const DOCUMENT_ANCHOR_SURFACE_STRONG_BONUS_WEIGHT: f64 = 48.0;
const DOCUMENT_ANCHOR_SURFACE_WEAK_BONUS_WEIGHT: f64 = 0.75;
const DOCUMENT_ANCHOR_MATCH_BONUS_MAX: f64 = 48.0;
const DOCUMENT_ANCHOR_MULTI_SURFACE_EXPONENT: i32 = 6;
const DOCUMENT_VECTOR_RAW_SCORE_WEIGHT: f64 = 0.75;
const DOCUMENT_BEST_CHUNK_KEYWORD_COVERAGE_WEIGHT: f64 = 1.25;
const DOCUMENT_BEST_CHUNK_ANCHOR_COVERAGE_WEIGHT: f64 = 0.75;
const DOCUMENT_SOFT_TITLE_SCORE_MIN: f64 = 45.0;
const DOCUMENT_SOFT_TITLE_SCORE_MAX: f64 = 50.5;
const DOCUMENT_SOFT_TITLE_LOW_EVIDENCE_MIN_SCALE: f64 = 0.25;
const DOCUMENT_SOFT_TITLE_IDENTITY_PRESERVE_THRESHOLD: f64 = 1.0;

#[derive(Debug, Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct KnowledgeDocumentSearchQuery {
    #[serde(alias = "q")]
    query: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    chunk_hit_limit_per_document: Option<usize>,
    #[serde(default)]
    evidence_sample_limit: Option<usize>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct KnowledgeDocumentSearchRequest {
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

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeSearchRevisionSummary {
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

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeSearchDocumentHit {
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

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeDocumentSearchResponse {
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
    text.to_string()
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

fn document_keyword_coverage_bonus(
    chunk_hits: &[KnowledgeChunkSearchRow],
    keywords: &[String],
) -> f64 {
    if chunk_hits.is_empty() || keywords.is_empty() {
        return 0.0;
    }
    let denominator = keywords.len() as f64;
    let best_chunk_coverage = chunk_hits
        .iter()
        .map(|hit| document_chunk_keyword_coverage(hit, keywords))
        .max()
        .unwrap_or_default() as f64;
    let aggregate_haystack = chunk_hits
        .iter()
        .take(3)
        .map(|hit| format!("{}\n{}", hit.content_text, hit.normalized_text))
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    let aggregate_coverage =
        keywords.iter().filter(|keyword| aggregate_haystack.contains(keyword.as_str())).count()
            as f64;
    let best_signal = (best_chunk_coverage / denominator).min(1.0);
    let aggregate_signal = (aggregate_coverage / denominator).min(1.0);
    (best_signal * DOCUMENT_KEYWORD_COVERAGE_BONUS_WEIGHT)
        + (aggregate_signal * DOCUMENT_KEYWORD_AGGREGATE_COVERAGE_BONUS_WEIGHT)
}

fn document_anchor_match_bonus(chunk_hits: &[KnowledgeChunkSearchRow], query_text: &str) -> f64 {
    if chunk_hits.is_empty() {
        return 0.0;
    }
    let token_anchors = document_search_anchor_tokens(query_text);
    let surface_anchors = document_search_exact_anchor_surfaces(query_text);
    if token_anchors.is_empty() && surface_anchors.is_empty() {
        return 0.0;
    }
    let haystack = chunk_hits
        .iter()
        .map(|hit| format!("{}\n{}", hit.content_text, hit.normalized_text))
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();

    let token_bonus = if token_anchors.is_empty() {
        0.0
    } else {
        let matches =
            token_anchors.iter().filter(|anchor| haystack.contains(anchor.as_str())).count() as f64;
        (matches / token_anchors.len() as f64).min(1.0) * DOCUMENT_ANCHOR_TOKEN_BONUS_WEIGHT
    };
    let surface_bonus = document_anchor_surface_coverage_bonus(&surface_anchors, &haystack);
    (token_bonus + surface_bonus).min(DOCUMENT_ANCHOR_MATCH_BONUS_MAX)
}

fn document_anchor_surface_coverage_bonus(surface_anchors: &[String], haystack: &str) -> f64 {
    if surface_anchors.is_empty() {
        return 0.0;
    }
    let total_weight = surface_anchors
        .iter()
        .map(|anchor| document_anchor_surface_bonus_weight(anchor))
        .sum::<f64>();
    if total_weight <= 0.0 {
        return 0.0;
    }
    let matched_weight = surface_anchors
        .iter()
        .filter(|anchor| haystack.contains(anchor.as_str()))
        .map(|anchor| document_anchor_surface_bonus_weight(anchor))
        .sum::<f64>();
    if total_weight > DOCUMENT_ANCHOR_SURFACE_STRONG_BONUS_WEIGHT {
        let coverage = (matched_weight / total_weight).min(1.0);
        coverage.powi(DOCUMENT_ANCHOR_MULTI_SURFACE_EXPONENT)
            * DOCUMENT_ANCHOR_SURFACE_STRONG_BONUS_WEIGHT
    } else {
        matched_weight.min(DOCUMENT_ANCHOR_SURFACE_STRONG_BONUS_WEIGHT)
    }
}

fn document_anchor_surface_bonus_weight(anchor: &str) -> f64 {
    if anchor.chars().any(|ch| matches!(ch, '_' | '-' | '.' | '/' | ':' | '@')) {
        DOCUMENT_ANCHOR_SURFACE_STRONG_BONUS_WEIGHT
    } else {
        DOCUMENT_ANCHOR_SURFACE_WEAK_BONUS_WEIGHT
    }
}

fn document_identity_bonus(document: &KnowledgeDocumentRow, query_text: &str) -> f64 {
    let query_tokens =
        document_search_anchor_tokens(query_text).into_iter().collect::<HashSet<_>>();
    if query_tokens.is_empty() {
        return 0.0;
    }
    let query_phrases =
        document_search_anchor_phrases(query_text).into_iter().collect::<HashSet<_>>();
    document_identity_values(document)
        .into_iter()
        .map(|value| {
            let normalized_value = value.to_ascii_lowercase();
            let normalized_source_phrase = normalized_alnum_token_sequence_by(
                &value,
                |token| token.chars().count() >= 2 || token.chars().any(|ch| ch.is_ascii_digit()),
                Some(64),
            )
            .join(" ");
            let identity_tokens = normalized_alnum_token_sequence_by(
                &value,
                |token| token.chars().count() >= 4 || token.chars().any(|ch| ch.is_ascii_digit()),
                Some(32),
            )
            .into_iter()
            .collect::<HashSet<_>>();
            let token_overlap = identity_tokens.intersection(&query_tokens).count() as f64;
            let phrase_overlap = query_phrases
                .iter()
                .filter(|phrase| {
                    normalized_value.contains(phrase.as_str())
                        || normalized_source_phrase.contains(phrase.as_str())
                })
                .count() as f64;
            let exact_source_phrase_overlap = query_phrases
                .iter()
                .filter(|phrase| {
                    phrase.split_whitespace().count() >= 3
                        && normalized_source_phrase.contains(phrase.as_str())
                })
                .count() as f64;
            token_overlap * DOCUMENT_IDENTITY_TOKEN_BONUS_WEIGHT
                + phrase_overlap * DOCUMENT_IDENTITY_PHRASE_BONUS_WEIGHT
                + exact_source_phrase_overlap * DOCUMENT_IDENTITY_EXACT_SOURCE_PHRASE_BONUS_WEIGHT
        })
        .fold(0.0, f64::max)
}

fn document_best_chunk_focus_bonus(
    chunk_hits: &[KnowledgeChunkSearchRow],
    keywords: &[String],
    query_text: &str,
) -> f64 {
    let signals = document_best_chunk_focus_signals(chunk_hits, keywords, query_text);
    signals.keyword * DOCUMENT_BEST_CHUNK_KEYWORD_COVERAGE_WEIGHT
        + signals.anchor * DOCUMENT_BEST_CHUNK_ANCHOR_COVERAGE_WEIGHT
}

fn document_best_chunk_evidence_coverage(
    chunk_hits: &[KnowledgeChunkSearchRow],
    keywords: &[String],
    query_text: &str,
) -> f64 {
    let signals = document_best_chunk_focus_signals(chunk_hits, keywords, query_text);
    signals.keyword.max(signals.anchor).clamp(0.0, 1.0)
}

#[derive(Debug, Clone, Copy, Default)]
struct DocumentChunkFocusSignals {
    keyword: f64,
    anchor: f64,
}

fn document_best_chunk_focus_signals(
    chunk_hits: &[KnowledgeChunkSearchRow],
    keywords: &[String],
    query_text: &str,
) -> DocumentChunkFocusSignals {
    if chunk_hits.is_empty() {
        return DocumentChunkFocusSignals::default();
    }
    let anchor_tokens = document_search_anchor_tokens(query_text);
    let anchor_phrases = document_search_anchor_phrases(query_text);
    let surface_anchors = document_search_exact_anchor_surfaces(query_text);
    chunk_hits
        .iter()
        .map(|hit| {
            let haystack = format!("{}\n{}", hit.content_text, hit.normalized_text).to_lowercase();
            let keyword_signal = if keywords.is_empty() {
                0.0
            } else {
                let matches =
                    keywords.iter().filter(|keyword| haystack.contains(keyword.as_str())).count();
                (matches as f64 / keywords.len() as f64).min(1.0)
            };
            let anchor_denominator =
                anchor_tokens.len() + anchor_phrases.len() + surface_anchors.len();
            let anchor_signal = if anchor_denominator == 0 {
                0.0
            } else {
                let token_matches =
                    anchor_tokens.iter().filter(|token| haystack.contains(token.as_str())).count();
                let phrase_matches = anchor_phrases
                    .iter()
                    .filter(|phrase| haystack.contains(phrase.as_str()))
                    .count();
                let surface_matches = surface_anchors
                    .iter()
                    .filter(|surface| haystack.contains(surface.as_str()))
                    .count();
                ((token_matches + phrase_matches + surface_matches) as f64
                    / anchor_denominator as f64)
                    .min(1.0)
            };
            DocumentChunkFocusSignals { keyword: keyword_signal, anchor: anchor_signal }
        })
        .fold(DocumentChunkFocusSignals::default(), |best, next| {
            if (next.keyword + next.anchor) > (best.keyword + best.anchor) { next } else { best }
        })
}

fn document_vector_raw_score_bonus(vector_score: Option<f64>) -> f64 {
    vector_score.unwrap_or_default().clamp(0.0, 1.0) * DOCUMENT_VECTOR_RAW_SCORE_WEIGHT
}

fn document_lexical_signal(
    lexical_score: Option<f64>,
    identity_bonus: f64,
    best_chunk_evidence_coverage: f64,
) -> f64 {
    let Some(lexical_score) = lexical_score else {
        return 0.0;
    };
    let base = lexical_score.ln_1p();
    if (DOCUMENT_SOFT_TITLE_SCORE_MIN..=DOCUMENT_SOFT_TITLE_SCORE_MAX).contains(&lexical_score)
        && identity_bonus < DOCUMENT_SOFT_TITLE_IDENTITY_PRESERVE_THRESHOLD
    {
        let evidence_coverage = best_chunk_evidence_coverage.clamp(0.0, 1.0);
        let scale = DOCUMENT_SOFT_TITLE_LOW_EVIDENCE_MIN_SCALE
            + ((1.0 - DOCUMENT_SOFT_TITLE_LOW_EVIDENCE_MIN_SCALE)
                * evidence_coverage
                * evidence_coverage);
        return base * scale;
    }
    base
}

fn document_identity_values(document: &KnowledgeDocumentRow) -> Vec<&str> {
    [
        document.title.as_deref(),
        document.file_name.as_deref(),
        Some(document.external_key.as_str()),
        document.source_uri.as_deref(),
        document.document_hint.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter(|value| !value.trim().is_empty())
    .collect()
}

fn resolved_evidence_sample_limit(value: Option<usize>) -> usize {
    value.unwrap_or(DEFAULT_EVIDENCE_SAMPLE_LIMIT)
}

fn document_search_vector_limits(
    limit: usize,
    chunk_hit_limit_per_document: usize,
) -> (usize, usize) {
    let chunk_limit =
        limit.saturating_mul(chunk_hit_limit_per_document).max(limit.saturating_mul(2)).max(1);
    let entity_limit = limit.max(1);
    (chunk_limit, entity_limit)
}

fn expand_document_search_queries(query_text: &str) -> Vec<String> {
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
    for phrase in document_search_anchor_phrases(query_text).into_iter().take(6) {
        push_query(&phrase);
    }

    queries
}

#[tracing::instrument(
    level = "info",
    name = "http.search_documents",
    skip_all,
    fields(library_id = %library_id, elapsed_ms)
)]
#[utoipa::path(
    get,
    path = "/v1/knowledge/libraries/{libraryId}/search/documents",
    tag = "search",
    operation_id = "searchKnowledgeDocuments",
    params(
        ("libraryId" = uuid::Uuid, Path, description = "Library identifier"),
        KnowledgeDocumentSearchQuery,
    ),
    responses(
        (status = 200, description = "Hybrid lexical + vector document search results", body = KnowledgeDocumentSearchResponse),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the library"),
    ),
)]
pub async fn search_documents(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
    Query(query): Query<KnowledgeDocumentSearchQuery>,
) -> Result<Json<KnowledgeDocumentSearchResponse>, ApiError> {
    let started_at = std::time::Instant::now();
    let result = search_documents_impl(
        auth,
        state,
        library_id,
        query.query,
        query.limit,
        query.chunk_hit_limit_per_document,
        query.evidence_sample_limit,
    )
    .await;
    tracing::Span::current().record("elapsed_ms", started_at.elapsed().as_millis() as u64);
    result
}

#[tracing::instrument(
    level = "info",
    name = "http.search_documents_by_library_query",
    skip_all,
    fields(library_id = %query.library_id, elapsed_ms)
)]
#[utoipa::path(
    get,
    path = "/v1/search/documents",
    tag = "search",
    operation_id = "searchDocuments",
    params(KnowledgeDocumentSearchRequest),
    responses(
        (status = 200, description = "Hybrid document search across the requested library", body = KnowledgeDocumentSearchResponse),
        (status = 400, description = "libraryId is required"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the library"),
    ),
)]
pub async fn search_documents_by_library_query(
    auth: AuthContext,
    State(state): State<AppState>,
    Query(query): Query<KnowledgeDocumentSearchRequest>,
) -> Result<Json<KnowledgeDocumentSearchResponse>, ApiError> {
    let started_at = std::time::Instant::now();
    let result = search_documents_impl(
        auth,
        state,
        query.library_id,
        query.query,
        query.limit,
        query.chunk_hit_limit_per_document,
        query.evidence_sample_limit,
    )
    .await;
    tracing::Span::current().record("elapsed_ms", started_at.elapsed().as_millis() as u64);
    result
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
    let evidence_sample_limit = resolved_evidence_sample_limit(evidence_sample_limit);
    let query_keywords = document_search_keywords(&query_text);
    let internal_candidate_limit =
        limit.saturating_mul(chunk_hit_limit_per_document.max(3)).saturating_mul(4).max(16);
    let (vector_chunk_limit, vector_entity_limit) =
        document_search_vector_limits(limit, chunk_hit_limit_per_document);
    let (lexical_chunk_hits_result, lexical_entity_hits, lexical_relation_hits, hybrid_context) = tokio::try_join!(
        search_expanded_lexical_chunks(&state, library_id, &query_text, internal_candidate_limit),
        search_entities_by_library(&state, library_id, &query_text, limit),
        search_relations_by_library(&state, library_id, &query_text, limit),
        resolve_hybrid_search_context(&state, library_id, &query_text),
    )?;
    let mut lexical_chunk_hits = lexical_chunk_hits_result;

    let (mut vector_chunk_hits, vector_entity_hits) = if let Some(context) = hybrid_context.as_ref()
    {
        let _vector_guard = state
            .canonical_services
            .search
            .vector_plane_read_guard(&state)
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        let embedding_model_key = context.model_catalog_id.to_string();
        let library_dim = library_vector_index_dimensions(&state, library_id)
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        let vector_chunk_future = state.arango_search_store.search_chunk_vectors_by_similarity(
            library_dim,
            library_id,
            &embedding_model_key,
            &context.query_vector,
            vector_chunk_limit,
            None,
            None,
            None,
        );
        let vector_entity_future = state.arango_search_store.search_entity_vectors_by_similarity(
            library_dim,
            library_id,
            &embedding_model_key,
            &context.query_vector,
            vector_entity_limit,
            None,
        );
        let (chunk_result, entity_result) = tokio::join!(vector_chunk_future, vector_entity_future);
        let chunk_rows = match chunk_result {
            Ok(rows) => rows,
            Err(error) => {
                warn!(
                    library_id = %library_id,
                    model_catalog_id = %context.model_catalog_id,
                    error = ?error,
                    "hybrid knowledge chunk vector search failed; returning lexical chunk hits only",
                );
                Vec::new()
            }
        };
        let entity_rows = match entity_result {
            Ok(rows) => rows,
            Err(error) => {
                warn!(
                    library_id = %library_id,
                    model_catalog_id = %context.model_catalog_id,
                    error = ?error,
                    "hybrid knowledge entity vector search failed; returning lexical entity hits only",
                );
                Vec::new()
            }
        };
        (chunk_rows, entity_rows)
    } else {
        (Vec::new(), Vec::new())
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
    if chunk_map.len() < chunk_ids.len() {
        warn!(
            library_id = %library_id,
            requested_chunk_count = chunk_ids.len(),
            loaded_chunk_count = chunk_map.len(),
            "knowledge document search ignored stale chunk hits missing from the canonical chunk store",
        );
    }

    let revision_ids: Vec<Uuid> = chunk_map
        .values()
        .map(|chunk| chunk.revision_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let document_ids: Vec<Uuid> = chunk_map
        .values()
        .map(|chunk| chunk.document_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let (revisions, documents) = tokio::try_join!(
        load_revisions_by_ids(&state, &revision_ids),
        load_documents_by_ids(&state, &document_ids),
    )?;
    let revision_map: HashMap<Uuid, KnowledgeRevisionRow> =
        revisions.into_iter().map(|revision| (revision.revision_id, revision)).collect();
    let document_map: HashMap<Uuid, KnowledgeDocumentRow> =
        documents.into_iter().map(|document| (document.document_id, document)).collect();
    if revision_map.len() < revision_ids.len() {
        warn!(
            library_id = %library_id,
            requested_revision_count = revision_ids.len(),
            loaded_revision_count = revision_map.len(),
            "knowledge document search ignored chunk hits whose revisions are no longer readable",
        );
    }
    if document_map.len() < document_ids.len() {
        warn!(
            library_id = %library_id,
            requested_document_count = document_ids.len(),
            loaded_document_count = document_map.len(),
            "knowledge document search ignored chunk hits whose documents are no longer readable",
        );
    }

    lexical_chunk_hits
        .retain(|hit| resolved_chunk_hit(&hit.chunk_id, &chunk_map, &revision_map, &document_map));
    vector_chunk_hits
        .retain(|hit| resolved_chunk_hit(&hit.chunk_id, &chunk_map, &revision_map, &document_map));

    let mut accumulators: HashMap<Uuid, KnowledgeDocumentAccumulator> = HashMap::new();
    for (rank, hit) in lexical_chunk_hits.iter().enumerate() {
        let Some(chunk) = chunk_map.get(&hit.chunk_id) else {
            continue;
        };
        let Some(revision) = revision_map.get(&chunk.revision_id) else {
            continue;
        };
        let Some(document) = document_map.get(&chunk.document_id) else {
            continue;
        };
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
        let Some(chunk) = chunk_map.get(&hit.chunk_id) else {
            continue;
        };
        let Some(revision) = revision_map.get(&chunk.revision_id) else {
            continue;
        };
        let Some(document) = document_map.get(&chunk.document_id) else {
            continue;
        };
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
        accumulator.score = score_document_accumulator(accumulator, &query_keywords, &query_text);
    }
    document_hits.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.document.document_id.cmp(&right.document.document_id))
    });

    let backfill_candidate_limit = limit
        .max(limit.saturating_mul(4).min(SEARCH_DOCUMENT_BACKFILL_MAX_CANDIDATES))
        .min(document_hits.len());
    document_hits.truncate(backfill_candidate_limit);
    let backfill_parallelism =
        SEARCH_DOCUMENT_ENRICHMENT_PARALLELISM.min(document_hits.len()).max(1);
    let prepared_results = stream::iter(document_hits.into_iter().map(|accumulator| {
        let state = state.clone();
        let query_text = query_text.clone();
        let query_keywords = query_keywords.clone();
        async move {
            prepare_document_search_candidate(
                state,
                query_text,
                query_keywords,
                chunk_hit_limit_per_document,
                evidence_sample_limit,
                accumulator,
            )
            .await
        }
    }))
    .buffer_unordered(backfill_parallelism)
    .collect::<Vec<_>>()
    .await;
    let mut document_hits = Vec::with_capacity(prepared_results.len());
    for result in prepared_results {
        document_hits.push(result?);
    }
    document_hits.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.document.document_id.cmp(&right.document.document_id))
    });
    document_hits.truncate(limit);
    let enrichment_parallelism =
        SEARCH_DOCUMENT_ENRICHMENT_PARALLELISM.min(document_hits.len()).max(1);
    let enriched_results =
        stream::iter(document_hits.into_iter().enumerate().map(|(index, accumulator)| {
            let state = state.clone();
            let query_text = query_text.clone();
            async move { enrich_document_search_hit(state, query_text, index, accumulator).await }
        }))
        .buffer_unordered(enrichment_parallelism)
        .collect::<Vec<_>>()
        .await;
    let mut indexed_response_hits = Vec::with_capacity(enriched_results.len());
    for result in enriched_results {
        indexed_response_hits.push(result?);
    }
    indexed_response_hits.sort_by_key(|(index, _)| *index);
    let response_hits = indexed_response_hits.into_iter().map(|(_, hit)| hit).collect::<Vec<_>>();

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

async fn search_expanded_lexical_chunks(
    state: &AppState,
    library_id: Uuid,
    query_text: &str,
    internal_candidate_limit: usize,
) -> Result<Vec<KnowledgeChunkSearchRow>, ApiError> {
    let search_queries = expand_document_search_queries(query_text);
    let parallelism = SEARCH_LEXICAL_QUERY_PARALLELISM.min(search_queries.len()).max(1);
    let search_results = stream::iter(search_queries.into_iter().map(|search_query| {
        let state = state.clone();
        async move {
            state
                .arango_search_store
                .search_chunks(library_id, &search_query, internal_candidate_limit, None, None)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))
        }
    }))
    .buffer_unordered(parallelism)
    .collect::<Vec<_>>()
    .await;

    let mut lexical_chunk_hit_map = HashMap::<Uuid, KnowledgeChunkSearchRow>::new();
    for result in search_results {
        for row in result? {
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
    Ok(lexical_chunk_hits)
}

async fn load_evidence_samples_by_chunk_ids(
    state: &AppState,
    document_id: Uuid,
    chunk_ids: &[Uuid],
) -> Result<HashMap<Uuid, Vec<KnowledgeEvidenceRow>>, ApiError> {
    if chunk_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let parallelism = SEARCH_CHUNK_EVIDENCE_PARALLELISM.min(chunk_ids.len()).max(1);
    let evidence_results = stream::iter(chunk_ids.iter().copied().map(|chunk_id| {
        let state = state.clone();
        async move {
            let evidence_rows = state
                .arango_graph_store
                .list_evidence_by_chunk(chunk_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
            Ok::<(Uuid, Vec<KnowledgeEvidenceRow>), ApiError>((chunk_id, evidence_rows))
        }
    }))
    .buffer_unordered(parallelism)
    .collect::<Vec<_>>()
    .await;

    let mut evidence_by_chunk: HashMap<Uuid, Vec<KnowledgeEvidenceRow>> = HashMap::new();
    for result in evidence_results {
        let (chunk_id, evidence_rows) = result?;
        let rows = evidence_rows
            .into_iter()
            .filter(|evidence| evidence.document_id == document_id)
            .collect::<Vec<_>>();
        if !rows.is_empty() {
            evidence_by_chunk.insert(chunk_id, rows);
        }
    }
    Ok(evidence_by_chunk)
}

async fn enrich_document_search_hit(
    state: AppState,
    query_text: String,
    index: usize,
    mut accumulator: KnowledgeDocumentAccumulator,
) -> Result<(usize, KnowledgeSearchDocumentHit), ApiError> {
    focus_document_chunk_hits_for_query(&query_text, &mut accumulator.chunk_hits);
    let technical_fact_samples = state
        .canonical_services
        .knowledge
        .list_typed_technical_facts(&state, accumulator.revision.revision_id)
        .await?;
    Ok((
        index,
        KnowledgeSearchDocumentHit {
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
        },
    ))
}

fn focus_document_chunk_hits_for_query(
    query_text: &str,
    chunk_hits: &mut [KnowledgeChunkSearchRow],
) {
    let focus_terms = document_search_focus_terms(query_text);
    if focus_terms.is_empty() {
        return;
    }
    for hit in chunk_hits {
        let haystack = format!("{}\n{}", hit.content_text, hit.normalized_text).to_lowercase();
        if !focus_terms.iter().any(|term| haystack.contains(term)) {
            continue;
        }
        let focused_content = focused_search_hit_text(&hit.content_text, &focus_terms, 1_600);
        if !focused_content.trim().is_empty() {
            hit.content_text = focused_content;
        }
        let focused_normalized = focused_search_hit_text(&hit.normalized_text, &focus_terms, 1_600);
        if !focused_normalized.trim().is_empty() {
            hit.normalized_text = focused_normalized;
        }
    }
}

fn document_search_focus_terms(query_text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    document_search_exact_anchor_surfaces(query_text)
        .into_iter()
        .chain(document_search_anchor_tokens(query_text))
        .chain(document_search_anchor_phrases(query_text))
        .chain(document_search_keywords(query_text))
        .map(|term| term.trim().to_lowercase())
        .filter(|term| term.chars().count() >= 2)
        .filter(|term| seen.insert(term.clone()))
        .collect()
}

fn focused_search_hit_text(text: &str, focus_terms: &[String], max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() || max_chars == 0 {
        return String::new();
    }
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let lowered = trimmed.to_lowercase();
    let best_match = focus_terms
        .iter()
        .filter_map(|term| {
            let term = term.trim().to_lowercase();
            if term.chars().count() < 2 {
                return None;
            }
            lowered
                .find(term.as_str())
                .map(|byte_index| (document_search_focus_term_score(&term), byte_index))
        })
        .max_by(|left, right| left.0.cmp(&right.0).then_with(|| right.1.cmp(&left.1)));

    let Some((_, match_byte_index)) = best_match else {
        return crate::services::query::execution::focused_excerpt_for(
            trimmed,
            focus_terms,
            max_chars,
        );
    };
    centered_char_window(trimmed, match_byte_index, max_chars)
}

fn document_search_focus_term_score(term: &str) -> usize {
    let has_structural_separator =
        term.chars().any(|ch| matches!(ch, '_' | '-' | '.' | '/' | ':' | '@'));
    let has_digit = term.chars().any(|ch| ch.is_ascii_digit());
    let has_uppercase_shape = term.chars().any(|ch| ch.is_ascii_uppercase());
    let token_count = term.split_whitespace().count();
    usize::from(has_structural_separator) * 1_000
        + usize::from(has_digit) * 700
        + usize::from(has_uppercase_shape) * 500
        + token_count.saturating_sub(1) * 100
        + term.chars().count().min(80)
}

fn centered_char_window(text: &str, center_byte_index: usize, max_chars: usize) -> String {
    let char_positions = text.char_indices().map(|(index, _)| index).collect::<Vec<_>>();
    if char_positions.len() <= max_chars {
        return text.to_string();
    }
    let center_char_index = char_positions
        .binary_search(&center_byte_index)
        .unwrap_or_else(|index| index.saturating_sub(1));
    let half_window = max_chars / 2;
    let start_char = center_char_index.saturating_sub(half_window);
    let end_char = (start_char + max_chars).min(char_positions.len());
    let start_byte = char_positions[start_char];
    let end_byte = char_positions.get(end_char).copied().unwrap_or(text.len());
    let prefix = if start_char > 0 { "... " } else { "" };
    let suffix = if end_char < char_positions.len() { " ..." } else { "" };
    format!("{prefix}{}{suffix}", text[start_byte..end_byte].trim())
}

fn resolved_chunk_hit(
    chunk_id: &Uuid,
    chunk_map: &HashMap<Uuid, KnowledgeChunkRow>,
    revision_map: &HashMap<Uuid, KnowledgeRevisionRow>,
    document_map: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> bool {
    let Some(chunk) = chunk_map.get(chunk_id) else {
        return false;
    };
    revision_map.contains_key(&chunk.revision_id) && document_map.contains_key(&chunk.document_id)
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

async fn prepare_document_search_candidate(
    state: AppState,
    query_text: String,
    query_keywords: Vec<String>,
    chunk_hit_limit_per_document: usize,
    evidence_sample_limit: usize,
    mut accumulator: KnowledgeDocumentAccumulator,
) -> Result<KnowledgeDocumentAccumulator, ApiError> {
    backfill_document_chunk_hits(
        &state,
        &query_text,
        chunk_hit_limit_per_document,
        &mut accumulator,
    )
    .await?;

    accumulator.chunk_hits.truncate(chunk_hit_limit_per_document);
    accumulator.vector_chunk_hits.truncate(chunk_hit_limit_per_document);
    let mut seen_evidence_chunks = HashSet::new();
    let candidate_chunk_ids: Vec<Uuid> = accumulator
        .chunk_hits
        .iter()
        .map(|hit| hit.chunk_id)
        .chain(accumulator.vector_chunk_hits.iter().map(|hit| hit.chunk_id))
        .collect();
    let mut deduped_chunk_ids = Vec::<Uuid>::new();
    for chunk_id in candidate_chunk_ids {
        if !seen_evidence_chunks.insert(chunk_id) {
            continue;
        }
        deduped_chunk_ids.push(chunk_id);
    }

    if evidence_sample_limit > 0 {
        let evidence_by_chunk = load_evidence_samples_by_chunk_ids(
            &state,
            accumulator.document.document_id,
            &deduped_chunk_ids,
        )
        .await?;
        for chunk_id in deduped_chunk_ids {
            let Some(evidence_rows) = evidence_by_chunk.get(&chunk_id) else {
                continue;
            };
            for evidence in evidence_rows {
                if accumulator.evidence_ids.insert(evidence.evidence_id) {
                    accumulator.evidence_samples.push(evidence.clone());
                }
                if accumulator.evidence_samples.len() >= evidence_sample_limit {
                    break;
                }
            }
            if accumulator.evidence_samples.len() >= evidence_sample_limit {
                break;
            }
        }
    }

    accumulator.score = score_document_accumulator(&accumulator, &query_keywords, &query_text);
    Ok(accumulator)
}

fn score_document_accumulator(
    accumulator: &KnowledgeDocumentAccumulator,
    query_keywords: &[String],
    query_text: &str,
) -> f64 {
    let lexical_rank = accumulator.lexical_rank.unwrap_or(usize::MAX / 2);
    let vector_rank = accumulator.vector_rank.unwrap_or(usize::MAX / 2);
    let vector_signal = accumulator.vector_score.map(f64::ln_1p).unwrap_or_default();
    let provenance_bonus = (accumulator.evidence_samples.len() as f64) / 1000.0;
    let keyword_coverage_bonus =
        document_keyword_coverage_bonus(&accumulator.chunk_hits, query_keywords);
    let anchor_match_bonus = document_anchor_match_bonus(&accumulator.chunk_hits, query_text);
    let best_chunk_focus_bonus =
        document_best_chunk_focus_bonus(&accumulator.chunk_hits, query_keywords, query_text);
    let vector_raw_score_bonus = document_vector_raw_score_bonus(accumulator.vector_score);
    let identity_bonus = document_identity_bonus(&accumulator.document, query_text);
    let best_chunk_evidence_coverage =
        document_best_chunk_evidence_coverage(&accumulator.chunk_hits, query_keywords, query_text);
    let lexical_signal = document_lexical_signal(
        accumulator.lexical_score,
        identity_bonus,
        best_chunk_evidence_coverage,
    );
    lexical_signal
        + vector_signal
        + (1.0 / (60.0 + lexical_rank as f64))
        + (1.0 / (60.0 + vector_rank as f64))
        + provenance_bonus
        + keyword_coverage_bonus
        + anchor_match_bonus
        + best_chunk_focus_bonus
        + vector_raw_score_bonus
        + identity_bonus
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
    let mut backfill_terms = keywords.clone();
    backfill_terms.extend(document_search_anchor_tokens(query_text));
    backfill_terms.extend(document_search_anchor_phrases(query_text));
    candidates.extend(
        state
            .arango_document_store
            .list_chunks_by_revision_matching_terms(
                accumulator.revision.revision_id,
                &backfill_terms,
                chunk_hit_limit_per_document.saturating_mul(4).max(chunk_hit_limit_per_document),
            )
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
    let lowered_text = format!("{} {}", hit.content_text, hit.normalized_text).to_lowercase();
    let keywords = crate::services::query::planner::extract_keywords(query_text);
    let keyword_score = keywords
        .iter()
        .map(|keyword| lowered_text.matches(keyword.as_str()).count())
        .sum::<usize>();
    let anchor_score = document_search_anchor_tokens(query_text)
        .into_iter()
        .filter(|token| lowered_text.contains(token))
        .count();
    let phrase_score = document_search_anchor_phrases(query_text)
        .into_iter()
        .filter(|phrase| lowered_text.contains(phrase))
        .count();
    keyword_score + anchor_score * 50 + phrase_score * 150
}

fn document_search_anchor_tokens(query_text: &str) -> Vec<String> {
    normalized_alnum_token_sequence_by(
        query_text,
        |token| token.chars().count() >= 4 || token.chars().any(|ch| ch.is_ascii_digit()),
        Some(32),
    )
}

fn document_search_anchor_phrases(query_text: &str) -> Vec<String> {
    let tokens = normalized_alnum_token_sequence_by(
        query_text,
        |token| token.chars().count() >= 2 || token.chars().any(|ch| ch.is_ascii_digit()),
        Some(48),
    );
    let anchors = document_search_anchor_tokens(query_text).into_iter().collect::<HashSet<_>>();
    let mut phrases = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    for window_size in 2..=5 {
        for window in tokens.windows(window_size) {
            if !window.iter().any(|token| anchors.contains(token)) {
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

fn document_search_exact_anchor_surfaces(query_text: &str) -> Vec<String> {
    let mut anchors = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    for raw in query_text.split_whitespace() {
        let candidate = raw
            .trim_matches(|ch: char| {
                !(ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':' | '@'))
            })
            .trim();
        if candidate.is_empty() {
            continue;
        }
        let alnum_count = candidate.chars().filter(|ch| ch.is_alphanumeric()).count();
        if alnum_count < 3 {
            continue;
        }
        let has_structural_separator =
            candidate.chars().any(|ch| matches!(ch, '_' | '-' | '.' | '/' | ':' | '@'));
        let has_digit = candidate.chars().any(|ch| ch.is_ascii_digit());
        let has_upper_after_first = candidate.chars().skip(1).any(|ch| ch.is_ascii_uppercase());
        let has_lower = candidate.chars().any(|ch| ch.is_ascii_lowercase());
        if !(has_structural_separator || has_digit || (has_upper_after_first && has_lower)) {
            continue;
        }
        let normalized = candidate.to_lowercase();
        if seen.insert(normalized.clone()) {
            anchors.push(normalized);
        }
    }
    anchors
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
            extra_parameters_json: binding.extra_parameters_json.clone(),
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
    state
        .arango_document_store
        .list_revisions_by_ids(revision_ids)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))
}

async fn load_documents_by_ids(
    state: &AppState,
    document_ids: &[Uuid],
) -> Result<Vec<KnowledgeDocumentRow>, ApiError> {
    if document_ids.is_empty() {
        return Ok(Vec::new());
    }
    state
        .arango_document_store
        .list_documents_by_ids(document_ids)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk_hit(text: &str) -> KnowledgeChunkSearchRow {
        KnowledgeChunkSearchRow {
            chunk_id: Uuid::nil(),
            workspace_id: Uuid::nil(),
            library_id: Uuid::nil(),
            revision_id: Uuid::nil(),
            content_text: text.to_string(),
            normalized_text: text.to_string(),
            section_path: Vec::new(),
            heading_trail: Vec::new(),
            score: 1.0,
            quality_score: None,
        }
    }

    #[test]
    fn document_keyword_coverage_bonus_rewards_concentrated_evidence() {
        let keywords = document_search_keywords("Alpha matrix beta gamma delta epsilon zeta");
        let weak = chunk_hit("Alpha matrix mentions beta once.");
        let strong = chunk_hit("Alpha matrix beta, gamma, delta, epsilon, and zeta.");

        let weak_bonus = document_keyword_coverage_bonus(&[weak], &keywords);
        let strong_bonus = document_keyword_coverage_bonus(&[strong], &keywords);

        assert!(strong_bonus > weak_bonus + 0.2);
    }

    #[test]
    fn document_identity_bonus_rewards_specific_title_tokens() {
        let specific = document_row("alpha_matrix_delta_index.yaml");
        let generic = document_row("alpha_matrix.yaml");

        let query = "Alpha Matrix delta index";
        assert!(
            document_identity_bonus(&specific, query) > document_identity_bonus(&generic, query)
        );
    }

    #[test]
    fn document_identity_bonus_rewards_structured_identity_match() {
        let exact = document_row("segment-alpha-control-plane.md");
        let generic = document_row("segment-alpha-overview.md");

        let query = "segment alpha control plane calibration";

        assert!(document_identity_bonus(&exact, query) > document_identity_bonus(&generic, query));
    }

    #[test]
    fn document_identity_bonus_rewards_multi_token_source_phrase() {
        let source = document_row("north-region-review-cycle.txt");
        let generic = document_row("north-region-review-notes.txt");

        let query = "north region review cycle threshold";

        assert!(document_identity_bonus(&source, query) > document_identity_bonus(&generic, query));
    }

    #[test]
    fn document_identity_bonus_rewards_source_phrase_with_separators() {
        let source = document_row("alpha_data_sequence.md");
        let generic = document_row("alpha_reference_notes.md");

        let query = "alpha data sequence retry policy exponential backoff max attempts delay";

        assert!(document_identity_bonus(&source, query) > document_identity_bonus(&generic, query));
    }

    #[test]
    fn document_best_chunk_focus_bonus_rewards_concentrated_evidence() {
        let keywords = document_search_keywords("alpha beta gamma delta epsilon");
        let diffuse = vec![
            chunk_hit("alpha beta notes"),
            chunk_hit("gamma delta notes"),
            chunk_hit("epsilon notes"),
        ];
        let focused = vec![chunk_hit("alpha beta gamma delta epsilon")];

        let query = "alpha beta gamma delta epsilon";

        assert!(
            document_best_chunk_focus_bonus(&focused, &keywords, query)
                > document_best_chunk_focus_bonus(&diffuse, &keywords, query)
        );
    }

    #[test]
    fn document_best_chunk_evidence_coverage_rewards_single_focused_chunk() {
        let keywords = document_search_keywords("alpha beta gamma delta epsilon");
        let diffuse = vec![
            chunk_hit("alpha beta notes"),
            chunk_hit("gamma delta notes"),
            chunk_hit("epsilon notes"),
        ];
        let focused = vec![chunk_hit("alpha beta gamma delta epsilon")];
        let query = "alpha beta gamma delta epsilon";

        assert!(
            document_best_chunk_evidence_coverage(&focused, &keywords, query)
                > document_best_chunk_evidence_coverage(&diffuse, &keywords, query) + 0.3
        );
    }

    #[test]
    fn document_lexical_signal_dampens_soft_title_without_evidence_coverage() {
        let weak = document_lexical_signal(Some(50.0), 0.0, 0.2);
        let strong = document_lexical_signal(Some(50.0), 0.0, 0.9);
        let identity = document_lexical_signal(Some(50.0), 1.25, 0.2);
        let exact_identity = document_lexical_signal(Some(1_000_000.0), 0.0, 0.0);

        assert!(strong > weak + 2.0);
        assert_eq!(identity, 50.0_f64.ln_1p());
        assert_eq!(exact_identity, 1_000_000.0_f64.ln_1p());
    }

    #[test]
    fn document_vector_raw_score_bonus_preserves_similarity_margin() {
        assert!(
            document_vector_raw_score_bonus(Some(0.91))
                > document_vector_raw_score_bonus(Some(0.73))
        );
        assert_eq!(document_vector_raw_score_bonus(Some(-0.5)), 0.0);
        assert_eq!(document_vector_raw_score_bonus(Some(2.0)), DOCUMENT_VECTOR_RAW_SCORE_WEIGHT);
    }

    #[test]
    fn document_anchor_match_bonus_rewards_exact_config_keys() {
        let generic = chunk_hit(
            "Configuration values include OTHER_PORT, OTHER_DATABASE_URL, and OTHER_SECRET.",
        );
        let exact =
            chunk_hit("Configuration values include APP_PORT, APP_DATABASE_URL, and APP_SECRET.");

        let query = "Which settings configure APP_PORT APP_DATABASE_URL APP_SECRET?";

        assert!(
            document_anchor_match_bonus(&[exact], query)
                > document_anchor_match_bonus(&[generic], query) + 1.0
        );
    }

    #[test]
    fn document_anchor_match_bonus_treats_structural_literals_as_strong_anchors() {
        let generic = chunk_hit("Trace limit window controls the request budget.");
        let exact = chunk_hit("TRACE_LIMIT_WINDOW controls the request budget.");

        let query = "trace limit TRACE_LIMIT_WINDOW";

        assert!(
            document_anchor_match_bonus(&[exact], query)
                > document_anchor_match_bonus(&[generic], query) + 20.0
        );
    }

    #[test]
    fn document_anchor_match_bonus_rewards_camel_case_identifiers() {
        let generic = chunk_hit("The preprocessor lowercases text and normalizes addresses.");
        let exact = chunk_hit("TextPreprocessor.lowercase, TextPreprocessor.normalize_address");

        let query = "What steps does TextPreprocessor apply?";

        assert!(
            document_anchor_match_bonus(&[exact], query)
                > document_anchor_match_bonus(&[generic], query)
        );
    }

    #[test]
    fn document_anchor_match_bonus_keeps_plain_camelcase_weaker_than_structural_literals() {
        assert!(
            document_anchor_surface_bonus_weight("retry_limit")
                > document_anchor_surface_bonus_weight("configmaps")
        );
        assert_eq!(
            document_anchor_surface_bonus_weight("base64"),
            document_anchor_surface_bonus_weight("configmaps")
        );
        let proper_name = chunk_hit("ConfigMaps are mentioned as a related object.");
        let substantive =
            chunk_hit("Sensitive data should use encryption and base64 encoding where required.");

        let query = "ConfigMaps sensitive data base64 encryption";

        assert!(
            document_anchor_match_bonus(&[substantive], query)
                > document_anchor_match_bonus(&[proper_name], query),
        );
    }

    #[test]
    fn document_anchor_match_bonus_scales_partial_multi_surface_matches() {
        let partial = chunk_hit("The RETRY_LIMIT_REQUESTS setting is available.");
        let complete = chunk_hit(
            "The RETRY_LIMIT_REQUESTS and RETRY_LIMIT_WINDOW_SECONDS settings are available.",
        );

        let query = "RETRY_LIMIT_REQUESTS RETRY_LIMIT_WINDOW_SECONDS";

        assert!(
            document_anchor_match_bonus(&[complete], query)
                > document_anchor_match_bonus(&[partial], query) + 10.0
        );
    }

    #[test]
    fn document_anchor_match_bonus_requires_multi_surface_coverage() {
        let partial = chunk_hit("The ALPHA_LIMIT setting is available.");
        let complete = chunk_hit("The ALPHA_LIMIT and BETA_TIMEOUT settings are available.");
        let query = "ALPHA_LIMIT BETA_TIMEOUT";

        assert!(document_anchor_match_bonus(&[partial], query) < 5.0);
        assert!(document_anchor_match_bonus(&[complete], query) > 40.0);
    }

    #[test]
    fn exact_anchor_surfaces_ignore_ordinary_title_case_words() {
        let anchors =
            document_search_exact_anchor_surfaces("Alpha Beta TextParser RETRY_LIMIT key-v2");

        assert!(!anchors.contains(&"alpha".to_string()));
        assert!(!anchors.contains(&"beta".to_string()));
        assert!(anchors.contains(&"textparser".to_string()));
        assert!(anchors.contains(&"retry_limit".to_string()));
        assert!(anchors.contains(&"key-v2".to_string()));
    }

    #[test]
    fn evidence_sample_limit_preserves_explicit_zero() {
        assert_eq!(resolved_evidence_sample_limit(Some(0)), 0);
        assert_eq!(resolved_evidence_sample_limit(Some(2)), 2);
        assert_eq!(resolved_evidence_sample_limit(None), DEFAULT_EVIDENCE_SAMPLE_LIMIT);
    }

    #[test]
    fn document_search_vector_limits_track_response_surface() {
        assert_eq!(document_search_vector_limits(3, 3), (9, 3));
        assert_eq!(document_search_vector_limits(1, 1), (2, 1));
    }

    #[test]
    fn anchor_phrases_are_generated_from_structure_without_topic_aliases() {
        let queries = expand_document_search_queries("Alpha Matrix delta channel");

        assert_eq!(queries[0], "Alpha Matrix delta channel");
        assert!(queries.iter().any(|query| query == "alpha matrix"));
        assert!(queries.iter().any(|query| query == "matrix delta"));
    }

    #[test]
    fn focused_search_hit_text_centers_late_structural_anchor() {
        let text = format!(
            "alpha matrix summary {}; blocks.alpha.entry.reason_code enum beta gamma delta epsilon zeta status",
            (0..220).map(|index| format!("early_{index}=value")).collect::<Vec<_>>().join("; ")
        );
        let focus_terms = document_search_focus_terms(
            "alpha matrix reason_code beta gamma delta epsilon zeta status",
        );

        let excerpt = focused_search_hit_text(&text, &focus_terms, 220);

        assert!(excerpt.contains("reason_code"), "{excerpt}");
        assert!(excerpt.contains("beta"), "{excerpt}");
        assert!(!excerpt.contains("early_0=value"), "{excerpt}");
    }

    fn document_row(file_name: &str) -> KnowledgeDocumentRow {
        let document_id = Uuid::now_v7();
        KnowledgeDocumentRow {
            key: document_id.to_string(),
            arango_id: None,
            arango_rev: None,
            document_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            external_key: file_name.to_string(),
            file_name: Some(file_name.to_string()),
            title: Some(file_name.to_string()),
            source_uri: None,
            document_hint: None,
            document_state: "active".to_string(),
            active_revision_id: Some(Uuid::now_v7()),
            readable_revision_id: Some(Uuid::now_v7()),
            latest_revision_no: Some(1),
            parent_document_id: None,
            document_role: crate::domains::content::DOCUMENT_ROLE_PRIMARY.to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
        }
    }
}
