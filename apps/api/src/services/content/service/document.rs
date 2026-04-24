use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::content::{
        ContentChunk, ContentDocument, ContentDocumentHead, ContentDocumentPipelineJob,
        ContentDocumentPipelineState, ContentDocumentSummary, ContentMutation, ContentRevision,
        WebPageProvenance,
    },
    domains::knowledge::{PreparedSegmentDetail, PreparedSegmentListItem, TypedTechnicalFact},
    domains::ops::{ASYNC_OP_STATUS_FAILED, MUTATION_KIND_DELETE},
    infra::arangodb::document_store::{KnowledgeDocumentRow, KnowledgeRevisionRow},
    infra::repositories::{
        self, catalog_repository,
        content_repository::{self, ContentDocumentListRow, DocumentListSortColumn},
        ingest_repository,
    },
    interfaces::http::router_support::ApiError,
    services::content::source_access::{derive_content_source_file_name, describe_content_source},
    services::knowledge::graph_stream::invalidate_graph_topology_cache,
    services::knowledge::service::PromoteKnowledgeDocumentCommand,
};

use super::{
    ContentService, PrefetchedDocumentSummaryData, ReconcileFailedIngestMutationCommand,
    map_document_pipeline_job, map_document_row, map_knowledge_chunk_row,
    map_knowledge_document_row, map_knowledge_revision_readiness, map_knowledge_revision_row,
    map_mutation_row, map_revision_row, map_structured_revision_row, map_web_page_provenance_row,
    segment_excerpt,
};

#[derive(Debug, Clone, Default)]
pub(crate) struct DeletedDocumentGraphCleanup {
    pub node_ids: Vec<Uuid>,
    pub edge_ids: Vec<Uuid>,
}

impl DeletedDocumentGraphCleanup {
    #[must_use]
    pub fn from_targets(targets: Vec<repositories::RuntimeGraphEvidenceTargetRow>) -> Self {
        let mut node_ids = HashSet::new();
        let mut edge_ids = HashSet::new();
        for target in targets {
            match target.target_kind.as_str() {
                "node" => {
                    node_ids.insert(target.target_id);
                }
                "edge" => {
                    edge_ids.insert(target.target_id);
                }
                _ => {}
            }
        }
        Self { node_ids: node_ids.into_iter().collect(), edge_ids: edge_ids.into_iter().collect() }
    }

    #[must_use]
    pub fn requires_graph_convergence(&self) -> bool {
        !self.node_ids.is_empty() || !self.edge_ids.is_empty()
    }
}

fn prefers_relative_external_key_display_name(
    external_key: &str,
    revision_kind: Option<&str>,
) -> bool {
    matches!(revision_kind, Some("upload" | "replace" | "edit"))
        && (external_key.contains('/') || external_key.contains('\\'))
}

fn resolve_document_display_name(
    external_key: &str,
    revision_kind: Option<&str>,
    knowledge_file_name: Option<String>,
    source_file_name: Option<String>,
    fallback_file_name: Option<String>,
    revision_title: Option<String>,
) -> String {
    if prefers_relative_external_key_display_name(external_key, revision_kind) {
        return external_key.to_string();
    }

    knowledge_file_name
        .or(source_file_name)
        .or(fallback_file_name)
        .or(revision_title)
        .unwrap_or_else(|| external_key.to_string())
}

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

    /// Canonical slim-listing path for /v1/content/documents. Unlike the
    /// full-summary `list_documents_with_deleted`, this method:
    ///
    ///   1. Applies keyset pagination on `(content_document.created_at, id)`
    ///      via a single Postgres query (`list_document_page_rows`) that
    ///      joins `content_document_head`, `content_revision`,
    ///      `content_mutation`, `ingest_job`, and the latest `ingest_attempt`
    ///      in one round-trip.
    ///   2. Makes a single ArangoDB batch call (`list_documents_by_ids`) to
    ///      fetch the per-document `knowledge_document.file_name` fallback
    ///      and the effective `knowledge_revision` readiness states
    ///      (text_state / graph_state / …) needed to derive the canonical
    ///      readiness bucket.
    ///   3. Derives the canonical `status` and `readiness` strings
    ///      server-side so every client agrees on the same vocabulary.
    ///
    /// Net: two round-trips per page regardless of library size, instead of
    /// the previous 6 batch calls over the *entire* library.
    #[tracing::instrument(
        level = "debug",
        name = "content.list_documents_page",
        skip_all,
        fields(library_id = %command.library_id, limit = command.limit)
    )]
    pub async fn list_documents_page(
        &self,
        state: &AppState,
        command: ListDocumentsPageCommand,
    ) -> Result<ContentDocumentListPageResult, ApiError> {
        let ListDocumentsPageCommand {
            library_id,
            include_deleted,
            cursor,
            limit,
            search,
            sort,
            sort_desc,
            status_filter,
        } = command;

        let library =
            catalog_repository::get_library_by_id(&state.persistence.postgres, library_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("library", library_id))?;

        let page = content_repository::list_document_page_rows(
            &state.persistence.postgres,
            library.id,
            include_deleted,
            cursor,
            limit,
            search.as_deref(),
            sort,
            sort_desc,
            &status_filter,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

        // Fetch per-document knowledge rows (file_name) + effective
        // revisions (readiness) in two Arango round-trips. For small pages
        // (<=200) the payload is trivial.
        let document_ids: Vec<Uuid> = page.rows.iter().map(|row| row.id).collect();
        let knowledge_documents_by_id: HashMap<Uuid, KnowledgeDocumentRow> =
            if document_ids.is_empty() {
                HashMap::new()
            } else {
                state
                    .arango_document_store
                    .list_documents_by_ids(&document_ids)
                    .await
                    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                    .into_iter()
                    .map(|row| (row.document_id, row))
                    .collect()
            };

        let revision_ids: Vec<Uuid> = page
            .rows
            .iter()
            .filter_map(|row| row.readable_revision_id.or(row.active_revision_id))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let revisions_by_id: HashMap<Uuid, KnowledgeRevisionRow> = if revision_ids.is_empty() {
            HashMap::new()
        } else {
            state
                .arango_document_store
                .list_revisions_by_ids(&revision_ids)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .into_iter()
                .map(|row| (row.revision_id, row))
                .collect()
        };

        let items: Vec<ContentDocumentListEntry> = page
            .rows
            .iter()
            .map(|row| {
                build_document_list_entry(
                    row,
                    knowledge_documents_by_id.get(&row.id),
                    row.readable_revision_id
                        .or(row.active_revision_id)
                        .and_then(|id| revisions_by_id.get(&id)),
                )
            })
            .collect();

        let next_cursor = if page.has_more {
            items.last().map(|item| DocumentListCursorValue {
                created_at: item.uploaded_at,
                document_id: item.id,
            })
        } else {
            None
        };

        Ok(ContentDocumentListPageResult { items, next_cursor, has_more: page.has_more })
    }

    pub async fn get_document(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<ContentDocumentSummary, ApiError> {
        // Phase 1: arango document fetch and PG head row fetch are
        // independent — start them in parallel. They each cost
        // 30-100 ms; running serially was ~150 ms of dead wall time on
        // every inspector poll.
        let row_fut = state.arango_document_store.get_document(document_id);
        let head_fut =
            content_repository::get_document_head(&state.persistence.postgres, document_id);
        let (row_res, head_res) = tokio::join!(row_fut, head_fut);
        let row = row_res
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;
        let content_head = head_res.map_err(|e| ApiError::internal_with_log(e, "internal"))?;

        // Phase 2: the mutation row and the job handle both key off the
        // same `latest_mutation_id`. Fetch them concurrently when that
        // id is present; skip the work entirely otherwise.
        let latest_mutation_id = content_head.as_ref().and_then(|head| head.latest_mutation_id);
        let (latest_mutation, latest_job) = if let Some(mutation_id) = latest_mutation_id {
            let mutation_fut =
                content_repository::get_mutation_by_id(&state.persistence.postgres, mutation_id);
            let job_fut =
                state.canonical_services.ingest.get_job_handle_by_mutation_id(state, mutation_id);
            let (mutation_res, job_res) = tokio::join!(mutation_fut, job_fut);
            let latest_mutation = mutation_res
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .map(map_mutation_row);
            let latest_job = job_res?.map(map_document_pipeline_job);
            (latest_mutation, latest_job)
        } else {
            (None, None)
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
        // Canonical source of truth for head pointers is Postgres
        // `content_document_head` — its `active_revision_id` /
        // `readable_revision_id` columns are FKs into `content_revision`
        // and are updated atomically by `promote_document_head` from
        // within the same ingest transaction that creates the revision.
        // The Arango `knowledge_document` projection of the same pointers
        // can drift after a crashed ingest (head was promoted in Arango
        // but the PG revision row was rolled back, or vice versa), and
        // reading pointers from there leaks orphan revision ids into
        // `admit_mutation`, which then writes them into
        // `content_mutation_item.base_revision_id` and trips the FK. All
        // callers downstream of this helper (including retry /
        // resolve_reprocess_revision) stay safe as long as we read PG.
        let row = content_repository::get_document_head(&state.persistence.postgres, document_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(ContentDocumentHead {
            document_id: row.document_id,
            active_revision_id: row.active_revision_id,
            readable_revision_id: row.readable_revision_id,
            latest_mutation_id: row.latest_mutation_id,
            latest_successful_attempt_id: row.latest_successful_attempt_id,
            head_updated_at: row.head_updated_at,
            document_summary: row.document_summary,
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

    /// Canonical paginated read for the inspector's prepared-segments
    /// tab. Returns `(page_items, total_across_all_pages)`. Pagination
    /// is pushed into AQL (`LIMIT offset, limit`) and only the
    /// requested window materializes full block rows — previously the
    /// handler loaded every block into memory, built the full list,
    /// then sliced with `paginate_items`, which cost ~1.2 s of wall
    /// time and a multi-MB internal Arango payload on PDF docs. The
    /// accompanying chunk read is projected to `(chunk_id,
    /// support_block_ids)` only; we never need the chunk text here.
    pub async fn list_prepared_segments_page(
        &self,
        state: &AppState,
        revision_id: Uuid,
        offset: usize,
        limit: usize,
    ) -> Result<(Vec<PreparedSegmentDetail>, usize), ApiError> {
        let page_fut = state.arango_document_store.list_structured_blocks_page_by_revision(
            revision_id,
            offset,
            limit,
        );
        let chunks_fut =
            state.arango_document_store.list_chunk_support_references_by_revision(revision_id);
        let (page_res, chunks_res) = tokio::join!(page_fut, chunks_fut);
        let (block_rows, total) =
            page_res.map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let chunk_refs = chunks_res.map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let mut support_chunk_ids_by_block = std::collections::BTreeMap::<Uuid, Vec<Uuid>>::new();
        for chunk in chunk_refs {
            for block_id in chunk.support_block_ids {
                support_chunk_ids_by_block.entry(block_id).or_default().push(chunk.chunk_id);
            }
        }
        let mut items = Vec::with_capacity(block_rows.len());
        for raw in block_rows {
            let block = crate::services::knowledge::service::map_structured_block_row(raw)?;
            let support_chunk_ids =
                support_chunk_ids_by_block.remove(&block.block_id).unwrap_or_default();
            items.push(PreparedSegmentDetail {
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
                support_chunk_ids,
            });
        }
        Ok((items, total))
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
        // Fan out the four revision-keyed reads concurrently:
        //   - Arango `get_revision(active)`
        //   - Arango `get_revision(readable)` (if distinct)
        //   - Arango `get_structured_revision_counts(effective)`
        //   - PG    `get_web_discovered_page_by_result_revision_id(active)`
        // They're all independent of each other; doing them serially
        // costs ~4 × round-trip latency per inspector poll. The
        // effective readiness revision id is `readable || active`, known
        // from `document_row`, so we can fire the structured-count
        // probe without waiting for the revision fetches first.
        let active_revision_id = document_row.active_revision_id;
        let readable_revision_id = document_row.readable_revision_id;
        let effective_readiness_revision_id = readable_revision_id.or(active_revision_id);
        let active_fut = async {
            match active_revision_id {
                Some(id) => state.arango_document_store.get_revision(id).await,
                None => Ok(None),
            }
        };
        let readable_fut = async {
            match readable_revision_id {
                Some(id) if Some(id) != active_revision_id => {
                    state.arango_document_store.get_revision(id).await
                }
                _ => Ok(None),
            }
        };
        let counts_fut = async {
            match effective_readiness_revision_id {
                Some(id) => state.arango_document_store.get_structured_revision_counts(id).await,
                None => Ok(None),
            }
        };
        let web_page_fut = async {
            match active_revision_id {
                Some(id) => ingest_repository::get_web_discovered_page_by_result_revision_id(
                    &state.persistence.postgres,
                    id,
                )
                .await
                .map(Some),
                None => Ok(None),
            }
        };
        let (active_res, readable_res, counts_res, web_page_res) =
            tokio::join!(active_fut, readable_fut, counts_fut, web_page_fut);
        let active_revision_row =
            active_res.map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        // When `readable == active` the fan-out short-circuited with
        // `Ok(None)`; reuse the active row so we don't lose it.
        let readable_revision_row = match (
            readable_revision_id,
            active_revision_row.as_ref(),
            readable_res.map_err(|e| ApiError::internal_with_log(e, "internal"))?,
        ) {
            (Some(readable_revision_id), Some(active_row), None)
                if readable_revision_id == active_row.revision_id =>
            {
                Some(active_row.clone())
            }
            (_, _, fetched) => fetched,
        };
        let effective_readiness_row =
            readable_revision_row.clone().or_else(|| active_revision_row.clone());
        // Slim count-only projection. The inspector surface only reads
        // `prepared_segment_count` and `technical_fact_count`; the full
        // `outline_json` blob (~4 MB on PDF-ingested docs) used to be
        // pulled here and thrown away.
        let prepared_revision_row = counts_res
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .and_then(|counts| {
                effective_readiness_row.as_ref().map(|readiness| {
                    crate::infra::arangodb::document_store::KnowledgeStructuredRevisionRow {
                        key: readiness.revision_id.to_string(),
                        arango_id: None,
                        arango_rev: None,
                        revision_id: readiness.revision_id,
                        workspace_id: readiness.workspace_id,
                        library_id: readiness.library_id,
                        document_id: readiness.document_id,
                        preparation_state: "ready".to_string(),
                        normalization_profile: String::new(),
                        source_format: String::new(),
                        language_code: None,
                        block_count: counts.block_count,
                        chunk_count: 0,
                        typed_fact_count: counts.typed_fact_count,
                        outline_json: serde_json::Value::Null,
                        prepared_at: chrono::Utc::now(),
                        updated_at: chrono::Utc::now(),
                    }
                })
            });
        // Only keep the web-page row when the active revision actually
        // came from a web capture; otherwise the PG lookup is wasted.
        // We still fired it speculatively to keep the fan-out flat, but
        // drop the result if the revision kind disagrees.
        let web_page_row = web_page_res
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .flatten()
            .filter(|_| {
                active_revision_row
                    .as_ref()
                    .is_some_and(|revision| revision.revision_kind == "web_page")
            });

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

        // Document summary is generated and persisted by the ingest
        // worker at the tail of every successful run
        // (`worker.rs::generate_document_summary_from_blocks` →
        // `update_document_summary`). The read path just surfaces the
        // stored value — no need to re-pull `list_structured_blocks` on
        // every inspector poll. Trimmed/empty strings are normalized to
        // `None` so the UI's empty-state branch renders correctly.
        let document_summary = content_head
            .and_then(|row| row.document_summary.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);

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

    #[tracing::instrument(
        level = "debug",
        name = "content.prefetch_document_summary_data",
        skip_all,
        fields(document_count = documents.len())
    )]
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
            file_name: resolve_document_display_name(
                &document_external_key,
                effective_readiness_row.as_ref().map(|revision| revision.revision_kind.as_str()),
                document_row.file_name.clone(),
                source_descriptor.as_ref().map(|descriptor| descriptor.file_name.clone()),
                fallback_file_name,
                active_revision.as_ref().and_then(|revision| revision.title.clone()),
            ),
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
        self.delete_document_with_context(state, document_id, None, true).await
    }

    pub(crate) async fn delete_document_with_context(
        &self,
        state: &AppState,
        document_id: Uuid,
        latest_mutation_id: Option<Uuid>,
        refresh_graph_projection: bool,
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
        self.promote_knowledge_document(
            state,
            PromoteKnowledgeDocumentCommand {
                document_id,
                document_state: document.document_state.clone(),
                active_revision_id: None,
                readable_revision_id,
                latest_revision_no,
                deleted_at: document.deleted_at,
            },
            "knowledge document sync failed after document delete committed; Postgres delete is committed and the Arango mirror may be stale until retry",
        )
        .await?;
        if let Err(error) = self.converge_document_technical_facts(state, document_id, None).await {
            tracing::warn!(
                %document_id,
                library_id = %document.library_id,
                ?error,
                "post-delete technical fact convergence failed after document delete committed"
            );
        }
        let graph_refresh = if refresh_graph_projection {
            self.refresh_deleted_document_graph_state(state, document.library_id, document_id).await
        } else {
            self.cleanup_deleted_document_local_graph_artifacts(
                state,
                document.library_id,
                document_id,
            )
            .await
        };
        if let Err(error) = graph_refresh {
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

    /// Resolves the revision that retry should re-run against.
    ///
    /// Canonical source of truth is Postgres `content_revision`, NOT the
    /// Arango knowledge projection. The retry ultimately writes a
    /// `content_mutation_item` row whose `base_revision_id` FK points into
    /// `content_revision`; if we pick a revision that only exists in
    /// Arango (projection drift after a crashed ingest) the admit fails
    /// with a raw FK violation and the document stays stuck.
    ///
    /// Selects the latest revision by `(revision_number desc, created_at
    /// desc)`. This covers both the healthy case (revision exists with a
    /// storage_key) and the failed-mid-pipeline case (revision was
    /// created before the crash, `content_document_head.active_revision_id`
    /// was never promoted). When the document has literally zero
    /// revisions in PG (orphan debris from an earlier crash that never
    /// persisted any source bytes), this function force-finalizes the
    /// document — marks any inflight mutation as `failed`, flips
    /// queued/leased ingest jobs to `canceled`, tombstones the document
    /// — and reports `NotFound` so the caller buckets it under
    /// `skipped_count` instead of looping forever.
    pub async fn resolve_reprocess_revision(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<ContentRevision, ApiError> {
        let rows = content_repository::list_revisions_by_document(
            &state.persistence.postgres,
            document_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        if let Some(row) = rows.into_iter().next() {
            return Ok(map_revision_row(row));
        }
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
        if let Err(error) =
            ingest_repository::cancel_jobs_for_document(&state.persistence.postgres, document_id)
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
                        "document has no content_revision rows; nothing to ingest from".to_string(),
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
        if let Err(error) =
            ingest_repository::cancel_jobs_for_document(&state.persistence.postgres, document_id)
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
                failure_message:
                    "document retry requested by user while previous ingest was still inflight"
                        .to_string(),
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
            return Err(ApiError::BadRequest(if operation_kind == MUTATION_KIND_DELETE {
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
        if operation_kind != MUTATION_KIND_DELETE
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
                .map(|operation| operation.status == ASYNC_OP_STATUS_FAILED)
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
        self.cleanup_deleted_document_local_graph_artifacts(state, library_id, document_id).await?;
        let cleanup =
            self.cleanup_deleted_document_graph_evidence(state, library_id, document_id).await?;
        self.refresh_deleted_library_graph_projection_for_cleanup(state, library_id, cleanup).await
    }

    pub(crate) async fn cleanup_deleted_document_local_graph_artifacts(
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

        Ok(())
    }

    pub(crate) async fn cleanup_deleted_document_graph_evidence(
        &self,
        state: &AppState,
        library_id: Uuid,
        document_id: Uuid,
    ) -> Result<DeletedDocumentGraphCleanup, ApiError> {
        let targets = repositories::deactivate_runtime_graph_evidence_by_document(
            &state.persistence.postgres,
            library_id,
            document_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(DeletedDocumentGraphCleanup::from_targets(targets))
    }

    pub(crate) async fn cleanup_deleted_documents_graph_evidence(
        &self,
        state: &AppState,
        library_id: Uuid,
        document_ids: &[Uuid],
    ) -> Result<DeletedDocumentGraphCleanup, ApiError> {
        let targets = repositories::deactivate_runtime_graph_evidence_by_documents(
            &state.persistence.postgres,
            library_id,
            document_ids,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(DeletedDocumentGraphCleanup::from_targets(targets))
    }

    pub(crate) async fn refresh_deleted_library_graph_projection_for_cleanup(
        &self,
        state: &AppState,
        library_id: Uuid,
        cleanup: DeletedDocumentGraphCleanup,
    ) -> Result<(), ApiError> {
        if !cleanup.requires_graph_convergence() {
            return Ok(());
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
        repositories::recalculate_runtime_graph_node_support_counts_by_ids(
            &state.persistence.postgres,
            library_id,
            projection_scope.projection_version,
            &cleanup.node_ids,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        repositories::recalculate_runtime_graph_edge_support_counts_by_ids(
            &state.persistence.postgres,
            library_id,
            projection_scope.projection_version,
            &cleanup.edge_ids,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let deleted_edge_keys = repositories::delete_runtime_graph_edges_without_support_by_ids(
            &state.persistence.postgres,
            library_id,
            projection_scope.projection_version,
            &cleanup.edge_ids,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let deleted_node_keys = repositories::delete_runtime_graph_nodes_without_support_by_ids(
            &state.persistence.postgres,
            library_id,
            projection_scope.projection_version,
            &cleanup.node_ids,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        if !deleted_edge_keys.is_empty() {
            let _ = state
                .arango_graph_store
                .delete_relations_by_canonical_keys(library_id, &deleted_edge_keys)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        }
        if !deleted_node_keys.is_empty() {
            let _ = state
                .arango_graph_store
                .delete_entities_by_canonical_keys(library_id, &deleted_node_keys)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        }
        let projection_scope =
            projection_scope.with_targeted_refresh(cleanup.node_ids, cleanup.edge_ids);
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
        if let Err(error) = invalidate_graph_topology_cache(
            &state.persistence.redis,
            library_id,
            projection_scope.projection_version,
        )
        .await
        {
            tracing::warn!(
                %library_id,
                projection_version = projection_scope.projection_version,
                error = format!("{error:#}"),
                "graph topology cache invalidation failed after deleted-document graph convergence"
            );
        }
        Ok(())
    }
}

// ============================================================================
// Canonical document-list types + derivation helpers.
// ============================================================================

#[derive(Debug, Clone)]
pub struct ListDocumentsPageCommand {
    pub library_id: Uuid,
    pub include_deleted: bool,
    pub cursor: Option<(DateTime<Utc>, Uuid)>,
    pub limit: u32,
    pub search: Option<String>,
    pub sort: DocumentListSortColumn,
    pub sort_desc: bool,
    /// Server-side status filter. Empty = no filter. Values must be one of
    /// `canceled`, `failed`, `processing`, `queued`, `ready` — matching the
    /// canonical `derived_status` column in `list_document_page_rows`.
    pub status_filter: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ContentDocumentListEntry {
    pub id: Uuid,
    pub library_id: Uuid,
    pub workspace_id: Uuid,
    pub file_name: String,
    pub file_type: Option<String>,
    pub file_size: Option<i64>,
    pub uploaded_at: DateTime<Utc>,
    pub document_state: String,
    pub status: String,
    pub readiness: String,
    pub stage: Option<String>,
    pub processing_started_at: Option<DateTime<Utc>>,
    pub processing_finished_at: Option<DateTime<Utc>>,
    pub failure_code: Option<String>,
    pub retryable: bool,
    pub source_kind: Option<String>,
    pub source_uri: Option<String>,
    pub source_access: Option<crate::domains::content::ContentSourceAccess>,
    /// Summed cost across every billable execution attributed to this
    /// document — computed server-side via the per-document LATERAL on
    /// `billing_execution_cost` in the list query, so the frontend never
    /// issues a library-wide `/billing/library-document-costs` fetch.
    pub cost_total: rust_decimal::Decimal,
    pub cost_currency_code: String,
}

#[derive(Debug, Clone, Copy)]
pub struct DocumentListCursorValue {
    pub created_at: DateTime<Utc>,
    pub document_id: Uuid,
}

#[derive(Debug, Clone)]
pub struct ContentDocumentListPageResult {
    pub items: Vec<ContentDocumentListEntry>,
    pub next_cursor: Option<DocumentListCursorValue>,
    pub has_more: bool,
}

/// Derives a `ContentDocumentListEntry` from the joined Postgres row and the
/// (optional) ArangoDB knowledge-document + effective-revision rows for the
/// same document. This is the single canonical place where document list
/// status / readiness strings are computed — both the list handler and the
/// library summary aggregator go through it so there is no drift between
/// surfaces.
fn build_document_list_entry(
    row: &ContentDocumentListRow,
    knowledge_document: Option<&KnowledgeDocumentRow>,
    effective_revision: Option<&KnowledgeRevisionRow>,
) -> ContentDocumentListEntry {
    use crate::services::content::source_access::{
        derive_content_source_file_name, describe_content_source,
    };

    let deleted = row.document_state == "deleted" || row.deleted_at.is_some();

    // readiness derivation — mirrors classify_document_knowledge_state on the
    // Postgres+Arango signals we have on the list path. Graph-sparse is not
    // distinguishable here from `readable` without the prepared revision, so
    // we surface it as `readable` and let the inspector show the split.
    let revision_failed = effective_revision.is_some_and(|revision| {
        matches!(revision.text_state.as_str(), "failed" | "unavailable")
            || revision.vector_state == "failed"
            || revision.graph_state == "failed"
    });
    let revision_text_ready = effective_revision
        .is_some_and(|revision| revision_text_state_is_readable(&revision.text_state));
    let revision_graph_ready = effective_revision
        .is_some_and(|revision| matches!(revision.graph_state.as_str(), "ready" | "graph_ready"));

    let mutation_failed = row
        .mutation_state
        .as_deref()
        .is_some_and(|state| matches!(state, "failed" | "conflicted" | "canceled"));
    let mutation_inflight =
        row.mutation_state.as_deref().is_some_and(|state| matches!(state, "accepted" | "running"));
    let job_failed =
        row.job_queue_state.as_deref().is_some_and(|state| matches!(state, "failed" | "canceled"));
    let job_inflight =
        row.job_queue_state.as_deref().is_some_and(|state| matches!(state, "queued" | "leased"));

    let readiness = if deleted {
        "processing"
    } else if revision_failed || mutation_failed || job_failed {
        "failed"
    } else if revision_graph_ready {
        "graph_ready"
    } else if revision_text_ready {
        "readable"
    } else {
        "processing"
    };

    // Status derivation MUST mirror `DERIVED_STATUS_CASE_SQL` in
    // content_repository.rs — that CASE drives the status filter on the
    // list SQL. If the orderings diverge the server filters on one
    // classification and the row renders with another (observed as
    // "filter=ready shows queued rows" when a re-ingest job queues
    // against a still-readable head). Chain, SQL-aligned:
    //   failed (mutation/revision/job) > leased → processing > readable
    //   revision wins → ready > canceled > queued > inflight → processing
    //   > zombie completed → failed > else queued.
    let status = if readiness == "failed" || row.job_queue_state.as_deref() == Some("failed") {
        "failed"
    } else if row.job_queue_state.as_deref() == Some("leased") {
        // list path does not carry activity_status; surface as `processing`
        // and let the inspector refine the blocked/retrying/stalled split.
        "processing"
    } else if matches!(readiness, "graph_ready" | "readable") {
        "ready"
    } else if row.job_queue_state.as_deref() == Some("canceled") {
        "canceled"
    } else if row.job_queue_state.as_deref() == Some("queued") {
        "queued"
    } else if mutation_inflight || job_inflight {
        "processing"
    } else if row.job_queue_state.as_deref() == Some("completed") {
        // zombie completion — job terminal but readiness never went green
        "failed"
    } else {
        "queued"
    };

    // Visible name: folder-backed uploads with a relative `external_key`
    // intentionally surface that canonical path; legacy uploads and all
    // non-file sources keep the existing filename/title-derived chain.
    let file_name_from_knowledge =
        knowledge_document.and_then(|doc| doc.file_name.clone()).filter(|name| !name.is_empty());

    let source_descriptor = effective_revision.map(|revision| {
        describe_content_source(
            revision.document_id,
            Some(revision.revision_id),
            &revision.revision_kind,
            revision.source_uri.as_deref(),
            revision.storage_ref.as_deref(),
            revision.title.as_deref(),
            &row.external_key,
        )
    });

    let fallback_file_name = effective_revision.map(|revision| {
        derive_content_source_file_name(
            revision.source_uri.as_deref(),
            revision.title.as_deref(),
            &row.external_key,
        )
    });

    let file_name = resolve_document_display_name(
        &row.external_key,
        effective_revision.map(|revision| revision.revision_kind.as_str()),
        file_name_from_knowledge,
        source_descriptor.as_ref().map(|descriptor| descriptor.file_name.clone()),
        fallback_file_name,
        row.revision_title.clone(),
    );

    // file_type: prefer the revision's real mime type.
    let file_type = row
        .revision_mime_type
        .clone()
        .or_else(|| effective_revision.map(|revision| revision.mime_type.clone()));

    let file_size =
        row.revision_byte_size.or_else(|| effective_revision.map(|revision| revision.byte_size));

    let stage = row.attempt_current_stage.clone();

    // processing_started_at: the first claim (attempt started) is the only
    // truthful anchor — mirrors the frontend "claimedAt" logic.
    let processing_started_at = row.attempt_started_at;
    let processing_finished_at = if status == "processing" {
        None
    } else {
        row.job_completed_at.or(row.attempt_finished_at)
    };

    let failure_code =
        row.attempt_failure_code.clone().or_else(|| row.mutation_failure_code.clone());

    ContentDocumentListEntry {
        id: row.id,
        library_id: row.library_id,
        workspace_id: row.workspace_id,
        file_name,
        file_type,
        file_size,
        uploaded_at: row.created_at,
        document_state: row.document_state.clone(),
        status: status.to_string(),
        readiness: readiness.to_string(),
        stage,
        processing_started_at,
        processing_finished_at,
        failure_code,
        retryable: row.attempt_retryable.unwrap_or(false),
        source_kind: row
            .revision_content_source_kind
            .clone()
            .or_else(|| effective_revision.map(|revision| revision.revision_kind.clone())),
        source_uri: row
            .revision_source_uri
            .clone()
            .or_else(|| effective_revision.and_then(|revision| revision.source_uri.clone())),
        source_access: source_descriptor.and_then(|d| d.access),
        cost_total: row.cost_total,
        cost_currency_code: row.cost_currency_code.clone(),
    }
}

fn revision_text_state_is_readable(state: &str) -> bool {
    crate::domains::content::revision_text_state_is_readable(state)
}

#[cfg(test)]
mod tests {
    use super::resolve_document_display_name;

    #[test]
    fn relative_upload_external_key_becomes_visible_document_name() {
        assert_eq!(
            resolve_document_display_name(
                "foo1/path/bar/file.txt",
                Some("upload"),
                Some("file.txt".to_string()),
                Some("file.txt".to_string()),
                Some("file.txt".to_string()),
                Some("file.txt".to_string()),
            ),
            "foo1/path/bar/file.txt"
        );
    }

    #[test]
    fn legacy_upload_without_relative_path_keeps_derived_file_name() {
        assert_eq!(
            resolve_document_display_name(
                "019d96b5-random-key",
                Some("upload"),
                Some("report.pdf".to_string()),
                Some("report.pdf".to_string()),
                Some("report.pdf".to_string()),
                Some("report.pdf".to_string()),
            ),
            "report.pdf"
        );
    }
}
