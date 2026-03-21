use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::knowledge::{
        KnowledgeChunk, KnowledgeContextBundle, KnowledgeDocument, KnowledgeLibraryGeneration,
        KnowledgeRevision,
    },
    interfaces::http::router_support::ApiError,
};

#[derive(Debug, Clone)]
pub struct CreateKnowledgeDocumentCommand {
    pub document_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub external_key: String,
    pub document_state: String,
}

#[derive(Debug, Clone)]
pub struct CreateKnowledgeRevisionCommand {
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
    pub byte_size: i64,
    pub title: Option<String>,
    pub normalized_text: Option<String>,
    pub text_checksum: Option<String>,
    pub text_state: String,
    pub vector_state: String,
    pub graph_state: String,
    pub text_readable_at: Option<chrono::DateTime<chrono::Utc>>,
    pub vector_ready_at: Option<chrono::DateTime<chrono::Utc>>,
    pub graph_ready_at: Option<chrono::DateTime<chrono::Utc>>,
    pub superseded_by_revision_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct PromoteKnowledgeDocumentCommand {
    pub document_id: Uuid,
    pub document_state: String,
    pub active_revision_id: Option<Uuid>,
    pub readable_revision_id: Option<Uuid>,
    pub latest_revision_no: Option<i64>,
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone)]
pub struct CreateKnowledgeChunkCommand {
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

#[derive(Debug, Clone)]
pub struct RefreshKnowledgeLibraryGenerationCommand {
    pub generation_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub active_text_generation: i64,
    pub active_vector_generation: i64,
    pub active_graph_generation: i64,
    pub degraded_state: String,
}

#[derive(Clone, Default)]
pub struct KnowledgeService;

impl KnowledgeService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    pub async fn create_document_shell(
        &self,
        state: &AppState,
        command: CreateKnowledgeDocumentCommand,
    ) -> Result<KnowledgeDocument, ApiError> {
        let now = chrono::Utc::now();
        let row = state
            .arango_document_store
            .upsert_document(&crate::infra::arangodb::document_store::KnowledgeDocumentRow {
                key: command.document_id.to_string(),
                arango_id: None,
                arango_rev: None,
                document_id: command.document_id,
                workspace_id: command.workspace_id,
                library_id: command.library_id,
                external_key: command.external_key,
                document_state: command.document_state,
                active_revision_id: None,
                readable_revision_id: None,
                latest_revision_no: None,
                created_at: now,
                updated_at: now,
                deleted_at: None,
            })
            .await
            .map_err(|_| ApiError::Internal)?;
        Ok(map_document_row(row))
    }

    pub async fn write_revision(
        &self,
        state: &AppState,
        command: CreateKnowledgeRevisionCommand,
    ) -> Result<KnowledgeRevision, ApiError> {
        let row = state
            .arango_document_store
            .upsert_revision(&crate::infra::arangodb::document_store::KnowledgeRevisionRow {
                key: command.revision_id.to_string(),
                arango_id: None,
                arango_rev: None,
                revision_id: command.revision_id,
                workspace_id: command.workspace_id,
                library_id: command.library_id,
                document_id: command.document_id,
                revision_number: command.revision_number,
                revision_state: command.revision_state,
                revision_kind: command.revision_kind,
                storage_ref: command.storage_ref,
                source_uri: command.source_uri,
                mime_type: command.mime_type,
                checksum: command.checksum,
                title: command.title,
                byte_size: command.byte_size,
                normalized_text: command.normalized_text,
                text_checksum: command.text_checksum,
                text_state: command.text_state,
                vector_state: command.vector_state,
                graph_state: command.graph_state,
                text_readable_at: command.text_readable_at,
                vector_ready_at: command.vector_ready_at,
                graph_ready_at: command.graph_ready_at,
                superseded_by_revision_id: command.superseded_by_revision_id,
                created_at: chrono::Utc::now(),
            })
            .await
            .map_err(|_| ApiError::Internal)?;
        state
            .arango_graph_store
            .upsert_document_revision_edge(command.document_id, command.revision_id)
            .await
            .map_err(|_| ApiError::Internal)?;
        Ok(map_revision_row(row))
    }

    pub async fn promote_document(
        &self,
        state: &AppState,
        command: PromoteKnowledgeDocumentCommand,
    ) -> Result<KnowledgeDocument, ApiError> {
        let row = state
            .arango_document_store
            .update_document_pointers(
                command.document_id,
                &command.document_state,
                command.active_revision_id,
                command.readable_revision_id,
                command.latest_revision_no,
                command.deleted_at,
            )
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| {
                ApiError::resource_not_found("knowledge_document", command.document_id)
            })?;
        Ok(map_document_row(row))
    }

    pub async fn set_revision_text_state(
        &self,
        state: &AppState,
        revision_id: Uuid,
        text_state: &str,
        normalized_text: Option<&str>,
        text_checksum: Option<&str>,
        text_readable_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<KnowledgeRevision, ApiError> {
        let current = state
            .arango_document_store
            .get_revision(revision_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("knowledge_revision", revision_id))?;
        let row = state
            .arango_document_store
            .update_revision_text_content(
                revision_id,
                normalized_text.or(current.normalized_text.as_deref()),
                text_checksum.or(current.text_checksum.as_deref()),
                text_state,
                text_readable_at.or(current.text_readable_at),
            )
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("knowledge_revision", revision_id))?;
        Ok(map_revision_row(row))
    }

    pub async fn write_chunk(
        &self,
        state: &AppState,
        command: CreateKnowledgeChunkCommand,
    ) -> Result<KnowledgeChunk, ApiError> {
        let row = state
            .arango_document_store
            .upsert_chunk(&crate::infra::arangodb::document_store::KnowledgeChunkRow {
                key: command.chunk_id.to_string(),
                arango_id: None,
                arango_rev: None,
                chunk_id: command.chunk_id,
                workspace_id: command.workspace_id,
                library_id: command.library_id,
                document_id: command.document_id,
                revision_id: command.revision_id,
                chunk_index: command.chunk_index,
                content_text: command.content_text,
                normalized_text: command.normalized_text,
                span_start: command.span_start,
                span_end: command.span_end,
                token_count: command.token_count,
                section_path: command.section_path,
                heading_trail: command.heading_trail,
                chunk_state: command.chunk_state,
                text_generation: command.text_generation,
                vector_generation: command.vector_generation,
            })
            .await
            .map_err(|_| ApiError::Internal)?;
        state
            .arango_graph_store
            .upsert_revision_chunk_edge(command.revision_id, command.chunk_id)
            .await
            .map_err(|_| ApiError::Internal)?;
        Ok(map_chunk_row(row))
    }

    pub async fn list_revision_chunks(
        &self,
        state: &AppState,
        revision_id: Uuid,
    ) -> Result<Vec<KnowledgeChunk>, ApiError> {
        let rows = state
            .arango_document_store
            .list_chunks_by_revision(revision_id)
            .await
            .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_chunk_row).collect())
    }

    pub async fn delete_revision_chunks(
        &self,
        state: &AppState,
        revision_id: Uuid,
    ) -> Result<Vec<KnowledgeChunk>, ApiError> {
        let _ = state
            .arango_graph_store
            .delete_revision_chunk_edges(revision_id)
            .await
            .map_err(|_| ApiError::Internal)?;
        let rows = state
            .arango_document_store
            .delete_chunks_by_revision(revision_id)
            .await
            .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_chunk_row).collect())
    }

    pub async fn refresh_library_generation(
        &self,
        state: &AppState,
        command: RefreshKnowledgeLibraryGenerationCommand,
    ) -> Result<KnowledgeLibraryGeneration, ApiError> {
        let row = state
            .arango_document_store
            .upsert_library_generation(
                &crate::infra::arangodb::document_store::KnowledgeLibraryGenerationRow {
                    key: format!("{}:{}", command.library_id, command.generation_id),
                    arango_id: None,
                    arango_rev: None,
                    generation_id: command.generation_id,
                    workspace_id: command.workspace_id,
                    library_id: command.library_id,
                    active_text_generation: command.active_text_generation,
                    active_vector_generation: command.active_vector_generation,
                    active_graph_generation: command.active_graph_generation,
                    degraded_state: command.degraded_state,
                    updated_at: chrono::Utc::now(),
                },
            )
            .await
            .map_err(|_| ApiError::Internal)?;
        Ok(map_library_generation_row(row))
    }

    pub async fn get_bundle(
        &self,
        _state: &AppState,
        bundle_id: Uuid,
    ) -> Result<KnowledgeContextBundle, ApiError> {
        Err(ApiError::context_bundle_not_found(bundle_id))
    }

    pub async fn list_library_generations(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<Vec<KnowledgeLibraryGeneration>, ApiError> {
        let rows = state
            .arango_document_store
            .list_library_generations(library_id)
            .await
            .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_library_generation_row).collect())
    }
}

fn map_document_row(
    row: crate::infra::arangodb::document_store::KnowledgeDocumentRow,
) -> KnowledgeDocument {
    KnowledgeDocument {
        id: row.document_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        external_key: row.external_key,
        document_state: row.document_state,
        active_revision_id: row.active_revision_id,
        readable_revision_id: row.readable_revision_id,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn map_revision_row(
    row: crate::infra::arangodb::document_store::KnowledgeRevisionRow,
) -> KnowledgeRevision {
    KnowledgeRevision {
        id: row.revision_id,
        document_id: row.document_id,
        revision_number: row.revision_number,
        revision_state: row.revision_state,
        source_uri: row.source_uri,
        mime_type: row.mime_type,
        checksum: row.checksum,
        title: row.title,
        byte_size: row.byte_size,
        normalized_text: row.normalized_text,
        text_checksum: row.text_checksum,
        text_state: row.text_state,
        vector_state: row.vector_state,
        graph_state: row.graph_state,
        text_readable_at: row.text_readable_at,
        vector_ready_at: row.vector_ready_at,
        graph_ready_at: row.graph_ready_at,
        created_at: row.created_at,
    }
}

fn map_chunk_row(row: crate::infra::arangodb::document_store::KnowledgeChunkRow) -> KnowledgeChunk {
    KnowledgeChunk {
        id: row.chunk_id,
        revision_id: row.revision_id,
        chunk_index: row.chunk_index,
        content_text: row.content_text,
        token_count: row.token_count,
    }
}

fn map_library_generation_row(
    row: crate::infra::arangodb::document_store::KnowledgeLibraryGenerationRow,
) -> KnowledgeLibraryGeneration {
    let generation_state = if row.active_graph_generation > 0 {
        "graph_ready"
    } else if row.active_vector_generation > 0 {
        "vector_ready"
    } else if row.active_text_generation > 0 {
        "text_readable"
    } else {
        "accepted"
    };
    KnowledgeLibraryGeneration {
        id: row.generation_id,
        library_id: row.library_id,
        generation_kind: "library".to_string(),
        generation_state: generation_state.to_string(),
        source_revision_id: None,
        created_at: row.updated_at,
        completed_at: None,
    }
}
