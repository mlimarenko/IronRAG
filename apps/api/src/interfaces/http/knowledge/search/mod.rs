use std::collections::{HashMap, HashSet};

use axum::{
    Json,
    extract::{Path, State},
};
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use tracing::warn;
use uuid::Uuid;

use super::{
    KnowledgeDocumentProvenanceSummary, KnowledgeGraphEvidenceSummary,
    KnowledgeTechnicalFactProvenanceSummary, load_chunks_by_ids, summarize_graph_evidence,
    summarize_typed_technical_facts,
};
use crate::{
    app::state::AppState,
    domains::ai::AiBindingPurpose,
    infra::{
        knowledge_plane::LexicalQueryOrigin,
        knowledge_rows::{
            KnowledgeChunkRow, KnowledgeChunkSearchRow, KnowledgeChunkVectorSearchRow,
            KnowledgeDocumentRow, KnowledgeEntitySearchRow, KnowledgeEntityVectorSearchRow,
            KnowledgeEvidenceRow, KnowledgeRelationSearchRow, KnowledgeRevisionRow,
        },
    },
    integrations::llm::EmbeddingRequest,
    interfaces::http::{
        auth::AuthContext,
        authorization::{POLICY_KNOWLEDGE_READ, load_library_and_authorize},
        router_support::{ApiError, map_runtime_lifecycle_error},
    },
    services::query::{
        error::QueryServiceError,
        vector_dimensions::{
            EmbeddingProfileIndexState, EmbeddingProfileInventoryVersion,
            ensure_active_embedding_profile_key,
            ensure_embedding_profile_inventory_version_current,
            ensure_library_embedding_profile_indexed, load_embedding_profile_inventory_version,
            validate_embedding_vector_dimensions,
        },
    },
    shared::{
        extraction::text_render::repair_technical_layout_noise,
        structural_declarations::content_declares_any_identifier,
        text_tokens::normalized_alnum_token_sequence_by,
    },
};

mod candidates;
mod orchestration;
mod ranking;
#[cfg(test)]
mod tests;

use orchestration::{KnowledgeSearchLimits, search_documents_impl};
#[cfg(test)]
use {candidates::*, ranking::*};

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
/// Exact query identifiers are eligible only when they occur in at most five
/// percent of readable documents.
const DOCUMENT_EXACT_IDENTIFIER_RARITY_MIN: f64 = 0.95;
/// Identifier evidence may strengthen relevance by at most half of the score a
/// document earned from the other ranking signals.
const DOCUMENT_EXACT_IDENTIFIER_RELEVANCE_MAX_FRACTION: f64 = 0.5;
/// The absolute identifier uplift is comparable to the existing token-anchor
/// bonus, regardless of the number of exact identifiers in a document.
const DOCUMENT_EXACT_IDENTIFIER_RELEVANCE_BONUS_MAX: f64 = 1.8;
const DOCUMENT_VECTOR_RAW_SCORE_WEIGHT: f64 = 0.75;
const DOCUMENT_BEST_CHUNK_KEYWORD_COVERAGE_WEIGHT: f64 = 1.25;
const DOCUMENT_BEST_CHUNK_ANCHOR_COVERAGE_WEIGHT: f64 = 0.75;
const DOCUMENT_SOFT_TITLE_SCORE_MIN: f64 = 45.0;
const DOCUMENT_SOFT_TITLE_SCORE_MAX: f64 = 50.5;
const DOCUMENT_SOFT_TITLE_LOW_EVIDENCE_MIN_SCALE: f64 = 0.25;
const DOCUMENT_SOFT_TITLE_IDENTITY_PRESERVE_THRESHOLD: f64 = 1.0;

/// JSON body for `POST /v1/knowledge/libraries/{libraryId}/search` — the
/// single hybrid lexical+vector search interface (plan §1.5.1: the one
/// sanctioned `POST`-that-reads exception in the whole redesign, because
/// free-text query input does not fit a query string safely/practically).
/// `text` replaces the former `query` field name to match the plan's wire
/// contract.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeSearchRequest {
    text: String,
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

/// Wire-facing projection of [`KnowledgeDocumentRow`]: camelCase and without
/// the internal `workspace_id` tenancy column, which the raw infra row
/// carries only for storage-layer scoping.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeSearchDocumentSummary {
    document_id: Uuid,
    library_id: Uuid,
    external_key: String,
    file_name: Option<String>,
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    document_hint: Option<String>,
    document_state: String,
    active_revision_id: Option<Uuid>,
    readable_revision_id: Option<Uuid>,
    latest_revision_no: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_document_id: Option<Uuid>,
    document_role: String,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl From<KnowledgeDocumentRow> for KnowledgeSearchDocumentSummary {
    fn from(row: KnowledgeDocumentRow) -> Self {
        Self {
            document_id: row.document_id,
            library_id: row.library_id,
            external_key: row.external_key,
            file_name: row.file_name,
            title: row.title,
            source_uri: row.source_uri,
            document_hint: row.document_hint,
            document_state: row.document_state,
            active_revision_id: row.active_revision_id,
            readable_revision_id: row.readable_revision_id,
            latest_revision_no: row.latest_revision_no,
            parent_document_id: row.parent_document_id,
            document_role: row.document_role,
            created_at: row.created_at,
            updated_at: row.updated_at,
            deleted_at: row.deleted_at,
        }
    }
}

/// Wire-facing projection of [`KnowledgeChunkSearchRow`] (camelCase, no
/// `workspace_id`).
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeSearchChunkHit {
    chunk_id: Uuid,
    library_id: Uuid,
    revision_id: Uuid,
    content_text: String,
    normalized_text: String,
    section_path: Vec<String>,
    heading_trail: Vec<String>,
    score: f64,
    quality_score: Option<f32>,
}

impl From<KnowledgeChunkSearchRow> for KnowledgeSearchChunkHit {
    fn from(row: KnowledgeChunkSearchRow) -> Self {
        Self {
            chunk_id: row.chunk_id,
            library_id: row.library_id,
            revision_id: row.revision_id,
            content_text: row.content_text,
            normalized_text: row.normalized_text,
            section_path: row.section_path,
            heading_trail: row.heading_trail,
            score: row.score,
            quality_score: row.quality_score,
        }
    }
}

/// Wire-facing projection of [`KnowledgeChunkVectorSearchRow`] (camelCase, no
/// `workspace_id`). Shared by the per-document hit and the top-level
/// response, mirroring the single raw row it wraps.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeSearchVectorChunkHit {
    vector_id: Uuid,
    library_id: Uuid,
    chunk_id: Uuid,
    revision_id: Uuid,
    embedding_model_key: String,
    vector_kind: String,
    freshness_generation: i64,
    score: f64,
}

impl From<KnowledgeChunkVectorSearchRow> for KnowledgeSearchVectorChunkHit {
    fn from(row: KnowledgeChunkVectorSearchRow) -> Self {
        Self {
            vector_id: row.vector_id,
            library_id: row.library_id,
            chunk_id: row.chunk_id,
            revision_id: row.revision_id,
            embedding_model_key: row.embedding_model_key,
            vector_kind: row.vector_kind,
            freshness_generation: row.freshness_generation,
            score: row.score,
        }
    }
}

/// Wire-facing projection of [`KnowledgeEvidenceRow`] (camelCase, no
/// `workspace_id`).
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeSearchEvidenceSample {
    evidence_id: Uuid,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    chunk_id: Option<Uuid>,
    block_id: Option<Uuid>,
    fact_id: Option<Uuid>,
    span_start: Option<i32>,
    span_end: Option<i32>,
    quote_text: String,
    literal_spans_json: serde_json::Value,
    evidence_kind: String,
    extraction_method: String,
    confidence: Option<f64>,
    evidence_state: String,
    freshness_generation: i64,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<KnowledgeEvidenceRow> for KnowledgeSearchEvidenceSample {
    fn from(row: KnowledgeEvidenceRow) -> Self {
        Self {
            evidence_id: row.evidence_id,
            library_id: row.library_id,
            document_id: row.document_id,
            revision_id: row.revision_id,
            chunk_id: row.chunk_id,
            block_id: row.block_id,
            fact_id: row.fact_id,
            span_start: row.span_start,
            span_end: row.span_end,
            quote_text: row.quote_text,
            literal_spans_json: row.literal_spans_json,
            evidence_kind: row.evidence_kind,
            extraction_method: row.extraction_method,
            confidence: row.confidence,
            evidence_state: row.evidence_state,
            freshness_generation: row.freshness_generation,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// Wire-facing projection of [`KnowledgeEntitySearchRow`] (camelCase, no
/// `workspace_id`).
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeSearchEntityHit {
    entity_id: Uuid,
    library_id: Uuid,
    #[serde(rename = "\u{63}anonicalLabel")]
    primary_label: String,
    entity_type: String,
    summary: Option<String>,
    score: f64,
}

impl From<KnowledgeEntitySearchRow> for KnowledgeSearchEntityHit {
    fn from(row: KnowledgeEntitySearchRow) -> Self {
        Self {
            entity_id: row.entity_id,
            library_id: row.library_id,
            primary_label: row.primary_label,
            entity_type: row.entity_type,
            summary: row.summary,
            score: row.score,
        }
    }
}

/// Wire-facing projection of [`KnowledgeRelationSearchRow`] (camelCase, no
/// `workspace_id`).
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeSearchRelationHit {
    relation_id: Uuid,
    library_id: Uuid,
    predicate: String,
    normalized_assertion: String,
    summary: Option<String>,
    score: f64,
}

impl From<KnowledgeRelationSearchRow> for KnowledgeSearchRelationHit {
    fn from(row: KnowledgeRelationSearchRow) -> Self {
        Self {
            relation_id: row.relation_id,
            library_id: row.library_id,
            predicate: row.predicate,
            normalized_assertion: row.normalized_assertion,
            summary: row.summary,
            score: row.score,
        }
    }
}

/// Wire-facing projection of [`KnowledgeEntityVectorSearchRow`] (camelCase,
/// no `workspace_id`).
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeSearchVectorEntityHit {
    vector_id: Uuid,
    library_id: Uuid,
    entity_id: Uuid,
    embedding_model_key: String,
    vector_kind: String,
    freshness_generation: i64,
    score: f64,
}

impl From<KnowledgeEntityVectorSearchRow> for KnowledgeSearchVectorEntityHit {
    fn from(row: KnowledgeEntityVectorSearchRow) -> Self {
        Self {
            vector_id: row.vector_id,
            library_id: row.library_id,
            entity_id: row.entity_id,
            embedding_model_key: row.embedding_model_key,
            vector_kind: row.vector_kind,
            freshness_generation: row.freshness_generation,
            score: row.score,
        }
    }
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeSearchDocumentHit {
    document: KnowledgeSearchDocumentSummary,
    revision: KnowledgeSearchRevisionSummary,
    score: f64,
    lexical_rank: Option<usize>,
    vector_rank: Option<usize>,
    lexical_score: Option<f64>,
    vector_score: Option<f64>,
    chunk_hits: Vec<KnowledgeSearchChunkHit>,
    vector_chunk_hits: Vec<KnowledgeSearchVectorChunkHit>,
    evidence_samples: Vec<KnowledgeSearchEvidenceSample>,
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
    entity_hits: Vec<KnowledgeSearchEntityHit>,
    relation_hits: Vec<KnowledgeSearchRelationHit>,
    vector_chunk_hits: Vec<KnowledgeSearchVectorChunkHit>,
    vector_entity_hits: Vec<KnowledgeSearchVectorEntityHit>,
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
    exact_identifier_rarity: f64,
    exact_identifier_terms: Vec<String>,
    rare_exact_identifier_terms: Vec<String>,
    declares_query_identifier: bool,
    declares_rare_exact_identifier: bool,
    chunk_hits: Vec<KnowledgeChunkSearchRow>,
    vector_chunk_hits: Vec<KnowledgeChunkVectorSearchRow>,
    evidence_samples: Vec<KnowledgeEvidenceRow>,
    evidence_ids: HashSet<Uuid>,
}

impl KnowledgeDocumentAccumulator {
    fn new(document: KnowledgeDocumentRow, revision: KnowledgeRevisionRow) -> Self {
        Self {
            document,
            revision,
            score: 0.0,
            lexical_rank: None,
            vector_rank: None,
            lexical_score: None,
            vector_score: None,
            exact_identifier_rarity: 0.0,
            exact_identifier_terms: Vec::new(),
            rare_exact_identifier_terms: Vec::new(),
            declares_query_identifier: false,
            declares_rare_exact_identifier: false,
            chunk_hits: Vec::new(),
            vector_chunk_hits: Vec::new(),
            evidence_samples: Vec::new(),
            evidence_ids: HashSet::new(),
        }
    }
}

#[derive(Debug, Clone)]
struct KnowledgeHybridSearchContext {
    provider_kind: String,
    model_name: String,
    model_catalog_id: Uuid,
    embedding_profile_key: String,
    freshness_generation: i64,
    inventory_version: EmbeddingProfileInventoryVersion,
    dimensions: u64,
    query_vector: Vec<f32>,
}

#[derive(Debug, Clone, Copy)]
struct DocumentIdentifierRankSignals {
    score: f64,
    exact_identifier_rarity: f64,
    declares_query_identifier: bool,
    declares_rare_exact_identifier: bool,
}

#[tracing::instrument(
    level = "info",
    name = "http.search_library",
    skip_all,
    fields(library_id = %library_id, elapsed_ms)
)]
#[utoipa::path(
    post,
    path = "/v1/knowledge/libraries/{libraryId}/search",
    tag = "knowledge",
    operation_id = "searchKnowledgeLibrary",
    params(("libraryId" = uuid::Uuid, Path, description = "Library identifier")),
    request_body = KnowledgeSearchRequest,
    responses(
        (status = 200, description = "Hybrid lexical + vector search results for the library", body = KnowledgeDocumentSearchResponse),
        (status = 400, description = "text must not be empty"),
        (status = 401, description = "Caller is not authenticated"),
        (status = 403, description = "Caller is not authorized for the library"),
    ),
)]
pub async fn search_library(
    auth: AuthContext,
    State(state): State<AppState>,
    Path(library_id): Path<Uuid>,
    Json(request): Json<KnowledgeSearchRequest>,
) -> Result<Json<KnowledgeDocumentSearchResponse>, ApiError> {
    let started_at = std::time::Instant::now();
    let library =
        load_library_and_authorize(&auth, &state, library_id, POLICY_KNOWLEDGE_READ).await?;
    let result = Box::pin(search_documents_impl(
        state.clone(),
        library_id,
        Some(request.text),
        KnowledgeSearchLimits {
            limit: request.limit,
            chunk_hit_limit_per_document: request.chunk_hit_limit_per_document,
            evidence_sample_limit: request.evidence_sample_limit,
        },
    ))
    .await?;
    #[cfg(feature = "test-support")]
    state.runtime_graph_projection_cache.pause_selected_search_read(library_id).await;
    for hit in &result.0.document_hits {
        if !crate::infra::repositories::content_fingerprint_repository::revision_is_current(
            &state.persistence.postgres,
            hit.revision.document_id,
            hit.revision.revision_id,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "search visibility lookup failed"))?
        {
            return Err(ApiError::knowledge_not_ready("search sources changed; retry the search"));
        }
    }
    if !result.0.entity_hits.is_empty()
        || !result.0.relation_hits.is_empty()
        || !result.0.vector_entity_hits.is_empty()
    {
        let entity_ids = result
            .0
            .entity_hits
            .iter()
            .map(|hit| hit.entity_id)
            .chain(result.0.vector_entity_hits.iter().map(|hit| hit.entity_id))
            .collect::<Vec<_>>();
        let relation_ids =
            result.0.relation_hits.iter().map(|hit| hit.relation_id).collect::<Vec<_>>();
        if !crate::infra::postgres::pg_search_store::knowledge_graph_targets_are_current(
            &state.persistence.postgres,
            library_id,
            &entity_ids,
            &relation_ids,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "search visibility lookup failed"))?
        {
            return Err(ApiError::knowledge_not_ready("search sources changed; retry the search"));
        }
    }
    // Vector hits are serialized independently of the truncated document page.
    // Freeze and revalidate both wire lists, never a replacement/current revision.
    let vector_sources = result
        .0
        .vector_chunk_hits
        .iter()
        .chain(result.0.document_hits.iter().flat_map(|hit| hit.vector_chunk_hits.iter()))
        .map(|hit| {
            crate::infra::repositories::content_fingerprint_repository::SelectedChunkSource {
                chunk_id: hit.chunk_id,
                revision_id: hit.revision_id,
                library_id: hit.library_id,
            }
        })
        .collect::<Vec<_>>();
    if !crate::infra::repositories::content_fingerprint_repository::chunk_sources_are_current(
        &state.persistence.postgres,
        library.workspace_id,
        library_id,
        &vector_sources,
    )
    .await
    .map_err(|error| ApiError::internal_with_log(error, "search vector visibility lookup failed"))?
    {
        return Err(ApiError::knowledge_not_ready("search sources changed; retry the search"));
    }
    tracing::Span::current().record("elapsed_ms", started_at.elapsed().as_millis() as u64);
    Ok(result)
}
