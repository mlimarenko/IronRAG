use std::sync::Arc;

use anyhow::{Context, anyhow};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use uuid::Uuid;

use crate::infra::arangodb::{
    client::ArangoClient,
    collections::{
        KNOWLEDGE_CHUNK_COLLECTION, KNOWLEDGE_DOCUMENT_COLLECTION,
        KNOWLEDGE_LIBRARY_GENERATION_COLLECTION, KNOWLEDGE_REVISION_COLLECTION,
    },
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeDocumentRow {
    #[serde(rename = "_key")]
    pub key: String,
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    pub arango_id: Option<String>,
    #[serde(rename = "_rev", default, skip_serializing_if = "Option::is_none")]
    pub arango_rev: Option<String>,
    pub document_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub external_key: String,
    pub title: Option<String>,
    pub document_state: String,
    pub active_revision_id: Option<Uuid>,
    pub readable_revision_id: Option<Uuid>,
    pub latest_revision_no: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeRevisionRow {
    #[serde(rename = "_key")]
    pub key: String,
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    pub arango_id: Option<String>,
    #[serde(rename = "_rev", default, skip_serializing_if = "Option::is_none")]
    pub arango_rev: Option<String>,
    pub revision_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub revision_number: i64,
    pub revision_state: String,
    pub revision_kind: String,
    pub storage_ref: Option<String>,
    pub source_uri: Option<String>,
    pub mime_type: String,
    pub checksum: String,
    pub title: Option<String>,
    pub byte_size: i64,
    pub normalized_text: Option<String>,
    pub text_checksum: Option<String>,
    pub text_state: String,
    pub vector_state: String,
    pub graph_state: String,
    pub text_readable_at: Option<DateTime<Utc>>,
    pub vector_ready_at: Option<DateTime<Utc>>,
    pub graph_ready_at: Option<DateTime<Utc>>,
    pub superseded_by_revision_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeChunkRow {
    #[serde(rename = "_key")]
    pub key: String,
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    pub arango_id: Option<String>,
    #[serde(rename = "_rev", default, skip_serializing_if = "Option::is_none")]
    pub arango_rev: Option<String>,
    pub chunk_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub revision_id: Uuid,
    pub chunk_index: i32,
    pub content_text: String,
    pub normalized_text: String,
    pub span_start: Option<i32>,
    pub span_end: Option<i32>,
    pub token_count: Option<i32>,
    pub section_path: Vec<String>,
    pub heading_trail: Vec<String>,
    pub chunk_state: String,
    pub text_generation: Option<i64>,
    pub vector_generation: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeLibraryGenerationRow {
    #[serde(rename = "_key")]
    pub key: String,
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    pub arango_id: Option<String>,
    #[serde(rename = "_rev", default, skip_serializing_if = "Option::is_none")]
    pub arango_rev: Option<String>,
    pub generation_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub active_text_generation: i64,
    pub active_vector_generation: i64,
    pub active_graph_generation: i64,
    pub degraded_state: String,
    pub updated_at: DateTime<Utc>,
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
    ) -> anyhow::Result<Vec<KnowledgeDocumentRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR doc IN @@collection
                 FILTER doc.workspace_id == @workspace_id
                   AND doc.library_id == @library_id
                 SORT doc.updated_at DESC, doc.document_id DESC
                 RETURN doc",
                serde_json::json!({
                    "@collection": KNOWLEDGE_DOCUMENT_COLLECTION,
                    "workspace_id": workspace_id,
                    "library_id": library_id,
                }),
            )
            .await
            .context("failed to list knowledge documents by library")?;
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
                    content_text: @content_text,
                    normalized_text: @normalized_text,
                    span_start: @span_start,
                    span_end: @span_end,
                    token_count: @token_count,
                    section_path: @section_path,
                    heading_trail: @heading_trail,
                    chunk_state: @chunk_state,
                    text_generation: @text_generation,
                    vector_generation: @vector_generation
                 }
                 UPDATE {
                    content_text: @content_text,
                    normalized_text: @normalized_text,
                    span_start: @span_start,
                    span_end: @span_end,
                    token_count: @token_count,
                    section_path: @section_path,
                    heading_trail: @heading_trail,
                    chunk_state: @chunk_state,
                    text_generation: @text_generation,
                    vector_generation: @vector_generation
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
                    "content_text": row.content_text,
                    "normalized_text": row.normalized_text,
                    "span_start": row.span_start,
                    "span_end": row.span_end,
                    "token_count": row.token_count,
                    "section_path": row.section_path,
                    "heading_trail": row.heading_trail,
                    "chunk_state": row.chunk_state,
                    "text_generation": row.text_generation,
                    "vector_generation": row.vector_generation,
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
                    "content_text": row.content_text,
                    "normalized_text": row.normalized_text,
                    "span_start": row.span_start,
                    "span_end": row.span_end,
                    "token_count": row.token_count,
                    "section_path": row.section_path,
                    "heading_trail": row.heading_trail,
                    "chunk_state": row.chunk_state,
                    "text_generation": row.text_generation,
                    "vector_generation": row.vector_generation,
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

    pub async fn upsert_library_generation(
        &self,
        row: &KnowledgeLibraryGenerationRow,
    ) -> anyhow::Result<KnowledgeLibraryGenerationRow> {
        let cursor = self
            .client
            .query_json(
                "UPSERT { _key: @key }
                 INSERT {
                    _key: @key,
                    generation_id: @generation_id,
                    workspace_id: @workspace_id,
                    library_id: @library_id,
                    active_text_generation: @active_text_generation,
                    active_vector_generation: @active_vector_generation,
                    active_graph_generation: @active_graph_generation,
                    degraded_state: @degraded_state,
                    updated_at: @updated_at
                 }
                 UPDATE {
                    active_text_generation: MAX([OLD.active_text_generation, @active_text_generation]),
                    active_vector_generation: MAX([OLD.active_vector_generation, @active_vector_generation]),
                    active_graph_generation: MAX([OLD.active_graph_generation, @active_graph_generation]),
                    degraded_state: @degraded_state,
                    updated_at: @updated_at
                 }
                 IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_LIBRARY_GENERATION_COLLECTION,
                    "key": row.key,
                    "generation_id": row.generation_id,
                    "workspace_id": row.workspace_id,
                    "library_id": row.library_id,
                    "active_text_generation": row.active_text_generation,
                    "active_vector_generation": row.active_vector_generation,
                    "active_graph_generation": row.active_graph_generation,
                    "degraded_state": row.degraded_state,
                    "updated_at": row.updated_at,
                }),
            )
            .await
            .context("failed to upsert knowledge library generation")?;
        decode_single_result(cursor)
    }

    pub async fn list_library_generations(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeLibraryGenerationRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR generation IN @@collection
                 FILTER generation.library_id == @library_id
                 SORT generation.updated_at DESC, generation.generation_id DESC
                 RETURN generation",
                serde_json::json!({
                    "@collection": KNOWLEDGE_LIBRARY_GENERATION_COLLECTION,
                    "library_id": library_id,
                }),
            )
            .await
            .context("failed to list knowledge library generations")?;
        decode_many_results(cursor)
    }

    pub async fn get_library_generation(
        &self,
        generation_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeLibraryGenerationRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR generation IN @@collection
                 FILTER generation.generation_id == @generation_id
                 LIMIT 1
                 RETURN generation",
                serde_json::json!({
                    "@collection": KNOWLEDGE_LIBRARY_GENERATION_COLLECTION,
                    "generation_id": generation_id,
                }),
            )
            .await
            .context("failed to get knowledge library generation")?;
        decode_optional_single_result(cursor)
    }
}

fn decode_single_result<T>(cursor: serde_json::Value) -> anyhow::Result<T>
where
    T: DeserializeOwned,
{
    decode_optional_single_result(cursor)?.ok_or_else(|| anyhow!("ArangoDB query returned no rows"))
}

fn decode_optional_single_result<T>(cursor: serde_json::Value) -> anyhow::Result<Option<T>>
where
    T: DeserializeOwned,
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
    T: DeserializeOwned,
{
    let result = cursor
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow!("ArangoDB cursor response is missing result"))?;
    serde_json::from_value(result).context("failed to decode ArangoDB result rows")
}
