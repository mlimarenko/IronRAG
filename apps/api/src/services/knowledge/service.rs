#![allow(
    clippy::all,
    clippy::missing_const_for_fn,
    clippy::missing_errors_doc,
    clippy::needless_pass_by_value,
    clippy::result_large_err,
    clippy::too_many_lines
)]

use sha2::Digest;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        content::LibraryKnowledgeCoverage,
        knowledge::{
            KnowledgeChunk, KnowledgeContextBundle, KnowledgeDocument, KnowledgeLibraryGeneration,
            KnowledgeLibrarySummary, KnowledgeRevision, StructuredBlock,
            StructuredDocumentRevision, TypedTechnicalFact,
        },
    },
    infra::repositories,
    interfaces::http::router_support::ApiError,
    shared::extraction::technical_facts::{
        TechnicalFactKind, TechnicalFactQualifier, TechnicalFactValue,
    },
};

#[derive(Debug, Clone)]
pub struct CreateKnowledgeDocumentCommand {
    pub document_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub external_key: String,
    pub file_name: Option<String>,
    pub title: Option<String>,
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
    pub chunk_kind: Option<String>,
    pub content_text: String,
    pub normalized_text: String,
    pub span_start: Option<i32>,
    pub span_end: Option<i32>,
    pub token_count: Option<i32>,
    pub support_block_ids: Vec<Uuid>,
    pub section_path: Vec<String>,
    pub heading_trail: Vec<String>,
    pub literal_digest: Option<String>,
    pub chunk_state: String,
    pub text_generation: Option<i64>,
    pub vector_generation: Option<i64>,
    pub quality_score: Option<f32>,
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
                file_name: command.file_name,
                title: command.title,
                document_state: command.document_state,
                active_revision_id: None,
                readable_revision_id: None,
                latest_revision_no: None,
                created_at: now,
                updated_at: now,
                deleted_at: None,
            })
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
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
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        state
            .arango_graph_store
            .upsert_document_revision_edge(
                command.document_id,
                command.revision_id,
                command.library_id,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(map_revision_row(row))
    }

    pub async fn promote_document(
        &self,
        state: &AppState,
        command: PromoteKnowledgeDocumentCommand,
    ) -> Result<KnowledgeDocument, ApiError> {
        let content_document = repositories::content_repository::get_document_by_id(
            &state.persistence.postgres,
            command.document_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("document", command.document_id))?;
        let existing_projection = state
            .arango_document_store
            .get_document(command.document_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let title_source_revision_id = command.readable_revision_id.or(command.active_revision_id);
        let resolved_title = match title_source_revision_id {
            Some(revision_id) => state
                .arango_document_store
                .get_revision(revision_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .and_then(|row| row.title),
            None => None,
        };
        let row = state
            .arango_document_store
            .upsert_document(&crate::infra::arangodb::document_store::KnowledgeDocumentRow {
                key: command.document_id.to_string(),
                arango_id: existing_projection.as_ref().and_then(|row| row.arango_id.clone()),
                arango_rev: existing_projection.as_ref().and_then(|row| row.arango_rev.clone()),
                document_id: command.document_id,
                workspace_id: content_document.workspace_id,
                library_id: content_document.library_id,
                external_key: content_document.external_key,
                file_name: existing_projection.as_ref().and_then(|row| row.file_name.clone()),
                title: resolved_title
                    .or_else(|| existing_projection.as_ref().and_then(|row| row.title.clone())),
                document_state: command.document_state,
                active_revision_id: command.active_revision_id,
                readable_revision_id: command.readable_revision_id,
                latest_revision_no: command.latest_revision_no,
                created_at: existing_projection
                    .as_ref()
                    .map(|row| row.created_at)
                    .unwrap_or(content_document.created_at),
                updated_at: chrono::Utc::now(),
                deleted_at: command.deleted_at,
            })
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
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
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
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
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("knowledge_revision", revision_id))?;
        Ok(map_revision_row(row))
    }

    pub async fn set_revision_extract_state(
        &self,
        state: &AppState,
        revision_id: Uuid,
        extract_state: &str,
        normalized_text: Option<&str>,
        text_checksum: Option<&str>,
    ) -> Result<KnowledgeRevision, ApiError> {
        let text_readable_at = matches!(extract_state, "readable" | "ready")
            .then_some(chrono::Utc::now())
            .filter(|_| normalized_text.is_some_and(|text| !text.trim().is_empty()));
        self.set_revision_text_state(
            state,
            revision_id,
            match extract_state {
                "readable" | "ready" => "text_readable",
                "failed" => "failed",
                "processing" => "extracting_text",
                _ => "accepted",
            },
            normalized_text,
            text_checksum,
            text_readable_at,
        )
        .await
    }

    pub async fn set_revision_storage_ref(
        &self,
        state: &AppState,
        revision_id: Uuid,
        storage_ref: Option<&str>,
    ) -> Result<KnowledgeRevision, ApiError> {
        let row = state
            .arango_document_store
            .update_revision_storage_ref(revision_id, storage_ref)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
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
                chunk_kind: command.chunk_kind,
                content_text: command.content_text,
                normalized_text: command.normalized_text,
                span_start: command.span_start,
                span_end: command.span_end,
                token_count: command.token_count,
                support_block_ids: command.support_block_ids,
                section_path: command.section_path,
                heading_trail: command.heading_trail,
                literal_digest: command.literal_digest,
                chunk_state: command.chunk_state,
                text_generation: command.text_generation,
                vector_generation: command.vector_generation,
                quality_score: command.quality_score,
            })
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        state
            .arango_graph_store
            .upsert_revision_chunk_edge(command.revision_id, command.chunk_id, command.library_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(map_chunk_row(row))
    }

    pub async fn write_chunks(
        &self,
        state: &AppState,
        commands: Vec<CreateKnowledgeChunkCommand>,
    ) -> Result<Vec<KnowledgeChunk>, ApiError> {
        if commands.is_empty() {
            return Ok(Vec::new());
        }

        let rows = commands
            .iter()
            .map(|command| crate::infra::arangodb::document_store::KnowledgeChunkRow {
                key: command.chunk_id.to_string(),
                arango_id: None,
                arango_rev: None,
                chunk_id: command.chunk_id,
                workspace_id: command.workspace_id,
                library_id: command.library_id,
                document_id: command.document_id,
                revision_id: command.revision_id,
                chunk_index: command.chunk_index,
                chunk_kind: command.chunk_kind.clone(),
                content_text: command.content_text.clone(),
                normalized_text: command.normalized_text.clone(),
                span_start: command.span_start,
                span_end: command.span_end,
                token_count: command.token_count,
                support_block_ids: command.support_block_ids.clone(),
                section_path: command.section_path.clone(),
                heading_trail: command.heading_trail.clone(),
                literal_digest: command.literal_digest.clone(),
                chunk_state: command.chunk_state.clone(),
                text_generation: command.text_generation,
                vector_generation: command.vector_generation,
                quality_score: command.quality_score,
            })
            .collect::<Vec<_>>();

        let inserted = state
            .arango_document_store
            .insert_chunks(&rows)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

        let mut chunk_ids_by_revision =
            std::collections::BTreeMap::<Uuid, (Uuid, Vec<Uuid>)>::new();
        for command in &commands {
            let entry = chunk_ids_by_revision
                .entry(command.revision_id)
                .or_insert_with(|| (command.library_id, Vec::new()));
            entry.1.push(command.chunk_id);
        }
        for (revision_id, (library_id, chunk_ids)) in chunk_ids_by_revision {
            state
                .arango_graph_store
                .insert_revision_chunk_edges(revision_id, &chunk_ids, library_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        }

        Ok(inserted.into_iter().map(map_chunk_row).collect())
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
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(rows.into_iter().map(map_chunk_row).collect())
    }

    pub async fn get_structured_revision(
        &self,
        state: &AppState,
        revision_id: Uuid,
    ) -> Result<Option<StructuredDocumentRevision>, ApiError> {
        let row = state
            .arango_document_store
            .get_structured_revision(revision_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        row.map(map_structured_revision_row).transpose()
    }

    pub async fn list_document_structured_revisions(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<Vec<StructuredDocumentRevision>, ApiError> {
        let rows = state
            .arango_document_store
            .list_structured_revisions_by_document(document_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        rows.into_iter().map(map_structured_revision_row).collect()
    }

    pub async fn list_structured_blocks(
        &self,
        state: &AppState,
        revision_id: Uuid,
    ) -> Result<Vec<StructuredBlock>, ApiError> {
        let rows = state
            .arango_document_store
            .list_structured_blocks_by_revision(revision_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        rows.into_iter().map(map_structured_block_row).collect()
    }

    pub async fn list_typed_technical_facts(
        &self,
        state: &AppState,
        revision_id: Uuid,
    ) -> Result<Vec<TypedTechnicalFact>, ApiError> {
        let rows = state
            .arango_document_store
            .list_technical_facts_by_revision(revision_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        rows.into_iter().map(map_typed_technical_fact_row).collect()
    }

    pub async fn list_typed_technical_facts_by_ids(
        &self,
        state: &AppState,
        fact_ids: &[Uuid],
    ) -> Result<Vec<TypedTechnicalFact>, ApiError> {
        if fact_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = state
            .arango_document_store
            .list_technical_facts_by_ids(fact_ids)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        rows.into_iter().map(map_typed_technical_fact_row).collect()
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
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let rows = state
            .arango_document_store
            .delete_chunks_by_revision(revision_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
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
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(map_library_generation_row(row))
    }

    pub fn get_bundle(
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
        let rows = self.derive_library_generation_rows(state, library_id).await?;
        Ok(rows.into_iter().map(map_library_generation_row).collect())
    }

    pub async fn get_library_knowledge_coverage(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<LibraryKnowledgeCoverage, ApiError> {
        let summary = self.get_library_summary(state, library_id).await?;
        let last_generation_id = summary.latest_generation.as_ref().map(|generation| generation.id);
        Ok(LibraryKnowledgeCoverage {
            library_id: summary.library_id,
            document_counts_by_readiness: summary.document_counts_by_readiness,
            graph_ready_document_count: summary.graph_ready_document_count,
            graph_sparse_document_count: summary.graph_sparse_document_count,
            typed_fact_document_count: summary.typed_fact_document_count,
            last_generation_id,
            updated_at: summary.updated_at,
        })
    }

    /// Canonical library summary. Was previously enumerating every document
    /// via `list_documents` (N × 6 prefetch round-trips — catastrophic on
    /// a reference library with ~5 k documents). Now a single aggregate Postgres query
    /// plus the graph-snapshot row plus the library generations list.
    pub async fn get_library_summary(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<KnowledgeLibrarySummary, ApiError> {
        let (readiness, generations, graph_snapshot) = tokio::try_join!(
            async {
                repositories::content_repository::aggregate_library_document_readiness(
                    &state.persistence.postgres,
                    library_id,
                )
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))
            },
            self.list_library_generations(state, library_id),
            async {
                repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
                    .await
                    .map_err(|e| ApiError::internal_with_log(e, "internal"))
            },
        )?;

        let latest_generation = generations.into_iter().next();

        // Canonical definition of "graph-ready": the document has a document
        // node in the current projection. The old proxy (readable AND any
        // graph snapshot exists) reported every readable document as
        // graph-ready the moment the library had a single node, so when
        // individual ingest jobs failed after LLM extraction but before the
        // graph merge (timeouts, cancelled stage) the dashboard kept
        // claiming full graph coverage while the graph viewer showed a
        // fraction of the documents. Counting document-typed nodes directly
        // in `runtime_graph_node` at the active projection version keeps
        // the counter honest.
        let graph_ready_document_count = if let Some(snapshot) = graph_snapshot.as_ref() {
            repositories::count_runtime_graph_document_nodes_by_library(
                &state.persistence.postgres,
                library_id,
                snapshot.projection_version,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        } else {
            0
        };
        let graph_sparse_document_count =
            readiness.readable_count.saturating_sub(graph_ready_document_count);

        let mut document_counts_by_readiness = std::collections::BTreeMap::<String, i64>::new();
        if readiness.failed_count > 0 {
            document_counts_by_readiness.insert("failed".to_string(), readiness.failed_count);
        }
        if readiness.processing_count > 0 {
            document_counts_by_readiness
                .insert("processing".to_string(), readiness.processing_count);
        }
        if graph_ready_document_count > 0 {
            document_counts_by_readiness
                .insert("graph_ready".to_string(), graph_ready_document_count);
        }
        if graph_sparse_document_count > 0 {
            document_counts_by_readiness
                .insert("graph_sparse".to_string(), graph_sparse_document_count);
        }

        Ok(KnowledgeLibrarySummary {
            library_id,
            document_counts_by_readiness,
            node_count: graph_snapshot
                .as_ref()
                .map_or(0, |snapshot| i64::from(snapshot.node_count)),
            edge_count: graph_snapshot
                .as_ref()
                .map_or(0, |snapshot| i64::from(snapshot.edge_count)),
            graph_ready_document_count,
            graph_sparse_document_count,
            // typed_fact count is a refinement that lives in ArangoDB's
            // structured revision rows; without enumerating documents we
            // cannot derive it cheaply. Report 0 rather than N round-trips —
            // clients that need the exact figure should hit the dedicated
            // library coverage endpoint.
            typed_fact_document_count: 0,
            updated_at: chrono::Utc::now(),
            latest_generation,
        })
    }
}

impl KnowledgeService {
    pub async fn derive_library_generation_rows(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<Vec<crate::infra::arangodb::document_store::KnowledgeLibraryGenerationRow>, ApiError>
    {
        // Canonical one-shot aggregate — the previous implementation
        // iterated every document + fetched its revision list one
        // document at a time, producing ~5k sequential Arango round-trips
        // on a 5k-doc library and dominating dashboard latency. The
        // aggregate returns the three readable revision numbers and a
        // `latest_created_at` field in a single AQL call.
        let library = state.canonical_services.catalog.get_library(state, library_id).await?;
        let signals = state
            .arango_document_store
            .aggregate_library_generation_signals(library_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

        if !signals.has_ready_text && !signals.has_ready_vector && !signals.has_ready_graph {
            return Ok(Vec::new());
        }

        let degraded_state =
            if signals.has_ready_text && signals.has_ready_vector && signals.has_ready_graph {
                "ready"
            } else {
                "degraded"
            };
        let generation_id = derive_library_generation_id(
            library_id,
            signals.active_text_generation,
            signals.active_vector_generation,
            signals.active_graph_generation,
            degraded_state,
        );
        Ok(vec![crate::infra::arangodb::document_store::KnowledgeLibraryGenerationRow {
            key: library_id.to_string(),
            arango_id: None,
            arango_rev: None,
            generation_id,
            workspace_id: library.workspace_id,
            library_id,
            active_text_generation: signals.active_text_generation,
            active_vector_generation: signals.active_vector_generation,
            active_graph_generation: signals.active_graph_generation,
            degraded_state: degraded_state.to_string(),
            updated_at: signals.latest_created_at.unwrap_or_else(chrono::Utc::now),
        }])
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
        title: row.title,
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

fn map_structured_revision_row(
    row: crate::infra::arangodb::document_store::KnowledgeStructuredRevisionRow,
) -> Result<StructuredDocumentRevision, ApiError> {
    let outline = serde_json::from_value(row.outline_json)
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    Ok(StructuredDocumentRevision {
        revision_id: row.revision_id,
        document_id: row.document_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        preparation_state: row.preparation_state,
        normalization_profile: row.normalization_profile,
        source_format: row.source_format,
        language_code: row.language_code,
        block_count: row.block_count,
        chunk_count: row.chunk_count,
        typed_fact_count: row.typed_fact_count,
        outline,
        prepared_at: row.prepared_at,
    })
}

pub(crate) fn map_structured_block_row(
    row: crate::infra::arangodb::document_store::KnowledgeStructuredBlockRow,
) -> Result<StructuredBlock, ApiError> {
    let block_kind =
        row.block_kind.parse().map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let table_coordinates = row
        .table_coordinates_json
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    Ok(StructuredBlock {
        block_id: row.block_id,
        revision_id: row.revision_id,
        ordinal: row.ordinal,
        block_kind,
        text: row.text,
        normalized_text: row.normalized_text,
        heading_trail: row.heading_trail,
        section_path: row.section_path,
        page_number: row.page_number,
        source_span: row.span_start.zip(row.span_end).map(|(start_offset, end_offset)| {
            crate::shared::extraction::structured_document::StructuredSourceSpan {
                start_offset,
                end_offset,
            }
        }),
        parent_block_id: row.parent_block_id,
        table_coordinates,
        code_language: row.code_language,
    })
}

fn map_typed_technical_fact_row(
    row: crate::infra::arangodb::document_store::KnowledgeTechnicalFactRow,
) -> Result<TypedTechnicalFact, ApiError> {
    let fact_kind = row
        .fact_kind
        .parse::<TechnicalFactKind>()
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let canonical_value = serde_json::from_value::<TechnicalFactValue>(row.canonical_value_json)
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let qualifiers = serde_json::from_value::<Vec<TechnicalFactQualifier>>(row.qualifiers_json)
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    Ok(TypedTechnicalFact {
        fact_id: row.fact_id,
        revision_id: row.revision_id,
        document_id: row.document_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        fact_kind,
        canonical_value,
        display_value: row.display_value,
        qualifiers,
        support_block_ids: row.support_block_ids,
        support_chunk_ids: row.support_chunk_ids,
        confidence: row.confidence,
        extraction_kind: row.extraction_kind,
        conflict_group_id: row.conflict_group_id,
        created_at: row.created_at,
    })
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

fn derive_library_generation_id(
    library_id: Uuid,
    active_text_generation: i64,
    active_vector_generation: i64,
    active_graph_generation: i64,
    degraded_state: &str,
) -> Uuid {
    let seed = format!(
        "library-generation:{library_id}:{active_text_generation}:{active_vector_generation}:{active_graph_generation}:{degraded_state}"
    );
    let digest = sha2::Sha256::digest(seed.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}
