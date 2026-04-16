#![allow(
    clippy::missing_const_for_fn,
    clippy::missing_errors_doc,
    clippy::needless_pass_by_value,
    clippy::too_many_arguments
)]

use std::sync::Arc;

use anyhow::Context;
use chrono::{DateTime, Utc};
use uuid::Uuid;

mod decode;
mod library_generations;
mod technical_facts;
mod types;

use self::decode::{decode_many_results, decode_optional_single_result, decode_single_result};
pub use self::types::{
    KnowledgeChunkRow, KnowledgeChunkSupportReferenceRow, KnowledgeDocumentRow,
    KnowledgeLibraryGenerationRow, KnowledgeRevisionRow, KnowledgeStructuredBlockRow,
    KnowledgeStructuredRevisionRow, KnowledgeTechnicalFactRow,
};

use crate::infra::arangodb::{
    client::ArangoClient,
    collections::{
        KNOWLEDGE_CHUNK_COLLECTION, KNOWLEDGE_DOCUMENT_COLLECTION, KNOWLEDGE_REVISION_COLLECTION,
        KNOWLEDGE_STRUCTURED_BLOCK_COLLECTION, KNOWLEDGE_STRUCTURED_REVISION_COLLECTION,
    },
};

/// Slim projection for the documents-page inspector counts.
#[derive(Debug, Clone, Copy, Default, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct StructuredRevisionCounts {
    #[serde(default)]
    pub block_count: i32,
    #[serde(default)]
    pub typed_fact_count: i32,
}

/// Output of [`ArangoDocumentStore::aggregate_library_generation_signals`].
/// Mirrors the per-state aggregates used to derive the synthetic library
/// generation row — one AQL round-trip instead of per-document fetches.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct LibraryGenerationSignals {
    #[serde(default)]
    pub active_text_generation: i64,
    #[serde(default)]
    pub active_vector_generation: i64,
    #[serde(default)]
    pub active_graph_generation: i64,
    #[serde(default)]
    pub has_ready_text: bool,
    #[serde(default)]
    pub has_ready_vector: bool,
    #[serde(default)]
    pub has_ready_graph: bool,
    #[serde(default)]
    pub latest_created_at: Option<DateTime<Utc>>,
}

#[derive(Clone)]
pub struct ArangoDocumentStore {
    client: Arc<ArangoClient>,
}

impl ArangoDocumentStore {
    #[must_use]
    pub fn new(client: Arc<ArangoClient>) -> Self {
        Self { client }
    }

    #[must_use]
    pub fn client(&self) -> &Arc<ArangoClient> {
        &self.client
    }

    pub async fn upsert_document(
        &self,
        row: &KnowledgeDocumentRow,
    ) -> anyhow::Result<KnowledgeDocumentRow> {
        let cursor = self
            .client
            .query_json(
                "UPSERT { _key: @key }
                 INSERT {
                    _key: @key,
                    document_id: @document_id,
                    workspace_id: @workspace_id,
                    library_id: @library_id,
                    external_key: @external_key,
                    file_name: @file_name,
                    title: @title,
                    document_state: @document_state,
                    active_revision_id: @active_revision_id,
                    readable_revision_id: @readable_revision_id,
                    latest_revision_no: @latest_revision_no,
                    created_at: @created_at,
                    updated_at: @updated_at,
                    deleted_at: @deleted_at
                 }
                 UPDATE {
                    workspace_id: @workspace_id,
                    library_id: @library_id,
                    external_key: @external_key,
                    file_name: @file_name,
                    title: @title,
                    document_state: @document_state,
                    active_revision_id: @active_revision_id,
                    readable_revision_id: @readable_revision_id,
                    latest_revision_no: @latest_revision_no,
                    updated_at: @updated_at,
                    deleted_at: @deleted_at
                 }
                 IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_DOCUMENT_COLLECTION,
                    "key": row.key,
                    "document_id": row.document_id,
                    "workspace_id": row.workspace_id,
                    "library_id": row.library_id,
                    "external_key": row.external_key,
                    "file_name": row.file_name,
                    "title": row.title,
                    "document_state": row.document_state,
                    "active_revision_id": row.active_revision_id,
                    "readable_revision_id": row.readable_revision_id,
                    "latest_revision_no": row.latest_revision_no,
                    "created_at": row.created_at,
                    "updated_at": row.updated_at,
                    "deleted_at": row.deleted_at,
                }),
            )
            .await
            .context("failed to upsert knowledge document")?;
        decode_single_result(cursor)
    }

    pub async fn get_document(
        &self,
        document_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeDocumentRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR doc IN @@collection
                 FILTER doc.document_id == @document_id
                 LIMIT 1
                 RETURN doc",
                serde_json::json!({
                    "@collection": KNOWLEDGE_DOCUMENT_COLLECTION,
                    "document_id": document_id,
                }),
            )
            .await
            .context("failed to get knowledge document")?;
        decode_optional_single_result(cursor)
    }

    pub async fn get_document_by_external_key(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
        external_key: &str,
    ) -> anyhow::Result<Option<KnowledgeDocumentRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR doc IN @@collection
                 FILTER doc.workspace_id == @workspace_id
                   AND doc.library_id == @library_id
                   AND doc.external_key == @external_key
                 LIMIT 1
                 RETURN doc",
                serde_json::json!({
                    "@collection": KNOWLEDGE_DOCUMENT_COLLECTION,
                    "workspace_id": workspace_id,
                    "library_id": library_id,
                    "external_key": external_key,
                }),
            )
            .await
            .context("failed to get knowledge document by external key")?;
        decode_optional_single_result(cursor)
    }

    pub async fn list_documents_by_library(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
        include_deleted: bool,
    ) -> anyhow::Result<Vec<KnowledgeDocumentRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR doc IN @@collection
                 FILTER doc.workspace_id == @workspace_id
                   AND doc.library_id == @library_id
                   AND (@include_deleted OR doc.document_state != 'deleted')
                 SORT doc.updated_at DESC, doc.document_id DESC
                 RETURN doc",
                serde_json::json!({
                    "@collection": KNOWLEDGE_DOCUMENT_COLLECTION,
                    "workspace_id": workspace_id,
                    "library_id": library_id,
                    "include_deleted": include_deleted,
                }),
            )
            .await
            .context("failed to list knowledge documents by library")?;
        decode_many_results(cursor)
    }

    pub async fn list_documents_by_ids(
        &self,
        document_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeDocumentRow>> {
        if document_ids.is_empty() {
            return Ok(Vec::new());
        }
        let cursor = self
            .client
            .query_json(
                "FOR doc IN @@collection
                 FILTER doc.document_id IN @document_ids
                   AND doc.document_state != 'deleted'
                 SORT doc.updated_at DESC, doc.document_id DESC
                 RETURN doc",
                serde_json::json!({
                    "@collection": KNOWLEDGE_DOCUMENT_COLLECTION,
                    "document_ids": document_ids,
                }),
            )
            .await
            .context("failed to list knowledge documents by ids")?;
        decode_many_results(cursor)
    }

    pub async fn update_document_pointers(
        &self,
        document_id: Uuid,
        document_state: &str,
        active_revision_id: Option<Uuid>,
        readable_revision_id: Option<Uuid>,
        latest_revision_no: Option<i64>,
        title: Option<&str>,
        deleted_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Option<KnowledgeDocumentRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR doc IN @@collection
                 FILTER doc.document_id == @document_id
                 LIMIT 1
                 UPDATE doc WITH {
                    document_state: @document_state,
                    active_revision_id: @active_revision_id,
                    readable_revision_id: @readable_revision_id,
                    latest_revision_no: @latest_revision_no,
                    title: @title,
                    updated_at: @updated_at,
                    deleted_at: @deleted_at
                 } IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_DOCUMENT_COLLECTION,
                    "document_id": document_id,
                    "document_state": document_state,
                    "active_revision_id": active_revision_id,
                    "readable_revision_id": readable_revision_id,
                    "latest_revision_no": latest_revision_no,
                    "title": title,
                    "updated_at": Utc::now(),
                    "deleted_at": deleted_at,
                }),
            )
            .await
            .context("failed to update knowledge document pointers")?;
        decode_optional_single_result(cursor)
    }

    pub async fn upsert_revision(
        &self,
        row: &KnowledgeRevisionRow,
    ) -> anyhow::Result<KnowledgeRevisionRow> {
        let cursor = self
            .client
            .query_json(
                "UPSERT { _key: @key }
                 INSERT {
                    _key: @key,
                    revision_id: @revision_id,
                    workspace_id: @workspace_id,
                    library_id: @library_id,
                    document_id: @document_id,
                    revision_number: @revision_number,
                    revision_state: @revision_state,
                    revision_kind: @revision_kind,
                    storage_ref: @storage_ref,
                    source_uri: @source_uri,
                    mime_type: @mime_type,
                    checksum: @checksum,
                    title: @title,
                    byte_size: @byte_size,
                    normalized_text: @normalized_text,
                    text_checksum: @text_checksum,
                    text_state: @text_state,
                    vector_state: @vector_state,
                    graph_state: @graph_state,
                    text_readable_at: @text_readable_at,
                    vector_ready_at: @vector_ready_at,
                    graph_ready_at: @graph_ready_at,
                    superseded_by_revision_id: @superseded_by_revision_id,
                    created_at: @created_at
                 }
                 UPDATE {
                    workspace_id: @workspace_id,
                    library_id: @library_id,
                    document_id: @document_id,
                    revision_number: @revision_number,
                    revision_state: @revision_state,
                    revision_kind: @revision_kind,
                    storage_ref: @storage_ref,
                    source_uri: @source_uri,
                    mime_type: @mime_type,
                    checksum: @checksum,
                    title: @title,
                    byte_size: @byte_size,
                    normalized_text: @normalized_text,
                    text_checksum: @text_checksum,
                    text_state: @text_state,
                    vector_state: @vector_state,
                    graph_state: @graph_state,
                    text_readable_at: @text_readable_at,
                    vector_ready_at: @vector_ready_at,
                    graph_ready_at: @graph_ready_at,
                    superseded_by_revision_id: @superseded_by_revision_id
                 }
                 IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_REVISION_COLLECTION,
                    "key": row.key,
                    "revision_id": row.revision_id,
                    "workspace_id": row.workspace_id,
                    "library_id": row.library_id,
                    "document_id": row.document_id,
                    "revision_number": row.revision_number,
                    "revision_state": row.revision_state,
                    "revision_kind": row.revision_kind,
                    "storage_ref": row.storage_ref,
                    "source_uri": row.source_uri,
                    "mime_type": row.mime_type,
                    "checksum": row.checksum,
                    "title": row.title,
                    "byte_size": row.byte_size,
                    "normalized_text": row.normalized_text,
                    "text_checksum": row.text_checksum,
                    "text_state": row.text_state,
                    "vector_state": row.vector_state,
                    "graph_state": row.graph_state,
                    "text_readable_at": row.text_readable_at,
                    "vector_ready_at": row.vector_ready_at,
                    "graph_ready_at": row.graph_ready_at,
                    "superseded_by_revision_id": row.superseded_by_revision_id,
                    "created_at": row.created_at,
                }),
            )
            .await
            .context("failed to upsert knowledge revision")?;
        decode_single_result(cursor)
    }

    pub async fn get_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR revision IN @@collection
                 FILTER revision.revision_id == @revision_id
                 LIMIT 1
                 RETURN revision",
                serde_json::json!({
                    "@collection": KNOWLEDGE_REVISION_COLLECTION,
                    "revision_id": revision_id,
                }),
            )
            .await
            .context("failed to get knowledge revision")?;
        decode_optional_single_result(cursor)
    }

    pub async fn list_revisions_by_ids(
        &self,
        revision_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeRevisionRow>> {
        if revision_ids.is_empty() {
            return Ok(Vec::new());
        }

        let cursor = self
            .client
            .query_json(
                "FOR revision IN @@collection
                 FILTER revision.revision_id IN @revision_ids
                 RETURN revision",
                serde_json::json!({
                    "@collection": KNOWLEDGE_REVISION_COLLECTION,
                    "revision_ids": revision_ids,
                }),
            )
            .await
            .context("failed to list knowledge revisions by ids")?;
        decode_many_results(cursor)
    }

    pub async fn list_revisions_by_document(
        &self,
        document_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRevisionRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR revision IN @@collection
                 FILTER revision.document_id == @document_id
                 SORT revision.revision_number DESC, revision.revision_id DESC
                 LIMIT 100
                 RETURN revision",
                serde_json::json!({
                    "@collection": KNOWLEDGE_REVISION_COLLECTION,
                    "document_id": document_id,
                }),
            )
            .await
            .context("failed to list knowledge revisions by document")?;
        decode_many_results(cursor)
    }

    /// Single-shot aggregate across every `knowledge_revision` row in a
    /// library. Returns the max readable text/vector/graph revision
    /// numbers plus the latest revision `created_at`. Used by the
    /// knowledge service to derive the synthetic library generation row
    /// without iterating 5k documents × N revisions sequentially —
    /// replacing the old per-document `list_revisions_by_document` loop
    /// that took 7+ seconds on a 5k-doc library.
    pub async fn aggregate_library_generation_signals(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<LibraryGenerationSignals> {
        let cursor = self
            .client
            .query_json(
                "LET rows = (
                     FOR revision IN @@collection
                         FILTER revision.library_id == @library_id
                         RETURN {
                             revision_number: revision.revision_number,
                             text_state: revision.text_state,
                             vector_state: revision.vector_state,
                             graph_state: revision.graph_state,
                             created_at: revision.created_at
                         }
                 )
                 LET text_ready_max = MAX(
                     FOR r IN rows
                         FILTER r.text_state IN [\"ready\", \"text_ready\", \"graph_ready\", \"vector_ready\"]
                         RETURN r.revision_number
                 )
                 LET vector_ready_max = MAX(
                     FOR r IN rows
                         FILTER r.vector_state IN [\"ready\", \"vector_ready\", \"graph_ready\"]
                         RETURN r.revision_number
                 )
                 LET graph_ready_max = MAX(
                     FOR r IN rows
                         FILTER r.graph_state IN [\"ready\", \"graph_ready\"]
                         RETURN r.revision_number
                 )
                 LET latest_created = MAX(FOR r IN rows RETURN r.created_at)
                 RETURN {
                     active_text_generation: text_ready_max == null ? 0 : text_ready_max,
                     active_vector_generation: vector_ready_max == null ? 0 : vector_ready_max,
                     active_graph_generation: graph_ready_max == null ? 0 : graph_ready_max,
                     has_ready_text: text_ready_max != null,
                     has_ready_vector: vector_ready_max != null,
                     has_ready_graph: graph_ready_max != null,
                     latest_created_at: latest_created
                 }",
                serde_json::json!({
                    "@collection": KNOWLEDGE_REVISION_COLLECTION,
                    "library_id": library_id.to_string(),
                }),
            )
            .await
            .context("failed to aggregate library generation signals")?;
        let rows =
            cursor.get("result").and_then(serde_json::Value::as_array).cloned().unwrap_or_default();
        let row = rows.into_iter().next().unwrap_or_else(|| serde_json::json!({}));
        let signals: LibraryGenerationSignals =
            serde_json::from_value(row).context("decode library generation signals aggregate")?;
        Ok(signals)
    }

    pub async fn update_revision_readiness(
        &self,
        revision_id: Uuid,
        text_state: &str,
        vector_state: &str,
        graph_state: &str,
        text_readable_at: Option<DateTime<Utc>>,
        vector_ready_at: Option<DateTime<Utc>>,
        graph_ready_at: Option<DateTime<Utc>>,
        superseded_by_revision_id: Option<Uuid>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR revision IN @@collection
                 FILTER revision.revision_id == @revision_id
                 LIMIT 1
                 UPDATE revision WITH {
                    text_state: @text_state,
                    vector_state: @vector_state,
                    graph_state: @graph_state,
                    text_readable_at: @text_readable_at,
                    vector_ready_at: @vector_ready_at,
                    graph_ready_at: @graph_ready_at,
                    superseded_by_revision_id: @superseded_by_revision_id
                 } IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_REVISION_COLLECTION,
                    "revision_id": revision_id,
                    "text_state": text_state,
                    "vector_state": vector_state,
                    "graph_state": graph_state,
                    "text_readable_at": text_readable_at,
                    "vector_ready_at": vector_ready_at,
                    "graph_ready_at": graph_ready_at,
                    "superseded_by_revision_id": superseded_by_revision_id,
                }),
            )
            .await
            .context("failed to update knowledge revision readiness")?;
        decode_optional_single_result(cursor)
    }

    pub async fn update_revision_text_content(
        &self,
        revision_id: Uuid,
        normalized_text: Option<&str>,
        text_checksum: Option<&str>,
        text_state: &str,
        text_readable_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR revision IN @@collection
                 FILTER revision.revision_id == @revision_id
                 LIMIT 1
                 UPDATE revision WITH {
                    normalized_text: @normalized_text,
                    text_checksum: @text_checksum,
                    text_state: @text_state,
                    text_readable_at: @text_readable_at
                 } IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_REVISION_COLLECTION,
                    "revision_id": revision_id,
                    "normalized_text": normalized_text,
                    "text_checksum": text_checksum,
                    "text_state": text_state,
                    "text_readable_at": text_readable_at,
                }),
            )
            .await
            .context("failed to update knowledge revision text content")?;
        decode_optional_single_result(cursor)
    }

    pub async fn update_revision_storage_ref(
        &self,
        revision_id: Uuid,
        storage_ref: Option<&str>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR revision IN @@collection
                 FILTER revision.revision_id == @revision_id
                 LIMIT 1
                 UPDATE revision WITH {
                    storage_ref: @storage_ref
                 } IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_REVISION_COLLECTION,
                    "revision_id": revision_id,
                    "storage_ref": storage_ref,
                }),
            )
            .await
            .context("failed to update knowledge revision storage ref")?;
        decode_optional_single_result(cursor)
    }

    pub async fn upsert_chunk(&self, row: &KnowledgeChunkRow) -> anyhow::Result<KnowledgeChunkRow> {
        let cursor = self
            .client
            .query_json(
                "UPSERT { _key: @key }
                 INSERT {
                    _key: @key,
                    chunk_id: @chunk_id,
                    workspace_id: @workspace_id,
                    library_id: @library_id,
                    document_id: @document_id,
                    revision_id: @revision_id,
                    chunk_index: @chunk_index,
                    chunk_kind: @chunk_kind,
                    content_text: @content_text,
                    normalized_text: @normalized_text,
                    span_start: @span_start,
                    span_end: @span_end,
                    token_count: @token_count,
                    support_block_ids: @support_block_ids,
                    section_path: @section_path,
                    heading_trail: @heading_trail,
                    literal_digest: @literal_digest,
                    chunk_state: @chunk_state,
                    text_generation: @text_generation,
                    vector_generation: @vector_generation,
                    quality_score: @quality_score
                 }
                 UPDATE {
                    chunk_kind: @chunk_kind,
                    content_text: @content_text,
                    normalized_text: @normalized_text,
                    span_start: @span_start,
                    span_end: @span_end,
                    token_count: @token_count,
                    support_block_ids: @support_block_ids,
                    section_path: @section_path,
                    heading_trail: @heading_trail,
                    literal_digest: @literal_digest,
                    chunk_state: @chunk_state,
                    text_generation: @text_generation,
                    vector_generation: @vector_generation,
                    quality_score: @quality_score
                 }
                 IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_CHUNK_COLLECTION,
                    "key": row.key,
                    "chunk_id": row.chunk_id,
                    "workspace_id": row.workspace_id,
                    "library_id": row.library_id,
                    "document_id": row.document_id,
                    "revision_id": row.revision_id,
                    "chunk_index": row.chunk_index,
                    "chunk_kind": row.chunk_kind,
                    "content_text": row.content_text,
                    "normalized_text": row.normalized_text,
                    "span_start": row.span_start,
                    "span_end": row.span_end,
                    "token_count": row.token_count,
                    "support_block_ids": row.support_block_ids,
                    "section_path": row.section_path,
                    "heading_trail": row.heading_trail,
                    "literal_digest": row.literal_digest,
                    "chunk_state": row.chunk_state,
                    "text_generation": row.text_generation,
                    "vector_generation": row.vector_generation,
                    "quality_score": row.quality_score,
                }),
            )
            .await
            .context("failed to upsert knowledge chunk")?;
        decode_single_result(cursor)
    }

    pub async fn insert_chunks(
        &self,
        rows: &[KnowledgeChunkRow],
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }

        let payload_rows = rows
            .iter()
            .map(|row| {
                serde_json::json!({
                    "_key": row.key,
                    "chunk_id": row.chunk_id,
                    "workspace_id": row.workspace_id,
                    "library_id": row.library_id,
                    "document_id": row.document_id,
                    "revision_id": row.revision_id,
                    "chunk_index": row.chunk_index,
                    "chunk_kind": row.chunk_kind,
                    "content_text": row.content_text,
                    "normalized_text": row.normalized_text,
                    "span_start": row.span_start,
                    "span_end": row.span_end,
                    "token_count": row.token_count,
                    "support_block_ids": row.support_block_ids,
                    "section_path": row.section_path,
                    "heading_trail": row.heading_trail,
                    "literal_digest": row.literal_digest,
                    "chunk_state": row.chunk_state,
                    "text_generation": row.text_generation,
                    "vector_generation": row.vector_generation,
                    "quality_score": row.quality_score,
                })
            })
            .collect::<Vec<_>>();

        let cursor = self
            .client
            .query_json(
                "FOR row IN @rows
                 INSERT row INTO @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_CHUNK_COLLECTION,
                    "rows": payload_rows,
                }),
            )
            .await
            .context("failed to insert knowledge chunks")?;
        decode_many_results(cursor)
    }

    pub async fn list_chunks_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR chunk IN @@collection
                 FILTER chunk.revision_id == @revision_id
                 SORT chunk.chunk_index ASC, chunk.chunk_id ASC
                 LIMIT 2000
                 RETURN chunk",
                serde_json::json!({
                    "@collection": KNOWLEDGE_CHUNK_COLLECTION,
                    "revision_id": revision_id,
                }),
            )
            .await
            .context("failed to list knowledge chunks by revision")?;
        decode_many_results(cursor)
    }

    pub async fn get_chunk(&self, chunk_id: Uuid) -> anyhow::Result<Option<KnowledgeChunkRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR chunk IN @@collection
                 FILTER chunk.chunk_id == @chunk_id
                 LIMIT 1
                 RETURN chunk",
                serde_json::json!({
                    "@collection": KNOWLEDGE_CHUNK_COLLECTION,
                    "chunk_id": chunk_id,
                }),
            )
            .await
            .context("failed to get knowledge chunk by id")?;
        decode_optional_single_result(cursor)
    }

    pub async fn list_chunks_by_ids(
        &self,
        chunk_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        if chunk_ids.is_empty() {
            return Ok(Vec::new());
        }
        let cursor = self
            .client
            .query_json(
                "FOR chunk IN @@collection
                 FILTER chunk.chunk_id IN @chunk_ids
                 SORT chunk.chunk_index ASC, chunk.chunk_id ASC
                 RETURN chunk",
                serde_json::json!({
                    "@collection": KNOWLEDGE_CHUNK_COLLECTION,
                    "chunk_ids": chunk_ids,
                }),
            )
            .await
            .context("failed to list knowledge chunks by ids")?;
        decode_many_results(cursor)
    }

    pub async fn delete_chunks_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR chunk IN @@collection
                 FILTER chunk.revision_id == @revision_id
                 REMOVE chunk IN @@collection
                 RETURN OLD",
                serde_json::json!({
                    "@collection": KNOWLEDGE_CHUNK_COLLECTION,
                    "revision_id": revision_id,
                }),
            )
            .await
            .context("failed to delete knowledge chunks by revision")?;
        decode_many_results(cursor)
    }

    pub async fn upsert_structured_revision(
        &self,
        row: &KnowledgeStructuredRevisionRow,
    ) -> anyhow::Result<KnowledgeStructuredRevisionRow> {
        let cursor = self
            .client
            .query_json(
                "UPSERT { _key: @key }
                 INSERT {
                    _key: @key,
                    revision_id: @revision_id,
                    workspace_id: @workspace_id,
                    library_id: @library_id,
                    document_id: @document_id,
                    preparation_state: @preparation_state,
                    normalization_profile: @normalization_profile,
                    source_format: @source_format,
                    language_code: @language_code,
                    block_count: @block_count,
                    chunk_count: @chunk_count,
                    typed_fact_count: @typed_fact_count,
                    outline_json: @outline_json,
                    prepared_at: @prepared_at,
                    updated_at: @updated_at
                 }
                 UPDATE {
                    preparation_state: @preparation_state,
                    normalization_profile: @normalization_profile,
                    source_format: @source_format,
                    language_code: @language_code,
                    block_count: @block_count,
                    chunk_count: @chunk_count,
                    typed_fact_count: @typed_fact_count,
                    outline_json: @outline_json,
                    prepared_at: @prepared_at,
                    updated_at: @updated_at
                 }
                 IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_STRUCTURED_REVISION_COLLECTION,
                    "key": row.key,
                    "revision_id": row.revision_id,
                    "workspace_id": row.workspace_id,
                    "library_id": row.library_id,
                    "document_id": row.document_id,
                    "preparation_state": row.preparation_state,
                    "normalization_profile": row.normalization_profile,
                    "source_format": row.source_format,
                    "language_code": row.language_code,
                    "block_count": row.block_count,
                    "chunk_count": row.chunk_count,
                    "typed_fact_count": row.typed_fact_count,
                    "outline_json": row.outline_json,
                    "prepared_at": row.prepared_at,
                    "updated_at": row.updated_at,
                }),
            )
            .await
            .context("failed to upsert knowledge structured revision")?;
        decode_single_result(cursor)
    }

    pub async fn get_structured_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeStructuredRevisionRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR revision IN @@collection
                 FILTER revision.revision_id == @revision_id
                 LIMIT 1
                 RETURN revision",
                serde_json::json!({
                    "@collection": KNOWLEDGE_STRUCTURED_REVISION_COLLECTION,
                    "revision_id": revision_id,
                }),
            )
            .await
            .context("failed to get structured revision")?;
        decode_optional_single_result(cursor)
    }

    /// Slim projection used by the documents-page inspector: returns only
    /// `(block_count, typed_fact_count)` without touching `outline_json`.
    /// The outline blob averages ~4 MB per PDF-ingested document and was
    /// dominating detail-response latency even though the frontend reads
    /// only these two scalars. Keep the full `get_structured_revision`
    /// method for places that actually render the outline.
    pub async fn get_structured_revision_counts(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Option<StructuredRevisionCounts>> {
        let cursor = self
            .client
            .query_json(
                "FOR revision IN @@collection
                 FILTER revision.revision_id == @revision_id
                 LIMIT 1
                 RETURN { block_count: revision.block_count, typed_fact_count: revision.typed_fact_count }",
                serde_json::json!({
                    "@collection": KNOWLEDGE_STRUCTURED_REVISION_COLLECTION,
                    "revision_id": revision_id,
                }),
            )
            .await
            .context("failed to get structured revision counts")?;
        let rows =
            cursor.get("result").and_then(serde_json::Value::as_array).cloned().unwrap_or_default();
        match rows.into_iter().next() {
            Some(row) => {
                Ok(Some(serde_json::from_value(row).context("decode structured revision counts")?))
            }
            None => Ok(None),
        }
    }

    pub async fn list_structured_revisions_by_revision_ids(
        &self,
        revision_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeStructuredRevisionRow>> {
        if revision_ids.is_empty() {
            return Ok(Vec::new());
        }

        let cursor = self
            .client
            .query_json(
                "FOR revision IN @@collection
                 FILTER revision.revision_id IN @revision_ids
                 RETURN revision",
                serde_json::json!({
                    "@collection": KNOWLEDGE_STRUCTURED_REVISION_COLLECTION,
                    "revision_ids": revision_ids,
                }),
            )
            .await
            .context("failed to list structured revisions by revision ids")?;
        decode_many_results(cursor)
    }

    pub async fn list_structured_revisions_by_document(
        &self,
        document_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeStructuredRevisionRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR revision IN @@collection
                 FILTER revision.document_id == @document_id
                 SORT revision.prepared_at DESC, revision.revision_id DESC
                 RETURN revision",
                serde_json::json!({
                    "@collection": KNOWLEDGE_STRUCTURED_REVISION_COLLECTION,
                    "document_id": document_id,
                }),
            )
            .await
            .context("failed to list structured revisions by document")?;
        decode_many_results(cursor)
    }

    pub async fn replace_structured_blocks(
        &self,
        revision_id: Uuid,
        rows: &[KnowledgeStructuredBlockRow],
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockRow>> {
        self.delete_structured_blocks_by_revision(revision_id).await?;
        if rows.is_empty() {
            return Ok(Vec::new());
        }

        let payload_rows = rows
            .iter()
            .map(|row| {
                serde_json::json!({
                    "_key": row.key,
                    "block_id": row.block_id,
                    "workspace_id": row.workspace_id,
                    "library_id": row.library_id,
                    "document_id": row.document_id,
                    "revision_id": row.revision_id,
                    "ordinal": row.ordinal,
                    "block_kind": row.block_kind,
                    "text": row.text,
                    "normalized_text": row.normalized_text,
                    "heading_trail": row.heading_trail,
                    "section_path": row.section_path,
                    "page_number": row.page_number,
                    "span_start": row.span_start,
                    "span_end": row.span_end,
                    "parent_block_id": row.parent_block_id,
                    "table_coordinates_json": row.table_coordinates_json,
                    "code_language": row.code_language,
                    "created_at": row.created_at,
                    "updated_at": row.updated_at,
                })
            })
            .collect::<Vec<_>>();

        let cursor = self
            .client
            .query_json(
                "FOR row IN @rows
                 INSERT row INTO @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_STRUCTURED_BLOCK_COLLECTION,
                    "rows": payload_rows,
                }),
            )
            .await
            .context("failed to replace structured blocks")?;
        decode_many_results(cursor)
    }

    pub async fn list_structured_blocks_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR block IN @@collection
                 FILTER block.revision_id == @revision_id
                 SORT block.ordinal ASC, block.block_id ASC
                 RETURN block",
                serde_json::json!({
                    "@collection": KNOWLEDGE_STRUCTURED_BLOCK_COLLECTION,
                    "revision_id": revision_id,
                }),
            )
            .await
            .context("failed to list structured blocks by revision")?;
        decode_many_results(cursor)
    }

    /// Canonical paginated read for the inspector's "prepared segments"
    /// tab. Returns the requested window plus the full count in a
    /// single AQL round-trip — `total` is computed by `LENGTH(FOR …
    /// RETURN 1)` which the `(revision_id, ordinal)` persistent index
    /// can cover without loading any block documents. The slice uses
    /// the same index for `LIMIT @offset, @limit`, so only the
    /// requested page materializes full block rows. This is what
    /// replaced the "load every block, slice in memory" path that
    /// used to blow ~1.2 s of wall time on PDF-sized documents.
    pub async fn list_structured_blocks_page_by_revision(
        &self,
        revision_id: Uuid,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<(Vec<KnowledgeStructuredBlockRow>, usize)> {
        let cursor = self
            .client
            .query_json(
                "LET total = LENGTH(
                     FOR block IN @@collection
                     FILTER block.revision_id == @revision_id
                     RETURN 1
                 )
                 LET page = (
                     FOR block IN @@collection
                     FILTER block.revision_id == @revision_id
                     SORT block.ordinal ASC, block.block_id ASC
                     LIMIT @offset, @limit
                     RETURN block
                 )
                 RETURN { total, page }",
                serde_json::json!({
                    "@collection": KNOWLEDGE_STRUCTURED_BLOCK_COLLECTION,
                    "revision_id": revision_id,
                    "offset": offset as i64,
                    "limit": limit as i64,
                }),
            )
            .await
            .context("failed to list structured block page by revision")?;
        let result =
            cursor.get("result").and_then(serde_json::Value::as_array).cloned().unwrap_or_default();
        let Some(envelope) = result.into_iter().next() else {
            return Ok((Vec::new(), 0));
        };
        let total = envelope.get("total").and_then(serde_json::Value::as_u64).unwrap_or(0) as usize;
        let page_value =
            envelope.get("page").cloned().unwrap_or(serde_json::Value::Array(Vec::new()));
        let rows: Vec<KnowledgeStructuredBlockRow> =
            serde_json::from_value(page_value).context("failed to decode structured block page")?;
        Ok((rows, total))
    }

    /// Slim projection of chunks for a revision used by the prepared
    /// segments surface to build `support_chunk_ids` per block. Only
    /// the id + the block back-references are returned — full chunk
    /// text (which can be several MB on PDF docs) is never serialized
    /// over the wire. The caller only needs to know which chunks
    /// reference each block, not the chunk contents.
    pub async fn list_chunk_support_references_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeChunkSupportReferenceRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR chunk IN @@collection
                 FILTER chunk.revision_id == @revision_id
                 SORT chunk.chunk_index ASC, chunk.chunk_id ASC
                 LIMIT 2000
                 RETURN { chunk_id: chunk.chunk_id, support_block_ids: chunk.support_block_ids }",
                serde_json::json!({
                    "@collection": KNOWLEDGE_CHUNK_COLLECTION,
                    "revision_id": revision_id,
                }),
            )
            .await
            .context("failed to list chunk support references by revision")?;
        decode_many_results(cursor)
    }

    pub async fn list_structured_blocks_by_ids(
        &self,
        block_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockRow>> {
        if block_ids.is_empty() {
            return Ok(Vec::new());
        }
        let cursor = self
            .client
            .query_json(
                "FOR block IN @@collection
                 FILTER block.block_id IN @block_ids
                 SORT block.ordinal ASC, block.block_id ASC
                 RETURN block",
                serde_json::json!({
                    "@collection": KNOWLEDGE_STRUCTURED_BLOCK_COLLECTION,
                    "block_ids": block_ids,
                }),
            )
            .await
            .context("failed to list structured blocks by ids")?;
        decode_many_results(cursor)
    }

    pub async fn delete_structured_blocks_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR block IN @@collection
                 FILTER block.revision_id == @revision_id
                 REMOVE block IN @@collection
                 RETURN OLD",
                serde_json::json!({
                    "@collection": KNOWLEDGE_STRUCTURED_BLOCK_COLLECTION,
                    "revision_id": revision_id,
                }),
            )
            .await
            .context("failed to delete structured blocks by revision")?;
        decode_many_results(cursor)
    }
}
