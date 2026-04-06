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
        KNOWLEDGE_ENTITY_VECTOR_COLLECTION, KNOWLEDGE_SEARCH_VIEW,
    },
};

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
                    created_at: @created_at
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
                    freshness_generation: @freshness_generation
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
                }),
            )
            .await
            .context("failed to upsert knowledge chunk vector")?;
        decode_single_result(cursor)
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
    ) -> anyhow::Result<Vec<KnowledgeChunkSearchRow>> {
        let normalized_limit = limit.max(1);
        let query_lower = query.trim().to_lowercase();
        let query_terms = lexical_query_terms(query);
        let cursor = self
            .client
            .query_json(
                "FOR doc IN @@view
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
                 LET score = BM25(doc)
                 SORT score DESC, doc.chunk_id ASC
                 LIMIT @limit
                 RETURN {
                    chunk_id: doc.chunk_id,
                    workspace_id: doc.workspace_id,
                    library_id: doc.library_id,
                    revision_id: doc.revision_id,
                    content_text: doc.content_text,
                    normalized_text: doc.normalized_text,
                    section_path: doc.section_path,
                    heading_trail: doc.heading_trail,
                    score: score
                 }",
                serde_json::json!({
                    "@view": KNOWLEDGE_SEARCH_VIEW,
                    "library_id": library_id,
                    "query": query,
                    "limit": normalized_limit,
                }),
            )
            .await
            .context("failed to search knowledge chunks")?;
        let rows = decode_many_results(cursor)?;
        if query_lower.is_empty() {
            return Ok(rows);
        }

        let should_merge_direct_scan = rows.is_empty() || query_terms.len() > 1;
        if !should_merge_direct_scan {
            return Ok(rows);
        }

        // Arango search views can lag briefly behind chunk writes. Also, for multi-token
        // queries they can surface partial token matches from older revisions before the
        // freshly written exact phrase becomes visible. Merge in a direct collection scan
        // so fresh exact-text hits are visible and rank above stale partial matches.
        let cursor = self
            .client
            .query_json(
                "FOR chunk IN @@collection
                 FILTER chunk.library_id == @library_id
                   AND chunk.chunk_state == 'ready'
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
                    "query_lower": query_lower,
                    "query_terms": query_terms,
                    "limit": normalized_limit,
                }),
            )
            .await
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
                 LET score = (exact_match ? 1000000 : 0) + BM25(doc)
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

    pub async fn search_chunk_vectors_by_similarity(
        &self,
        library_id: Uuid,
        embedding_model_key: &str,
        freshness_generation: i64,
        query_vector: &[f32],
        limit: usize,
        n_probe: Option<u64>,
    ) -> anyhow::Result<Vec<KnowledgeChunkVectorSearchRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR vector IN @@collection
                 FILTER vector.library_id == @library_id
                   AND vector.embedding_model_key == @embedding_model_key
                   AND vector.freshness_generation == @freshness_generation
                 LET score = APPROX_NEAR_COSINE(vector.vector, @query_vector, @options)
                 SORT score DESC, vector.chunk_id ASC
                 LIMIT @limit
                 RETURN {
                    vector_id: vector.vector_id,
                    workspace_id: vector.workspace_id,
                    library_id: vector.library_id,
                    chunk_id: vector.chunk_id,
                    revision_id: vector.revision_id,
                    embedding_model_key: vector.embedding_model_key,
                    vector_kind: vector.vector_kind,
                    freshness_generation: vector.freshness_generation,
                    score: score
                 }",
                serde_json::json!({
                    "@collection": KNOWLEDGE_CHUNK_VECTOR_COLLECTION,
                    "library_id": library_id,
                    "embedding_model_key": embedding_model_key,
                    "freshness_generation": freshness_generation,
                    "query_vector": query_vector,
                    "limit": limit.max(1),
                    "options": vector_search_options(n_probe),
                }),
            )
            .await
            .context("failed to search knowledge chunk vectors by similarity")?;
        decode_many_results(cursor)
    }

    pub async fn search_entity_vectors_by_similarity(
        &self,
        library_id: Uuid,
        embedding_model_key: &str,
        freshness_generation: i64,
        query_vector: &[f32],
        limit: usize,
        n_probe: Option<u64>,
    ) -> anyhow::Result<Vec<KnowledgeEntityVectorSearchRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR vector IN @@collection
                 FILTER vector.library_id == @library_id
                   AND vector.embedding_model_key == @embedding_model_key
                   AND vector.freshness_generation == @freshness_generation
                 LET score = APPROX_NEAR_COSINE(vector.vector, @query_vector, @options)
                 SORT score DESC, vector.entity_id ASC
                 LIMIT @limit
                 RETURN {
                    vector_id: vector.vector_id,
                    workspace_id: vector.workspace_id,
                    library_id: vector.library_id,
                    entity_id: vector.entity_id,
                    embedding_model_key: vector.embedding_model_key,
                    vector_kind: vector.vector_kind,
                    freshness_generation: vector.freshness_generation,
                    score: score
                 }",
                serde_json::json!({
                    "@collection": KNOWLEDGE_ENTITY_VECTOR_COLLECTION,
                    "library_id": library_id,
                    "embedding_model_key": embedding_model_key,
                    "freshness_generation": freshness_generation,
                    "query_vector": query_vector,
                    "limit": limit.max(1),
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
    use super::{KnowledgeChunkVectorRow, KnowledgeEntityVectorRow, lexical_query_terms};

    #[test]
    fn lexical_query_terms_keep_cyrillic_and_deduplicate() {
        assert_eq!(
            lexical_query_terms("Сервер checkout-api /system/info endpoint сервер"),
            vec![
                "сервер".to_string(),
                "checkout".to_string(),
                "api".to_string(),
                "/system/info".to_string(),
                "endpoint".to_string(),
            ]
        );
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
