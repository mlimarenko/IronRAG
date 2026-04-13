use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::content::{
        ContentChunk, ContentDocument, ContentDocumentHead, ContentDocumentPipelineJob,
        ContentDocumentPipelineState, ContentDocumentSummary, ContentMutation, ContentRevision,
        WebPageProvenance,
    },
    domains::knowledge::{PreparedSegmentDetail, PreparedSegmentListItem, TypedTechnicalFact},
    infra::arangodb::document_store::{KnowledgeDocumentRow, KnowledgeRevisionRow},
    infra::repositories::{self, catalog_repository, content_repository, ingest_repository},
    interfaces::http::router_support::ApiError,
    services::content::source_access::{derive_content_source_file_name, describe_content_source},
    services::knowledge::service::PromoteKnowledgeDocumentCommand,
    shared::extraction::document_summary::{
        DocumentSummaryBlock, build_document_summary_from_blocks,
    },
};

use super::{
    ContentService, PrefetchedDocumentSummaryData, ReconcileFailedIngestMutationCommand,
    map_document_pipeline_job, map_document_row, map_knowledge_chunk_row,
    map_knowledge_document_row, map_knowledge_revision_readiness, map_knowledge_revision_row,
    map_mutation_row, map_revision_row, map_structured_revision_row, map_web_page_provenance_row,
    segment_excerpt,
};

impl ContentService {
    pub async fn list_documents(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<Vec<ContentDocumentSummary>, ApiError> {
        self.list_documents_with_deleted(state, library_id, false).await
    }

    pub async fn list_documents_with_deleted(
        &self,
        state: &AppState,
        library_id: Uuid,
        include_deleted: bool,
    ) -> Result<Vec<ContentDocumentSummary>, ApiError> {
        let library =
            catalog_repository::get_library_by_id(&state.persistence.postgres, library_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("library", library_id))?;
        let documents = state
            .arango_document_store
            .list_documents_by_library(library.workspace_id, library_id, include_deleted)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let prefetched_summary_data =
            self.prefetch_document_summary_data(state, &documents).await?;
        let document_ids = documents.iter().map(|row| row.document_id).collect::<Vec<_>>();
        let content_heads = content_repository::list_document_heads_by_document_ids(
            &state.persistence.postgres,
            &document_ids,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let latest_mutation_ids =
            content_heads.iter().filter_map(|row| row.latest_mutation_id).collect::<Vec<_>>();
        let mutations_by_id = content_repository::list_mutations_by_ids(
            &state.persistence.postgres,
            &latest_mutation_ids,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .into_iter()
        .map(|row| (row.id, row))
        .collect::<HashMap<_, _>>();
        let job_handles_by_mutation_id = state
            .canonical_services
            .ingest
            .list_job_handles_by_mutation_ids(
                state,
                library.workspace_id,
                library_id,
                &latest_mutation_ids,
            )
            .await?
            .into_iter()
            .filter_map(|handle| handle.job.mutation_id.map(|mutation_id| (mutation_id, handle)))
            .collect::<HashMap<_, _>>();
        let heads_by_document_id =
            content_heads.into_iter().map(|row| (row.document_id, row)).collect::<HashMap<_, _>>();
        let mut summaries = Vec::with_capacity(documents.len());
        for row in documents {
            let content_head = heads_by_document_id.get(&row.document_id);
            let latest_mutation = content_head
                .and_then(|head| head.latest_mutation_id)
                .and_then(|mutation_id| mutations_by_id.get(&mutation_id).cloned())
                .map(map_mutation_row);
            let latest_job = content_head
                .and_then(|head| head.latest_mutation_id)
                .and_then(|mutation_id| job_handles_by_mutation_id.get(&mutation_id).cloned())
                .map(map_document_pipeline_job);
            summaries.push(self.build_document_summary_from_prefetched(
                state,
                row,
                content_head,
                latest_mutation,
                latest_job,
                &prefetched_summary_data,
            ));
        }
        Ok(summaries)
    }

    pub async fn get_document(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<ContentDocumentSummary, ApiError> {
        let row = state
            .arango_document_store
            .get_document(document_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;
        let content_head =
            content_repository::get_document_head(&state.persistence.postgres, document_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let latest_mutation = match content_head.as_ref().and_then(|head| head.latest_mutation_id) {
            Some(mutation_id) => {
                content_repository::get_mutation_by_id(&state.persistence.postgres, mutation_id)
                    .await
                    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                    .map(map_mutation_row)
            }
            None => None,
        };
        let latest_job = match content_head.as_ref().and_then(|head| head.latest_mutation_id) {
            Some(mutation_id) => state
                .canonical_services
                .ingest
                .get_job_handle_by_mutation_id(state, mutation_id)
                .await?
                .map(map_document_pipeline_job),
            None => None,
        };
        self.build_document_summary_from_knowledge(
            state,
            row,
            content_head.as_ref(),
            latest_mutation,
            latest_job,
        )
        .await
    }

    pub async fn get_document_by_external_key(
        &self,
        state: &AppState,
        library_id: Uuid,
        external_key: &str,
    ) -> Result<Option<ContentDocument>, ApiError> {
        let row = content_repository::get_document_by_external_key(
            &state.persistence.postgres,
            library_id,
            external_key,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(row.map(map_document_row))
    }

    pub async fn get_document_head(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<Option<ContentDocumentHead>, ApiError> {
        let document = state
            .arango_document_store
            .get_document(document_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let Some(document) = document else {
            return Ok(None);
        };
        let row = content_repository::get_document_head(&state.persistence.postgres, document_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(Some(ContentDocumentHead {
            document_id,
            active_revision_id: document.active_revision_id,
            readable_revision_id: document.readable_revision_id,
            latest_mutation_id: row.as_ref().and_then(|head| head.latest_mutation_id),
            latest_successful_attempt_id: row
                .as_ref()
                .and_then(|head| head.latest_successful_attempt_id),
            head_updated_at: row.as_ref().map_or(document.updated_at, |head| head.head_updated_at),
            document_summary: row.and_then(|head| head.document_summary),
        }))
    }

    pub async fn list_revisions(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<Vec<ContentRevision>, ApiError> {
        let rows = state
            .arango_document_store
            .list_revisions_by_document(document_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(rows.into_iter().map(map_knowledge_revision_row).collect())
    }

    pub async fn list_chunks(
        &self,
        state: &AppState,
        revision_id: Uuid,
    ) -> Result<Vec<ContentChunk>, ApiError> {
        let rows = state
            .arango_document_store
            .list_chunks_by_revision(revision_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(rows.into_iter().map(map_knowledge_chunk_row).collect())
    }

    pub async fn list_prepared_segments(
        &self,
        state: &AppState,
        revision_id: Uuid,
    ) -> Result<Vec<PreparedSegmentDetail>, ApiError> {
        let blocks =
            state.canonical_services.knowledge.list_structured_blocks(state, revision_id).await?;
        let chunk_rows = state
            .arango_document_store
            .list_chunks_by_revision(revision_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let mut support_chunk_ids_by_block = std::collections::BTreeMap::<Uuid, Vec<Uuid>>::new();
        for chunk in chunk_rows {
            for block_id in chunk.support_block_ids {
                support_chunk_ids_by_block.entry(block_id).or_default().push(chunk.chunk_id);
            }
        }
        Ok(blocks
            .into_iter()
            .map(|block| PreparedSegmentDetail {
                segment: PreparedSegmentListItem {
                    segment_id: block.block_id,
                    revision_id: block.revision_id,
                    ordinal: block.ordinal,
                    block_kind: block.block_kind.clone(),
                    heading_trail: block.heading_trail.clone(),
                    section_path: block.section_path.clone(),
                    page_number: block.page_number,
                    excerpt: segment_excerpt(&block.normalized_text),
                },
                text: block.text,
                normalized_text: block.normalized_text,
                source_span: block.source_span,
                parent_block_id: block.parent_block_id,
                table_coordinates: block.table_coordinates,
                code_language: block.code_language,
                support_chunk_ids: support_chunk_ids_by_block
                    .remove(&block.block_id)
                    .unwrap_or_default(),
            })
            .collect())
    }

    pub async fn list_technical_facts(
        &self,
        state: &AppState,
        revision_id: Uuid,
    ) -> Result<Vec<TypedTechnicalFact>, ApiError> {
        state.canonical_services.knowledge.list_typed_technical_facts(state, revision_id).await
    }

    // --- Document summary construction ---

    pub(crate) async fn build_document_summary_from_knowledge(
        &self,
        state: &AppState,
        document_row: KnowledgeDocumentRow,
        content_head: Option<&content_repository::ContentDocumentHeadRow>,
        latest_mutation: Option<ContentMutation>,
        latest_job: Option<ContentDocumentPipelineJob>,
    ) -> Result<ContentDocumentSummary, ApiError> {
        let active_revision_row = if let Some(revision_id) = document_row.active_revision_id {
            state
                .arango_document_store
                .get_revision(revision_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        } else {
            None
        };
        let readable_revision_row =
            match (document_row.readable_revision_id, active_revision_row.as_ref()) {
                (Some(readable_revision_id), Some(active_row))
                    if readable_revision_id == active_row.revision_id =>
                {
                    Some(active_row.clone())
                }
                (Some(readable_revision_id), _) => state
                    .arango_document_store
                    .get_revision(readable_revision_id)
                    .await
                    .map_err(|e| ApiError::internal_with_log(e, "internal"))?,
                (None, _) => None,
            };
        let effective_readiness_row =
            readable_revision_row.clone().or_else(|| active_revision_row.clone());
        let prepared_revision_row =
            match effective_readiness_row.as_ref().map(|row| row.revision_id) {
                Some(revision_id) => state
                    .arango_document_store
                    .get_structured_revision(revision_id)
                    .await
                    .map_err(|e| ApiError::internal_with_log(e, "internal"))?,
                None => None,
            };
        let web_page_row = match active_revision_row
            .as_ref()
            .filter(|revision| revision.revision_kind == "web_page")
            .map(|revision| revision.revision_id)
        {
            Some(revision_id) => ingest_repository::get_web_discovered_page_by_result_revision_id(
                &state.persistence.postgres,
                revision_id,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?,
            None => None,
        };

        let mut revisions_by_id = HashMap::new();
        if let Some(revision) = active_revision_row.clone() {
            revisions_by_id.insert(revision.revision_id, revision);
        }
        if let Some(revision) = readable_revision_row {
            revisions_by_id.entry(revision.revision_id).or_insert(revision);
        }
        if let Some(revision) = effective_readiness_row.clone() {
            revisions_by_id.entry(revision.revision_id).or_insert(revision);
        }

        let mut structured_revisions_by_revision_id = HashMap::new();
        if let Some(revision) = prepared_revision_row {
            structured_revisions_by_revision_id.insert(revision.revision_id, revision);
        }

        let mut web_pages_by_result_revision_id = HashMap::new();
        if let Some(page) = web_page_row
            .and_then(|row| row.result_revision_id.map(|revision_id| (revision_id, row)))
        {
            web_pages_by_result_revision_id.insert(page.0, page.1);
        }

        let document_summary = self
            .resolve_document_summary_from_revision(
                state,
                document_row.document_id,
                effective_readiness_row.as_ref().map(|revision| revision.revision_id),
                content_head.and_then(|row| row.document_summary.as_deref()),
            )
            .await?;

        let mut summary = self.build_document_summary_from_prefetched(
            state,
            document_row,
            content_head,
            latest_mutation,
            latest_job,
            &PrefetchedDocumentSummaryData {
                revisions_by_id,
                structured_revisions_by_revision_id,
                web_pages_by_result_revision_id,
            },
        );
        if let Some(head) = summary.head.as_mut() {
            head.document_summary = document_summary;
        }
        Ok(summary)
    }

    async fn resolve_document_summary_from_revision(
        &self,
        state: &AppState,
        document_id: Uuid,
        revision_id: Option<Uuid>,
        stored_summary: Option<&str>,
    ) -> Result<Option<String>, ApiError> {
        let stored_summary =
            stored_summary.map(str::trim).filter(|value| !value.is_empty()).map(str::to_string);
        let Some(revision_id) = revision_id else {
            return Ok(stored_summary);
        };

        let blocks = state
            .arango_document_store
            .list_structured_blocks_by_revision(revision_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        if blocks.is_empty() {
            return Ok(stored_summary);
        }

        let summary = build_document_summary_from_blocks(blocks.iter().map(|block| {
            DocumentSummaryBlock { block_kind: &block.block_kind, text: &block.text }
        }));
        if summary.is_empty() {
            return Ok(stored_summary);
        }

        if stored_summary.as_deref() != Some(summary.as_str()) {
            content_repository::update_document_summary(
                &state.persistence.postgres,
                document_id,
                &summary,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        }

        Ok(Some(summary))
    }

    pub(super) async fn prefetch_document_summary_data(
        &self,
        state: &AppState,
        documents: &[KnowledgeDocumentRow],
    ) -> Result<PrefetchedDocumentSummaryData, ApiError> {
        let revision_ids = documents
            .iter()
            .flat_map(|document| [document.active_revision_id, document.readable_revision_id])
            .flatten()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let revisions_by_id = state
            .arango_document_store
            .list_revisions_by_ids(&revision_ids)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .into_iter()
            .map(|row| (row.revision_id, row))
            .collect::<HashMap<_, _>>();

        let effective_revision_ids = documents
            .iter()
            .filter_map(|document| {
                self.resolve_effective_readiness_row(document, None, &revisions_by_id)
            })
            .map(|revision| revision.revision_id)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let structured_revisions_by_revision_id = state
            .arango_document_store
            .list_structured_revisions_by_revision_ids(&effective_revision_ids)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .into_iter()
            .map(|row| (row.revision_id, row))
            .collect::<HashMap<_, _>>();

        let web_page_revision_ids = documents
            .iter()
            .filter_map(|document| document.active_revision_id)
            .filter(|revision_id| {
                revisions_by_id
                    .get(revision_id)
                    .is_some_and(|revision| revision.revision_kind == "web_page")
            })
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let web_pages_by_result_revision_id =
            ingest_repository::list_web_discovered_pages_by_result_revision_ids(
                &state.persistence.postgres,
                &web_page_revision_ids,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .into_iter()
            .filter_map(|row| row.result_revision_id.map(|revision_id| (revision_id, row)))
            .collect::<HashMap<_, _>>();

        Ok(PrefetchedDocumentSummaryData {
            revisions_by_id,
            structured_revisions_by_revision_id,
            web_pages_by_result_revision_id,
        })
    }

    pub(super) fn build_document_summary_from_prefetched(
        &self,
        state: &AppState,
        document_row: KnowledgeDocumentRow,
        content_head: Option<&content_repository::ContentDocumentHeadRow>,
        latest_mutation: Option<ContentMutation>,
        latest_job: Option<ContentDocumentPipelineJob>,
        prefetched: &PrefetchedDocumentSummaryData,
    ) -> ContentDocumentSummary {
        let document_external_key = document_row.external_key.clone();
        let deleted_document =
            document_row.document_state == "deleted" || document_row.deleted_at.is_some();
        let active_revision_row = document_row
            .active_revision_id
            .and_then(|revision_id| prefetched.revisions_by_id.get(&revision_id).cloned());
        let active_revision = active_revision_row.clone().map(map_knowledge_revision_row);
        let display_revision_row = self.resolve_effective_readiness_row(
            &document_row,
            active_revision_row.as_ref(),
            &prefetched.revisions_by_id,
        );
        let effective_readiness_row =
            (!deleted_document).then_some(display_revision_row.clone()).flatten();
        let prepared_revision = effective_readiness_row
            .as_ref()
            .and_then(|revision| {
                prefetched.structured_revisions_by_revision_id.get(&revision.revision_id)
            })
            .cloned()
            .map(map_structured_revision_row);
        let head = Some(ContentDocumentHead {
            document_id: document_row.document_id,
            active_revision_id: document_row.active_revision_id,
            readable_revision_id: document_row.readable_revision_id,
            latest_mutation_id: content_head.and_then(|row| row.latest_mutation_id),
            latest_successful_attempt_id: content_head
                .and_then(|row| row.latest_successful_attempt_id),
            head_updated_at: content_head
                .map_or(document_row.updated_at, |row| row.head_updated_at),
            document_summary: content_head.and_then(|row| row.document_summary.clone()),
        });
        let readiness_summary = (!deleted_document).then(|| {
            state.canonical_services.ops.derive_document_readiness_summary(
                state,
                document_row.document_id,
                document_row.active_revision_id,
                effective_readiness_row.as_ref(),
                prepared_revision.as_ref(),
                latest_mutation.as_ref(),
                latest_job.as_ref(),
                document_row.created_at,
            )
        });
        let source_descriptor = effective_readiness_row.as_ref().map(|revision| {
            describe_content_source(
                revision.document_id,
                Some(revision.revision_id),
                &revision.revision_kind,
                revision.source_uri.as_deref(),
                revision.storage_ref.as_deref(),
                revision.title.as_deref(),
                &document_external_key,
            )
        });
        let fallback_file_name = display_revision_row.as_ref().map(|revision| {
            derive_content_source_file_name(
                revision.source_uri.as_deref(),
                revision.title.as_deref(),
                &document_external_key,
            )
        });
        let web_page_provenance = effective_readiness_row
            .as_ref()
            .filter(|revision| revision.revision_kind == "web_page")
            .and_then(|revision| {
                prefetched
                    .web_pages_by_result_revision_id
                    .get(&revision.revision_id)
                    .map(map_web_page_provenance_row)
                    .or_else(|| {
                        Some(WebPageProvenance {
                            run_id: None,
                            candidate_id: None,
                            source_uri: revision.source_uri.clone(),
                            canonical_url: revision.source_uri.clone(),
                        })
                    })
            });

        ContentDocumentSummary {
            document: map_knowledge_document_row(&document_row),
            file_name: document_row
                .file_name
                .clone()
                .or_else(|| {
                    source_descriptor.as_ref().map(|descriptor| descriptor.file_name.clone())
                })
                .or(fallback_file_name)
                .or_else(|| active_revision.as_ref().and_then(|r| r.title.clone()))
                .unwrap_or_else(|| document_external_key.clone()),
            head,
            active_revision,
            source_access: source_descriptor.and_then(|descriptor| descriptor.access),
            readiness: effective_readiness_row.map(map_knowledge_revision_readiness),
            readiness_summary,
            prepared_revision,
            web_page_provenance,
            pipeline: ContentDocumentPipelineState { latest_mutation, latest_job },
        }
    }

    pub(crate) fn resolve_effective_readiness_row(
        &self,
        document_row: &KnowledgeDocumentRow,
        active_revision_row: Option<&KnowledgeRevisionRow>,
        revisions_by_id: &HashMap<Uuid, KnowledgeRevisionRow>,
    ) -> Option<KnowledgeRevisionRow> {
        match (
            document_row.readable_revision_id,
            document_row.active_revision_id,
            active_revision_row,
        ) {
            (Some(readable_revision_id), Some(active_revision_id), Some(active_row))
                if readable_revision_id == active_revision_id =>
            {
                Some(active_row.clone())
            }
            (Some(readable_revision_id), _, _) => revisions_by_id
                .get(&readable_revision_id)
                .cloned()
                .or_else(|| active_revision_row.cloned()),
            (None, Some(_), Some(active_row)) => Some(active_row.clone()),
            (None, Some(active_revision_id), None) => {
                revisions_by_id.get(&active_revision_id).cloned()
            }
            (None, None, _) => None,
        }
    }

    // --- Document lifecycle ---

    pub async fn delete_document(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<ContentDocument, ApiError> {
        self.delete_document_with_context(state, document_id, None).await
    }

    pub(crate) async fn delete_document_with_context(
        &self,
        state: &AppState,
        document_id: Uuid,
        latest_mutation_id: Option<Uuid>,
    ) -> Result<ContentDocument, ApiError> {
        let current_document =
            content_repository::get_document_by_id(&state.persistence.postgres, document_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;
        let head = self.get_document_head(state, document_id).await?;
        let readable_revision_id = head.as_ref().and_then(|row| row.readable_revision_id);
        let resolved_latest_mutation_id =
            latest_mutation_id.or_else(|| head.as_ref().and_then(|row| row.latest_mutation_id));
        let latest_successful_attempt_id =
            head.as_ref().and_then(|row| row.latest_successful_attempt_id);
        let latest_revision_no = self.load_document_latest_revision_no(state, document_id).await?;
        let deleted_at = current_document.deleted_at.or_else(|| Some(chrono::Utc::now()));

        let mut transaction = state
            .persistence
            .postgres
            .begin()
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let _ = ingest_repository::cancel_jobs_for_document_with_executor(
            &mut *transaction,
            document_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let document = content_repository::update_document_state_with_executor(
            &mut *transaction,
            document_id,
            "deleted",
            deleted_at,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;
        let _ = content_repository::upsert_document_head_with_executor(
            &mut *transaction,
            &content_repository::NewContentDocumentHead {
                document_id,
                active_revision_id: None,
                readable_revision_id,
                latest_mutation_id: resolved_latest_mutation_id,
                latest_successful_attempt_id,
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        transaction.commit().await.map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        self.promote_knowledge_document_best_effort(
            state,
            PromoteKnowledgeDocumentCommand {
                document_id,
                document_state: document.document_state.clone(),
                active_revision_id: None,
                readable_revision_id,
                latest_revision_no,
                deleted_at: document.deleted_at,
            },
            "post-delete knowledge promotion failed after document delete committed",
        )
        .await;
        if let Err(error) = self.converge_document_technical_facts(state, document_id, None).await {
            tracing::warn!(
                %document_id,
                library_id = %document.library_id,
                ?error,
                "post-delete technical fact convergence failed after document delete committed"
            );
        }
        if let Err(error) =
            self.refresh_deleted_document_graph_state(state, document.library_id, document_id).await
        {
            tracing::warn!(
                %document_id,
                library_id = %document.library_id,
                ?error,
                "post-delete graph convergence failed after document delete committed"
            );
        }

        Ok(ContentDocument {
            id: document.id,
            workspace_id: document.workspace_id,
            library_id: document.library_id,
            external_key: document.external_key,
            document_state: document.document_state,
            created_at: document.created_at,
        })
    }

    /// Resolves the revision that retry should re-run against. Prefers the
    /// currently-active revision (head pointer), but falls back to the latest
    /// revision row when the head was never promoted — this is the common
    /// shape for documents whose previous ingest crashed mid-pipeline before
    /// `promote_document_head` ever fired. The revision row exists in
    /// `content_revision` with a `storage_key`, the blob is in storage, but
    /// `content_document_head.active_revision_id` is still `NULL`, so the
    /// older retry path (`summary.active_revision`) would bail out and leave
    /// the document permanently stuck.
    ///
    /// When the document has literally zero revisions (an orphan document
    /// row left over from an even earlier crash that never persisted any
    /// source bytes), this function force-finalizes the document — it marks
    /// any inflight mutation as `failed`, flips queued/leased ingest jobs to
    /// `canceled`, and reports a `BadRequest` with `unrecoverable_no_source`.
    /// That moves the document from "needs attention" to "failed" so the
    /// operator can see it in the failed bucket and decide whether to delete
    /// it manually, instead of having it block retry forever with no way to
    /// recover.
    pub async fn resolve_reprocess_revision(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<ContentRevision, ApiError> {
        let summary = self.get_document(state, document_id).await?;
        if let Some(active) = summary.active_revision {
            return Ok(active);
        }
        let rows = content_repository::list_revisions_by_document(
            &state.persistence.postgres,
            document_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        if let Some(row) = rows.into_iter().next() {
            return Ok(map_revision_row(row));
        }
        // Zero revisions: this document is orphan debris with nothing to
        // recover from. Tombstone it (`document_state='deleted'`) so it
        // leaves the active list, and report it as `NotFound` — by the time
        // this function returns, the document is genuinely gone, and the
        // batch handler should bucket it under `skipped_count` (already
        // removed) instead of `failed_count`.
        self.force_fail_unrecoverable_document(state, document_id).await?;
        Err(ApiError::resource_not_found("document", document_id))
    }

    /// Permanently retires an orphan document that has no recoverable source.
    /// Cancels any inflight ingest jobs, flips an inflight mutation to
    /// `failed` (if any), and tombstones the document itself by setting
    /// `document_state='deleted'`. The document then disappears from the
    /// active list and stops jamming the retry loop. Idempotent — safe to
    /// call repeatedly.
    async fn force_fail_unrecoverable_document(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<(), ApiError> {
        if let Err(error) = ingest_repository::cancel_jobs_for_document(
            &state.persistence.postgres,
            document_id,
        )
        .await
        {
            return Err(ApiError::internal_with_log(error, "internal"));
        }
        if let Some(head) = self.get_document_head(state, document_id).await?
            && let Some(latest_mutation_id) = head.latest_mutation_id
            && let Some(latest_mutation) = content_repository::get_mutation_by_id(
                &state.persistence.postgres,
                latest_mutation_id,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            && matches!(latest_mutation.mutation_state.as_str(), "accepted" | "running")
        {
            self.reconcile_failed_ingest_mutation(
                state,
                ReconcileFailedIngestMutationCommand {
                    mutation_id: latest_mutation_id,
                    failure_code: "unrecoverable_no_source".to_string(),
                    failure_message:
                        "document has no content_revision rows; nothing to ingest from"
                            .to_string(),
                },
            )
            .await?;
        }
        // Tombstone the document so it leaves the active listing. Without
        // this the retry caller would see the same orphan back on the next
        // page refresh and try again forever.
        let _ = content_repository::update_document_state(
            &state.persistence.postgres,
            document_id,
            "deleted",
            Some(chrono::Utc::now()),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(())
    }

    /// Force-aborts any ingest work still attached to `document_id` and
    /// finalizes the latest mutation so a subsequent `admit_mutation` call
    /// can proceed. Called from the retry path — a user-initiated retry is
    /// an explicit "stop whatever was happening and start over", which the
    /// automatic `reconcile_stale_inflight_mutation_if_terminal` refuses to
    /// do because it only acts on jobs that reached a terminal failure state
    /// on their own.
    ///
    /// Sequence:
    /// 1. `cancel_jobs_for_document` — transitions every queued/leased
    ///    `ingest_job` for this document to `queue_state='canceled'`. Queued
    ///    rows are atomically terminal. Leased rows become a signal that the
    ///    worker's heartbeat observer picks up within `≤15s`, aborting the
    ///    pipeline cooperatively and finalizing the attempt as canceled.
    /// 2. `reconcile_failed_ingest_mutation` — flips the stuck mutation
    ///    (`accepted`/`running`) to `failed` with
    ///    `failure_code='superseded_by_retry'`, updates mutation items and
    ///    async operation, and re-promotes the document head. From this
    ///    point `ensure_document_accepts_new_mutation` no longer blocks
    ///    a fresh mutation on this document.
    ///
    /// Terminal mutations (`failed`, `canceled`, `applied`) are left alone.
    pub async fn force_reset_inflight_for_retry(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<(), ApiError> {
        if let Err(error) = ingest_repository::cancel_jobs_for_document(
            &state.persistence.postgres,
            document_id,
        )
        .await
        {
            return Err(ApiError::internal_with_log(error, "internal"));
        }

        let Some(head) = self.get_document_head(state, document_id).await? else {
            return Ok(());
        };
        let Some(latest_mutation_id) = head.latest_mutation_id else {
            return Ok(());
        };
        let Some(latest_mutation) =
            content_repository::get_mutation_by_id(&state.persistence.postgres, latest_mutation_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        else {
            return Ok(());
        };
        if !matches!(latest_mutation.mutation_state.as_str(), "accepted" | "running") {
            return Ok(());
        }

        self.reconcile_failed_ingest_mutation(
            state,
            ReconcileFailedIngestMutationCommand {
                mutation_id: latest_mutation_id,
                failure_code: "superseded_by_retry".to_string(),
                failure_message: "document retry requested by user while previous ingest was still inflight".to_string(),
            },
        )
        .await?;
        Ok(())
    }

    pub async fn ensure_document_accepts_new_mutation(
        &self,
        state: &AppState,
        document_id: Uuid,
        operation_kind: &str,
    ) -> Result<(), ApiError> {
        let document =
            content_repository::get_document_by_id(&state.persistence.postgres, document_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;
        if document.document_state == "deleted" || document.deleted_at.is_some() {
            return Err(ApiError::BadRequest(if operation_kind == "delete" {
                "document is already deleted".to_string()
            } else {
                "deleted documents do not accept new mutations".to_string()
            }));
        }
        let Some(head) = self.get_document_head(state, document_id).await? else {
            return Ok(());
        };
        let Some(latest_mutation_id) = head.latest_mutation_id else {
            return Ok(());
        };
        let Some(latest_mutation) =
            content_repository::get_mutation_by_id(&state.persistence.postgres, latest_mutation_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        else {
            return Ok(());
        };
        let latest_mutation_state =
            if matches!(latest_mutation.mutation_state.as_str(), "accepted" | "running") {
                self.reconcile_stale_inflight_mutation_if_terminal(state, &latest_mutation)
                    .await?
                    .unwrap_or(latest_mutation.mutation_state)
            } else {
                latest_mutation.mutation_state
            };
        if operation_kind != "delete"
            && matches!(latest_mutation_state.as_str(), "accepted" | "running")
        {
            return Err(ApiError::ConflictingMutation(
                "document is still processing a previous mutation".to_string(),
            ));
        }
        Ok(())
    }

    async fn reconcile_stale_inflight_mutation_if_terminal(
        &self,
        state: &AppState,
        latest_mutation: &content_repository::ContentMutationRow,
    ) -> Result<Option<String>, ApiError> {
        let admission = self.get_mutation_admission(state, latest_mutation.id).await?;
        let job_handle = state
            .canonical_services
            .ingest
            .get_job_handle_by_mutation_id(state, latest_mutation.id)
            .await?;
        let job_failed =
            job_handle.as_ref().is_some_and(|handle| handle.job.queue_state == "failed");
        let attempt_failed = job_handle
            .as_ref()
            .and_then(|handle| handle.latest_attempt.as_ref())
            .is_some_and(|attempt| {
                matches!(attempt.attempt_state.as_str(), "failed" | "abandoned" | "canceled")
            });
        let async_operation_failed = admission.async_operation_id.and_then(|operation_id| {
            job_handle
                .as_ref()
                .and_then(|handle| handle.async_operation.as_ref())
                .filter(|operation| operation.id == operation_id)
                .map(|operation| operation.status == "failed")
        }) == Some(true);

        if !(job_failed || attempt_failed || async_operation_failed) {
            return Ok(None);
        }

        let failure_code = job_handle
            .as_ref()
            .and_then(|handle| handle.latest_attempt.as_ref())
            .and_then(|attempt| attempt.failure_code.clone())
            .or_else(|| {
                job_handle
                    .as_ref()
                    .and_then(|handle| handle.async_operation.as_ref())
                    .and_then(|operation| operation.failure_code.clone())
            })
            .unwrap_or_else(|| "canonical_pipeline_failed".to_string());
        let failure_message = format!(
            "terminal ingest failure left mutation {} in {}",
            latest_mutation.id, latest_mutation.mutation_state
        );
        let reconciled = self
            .reconcile_failed_ingest_mutation(
                state,
                ReconcileFailedIngestMutationCommand {
                    mutation_id: latest_mutation.id,
                    failure_code,
                    failure_message,
                },
            )
            .await?;
        Ok(Some(reconciled.mutation.mutation_state))
    }

    pub async fn converge_document_technical_facts(
        &self,
        state: &AppState,
        document_id: Uuid,
        retained_revision_id: Option<Uuid>,
    ) -> Result<(), ApiError> {
        let revisions = content_repository::list_revisions_by_document(
            &state.persistence.postgres,
            document_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        for revision in revisions {
            if Some(revision.id) == retained_revision_id {
                continue;
            }
            if let Err(error) =
                state.arango_document_store.delete_technical_facts_by_revision(revision.id).await
            {
                tracing::warn!(
                    %document_id,
                    revision_id = %revision.id,
                    ?error,
                    "failed to delete ArangoDB technical facts for document revision"
                );
            }
        }
        Ok(())
    }

    pub(crate) async fn refresh_deleted_document_graph_state(
        &self,
        state: &AppState,
        library_id: Uuid,
        document_id: Uuid,
    ) -> Result<(), ApiError> {
        repositories::delete_query_execution_references_by_document(
            &state.persistence.postgres,
            library_id,
            document_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        repositories::deactivate_runtime_graph_evidence_by_document(
            &state.persistence.postgres,
            library_id,
            document_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

        // Clean up ArangoDB artifacts for all revisions of the deleted document.
        let revisions = state
            .arango_document_store
            .list_revisions_by_document(document_id)
            .await
            .unwrap_or_default();
        for revision in &revisions {
            if let Err(e) =
                state.arango_document_store.delete_chunks_by_revision(revision.revision_id).await
            {
                tracing::warn!(
                    %document_id,
                    revision_id = %revision.revision_id,
                    ?e,
                    "failed to delete ArangoDB chunks for deleted document"
                );
            }
            if let Err(e) = state
                .arango_document_store
                .delete_structured_blocks_by_revision(revision.revision_id)
                .await
            {
                tracing::warn!(
                    %document_id,
                    revision_id = %revision.revision_id,
                    ?e,
                    "failed to delete ArangoDB blocks for deleted document"
                );
            }
            if let Err(e) = state
                .arango_graph_store
                .delete_entity_candidates_by_revision(revision.revision_id)
                .await
            {
                tracing::warn!(
                    %document_id,
                    revision_id = %revision.revision_id,
                    ?e,
                    "failed to delete ArangoDB entity candidates for deleted document"
                );
            }
            if let Err(e) = state
                .arango_graph_store
                .delete_relation_candidates_by_revision(revision.revision_id)
                .await
            {
                tracing::warn!(
                    %document_id,
                    revision_id = %revision.revision_id,
                    ?e,
                    "failed to delete ArangoDB relation candidates for deleted document"
                );
            }
        }

        let projection_scope = crate::services::graph::projection::resolve_projection_scope(
            state, library_id,
        )
        .await
        .map_err(|error| {
            ApiError::SettlementRefreshFailed(format!(
                "settlement refresh failed: document delete graph projection scope failed: {error}"
            ))
        })?;
        state
            .canonical_services
            .graph
            .project_canonical_graph(state, &projection_scope)
            .await
            .map_err(|error| {
                ApiError::SettlementRefreshFailed(format!(
                    "settlement refresh failed: document delete graph projection refresh failed: {error}"
                ))
            })?;
        Ok(())
    }
}
