#![allow(
    clippy::drain_collect,
    clippy::map_unwrap_or,
    clippy::missing_const_for_fn,
    clippy::missing_errors_doc,
    clippy::needless_pass_by_value,
    clippy::too_many_lines
)]

use std::sync::Arc;

use anyhow::{Context, anyhow};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::infra::arangodb::{
    client::ArangoClient,
    collections::{
        KNOWLEDGE_CHUNK_COLLECTION, KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
        KNOWLEDGE_ENTITY_VECTOR_COLLECTION, KNOWLEDGE_NGRAM_ANALYZER, KNOWLEDGE_SEARCH_VIEW,
    },
};

const TITLE_NGRAM_MIN_TERM_CHARS: usize = 8;
const TITLE_NGRAM_MAX_TERMS: usize = 4;
const TITLE_IDENTITY_MAX_TERMS: usize = 6;

/// ANN over-fetch when no temporal hard-filter is active. 8× covers the
/// typical post-rank dedup cliff while staying inside Arango's per-query
/// budget on a 5-turn concurrency floor.
const VECTOR_OVER_FETCH_DEFAULT_FACTOR: usize = 8;
const VECTOR_OVER_FETCH_DEFAULT_FLOOR: usize = 64;

/// Hard cap on `over_fetch` regardless of which factor applied. Without
/// it, a pathological caller passing `limit=1000` would queue a 32 000-row
/// post-filter that can hold an Arango worker thread for seconds. 8 192
/// keeps a 256 GB Arango heap-budget headroom even on the temporal lane.
const VECTOR_OVER_FETCH_MAX: usize = 8_192;

// TODO[T1.x-followup]: extract the duplicated `FILTER (@temporal_start_iso ==
// null AND @temporal_end_iso == null) OR (X.occurred_at != null AND ...)`
// snippet from the four lexical lanes of `search_chunks` (text view,
// title-identity, title-soft, backstop fallback) and the vector ANN sieve
// into a shared helper string. Architect-amendment 1 will mirror
// `occurred_at` / `occurred_until` onto `KnowledgeChunkVectorRow`, which
// removes the vector-side `DOCUMENT()` lookup and lets all five sites use
// the same filter shape against the iteration variable directly. Until
// that lands, the filter snippet stays duplicated (5 sites) so each AQL
// stays a single string literal — restructuring before the schema change
// would force a `format!`-based AQL build that this diff is not large
// enough to also include safely.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeChunkVectorRow {
    #[serde(rename = "_key")]
    pub key: String,
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    pub arango_id: Option<String>,
    #[serde(rename = "_rev", default, skip_serializing_if = "Option::is_none")]
    pub arango_rev: Option<String>,
    pub vector_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub chunk_id: Uuid,
    pub revision_id: Uuid,
    pub embedding_model_key: String,
    pub vector_kind: String,
    pub dimensions: i32,
    pub vector: Vec<f32>,
    pub freshness_generation: i64,
    pub created_at: DateTime<Utc>,
    /// Mirror of `KnowledgeChunkRow.occurred_at` so the ANN post-filter
    /// can hard-bound by time without a per-candidate `DOCUMENT()`
    /// lookup. JSONL ingest only; None for non-temporal sources.
    /// (Architect-amendment-1.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurred_at: Option<DateTime<Utc>>,
    /// Mirror of `KnowledgeChunkRow.occurred_until`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurred_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeEntityVectorRow {
    #[serde(rename = "_key")]
    pub key: String,
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    pub arango_id: Option<String>,
    #[serde(rename = "_rev", default, skip_serializing_if = "Option::is_none")]
    pub arango_rev: Option<String>,
    pub vector_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub entity_id: Uuid,
    pub embedding_model_key: String,
    pub vector_kind: String,
    pub dimensions: i32,
    pub vector: Vec<f32>,
    pub freshness_generation: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeChunkSearchRow {
    pub chunk_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub revision_id: Uuid,
    pub content_text: String,
    pub normalized_text: String,
    pub section_path: Vec<String>,
    pub heading_trail: Vec<String>,
    pub score: f64,
    #[serde(default)]
    pub quality_score: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeStructuredBlockSearchRow {
    pub block_id: Uuid,
    pub document_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub revision_id: Uuid,
    pub ordinal: i32,
    pub block_kind: String,
    pub text: String,
    pub normalized_text: String,
    pub section_path: Vec<String>,
    pub heading_trail: Vec<String>,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeTechnicalFactSearchRow {
    pub fact_id: Uuid,
    pub document_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub revision_id: Uuid,
    pub fact_kind: String,
    pub canonical_value_text: String,
    pub display_value: String,
    pub exact_match: bool,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeEntitySearchRow {
    pub entity_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub canonical_label: String,
    pub entity_type: String,
    pub summary: Option<String>,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeRelationSearchRow {
    pub relation_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub predicate: String,
    pub normalized_assertion: String,
    pub summary: Option<String>,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeChunkVectorSearchRow {
    pub vector_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub chunk_id: Uuid,
    pub revision_id: Uuid,
    pub embedding_model_key: String,
    pub vector_kind: String,
    pub freshness_generation: i64,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeEntityVectorSearchRow {
    pub vector_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub entity_id: Uuid,
    pub embedding_model_key: String,
    pub vector_kind: String,
    pub freshness_generation: i64,
    pub score: f64,
}

#[derive(Clone)]
pub struct ArangoSearchStore {
    client: Arc<ArangoClient>,
}

impl ArangoSearchStore {
    #[must_use]
    pub fn new(client: Arc<ArangoClient>) -> Self {
        Self { client }
    }

    #[must_use]
    pub fn client(&self) -> &Arc<ArangoClient> {
        &self.client
    }

    pub async fn upsert_chunk_vector(
        &self,
        row: &KnowledgeChunkVectorRow,
    ) -> anyhow::Result<KnowledgeChunkVectorRow> {
        let cursor = self
            .client
            .query_json(
                "UPSERT { _key: @key }
                 INSERT {
                    _key: @key,
                    vector_id: @vector_id,
                    workspace_id: @workspace_id,
                    library_id: @library_id,
                    chunk_id: @chunk_id,
                    revision_id: @revision_id,
                    embedding_model_key: @embedding_model_key,
                    vector_kind: @vector_kind,
                    dimensions: @dimensions,
                    vector: @vector,
                    freshness_generation: @freshness_generation,
                    created_at: @created_at,
                    occurred_at: @occurred_at,
                    occurred_until: @occurred_until
                 }
                 UPDATE {
                    workspace_id: @workspace_id,
                    library_id: @library_id,
                    chunk_id: @chunk_id,
                    revision_id: @revision_id,
                    embedding_model_key: @embedding_model_key,
                    vector_kind: @vector_kind,
                    dimensions: @dimensions,
                    vector: @vector,
                    freshness_generation: @freshness_generation,
                    occurred_at: @occurred_at,
                    occurred_until: @occurred_until
                 }
                 IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
                    "key": row.key,
                    "vector_id": row.vector_id,
                    "workspace_id": row.workspace_id,
                    "library_id": row.library_id,
                    "chunk_id": row.chunk_id,
                    "revision_id": row.revision_id,
                    "embedding_model_key": row.embedding_model_key,
                    "vector_kind": row.vector_kind,
                    "dimensions": row.dimensions,
                    "vector": row.vector,
                    "freshness_generation": row.freshness_generation,
                    "created_at": row.created_at,
                    "occurred_at": row.occurred_at,
                    "occurred_until": row.occurred_until,
                }),
            )
            .await
            .context("failed to upsert knowledge chunk vector")?;
        decode_single_result(cursor)
    }

    /// Bulk UPSERT of chunk vector rows. One AQL round-trip replaces N
    /// sequential `upsert_chunk_vector` calls — on a typical embed
    /// batch (16 vectors) this collapses 16 serial `UPSERT .. IN ..
    /// RETURN NEW` round-trips into one, cutting the Arango I/O tail
    /// on the chunk embed stage from O(chunks × RTT) to O(chunks /
    /// batch_size × RTT). Payload per request is roughly
    /// `len × dimensions × 4 bytes` plus metadata — 16 × 3072 × 4 =
    /// ~192 KB, comfortably inside the default Arango request size.
    pub async fn upsert_chunk_vectors_bulk(
        &self,
        rows: &[KnowledgeChunkVectorRow],
    ) -> anyhow::Result<Vec<KnowledgeChunkVectorRow>> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        // Note on field names: `KnowledgeChunkVectorRow` serialises its
        // key column as `_key` (serde rename) to match Arango's
        // canonical document-key field. That means inside the AQL body
        // we read `row._key`, NOT `row.key` — the latter is
        // `null`, which Arango rejects at runtime with "illegal
        // document key". A first deploy of this function used
        // `row.key` and collapsed every bulk embed batch on prod.
        let cursor = self
            .client
            .query_json(
                "FOR row IN @rows
                 UPSERT { _key: row._key }
                 INSERT {
                    _key: row._key,
                    vector_id: row.vector_id,
                    workspace_id: row.workspace_id,
                    library_id: row.library_id,
                    chunk_id: row.chunk_id,
                    revision_id: row.revision_id,
                    embedding_model_key: row.embedding_model_key,
                    vector_kind: row.vector_kind,
                    dimensions: row.dimensions,
                    vector: row.vector,
                    freshness_generation: row.freshness_generation,
                    created_at: row.created_at,
                    occurred_at: row.occurred_at,
                    occurred_until: row.occurred_until
                 }
                 UPDATE {
                    workspace_id: row.workspace_id,
                    library_id: row.library_id,
                    chunk_id: row.chunk_id,
                    revision_id: row.revision_id,
                    embedding_model_key: row.embedding_model_key,
                    vector_kind: row.vector_kind,
                    dimensions: row.dimensions,
                    vector: row.vector,
                    freshness_generation: row.freshness_generation,
                    occurred_at: row.occurred_at,
                    occurred_until: row.occurred_until
                 }
                 IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
                    "rows": rows,
                }),
            )
            .await
            .context("failed to bulk-upsert knowledge chunk vectors")?;
        decode_many_results(cursor)
    }

    pub async fn delete_chunk_vector(
        &self,
        chunk_id: Uuid,
        embedding_model_key: &str,
        freshness_generation: i64,
    ) -> anyhow::Result<Option<KnowledgeChunkVectorRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR vector IN @@collection
                 FILTER vector.chunk_id == @chunk_id
                   AND vector.embedding_model_key == @embedding_model_key
                   AND vector.freshness_generation == @freshness_generation
                 LIMIT 1
                 REMOVE vector IN @@collection
                 RETURN OLD",
                serde_json::json!({
                    "@collection": KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
                    "chunk_id": chunk_id,
                    "embedding_model_key": embedding_model_key,
                    "freshness_generation": freshness_generation,
                }),
            )
            .await
            .context("failed to delete knowledge chunk vector")?;
        decode_optional_single_result(cursor)
    }

    pub async fn list_chunk_vectors_by_chunk(
        &self,
        chunk_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeChunkVectorRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR vector IN @@collection
                 FILTER vector.chunk_id == @chunk_id
                 SORT vector.freshness_generation DESC, vector.created_at DESC
                 RETURN vector",
                serde_json::json!({
                    "@collection": KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
                    "chunk_id": chunk_id,
                }),
            )
            .await
            .context("failed to list knowledge chunk vectors")?;
        decode_many_results(cursor)
    }

    pub async fn list_chunk_vectors_by_chunks(
        &self,
        chunk_ids: &[Uuid],
        embedding_model_key: &str,
        vector_kind: &str,
    ) -> anyhow::Result<Vec<KnowledgeChunkVectorRow>> {
        if chunk_ids.is_empty() {
            return Ok(Vec::new());
        }
        let cursor = self
            .client
            .query_json(
                "FOR vector IN @@collection
                 FILTER vector.chunk_id IN @chunk_ids
                   AND vector.embedding_model_key == @embedding_model_key
                   AND vector.vector_kind == @vector_kind
                 SORT vector.chunk_id ASC, vector.freshness_generation DESC, vector.created_at DESC
                 RETURN vector",
                serde_json::json!({
                    "@collection": KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
                    "chunk_ids": chunk_ids,
                    "embedding_model_key": embedding_model_key,
                    "vector_kind": vector_kind,
                }),
            )
            .await
            .context("failed to list knowledge chunk vectors by chunk batch")?;
        decode_many_results(cursor)
    }

    pub async fn count_chunk_vectors_by_revision(
        &self,
        revision_id: Uuid,
        embedding_model_key: &str,
        vector_kind: &str,
        freshness_generation: i64,
    ) -> anyhow::Result<usize> {
        let cursor = self
            .client
            .query_json(
                "FOR vector IN @@collection
                 FILTER vector.revision_id == @revision_id
                   AND vector.embedding_model_key == @embedding_model_key
                   AND vector.vector_kind == @vector_kind
                   AND vector.freshness_generation == @freshness_generation
                 COLLECT WITH COUNT INTO vector_count
                 RETURN vector_count",
                serde_json::json!({
                    "@collection": KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
                    "revision_id": revision_id,
                    "embedding_model_key": embedding_model_key,
                    "vector_kind": vector_kind,
                    "freshness_generation": freshness_generation,
                }),
            )
            .await
            .context("failed to count chunk vectors by revision")?;
        let count: i64 = decode_single_result(cursor)?;
        usize::try_from(count).context("chunk vector count overflowed usize")
    }

    pub async fn upsert_entity_vector(
        &self,
        row: &KnowledgeEntityVectorRow,
    ) -> anyhow::Result<KnowledgeEntityVectorRow> {
        let cursor = self
            .client
            .query_json(
                "UPSERT { _key: @key }
                 INSERT {
                    _key: @key,
                    vector_id: @vector_id,
                    workspace_id: @workspace_id,
                    library_id: @library_id,
                    entity_id: @entity_id,
                    embedding_model_key: @embedding_model_key,
                    vector_kind: @vector_kind,
                    dimensions: @dimensions,
                    vector: @vector,
                    freshness_generation: @freshness_generation,
                    created_at: @created_at
                 }
                 UPDATE {
                    workspace_id: @workspace_id,
                    library_id: @library_id,
                    entity_id: @entity_id,
                    embedding_model_key: @embedding_model_key,
                    vector_kind: @vector_kind,
                    dimensions: @dimensions,
                    vector: @vector,
                    freshness_generation: @freshness_generation
                 }
                 IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_ENTITY_VECTOR_COLLECTION,
                    "key": row.key,
                    "vector_id": row.vector_id,
                    "workspace_id": row.workspace_id,
                    "library_id": row.library_id,
                    "entity_id": row.entity_id,
                    "embedding_model_key": row.embedding_model_key,
                    "vector_kind": row.vector_kind,
                    "dimensions": row.dimensions,
                    "vector": row.vector,
                    "freshness_generation": row.freshness_generation,
                    "created_at": row.created_at,
                }),
            )
            .await
            .context("failed to upsert knowledge entity vector")?;
        decode_single_result(cursor)
    }

    pub async fn delete_entity_vector(
        &self,
        entity_id: Uuid,
        embedding_model_key: &str,
        freshness_generation: i64,
    ) -> anyhow::Result<Option<KnowledgeEntityVectorRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR vector IN @@collection
                 FILTER vector.entity_id == @entity_id
                   AND vector.embedding_model_key == @embedding_model_key
                   AND vector.freshness_generation == @freshness_generation
                 LIMIT 1
                 REMOVE vector IN @@collection
                 RETURN OLD",
                serde_json::json!({
                    "@collection": KNOWLEDGE_ENTITY_VECTOR_COLLECTION,
                    "entity_id": entity_id,
                    "embedding_model_key": embedding_model_key,
                    "freshness_generation": freshness_generation,
                }),
            )
            .await
            .context("failed to delete knowledge entity vector")?;
        decode_optional_single_result(cursor)
    }

    pub async fn delete_entity_vectors_by_library(&self, library_id: Uuid) -> anyhow::Result<()> {
        let cursor = self
            .client
            .query_json(
                "FOR vector IN @@collection
                 FILTER vector.library_id == @library_id
                 REMOVE vector IN @@collection
                 RETURN OLD._key",
                serde_json::json!({
                    "@collection": KNOWLEDGE_ENTITY_VECTOR_COLLECTION,
                    "library_id": library_id,
                }),
            )
            .await
            .context("failed to delete knowledge entity vectors by library")?;
        let _: Vec<String> = decode_many_results(cursor)?;
        Ok(())
    }

    pub async fn list_entity_vectors_by_entity(
        &self,
        entity_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityVectorRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR vector IN @@collection
                 FILTER vector.entity_id == @entity_id
                 SORT vector.freshness_generation DESC, vector.created_at DESC
                 LIMIT 1000
                 RETURN vector",
                serde_json::json!({
                    "@collection": KNOWLEDGE_ENTITY_VECTOR_COLLECTION,
                    "entity_id": entity_id,
                }),
            )
            .await
            .context("failed to list knowledge entity vectors")?;
        decode_many_results(cursor)
    }

    pub async fn search_chunks(
        &self,
        library_id: Uuid,
        query: &str,
        limit: usize,
        temporal_start: Option<chrono::DateTime<chrono::Utc>>,
        temporal_end: Option<chrono::DateTime<chrono::Utc>>,
    ) -> anyhow::Result<Vec<KnowledgeChunkSearchRow>> {
        let normalized_limit = limit.max(1);
        let query_lower = query.trim().to_lowercase();
        let query_terms = lexical_query_terms(query);
        let title_ngram_terms = title_ngram_terms(&query_terms);
        let title_identity_terms = title_identity_terms(query, &query_terms);
        let title_soft_raw_enabled = title_soft_raw_enabled(&query_terms);
        // Hard-filter on `occurred_at` / `occurred_until` is applied to
        // every chunk-touching lane (text view, title-identity, title-soft,
        // backstop). When the user explicitly scopes a question to a date
        // range the LLM compiler populates `QueryIR.temporal_constraints`,
        // and retrieve.rs threads the resolved typed bounds in here. RFC3339
        // strings sort lexicographically equal to chronological order, so a
        // single `>=` / `<=` comparison covers any precision.
        let temporal_start_iso = temporal_start.map(|value| value.to_rfc3339());
        let temporal_end_iso = temporal_end.map(|value| value.to_rfc3339());
        // Over-fetch by 4x so the per-document dedup below has a
        // realistic candidate pool. On short configure-style queries,
        // bare BM25 ranking can fill the first slots with chunks from
        // one analyzer-collision document, drowning out the actual
        // answer. With over-fetch followed by per-document limiting,
        // the final slots go to different documents ranked by their top
        // chunk's BM25.
        let over_fetch = normalized_limit.saturating_mul(4).max(48);
        let title_ngram_0 = title_ngram_terms.first().map_or("", String::as_str);
        let title_ngram_1 = title_ngram_terms.get(1).map_or("", String::as_str);
        let title_ngram_2 = title_ngram_terms.get(2).map_or("", String::as_str);
        let title_ngram_3 = title_ngram_terms.get(3).map_or("", String::as_str);
        let cursor = self
            .client
            .query_json(
                "/* Title-match subquery. Docs whose `title` or
                    `file_name` contains query tokens get a title lane.
                    Only `title_identity_docs` receives identity-scale
                    scores. Broad token/fuzzy matches stay as ordinary
                    relevance boosts so generic multi-document questions
                    cannot collapse into arbitrary title collisions. */
                 LET token_title_match_docs = (
                   FOR d IN @@view
                     SEARCH d.library_id == @library_id
                       AND d.document_state == 'active'
                       AND (d.title != null OR d.file_name != null)
                       AND (
                            ANALYZER(d.title IN TOKENS(@query, 'text_ru'), 'text_ru')
                            OR ANALYZER(d.title IN TOKENS(@query, 'text_en'), 'text_en')
                            OR ANALYZER(d.file_name IN TOKENS(@query, 'text_ru'), 'text_ru')
                            OR ANALYZER(d.file_name IN TOKENS(@query, 'text_en'), 'text_en')
                            OR ANALYZER(PHRASE(d.title, @query, 'text_ru'), 'text_ru')
                            OR ANALYZER(PHRASE(d.title, @query, 'text_en'), 'text_en')
                       )
                     LIMIT 50
                     RETURN d.document_id
                 )
                 LET title_identity_docs = (
                   FOR d IN @@view
                     SEARCH d.library_id == @library_id
                       AND d.document_state == 'active'
                       AND (d.title != null OR d.file_name != null)
                       AND (
                            ANALYZER(d.title IN TOKENS(@query, 'text_ru'), 'text_ru')
                            OR ANALYZER(d.title IN TOKENS(@query, 'text_en'), 'text_en')
                            OR ANALYZER(d.file_name IN TOKENS(@query, 'text_ru'), 'text_ru')
                            OR ANALYZER(d.file_name IN TOKENS(@query, 'text_en'), 'text_en')
                            OR ANALYZER(PHRASE(d.title, @query, 'text_ru'), 'text_ru')
                            OR ANALYZER(PHRASE(d.title, @query, 'text_en'), 'text_en')
                       )
                     LET title_blob = LOWER(CONCAT_SEPARATOR(
                       ' ',
                       d.title != null ? d.title : '',
                       d.file_name != null ? d.file_name : ''
                     ))
                     LET padded_title_blob = CONCAT(' ', title_blob, ' ')
                     LET identity_term_hits = LENGTH(
                       FOR term IN @title_identity_terms
                         FILTER (REGEX_TEST(term, '\\\\d')
                           ? CONTAINS(padded_title_blob, CONCAT(' ', term, ' '))
                           : CONTAINS(title_blob, term))
                         LIMIT @title_identity_term_count
                         RETURN 1
                     )
                     FILTER @title_identity_term_count > 0
                       AND identity_term_hits == @title_identity_term_count
                     LIMIT 50
                     RETURN d.document_id
                 )
                 LET fuzzy_title_match_docs = (
                   FOR d IN @@view
                     SEARCH d.library_id == @library_id
                       AND d.document_state == 'active'
                       AND (d.title != null OR d.file_name != null)
                       AND (
                            /* Trigram-level fuzzy match covers small
                               spelling variants the stemmers miss. It
                               only receives long query terms, so
                               low-signal suffix words do not enter the
                               document-identity lane and outrank exact
                               release/version documents. */
                            (@title_ngram_0 != '' AND (
                                ANALYZER(NGRAM_MATCH(d.title, @title_ngram_0, 0.55, @ngram_analyzer), @ngram_analyzer)
                                OR ANALYZER(NGRAM_MATCH(d.file_name, @title_ngram_0, 0.55, @ngram_analyzer), @ngram_analyzer)
                            ))
                            OR (@title_ngram_1 != '' AND (
                                ANALYZER(NGRAM_MATCH(d.title, @title_ngram_1, 0.55, @ngram_analyzer), @ngram_analyzer)
                                OR ANALYZER(NGRAM_MATCH(d.file_name, @title_ngram_1, 0.55, @ngram_analyzer), @ngram_analyzer)
                            ))
                            OR (@title_ngram_2 != '' AND (
                                ANALYZER(NGRAM_MATCH(d.title, @title_ngram_2, 0.55, @ngram_analyzer), @ngram_analyzer)
                                OR ANALYZER(NGRAM_MATCH(d.file_name, @title_ngram_2, 0.55, @ngram_analyzer), @ngram_analyzer)
                            ))
                            OR (@title_ngram_3 != '' AND (
                                ANALYZER(NGRAM_MATCH(d.title, @title_ngram_3, 0.55, @ngram_analyzer), @ngram_analyzer)
                                OR ANALYZER(NGRAM_MATCH(d.file_name, @title_ngram_3, 0.55, @ngram_analyzer), @ngram_analyzer)
                            ))
                       )
                     LIMIT 50
                     RETURN d.document_id
                 )
                 LET title_match_docs = UNION_DISTINCT(token_title_match_docs, fuzzy_title_match_docs)
                 LET soft_title_match_docs = MINUS(title_match_docs, title_identity_docs)
                 LET text_raw = (
                   FOR doc IN @@view
                     SEARCH doc.library_id == @library_id
                       AND doc.chunk_id != null
                       AND doc.chunk_state == 'ready'
                       AND (
                            ANALYZER(doc.normalized_text IN TOKENS(@query, 'text_en'), 'text_en')
                            OR ANALYZER(doc.content_text IN TOKENS(@query, 'text_en'), 'text_en')
                            OR ANALYZER(doc.normalized_text IN TOKENS(@query, 'text_ru'), 'text_ru')
                            OR ANALYZER(doc.content_text IN TOKENS(@query, 'text_ru'), 'text_ru')
                            OR
                            ANALYZER(PHRASE(doc.normalized_text, @query, 'text_en'), 'text_en')
                            OR ANALYZER(PHRASE(doc.content_text, @query, 'text_en'), 'text_en')
                            OR ANALYZER(PHRASE(doc.normalized_text, @query, 'text_ru'), 'text_ru')
                            OR ANALYZER(PHRASE(doc.content_text, @query, 'text_ru'), 'text_ru')
                       )
                     /* Temporal hard-filter (T1.4): when QueryIR carries
                        resolved RFC3339 bounds, exclude chunks whose
                        [occurred_at, occurred_until] does not overlap the
                        requested range. Chunks without occurred_at are
                        excluded only when bounds are present so date-only
                        questions don't bleed into non-temporal corpora. */
                     FILTER (@temporal_start_iso == null AND @temporal_end_iso == null)
                         OR (
                              doc.occurred_at != null
                              AND (@temporal_start_iso == null
                                   OR (doc.occurred_until != null ? doc.occurred_until : doc.occurred_at) >= @temporal_start_iso)
                              AND (@temporal_end_iso == null
                                   OR doc.occurred_at <= @temporal_end_iso)
                         )
                     LET base_score = BM25(doc)
                     /* Title boost (doc-level) dominates heading boost
                        (chunk-local). When both fire they do NOT
                        compose multiplicatively — we take the max so a
                        title hit stays at 8× and a title-miss with
                        heading hit stays at 3×. Substring-on-blob
                        heading match is a pragmatic fallback for
                        queries whose winning signal lives inside a
                        chunk's local heading/section path rather than
                        the document title — same caveat about stem-
                        token noise, kept conservative at 3×. */
                     LET q_tokens = (
                       FOR t IN TOKENS(LOWER(@query), 'text_ru')
                         FILTER LENGTH(t) >= 3
                         RETURN t
                     )
                     LET heading_blob = LENGTH(doc.heading_trail) > 0
                       ? LOWER(CONCAT_SEPARATOR(' ', doc.heading_trail))
                       : ''
                     LET section_blob = LENGTH(doc.section_path) > 0
                       ? LOWER(CONCAT_SEPARATOR(' ', doc.section_path))
                       : ''
                     LET heading_token_match = heading_blob != '' AND LENGTH(
                       FOR t IN q_tokens FILTER CONTAINS(heading_blob, t) LIMIT 1 RETURN 1
                     ) > 0
                     LET section_token_match = section_blob != '' AND LENGTH(
                       FOR t IN q_tokens FILTER CONTAINS(section_blob, t) LIMIT 1 RETURN 1
                     ) > 0
                     LET title_identity_match = doc.document_id IN title_identity_docs
                     LET title_soft_match = doc.document_id IN soft_title_match_docs
                     LET identity_boost = title_identity_match
                       ? 8.0
                       : (title_soft_match ? 2.0 : (heading_token_match ? 3.0 : 1.0))
                     LET section_boost = section_token_match ? 1.5 : 1.0
                     LET quality_boost = doc.quality_score != null ? doc.quality_score : 1.0
                     LET score = base_score * identity_boost * section_boost * quality_boost
                     SORT score DESC, doc.chunk_id ASC
                     LIMIT @over_fetch
                     RETURN {
                        chunk_id: doc.chunk_id,
                        workspace_id: doc.workspace_id,
                        library_id: doc.library_id,
                        document_id: doc.document_id,
                        revision_id: doc.revision_id,
                        content_text: doc.content_text,
                        normalized_text: doc.normalized_text,
                        section_path: doc.section_path,
                        heading_trail: doc.heading_trail,
                        score: score,
                        quality_score: doc.quality_score
                     }
                 )
                 LET title_identity_raw = (
                     FOR title_doc_id IN title_identity_docs
                       FOR title_chunk IN (
                         FOR chunk IN @@chunk_collection
                           FILTER chunk.library_id == @library_id
                             AND chunk.chunk_state == 'ready'
                             AND chunk.document_id == title_doc_id
                             AND ((@temporal_start_iso == null AND @temporal_end_iso == null)
                                  OR (chunk.occurred_at != null
                                      AND (@temporal_start_iso == null
                                           OR (chunk.occurred_until != null ? chunk.occurred_until : chunk.occurred_at) >= @temporal_start_iso)
                                      AND (@temporal_end_iso == null
                                           OR chunk.occurred_at <= @temporal_end_iso)))
                           LET quality_boost = chunk.quality_score != null ? chunk.quality_score : 1.0
                           LET score = (1000000 - chunk.chunk_index) * quality_boost
                           SORT score DESC, chunk.revision_id DESC, chunk.chunk_index ASC
                           LIMIT 2
                           RETURN {
                              chunk_id: chunk.chunk_id,
                              workspace_id: chunk.workspace_id,
                              library_id: chunk.library_id,
                              document_id: chunk.document_id,
                              revision_id: chunk.revision_id,
                              chunk_index: chunk.chunk_index,
                              content_text: chunk.content_text,
                              normalized_text: chunk.normalized_text,
                              section_path: chunk.section_path,
                              heading_trail: chunk.heading_trail,
                              score: score,
                              quality_score: chunk.quality_score
                           }
                       )
                       SORT title_chunk.score DESC, title_chunk.revision_id DESC, title_chunk.chunk_index ASC
                       LIMIT @over_fetch
                       RETURN title_chunk
                 )
                 LET title_soft_raw = (
                     FOR soft_doc_id IN soft_title_match_docs
                       FILTER @title_soft_raw_enabled
                       FOR title_chunk IN (
                         FOR chunk IN @@chunk_collection
                           FILTER chunk.library_id == @library_id
                             AND chunk.chunk_state == 'ready'
                             AND chunk.document_id == soft_doc_id
                             AND ((@temporal_start_iso == null AND @temporal_end_iso == null)
                                  OR (chunk.occurred_at != null
                                      AND (@temporal_start_iso == null
                                           OR (chunk.occurred_until != null ? chunk.occurred_until : chunk.occurred_at) >= @temporal_start_iso)
                                      AND (@temporal_end_iso == null
                                           OR chunk.occurred_at <= @temporal_end_iso)))
                           LET quality_boost = chunk.quality_score != null ? chunk.quality_score : 1.0
                           LET score = (50 - (chunk.chunk_index * 0.001)) * quality_boost
                           SORT score DESC, chunk.revision_id DESC, chunk.chunk_index ASC
                           LIMIT 2
                           RETURN {
                              chunk_id: chunk.chunk_id,
                              workspace_id: chunk.workspace_id,
                              library_id: chunk.library_id,
                              document_id: chunk.document_id,
                              revision_id: chunk.revision_id,
                              chunk_index: chunk.chunk_index,
                              content_text: chunk.content_text,
                              normalized_text: chunk.normalized_text,
                              section_path: chunk.section_path,
                              heading_trail: chunk.heading_trail,
                              score: score,
                              quality_score: chunk.quality_score
                           }
                       )
                       SORT title_chunk.score DESC, title_chunk.revision_id DESC, title_chunk.chunk_index ASC
                       LIMIT @over_fetch
                       RETURN title_chunk
                 )
                 LET raw = APPEND(text_raw, APPEND(title_identity_raw, title_soft_raw))
                 /* Per-document dedup: keep at most 2 chunks per
                   document_id out of `raw`, then return @limit. Runs
                    inside Arango so we never ship 48 rows over the
                    wire when the client only needs 12 diverse ones. */
                 LET diversified = (
                   FOR r IN raw
                     COLLECT doc = r.document_id INTO per_doc = r
                     FOR c IN (
                       FOR x IN per_doc
                         SORT x.score DESC, x.chunk_id ASC
                         LIMIT 2
                         RETURN x
                     )
                     RETURN c
                 )
                 FOR r IN diversified
                   SORT r.score DESC, r.chunk_id ASC
                   LIMIT @limit
                   RETURN {
                      chunk_id: r.chunk_id,
                      workspace_id: r.workspace_id,
                      library_id: r.library_id,
                      revision_id: r.revision_id,
                      content_text: r.content_text,
                      normalized_text: r.normalized_text,
                      section_path: r.section_path,
                      heading_trail: r.heading_trail,
                      score: r.score,
                      quality_score: r.quality_score
                   }",
                serde_json::json!({
                    "@view": KNOWLEDGE_SEARCH_VIEW,
                    "@chunk_collection": KNOWLEDGE_CHUNK_COLLECTION,
                    "library_id": library_id,
                    "query": query,
                    "limit": normalized_limit,
                    "over_fetch": over_fetch,
                    "ngram_analyzer": KNOWLEDGE_NGRAM_ANALYZER,
                    "title_ngram_0": title_ngram_0,
                    "title_ngram_1": title_ngram_1,
                    "title_ngram_2": title_ngram_2,
                    "title_ngram_3": title_ngram_3,
                    "title_identity_terms": title_identity_terms,
                    "title_identity_term_count": title_identity_terms.len(),
                    "title_soft_raw_enabled": title_soft_raw_enabled,
                    "temporal_start_iso": temporal_start_iso,
                    "temporal_end_iso": temporal_end_iso,
                }),
            )
            .await
            .context("failed to search knowledge chunks")?;
        let rows = decode_many_results(cursor)?;
        if query_lower.is_empty() || !rows.is_empty() {
            // View is the canonical lexical lane. If it returned anything at all
            // we trust it — BM25 + title_match_docs already prioritise fresh
            // exact hits over stale stem matches, and falling back to a full
            // CONTAINS scan over `knowledge_chunk` is an O(chunks × terms)
            // operation that saturates the Arango request timeout on any
            // non-trivial library (observed 18–26 s on a 60k-chunk corpus for
            // 20-token clarify-context follow-ups).
            return Ok(rows);
        }

        // Backstop: the ArangoSearch view can briefly lag behind chunk writes
        // (commitIntervalMsec window). Only when the view returns zero rows do
        // we run a direct collection scan so freshly written chunks remain
        // retrievable. Terms are capped so a clarify-context follow-up cannot
        // explode this into a 25 s scan.
        const FALLBACK_MAX_TERMS: usize = 8;
        const FALLBACK_MAX_QUERY_LOWER_LEN: usize = 128;
        let mut fallback_terms: Vec<String> = query_terms.to_vec();
        fallback_terms.sort_by_key(|t| std::cmp::Reverse(t.chars().count()));
        fallback_terms.truncate(FALLBACK_MAX_TERMS);
        let fallback_query_lower: String =
            query_lower.chars().take(FALLBACK_MAX_QUERY_LOWER_LEN).collect();
        let cursor = self
            .client
            .query_json(
                "FOR chunk IN @@collection
                 FILTER chunk.library_id == @library_id
                   AND chunk.chunk_state == 'ready'
                   AND ((@temporal_start_iso == null AND @temporal_end_iso == null)
                        OR (chunk.occurred_at != null
                            AND (@temporal_start_iso == null
                                 OR (chunk.occurred_until != null ? chunk.occurred_until : chunk.occurred_at) >= @temporal_start_iso)
                            AND (@temporal_end_iso == null
                                 OR chunk.occurred_at <= @temporal_end_iso)))
                 LET normalized_lower = LOWER(chunk.normalized_text)
                 LET content_lower = LOWER(chunk.content_text)
                 LET matched_terms = UNIQUE(
                    FOR term IN @query_terms
                      FILTER CONTAINS(normalized_lower, term)
                         OR CONTAINS(content_lower, term)
                      RETURN term
                 )
                 FILTER LENGTH(matched_terms) > 0
                 LET exact_match =
                    CONTAINS(normalized_lower, @query_lower)
                    OR CONTAINS(content_lower, @query_lower)
                 LET earliest_pos = MIN(
                    FOR term IN matched_terms
                      LET normalized_pos = FIND_FIRST(normalized_lower, term)
                      LET content_pos = FIND_FIRST(content_lower, term)
                      RETURN MIN([
                        normalized_pos >= 0 ? normalized_pos : 2147483647,
                        content_pos >= 0 ? content_pos : 2147483647
                      ])
                 )
                 LET score =
                    (exact_match ? 1000000 : 0)
                    + (LENGTH(matched_terms) * 10000)
                    - earliest_pos
                 SORT score DESC, chunk.revision_id DESC, chunk.chunk_index ASC
                 LIMIT @limit
                 RETURN {
                    chunk_id: chunk.chunk_id,
                    workspace_id: chunk.workspace_id,
                    library_id: chunk.library_id,
                    revision_id: chunk.revision_id,
                    content_text: chunk.content_text,
                    normalized_text: chunk.normalized_text,
                    section_path: chunk.section_path,
                    heading_trail: chunk.heading_trail,
                    score: score
                 }",
                serde_json::json!({
                    "@collection": KNOWLEDGE_CHUNK_COLLECTION,
                    "library_id": library_id,
                    "query_lower": fallback_query_lower,
                    "query_terms": fallback_terms,
                    "limit": normalized_limit,
                    "temporal_start_iso": temporal_start_iso,
                    "temporal_end_iso": temporal_end_iso,
                }),
            )
            .await
            .map_err(|err| {
                tracing::error!(
                    target: "ironrag::retrieval",
                    error = %err,
                    library_id = %library_id,
                    query_len = query.len(),
                    term_count = fallback_terms.len(),
                    "lexical chunk search fallback scan failed"
                );
                err
            })
            .context("failed to search knowledge chunks via direct fallback scan")?;
        let direct_rows: Vec<KnowledgeChunkSearchRow> = decode_many_results(cursor)?;
        if direct_rows.is_empty() {
            return Ok(rows);
        }

        let mut by_chunk_id = rows
            .into_iter()
            .map(|row| (row.chunk_id, row))
            .collect::<std::collections::HashMap<_, _>>();
        for row in direct_rows {
            match by_chunk_id.entry(row.chunk_id) {
                std::collections::hash_map::Entry::Occupied(mut existing) => {
                    if row.score > existing.get().score {
                        existing.insert(row);
                    }
                }
                std::collections::hash_map::Entry::Vacant(vacant) => {
                    vacant.insert(row);
                }
            }
        }

        let mut merged = by_chunk_id.into_values().collect::<Vec<_>>();
        merged.sort_by(|left, right| {
            right.score.total_cmp(&left.score).then_with(|| left.chunk_id.cmp(&right.chunk_id))
        });
        merged.truncate(normalized_limit);
        Ok(merged)
    }

    pub async fn search_structured_blocks(
        &self,
        library_id: Uuid,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockSearchRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR doc IN @@view
                 SEARCH doc.library_id == @library_id
                   AND doc.block_id != null
                   AND (
                        ANALYZER(doc.normalized_text IN TOKENS(@query, 'text_en'), 'text_en')
                        OR ANALYZER(doc.text IN TOKENS(@query, 'text_en'), 'text_en')
                        OR ANALYZER(doc.heading_trail[*] IN TOKENS(@query, 'text_en'), 'text_en')
                        OR ANALYZER(doc.section_path[*] IN TOKENS(@query, 'text_en'), 'text_en')
                        OR ANALYZER(doc.normalized_text IN TOKENS(@query, 'text_ru'), 'text_ru')
                        OR ANALYZER(doc.text IN TOKENS(@query, 'text_ru'), 'text_ru')
                        OR ANALYZER(doc.heading_trail[*] IN TOKENS(@query, 'text_ru'), 'text_ru')
                        OR ANALYZER(doc.section_path[*] IN TOKENS(@query, 'text_ru'), 'text_ru')
                        OR ANALYZER(PHRASE(doc.normalized_text, @query, 'text_en'), 'text_en')
                        OR ANALYZER(PHRASE(doc.normalized_text, @query, 'text_ru'), 'text_ru')
                   )
                 LET score = BM25(doc)
                 SORT score DESC, doc.revision_id DESC, doc.ordinal ASC, doc.block_id ASC
                 LIMIT @limit
                 RETURN {
                    block_id: doc.block_id,
                    document_id: doc.document_id,
                    workspace_id: doc.workspace_id,
                    library_id: doc.library_id,
                    revision_id: doc.revision_id,
                    ordinal: doc.ordinal,
                    block_kind: doc.block_kind,
                    text: doc.text,
                    normalized_text: doc.normalized_text,
                    section_path: doc.section_path,
                    heading_trail: doc.heading_trail,
                    score: score
                 }",
                serde_json::json!({
                    "@view": KNOWLEDGE_SEARCH_VIEW,
                    "library_id": library_id,
                    "query": query,
                    "limit": limit.max(1),
                }),
            )
            .await
            .context("failed to search structured blocks")?;
        decode_many_results(cursor)
    }

    pub async fn search_technical_facts(
        &self,
        library_id: Uuid,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactSearchRow>> {
        let query_exact = query.split_whitespace().collect::<String>();
        let cursor = self
            .client
            .query_json(
                "FOR doc IN @@view
                 SEARCH doc.library_id == @library_id
                   AND doc.fact_id != null
                   AND (
                        doc.canonical_value_exact == @query_exact
                        OR ANALYZER(doc.canonical_value_text IN TOKENS(@query, 'text_en'), 'text_en')
                        OR ANALYZER(doc.display_value IN TOKENS(@query, 'text_en'), 'text_en')
                        OR ANALYZER(doc.canonical_value_text IN TOKENS(@query, 'text_ru'), 'text_ru')
                        OR ANALYZER(doc.display_value IN TOKENS(@query, 'text_ru'), 'text_ru')
                        OR ANALYZER(PHRASE(doc.canonical_value_text, @query, 'text_en'), 'text_en')
                        OR ANALYZER(PHRASE(doc.display_value, @query, 'text_en'), 'text_en')
                        OR ANALYZER(PHRASE(doc.canonical_value_text, @query, 'text_ru'), 'text_ru')
                        OR ANALYZER(PHRASE(doc.display_value, @query, 'text_ru'), 'text_ru')
                   )
                 LET exact_match = doc.canonical_value_exact == @query_exact
                 LET quality_boost = doc.quality_score != null ? doc.quality_score : 1.0
                 LET score = ((exact_match ? 1000000 : 0) + BM25(doc)) * quality_boost
                 SORT score DESC, doc.fact_id ASC
                 LIMIT @limit
                 RETURN {
                    fact_id: doc.fact_id,
                    document_id: doc.document_id,
                    workspace_id: doc.workspace_id,
                    library_id: doc.library_id,
                    revision_id: doc.revision_id,
                    fact_kind: doc.fact_kind,
                    canonical_value_text: doc.canonical_value_text,
                    display_value: doc.display_value,
                    exact_match: exact_match,
                    score: score
                 }",
                serde_json::json!({
                    "@view": KNOWLEDGE_SEARCH_VIEW,
                    "library_id": library_id,
                    "query": query,
                    "query_exact": query_exact,
                    "limit": limit.max(1),
                }),
            )
            .await
            .context("failed to search technical facts")?;
        decode_many_results(cursor)
    }

    pub async fn search_entities(
        &self,
        library_id: Uuid,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeEntitySearchRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR doc IN @@view
                 SEARCH doc.library_id == @library_id
                   AND doc.entity_id != null
                   AND doc.entity_state == 'active'
                   AND (
                        ANALYZER(doc.canonical_label IN TOKENS(@query, 'text_en'), 'text_en')
                        OR ANALYZER(doc.summary IN TOKENS(@query, 'text_en'), 'text_en')
                        OR ANALYZER(doc.canonical_label IN TOKENS(@query, 'text_ru'), 'text_ru')
                        OR ANALYZER(doc.summary IN TOKENS(@query, 'text_ru'), 'text_ru')
                        OR
                        ANALYZER(PHRASE(doc.canonical_label, @query, 'text_en'), 'text_en')
                        OR ANALYZER(PHRASE(doc.summary, @query, 'text_en'), 'text_en')
                        OR ANALYZER(PHRASE(doc.canonical_label, @query, 'text_ru'), 'text_ru')
                        OR ANALYZER(PHRASE(doc.summary, @query, 'text_ru'), 'text_ru')
                   )
                 LET score = BM25(doc)
                 SORT score DESC, doc.entity_id ASC
                 LIMIT @limit
                 RETURN {
                    entity_id: doc.entity_id,
                    workspace_id: doc.workspace_id,
                    library_id: doc.library_id,
                    canonical_label: doc.canonical_label,
                    entity_type: doc.entity_type,
                    summary: doc.summary,
                    score: score
                 }",
                serde_json::json!({
                    "@view": KNOWLEDGE_SEARCH_VIEW,
                    "library_id": library_id,
                    "query": query,
                    "limit": limit.max(1),
                }),
            )
            .await
            .context("failed to search knowledge entities")?;
        decode_many_results(cursor)
    }

    pub async fn search_relations(
        &self,
        library_id: Uuid,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeRelationSearchRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR doc IN @@view
                 SEARCH doc.library_id == @library_id
                   AND doc.relation_id != null
                   AND doc.relation_state == 'active'
                   AND (
                        ANALYZER(doc.predicate IN TOKENS(@query, 'text_en'), 'text_en')
                        OR ANALYZER(doc.normalized_assertion IN TOKENS(@query, 'text_en'), 'text_en')
                        OR ANALYZER(doc.summary IN TOKENS(@query, 'text_en'), 'text_en')
                        OR ANALYZER(doc.predicate IN TOKENS(@query, 'text_ru'), 'text_ru')
                        OR ANALYZER(doc.normalized_assertion IN TOKENS(@query, 'text_ru'), 'text_ru')
                        OR ANALYZER(doc.summary IN TOKENS(@query, 'text_ru'), 'text_ru')
                        OR
                        ANALYZER(PHRASE(doc.predicate, @query, 'text_en'), 'text_en')
                        OR ANALYZER(PHRASE(doc.normalized_assertion, @query, 'text_en'), 'text_en')
                        OR ANALYZER(PHRASE(doc.summary, @query, 'text_en'), 'text_en')
                        OR ANALYZER(PHRASE(doc.predicate, @query, 'text_ru'), 'text_ru')
                        OR ANALYZER(PHRASE(doc.normalized_assertion, @query, 'text_ru'), 'text_ru')
                        OR ANALYZER(PHRASE(doc.summary, @query, 'text_ru'), 'text_ru')
                   )
                 LET score = BM25(doc)
                 SORT score DESC, doc.relation_id ASC
                 LIMIT @limit
                 RETURN {
                    relation_id: doc.relation_id,
                    workspace_id: doc.workspace_id,
                    library_id: doc.library_id,
                    predicate: doc.predicate,
                    normalized_assertion: doc.normalized_assertion,
                    summary: doc.summary,
                    score: score
                 }",
                serde_json::json!({
                    "@view": KNOWLEDGE_SEARCH_VIEW,
                    "library_id": library_id,
                    "query": query,
                    "limit": limit.max(1),
                }),
            )
            .await
            .context("failed to search knowledge relations")?;
        decode_many_results(cursor)
    }

    /// Runs APPROX_NEAR_COSINE over `knowledge_chunk_vector`, then post-
    /// filters the global ANN candidates by library + embedding model.
    ///
    /// Arango 3.12's vector index requires the `LET score =
    /// APPROX_NEAR_COSINE(...)` calculation to live in a `FOR` loop with
    /// no upstream FILTER on indexed columns; mixing them yields
    /// "AQL: failed vector search" at runtime. The canonical workaround
    /// (per Arango docs) is unfiltered ANN with over-fetch + an outer
    /// FILTER on the candidate set.
    ///
    /// We over-fetch by a constant factor so a heavily heterogeneous
    /// collection (multiple libraries, multiple embedding models)
    /// still has enough candidates after filtering to fill `@limit`.
    /// `freshness_generation` deliberately is NOT filtered here — it
    /// was the source of a previous incident where the eq-filter on
    /// library-wide MAX revision_number dropped most vectors on
    /// heterogeneous libraries with mixed per-document revision
    /// numbers. The coherence boundary is
    /// `document.readable_revision_id`, enforced downstream by
    /// `map_chunk_hit`.
    pub async fn search_chunk_vectors_by_similarity(
        &self,
        library_id: Uuid,
        embedding_model_key: &str,
        query_vector: &[f32],
        limit: usize,
        n_probe: Option<u64>,
        temporal_start: Option<chrono::DateTime<chrono::Utc>>,
        temporal_end: Option<chrono::DateTime<chrono::Utc>>,
    ) -> anyhow::Result<Vec<KnowledgeChunkVectorSearchRow>> {
        let limit = limit.max(1);
        // Larger over_fetch when temporal hard-filter is active so the
        // post-ANN sieve still has enough candidates to fill top-K after
        // dropping out-of-range chunks. ANN is ranked by cosine similarity
        // which has no temporal awareness, so low-score-but-in-range
        // chunks would otherwise never bubble up.
        // Architect-amendment-1: `occurred_at` and `occurred_until`
        // are mirrored onto the vector row at embed-write time, so the
        // post-ANN filter operates directly on the candidate doc and
        // we no longer need the 32× over_fetch cliff that the previous
        // `DOCUMENT()` lookup pattern required. Default 8× covers the
        // typical filter dropout while staying inside Arango's
        // per-query budget on a 5+ concurrency floor.
        let over_fetch = limit
            .saturating_mul(VECTOR_OVER_FETCH_DEFAULT_FACTOR)
            .max(VECTOR_OVER_FETCH_DEFAULT_FLOOR)
            .min(VECTOR_OVER_FETCH_MAX);
        let temporal_start_iso = temporal_start.map(|value| value.to_rfc3339());
        let temporal_end_iso = temporal_end.map(|value| value.to_rfc3339());
        let cursor = self
            .client
            .query_json(
                "LET candidates = (
                     FOR vector IN @@collection
                         LET score = APPROX_NEAR_COSINE(vector.vector, @query_vector, @options)
                         SORT score DESC
                         LIMIT @over_fetch
                         RETURN {
                             vector_id: vector.vector_id,
                             workspace_id: vector.workspace_id,
                             library_id: vector.library_id,
                             chunk_id: vector.chunk_id,
                             revision_id: vector.revision_id,
                             embedding_model_key: vector.embedding_model_key,
                             vector_kind: vector.vector_kind,
                             freshness_generation: vector.freshness_generation,
                             occurred_at: vector.occurred_at,
                             occurred_until: vector.occurred_until,
                             score: score
                         }
                 )
                 FOR c IN candidates
                     FILTER c.library_id == @library_id
                       AND c.embedding_model_key == @embedding_model_key
                     /* Temporal hard-filter: occurred_at / occurred_until
                        are mirrored onto the vector row by ingest, so the
                        sieve runs directly on the candidate doc — no
                        per-row DOCUMENT() lookup. Skipped when bounds
                        are not bound. */
                     FILTER (@temporal_start_iso == null AND @temporal_end_iso == null)
                         OR (c.occurred_at != null
                             AND (@temporal_start_iso == null
                                  OR (c.occurred_until != null ? c.occurred_until : c.occurred_at) >= @temporal_start_iso)
                             AND (@temporal_end_iso == null
                                  OR c.occurred_at <= @temporal_end_iso))
                     SORT c.score DESC, c.chunk_id ASC
                     LIMIT @limit
                     RETURN c",
                serde_json::json!({
                    "@collection": KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
                    "library_id": library_id,
                    "embedding_model_key": embedding_model_key,
                    "query_vector": query_vector,
                    "limit": limit,
                    "over_fetch": over_fetch,
                    "options": vector_search_options(n_probe),
                    "temporal_start_iso": temporal_start_iso,
                    "temporal_end_iso": temporal_end_iso,
                }),
            )
            .await
            .context("failed to search knowledge chunk vectors by similarity")?;
        decode_many_results(cursor)
    }

    /// See docs on `search_chunk_vectors_by_similarity` for why ANN runs
    /// before FILTER and why we over-fetch.
    pub async fn search_entity_vectors_by_similarity(
        &self,
        library_id: Uuid,
        embedding_model_key: &str,
        query_vector: &[f32],
        limit: usize,
        n_probe: Option<u64>,
    ) -> anyhow::Result<Vec<KnowledgeEntityVectorSearchRow>> {
        let limit = limit.max(1);
        let over_fetch = limit.saturating_mul(8).max(64);
        let cursor = self
            .client
            .query_json(
                "LET candidates = (
                     FOR vector IN @@collection
                         LET score = APPROX_NEAR_COSINE(vector.vector, @query_vector, @options)
                         SORT score DESC
                         LIMIT @over_fetch
                         RETURN {
                             vector_id: vector.vector_id,
                             workspace_id: vector.workspace_id,
                             library_id: vector.library_id,
                             entity_id: vector.entity_id,
                             embedding_model_key: vector.embedding_model_key,
                             vector_kind: vector.vector_kind,
                             freshness_generation: vector.freshness_generation,
                             score: score
                         }
                 )
                 FOR c IN candidates
                     FILTER c.library_id == @library_id
                       AND c.embedding_model_key == @embedding_model_key
                     SORT c.score DESC, c.entity_id ASC
                     LIMIT @limit
                     RETURN c",
                serde_json::json!({
                    "@collection": KNOWLEDGE_ENTITY_VECTOR_COLLECTION,
                    "library_id": library_id,
                    "embedding_model_key": embedding_model_key,
                    "query_vector": query_vector,
                    "limit": limit,
                    "over_fetch": over_fetch,
                    "options": vector_search_options(n_probe),
                }),
            )
            .await
            .context("failed to search knowledge entity vectors by similarity")?;
        decode_many_results(cursor)
    }
}

fn vector_search_options(n_probe: Option<u64>) -> serde_json::Value {
    n_probe
        .map(|n_probe| serde_json::json!({ "nProbe": n_probe }))
        .unwrap_or_else(|| serde_json::json!({}))
}

fn lexical_query_terms(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for token in query
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '/')
        .map(str::trim)
        .filter(|token| token.chars().count() >= 3)
        .map(str::to_lowercase)
    {
        if seen.insert(token.clone()) {
            terms.push(token);
        }
    }
    terms
}

fn title_ngram_terms(query_terms: &[String]) -> Vec<String> {
    let mut terms = query_terms
        .iter()
        .filter(|term| term.chars().count() >= TITLE_NGRAM_MIN_TERM_CHARS)
        .cloned()
        .collect::<Vec<_>>();
    terms.sort_by(|left, right| {
        right.chars().count().cmp(&left.chars().count()).then_with(|| left.cmp(right))
    });
    terms.truncate(TITLE_NGRAM_MAX_TERMS);
    terms
}

fn title_identity_terms(query: &str, query_terms: &[String]) -> Vec<String> {
    let numeric_literals = numeric_title_literals(query);
    if !numeric_literals.is_empty() {
        return numeric_literals;
    }
    if query_terms.len() > TITLE_IDENTITY_MAX_TERMS {
        return Vec::new();
    }
    query_terms.to_vec()
}

fn numeric_title_literals(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for token in query
        .split(|ch: char| !ch.is_alphanumeric() && ch != '.' && ch != '-' && ch != '_' && ch != '/')
        .map(|token| {
            token.trim_matches(|ch: char| {
                !ch.is_alphanumeric() && ch != '.' && ch != '-' && ch != '_' && ch != '/'
            })
        })
        .filter(|token| token.chars().count() >= 2)
        .filter(|token| token.chars().any(|ch| ch.is_ascii_digit()))
        .map(str::to_lowercase)
    {
        if seen.insert(token.clone()) {
            terms.push(token);
        }
    }
    terms
}

fn title_soft_raw_enabled(query_terms: &[String]) -> bool {
    query_terms.len() <= TITLE_IDENTITY_MAX_TERMS
}

fn decode_single_result<T>(cursor: serde_json::Value) -> anyhow::Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    decode_optional_single_result(cursor)?.ok_or_else(|| anyhow!("ArangoDB query returned no rows"))
}

fn decode_optional_single_result<T>(cursor: serde_json::Value) -> anyhow::Result<Option<T>>
where
    T: for<'de> Deserialize<'de>,
{
    let result = cursor
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow!("ArangoDB cursor response is missing result"))?;
    let mut rows: Vec<T> =
        serde_json::from_value(result).context("failed to decode ArangoDB result rows")?;
    Ok((!rows.is_empty()).then(|| rows.remove(0)))
}

fn decode_many_results<T>(cursor: serde_json::Value) -> anyhow::Result<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    let result = cursor
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow!("ArangoDB cursor response is missing result"))?;
    serde_json::from_value(result).context("failed to decode ArangoDB result rows")
}

#[cfg(test)]
mod tests {
    use super::{
        KnowledgeChunkVectorRow, KnowledgeEntityVectorRow, lexical_query_terms,
        numeric_title_literals, title_identity_terms, title_ngram_terms, title_soft_raw_enabled,
    };

    #[test]
    fn lexical_query_terms_deduplicate_repeated_tokens() {
        assert_eq!(
            lexical_query_terms("Server checkout-api /system/info endpoint server"),
            vec![
                "server".to_string(),
                "checkout".to_string(),
                "api".to_string(),
                "/system/info".to_string(),
                "endpoint".to_string(),
            ]
        );
    }

    #[test]
    fn title_ngram_terms_keep_longest_search_terms() {
        let terms = lexical_query_terms("how configure TargetName callback payment");
        assert_eq!(
            title_ngram_terms(&terms),
            vec!["targetname".to_string(), "configure".to_string(), "callback".to_string(),]
        );
    }

    #[test]
    fn title_ngram_terms_drop_short_suffix_noise() {
        let terms = lexical_query_terms("what is new in the latest releases");
        assert_eq!(title_ngram_terms(&terms), vec!["releases".to_string()]);
    }

    #[test]
    fn title_identity_terms_keep_numeric_literal_and_drop_surrounding_noise() {
        let terms = lexical_query_terms("what is new in version 9.8.765");
        assert_eq!(
            title_identity_terms("what is new in version 9.8.765", &terms),
            vec!["9.8.765".to_string()]
        );
    }

    #[test]
    fn numeric_title_literals_preserve_dotted_versions() {
        assert_eq!(
            numeric_title_literals("release 9.8.765, build 432-1"),
            vec!["9.8.765".to_string(), "432-1".to_string()]
        );
    }

    #[test]
    fn title_identity_terms_keep_short_exact_title_queries() {
        let terms = lexical_query_terms("Change History");
        assert_eq!(
            title_identity_terms("Change History", &terms),
            vec!["change".to_string(), "history".to_string()]
        );
    }

    #[test]
    fn title_identity_terms_drop_long_natural_language_queries() {
        let terms =
            lexical_query_terms("what is new in the latest releases for every version change list");
        assert!(
            title_identity_terms(
                "what is new in the latest releases for every version change list",
                &terms
            )
            .is_empty()
        );
    }

    #[test]
    fn title_soft_raw_disabled_for_long_natural_language_queries() {
        let terms =
            lexical_query_terms("what is new in the latest releases for every version change list");
        assert!(!title_soft_raw_enabled(&terms));
    }

    #[test]
    fn title_soft_raw_enabled_for_short_title_lookup_queries() {
        let terms = lexical_query_terms("how configure payment");
        assert!(title_soft_raw_enabled(&terms));
    }

    #[test]
    fn chunk_vector_row_deserializes_arango_key_field() {
        let row = serde_json::from_value::<KnowledgeChunkVectorRow>(serde_json::json!({
            "_key": "chunk-vector",
            "vector_id": "019d45de-500e-77c3-be35-537bf0954323",
            "workspace_id": "019d45de-500e-77c3-be35-537bf0954324",
            "library_id": "019d45de-500e-77c3-be35-537bf0954325",
            "chunk_id": "019d45de-500e-77c3-be35-537bf0954326",
            "revision_id": "019d45de-500e-77c3-be35-537bf0954327",
            "embedding_model_key": "model",
            "vector_kind": "chunk_embedding",
            "dimensions": 3,
            "vector": [0.1, 0.2, 0.3],
            "freshness_generation": 1,
            "created_at": "2026-04-01T00:00:00Z"
        }))
        .expect("chunk vector row should deserialize");

        assert_eq!(row.key, "chunk-vector");
    }

    #[test]
    fn entity_vector_row_deserializes_arango_key_field() {
        let row = serde_json::from_value::<KnowledgeEntityVectorRow>(serde_json::json!({
            "_key": "entity-vector",
            "vector_id": "019d45de-500e-77c3-be35-537bf0954330",
            "workspace_id": "019d45de-500e-77c3-be35-537bf0954331",
            "library_id": "019d45de-500e-77c3-be35-537bf0954332",
            "entity_id": "019d45de-500e-77c3-be35-537bf0954333",
            "embedding_model_key": "model",
            "vector_kind": "entity_embedding",
            "dimensions": 3,
            "vector": [0.1, 0.2, 0.3],
            "freshness_generation": 1,
            "created_at": "2026-04-01T00:00:00Z"
        }))
        .expect("entity vector row should deserialize");

        assert_eq!(row.key, "entity-vector");
    }
}
