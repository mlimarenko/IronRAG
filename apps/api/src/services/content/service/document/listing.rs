use super::*;

/// The page either served real rows or short-circuited to an explicit pending
/// empty page because a cost-dependent read met a reconciling rollup.
enum ContentDocumentListPageData {
    Rows(content_document_list_repository::ContentDocumentListPage),
    Pending,
}

impl ContentService {
    #[cfg(feature = "test-support")]
    pub async fn pause_next_document_detail_read(
        &self,
        document_id: Uuid,
    ) -> (tokio::sync::oneshot::Receiver<()>, tokio::sync::oneshot::Sender<()>) {
        let (reached, wait) = tokio::sync::oneshot::channel();
        let (release, resume) = tokio::sync::oneshot::channel();
        *self.document_read_pause.lock().await = Some(super::super::contracts::DocumentReadPause {
            document_id,
            reached,
            release: resume,
        });
        (wait, release)
    }

    #[cfg(feature = "test-support")]
    async fn pause_selected_document_detail(&self, document_id: Uuid) {
        let pause = {
            let mut slot = self.document_read_pause.lock().await;
            if slot.as_ref().is_some_and(|pause| pause.document_id == document_id) {
                slot.take()
            } else {
                None
            }
        };
        if let Some(pause) = pause {
            let _ = pause.reached.send(());
            let _ = tokio::time::timeout(std::time::Duration::from_secs(10), pause.release).await;
        }
    }

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
        let mut documents = state
            .document_store
            .list_documents_by_library(library.workspace_id, library_id, include_deleted)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let snapshots = content_document_head_repository::list_document_read_snapshots(
            &state.persistence.postgres,
            &documents.iter().map(|row| row.document_id).collect::<Vec<_>>(),
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "document visibility lookup failed"))?
        .into_iter()
        .map(|snapshot| (snapshot.head.document_id, snapshot))
        .collect::<HashMap<_, _>>();
        documents.retain(|document| {
            snapshots.get(&document.document_id).is_some_and(|snapshot| {
                snapshot.workspace_id == document.workspace_id
                    && snapshot.library_id == document.library_id
                    && (include_deleted || snapshot.is_active())
            })
        });
        for document in &mut documents {
            let snapshot = snapshots.get(&document.document_id);
            if let Some(snapshot) = snapshot {
                document.document_state.clone_from(&snapshot.document_state);
                document.deleted_at = snapshot.deleted_at;
            }
            document.readable_revision_id =
                snapshot.and_then(|snapshot| snapshot.head.readable_revision_id);
            document.file_name = snapshot.and_then(|snapshot| snapshot.source_file_name.clone());
        }
        let prefetched_summary_data =
            self.prefetch_document_summary_data(state, &documents).await?;
        let latest_mutation_ids = documents
            .iter()
            .filter_map(|row| snapshots.get(&row.document_id)?.head.latest_mutation_id)
            .collect::<Vec<_>>();
        let mutations_by_id = content_mutation_repository::list_mutations_by_ids(
            &state.persistence.postgres,
            &latest_mutation_ids,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .into_iter()
        .map(|row| (row.id, row))
        .collect::<HashMap<_, _>>();
        let modern_mutation_ids = mutations_by_id.keys().copied().collect::<Vec<_>>();
        let job_handles_by_mutation_id = state
            .application_services
            .ingest
            .list_job_handles_by_mutation_ids(
                state,
                library.workspace_id,
                library_id,
                &modern_mutation_ids,
            )
            .await?
            .into_iter()
            .filter_map(|handle| handle.job.mutation_id.map(|mutation_id| (mutation_id, handle)))
            .collect::<HashMap<_, _>>();
        let mut summaries = Vec::with_capacity(documents.len());
        for row in documents {
            let content_head = snapshots.get(&row.document_id).map(|snapshot| &snapshot.head);
            let latest_mutation = content_head
                .and_then(|head| head.latest_mutation_id)
                .and_then(|mutation_id| mutations_by_id.get(&mutation_id).cloned())
                .map(map_mutation_row);
            let latest_job = latest_mutation
                .as_ref()
                .and_then(|mutation| job_handles_by_mutation_id.get(&mutation.id).cloned())
                .map(map_document_pipeline_job);
            summaries.push(self.build_document_summary_from_prefetched(
                state,
                DocumentSummaryInput {
                    document_row: row,
                    content_head,
                    latest_mutation,
                    latest_job,
                    prefetched: &prefetched_summary_data,
                },
            ));
        }
        let current_after = content_document_head_repository::list_document_read_snapshots(
            &state.persistence.postgres,
            &summaries.iter().map(|summary| summary.document.id).collect::<Vec<_>>(),
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "document visibility lookup failed"))?
        .into_iter()
        .map(|snapshot| (snapshot.head.document_id, snapshot))
        .collect::<HashMap<_, _>>();
        summaries.retain(|summary| {
            snapshots.get(&summary.document.id).is_some_and(|selected| {
                current_after
                    .get(&summary.document.id)
                    .is_some_and(|current| selected.has_same_publication(current))
            })
        });
        Ok(summaries)
    }

    /// Primary slim-listing path for /v1/content/documents. Unlike the
    /// full-summary `list_documents_with_deleted`, this method:
    ///
    ///   1. Applies keyset pagination on `(content_document.created_at, id)`
    ///      via a single Postgres query (`list_document_page_rows`) that
    ///      joins `content_document_head`, `content_revision`,
    ///      `content_mutation`, `ingest_job`, and the latest `ingest_attempt`
    ///      in one round-trip.
    ///   2. Loads knowledge revision readiness in a batch, accepting it only for
    ///      the selected canonical publication. Source metadata comes from the
    ///      same page query, never the mutable knowledge-document label.
    ///   3. Derives the current `status` and `readiness` enums
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
            normalized_search,
            search_statuses,
            filters,
            search_ranges,
            snapshot_at,
            sort,
            sort_desc,
            status_filter,
            id_filter,
            include_status_counts,
        } = command;

        let library =
            catalog_repository::get_library_by_id(&state.persistence.postgres, library_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("library", library_id))?;

        let status_filter_values =
            status_filter.iter().map(|status| status.as_str().to_string()).collect::<Vec<_>>();
        // One statement answers the page AND the scope health, so health is
        // observable whether or not the page has rows and never comes from a
        // different database snapshot than the rows it describes.
        let count_query = content_document_list_repository::DocumentListCountQuery {
            library_id: library.id,
            include_deleted,
            search: search.as_deref(),
            normalized_search: normalized_search.as_deref(),
            search_statuses: &search_statuses,
            filters: &filters,
            search_ranges: &search_ranges,
            status_filter: &status_filter_values,
            id_filter: id_filter.as_deref(),
            snapshot_at,
        };
        // The scope aggregate and the page are read in one REPEATABLE READ
        // snapshot: the health marker, the total/status counts and the rows can
        // never describe different rollup generations, and health is observable
        // when the page has zero rows.
        let content_document_list_repository::DocumentListPageRead { page, scope: scope_health } =
            content_document_list_repository::read_document_list_page(
                &state.persistence.postgres,
                content_document_list_repository::DocumentListPageQuery {
                    library_id: library.id,
                    include_deleted,
                    cursor: cursor.as_ref(),
                    limit,
                    search: search.as_deref(),
                    normalized_search: normalized_search.as_deref(),
                    search_statuses: &search_statuses,
                    filters: &filters,
                    search_ranges: &search_ranges,
                    snapshot_at,
                    sort,
                    sort_desc,
                    status_filter: &status_filter_values,
                    id_filter: id_filter.as_deref(),
                },
                count_query,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

        // Terminal mixed-currency still fails closed (409); a merely dirty
        // rollup is reported as `reconciling` and handled below.
        crate::services::ops::billing::ensure_no_terminal_billing_rollup(
            "library",
            library.id,
            scope_health.terminal_error_code.as_deref(),
        )?;
        let billing_rollup_freshness =
            CostRollupFreshness::from_rollup_dirty(scope_health.rollup_dirty);
        // A cost-dependent query cannot serve a result while the rollup
        // reconciles: any order, match or aggregate would be over stale costs.
        // The endpoint answers an explicit pending page — no items, no
        // continuation cursor, no total — so the client drops cost-dependent
        // continuation instead of paging a stale or NULL-ordered result. A
        // metadata-only dirty read stays available and truthfully reconciling.
        let scope_pending = billing_rollup_freshness == CostRollupFreshness::Reconciling
            && content_document_list_repository::document_scope_reads_cost(
                Some(sort),
                search.as_deref(),
                normalized_search.as_deref(),
                &filters,
                None,
            );

        let data = if scope_pending {
            ContentDocumentListPageData::Pending
        } else {
            ContentDocumentListPageData::Rows(page)
        };

        let (knowledge_documents_by_id, revisions_by_id) = match &data {
            ContentDocumentListPageData::Rows(page) => {
                load_document_list_knowledge(state, &page.rows).await?
            }
            ContentDocumentListPageData::Pending => (HashMap::new(), HashMap::new()),
        };

        let (items, next_cursor, has_more) = match data {
            ContentDocumentListPageData::Pending => (Vec::new(), None, false),
            ContentDocumentListPageData::Rows(page) => {
                let current_after =
                    repositories::content_fingerprint_repository::list_current_revision_ids(
                        &state.persistence.postgres,
                        &page.rows.iter().map(|row| row.id).collect::<Vec<_>>(),
                    )
                    .await
                    .map_err(|error| {
                        ApiError::internal_with_log(error, "document visibility lookup failed")
                    })?
                    .into_iter()
                    .collect::<HashMap<_, _>>();
                if page
                    .rows
                    .iter()
                    .any(|row| current_after.get(&row.id).copied() != row.readable_revision_id)
                {
                    return Err(ApiError::knowledge_not_ready(
                        "document sources changed; retry the list",
                    ));
                }
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
                    .collect::<Result<Vec<_>, ApiError>>()?;
                let next_cursor =
                    next_document_list_cursor(&page.rows, &items, sort, page.has_more);
                (items, next_cursor, page.has_more)
            }
        };

        Ok(ContentDocumentListPageResult {
            items,
            next_cursor,
            has_more,
            billing_rollup_freshness,
            scope_pending,
            total_count: if scope_pending { None } else { Some(scope_health.row_count) },
            status_counts: if include_status_counts && !scope_pending {
                Some(scope_health.status_counts())
            } else {
                None
            },
        })
    }

    pub async fn get_document(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<ContentDocumentSummary, ApiError> {
        // Phase 1: knowledge document fetch and PG head row fetch are
        // independent — start them in parallel. They each cost
        // 30-100 ms; running serially was ~150 ms of dead wall time on
        // every inspector poll.
        let row_fut = state.document_store.get_document(document_id);
        let document_ids = [document_id];
        let head_fut = content_document_head_repository::list_document_read_snapshots(
            &state.persistence.postgres,
            &document_ids,
        );
        let (row_res, head_res) = tokio::join!(row_fut, head_fut);
        let mut row = row_res
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;
        let snapshot = head_res
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .pop()
            .filter(|snapshot| {
                snapshot.workspace_id == row.workspace_id && snapshot.library_id == row.library_id
            })
            .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;
        #[cfg(feature = "test-support")]
        self.pause_selected_document_detail(document_id).await;
        let current = match snapshot.head.readable_revision_id {
            Some(revision_id) => content_revision_repository::get_revision_by_id(
                &state.persistence.postgres,
                revision_id,
            )
            .await
            .map_err(|error| {
                ApiError::internal_with_log(error, "published revision lookup failed")
            })?,
            None => None,
        };
        row.document_state.clone_from(&snapshot.document_state);
        row.deleted_at = snapshot.deleted_at;
        row.readable_revision_id = snapshot.head.readable_revision_id;
        row.title = current.as_ref().and_then(|revision| revision.title.clone());
        row.file_name = snapshot.source_file_name.clone();

        // A historical audit receipt can retain this reference without owning
        // a modern mutation or a live progress projection.
        let latest_mutation_id = snapshot.head.latest_mutation_id;
        let (latest_mutation, latest_job) = if let Some(mutation_id) = latest_mutation_id {
            let latest_mutation = content_mutation_repository::get_mutation_by_id(
                &state.persistence.postgres,
                mutation_id,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .map(map_mutation_row);
            let latest_job = if latest_mutation.is_some() {
                state
                    .application_services
                    .ingest
                    .get_job_handle_by_mutation_id(state, mutation_id)
                    .await?
                    .map(map_document_pipeline_job)
            } else {
                None
            };
            (latest_mutation, latest_job)
        } else {
            (None, None)
        };
        let summary = self
            .build_document_summary_from_knowledge(
                state,
                row,
                Some(&snapshot.head),
                latest_mutation,
                latest_job,
            )
            .await?;
        if !content_document_head_repository::document_read_snapshot_is_current(
            &state.persistence.postgres,
            &snapshot,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "document snapshot lookup failed"))?
        {
            return Err(ApiError::resource_not_found("document", document_id));
        }
        Ok(summary)
    }

    pub async fn update_active_revision_document_hint(
        &self,
        state: &AppState,
        document_id: Uuid,
        hint: Option<String>,
    ) -> Result<ContentDocumentSummary, ApiError> {
        let head = self.get_document_head(state, document_id).await?;
        let revision_id =
            head.as_ref().and_then(|row| row.readable_revision_id).ok_or_else(|| {
                ApiError::unreadable_document("document has no readable revision".to_string())
            })?;
        let updated_revision = match content_revision_repository::update_revision_document_hint(
            &state.persistence.postgres,
            revision_id,
            hint.as_deref(),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        {
            content_revision_repository::UpdateRevisionDocumentHintOutcome::Updated(revision) => revision,
            content_revision_repository::UpdateRevisionDocumentHintOutcome::RevisionNotFound => {
                return Err(ApiError::resource_not_found("revision", revision_id));
            }
            content_revision_repository::UpdateRevisionDocumentHintOutcome::KnowledgeProjectionNotFound => {
                return Err(ApiError::service_unavailable(
                    "document projection is converging; retry the update shortly",
                    "document_projection_converging",
                ));
            }
            content_revision_repository::UpdateRevisionDocumentHintOutcome::VisibilityIntentRequired => {
                return Err(ApiError::VisibilityIntentRequired("protected metadata requires an accepted replacement intent".to_owned()));
            }
        };

        let mut summary = self.get_document(state, document_id).await?;
        apply_document_hint_to_active_revision(
            &mut summary,
            updated_revision.id,
            updated_revision.document_hint,
        );
        Ok(summary)
    }

    pub async fn get_document_by_external_key(
        &self,
        state: &AppState,
        library_id: Uuid,
        external_key: &str,
    ) -> Result<Option<ContentDocument>, ApiError> {
        let row = content_document_repository::get_document_by_external_key(
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
        // Authoritative source for head pointers is Postgres
        // `content_document_head` — its `active_revision_id` /
        // `readable_revision_id` columns are FKs into `content_revision`
        // and are updated atomically by `promote_document_head` from
        // within the same ingest transaction that creates the revision.
        // A derived `knowledge_document` projection of the same pointers can
        // drift after a crashed ingest, and reading pointers from there leaks
        // orphan revision ids into `admit_mutation`, which then writes them into
        // `content_mutation_item.base_revision_id` and trips the FK. All
        // callers downstream of this helper (including retry /
        // resolve_reprocess_revision) stay safe as long as we read PG.
        let row = content_document_head_repository::get_document_head(
            &state.persistence.postgres,
            document_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(ContentDocumentHead {
            visibility_intent_id: row.visibility_intent_id,
            document_id: row.document_id,
            active_revision_id: row.active_revision_id,
            readable_revision_id: row.readable_revision_id,
            latest_mutation_id: row.latest_mutation_id,
            latest_successful_attempt_id: row.latest_successful_attempt_id,
            head_updated_at: row.head_updated_at,
            document_summary: row.document_summary,
        }))
    }

    pub async fn get_current_revision(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<Option<ContentRevision>, ApiError> {
        let Some(id) =
            crate::infra::repositories::content_fingerprint_repository::get_current_revision_id(
                &state.persistence.postgres,
                document_id,
            )
            .await
            .map_err(|error| {
                ApiError::internal_with_log(error, "current revision lookup failed")
            })?
        else {
            return Ok(None);
        };
        let row = crate::infra::repositories::content_revision_repository::get_revision_by_id(
            &state.persistence.postgres,
            id,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "current revision lookup failed"))?;
        Ok(row.filter(|row| row.document_id == document_id).map(super::super::map_revision_row))
    }

    pub async fn list_revisions(
        &self,
        state: &AppState,
        document_id: Uuid,
    ) -> Result<Vec<ContentRevision>, ApiError> {
        let rows = state
            .document_store
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
        // Content_chunk rows are the source of truth for the
        // content surface and carry temporal bounds. The exact prepared-state
        // gate is part of this same SQL statement so a concurrent replacement
        // cannot cross the statement boundary and leak unfinished chunks.
        let rows = content_chunk_repository::list_prepared_chunks_by_revision(
            &state.persistence.postgres,
            revision_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(rows
            .into_iter()
            .map(|row| crate::domains::content::ContentChunk {
                id: row.id,
                revision_id: row.revision_id,
                chunk_index: row.chunk_index,
                start_offset: row.start_offset,
                end_offset: row.end_offset,
                token_count: row.token_count,
                normalized_text: row.normalized_text,
                text_checksum: row.text_checksum,
                occurred_at: row.occurred_at,
                occurred_until: row.occurred_until,
            })
            .collect())
    }

    /// Paginated inspector read for the inspector's prepared-segments
    /// tab. Returns `(page_items, total_across_all_pages)`. Pagination
    /// is pushed into storage pagination so only the requested window
    /// materializes full block rows. The
    /// accompanying chunk read is projected to `(chunk_id,
    /// support_block_ids)` only; we never need the chunk text here.
    pub async fn list_prepared_segments_page(
        &self,
        state: &AppState,
        revision_id: Uuid,
        offset: usize,
        limit: usize,
    ) -> Result<(Vec<PreparedSegmentDetail>, usize), ApiError> {
        let page_fut = state.document_store.list_structured_blocks_page_by_revision(
            revision_id,
            offset,
            limit,
        );
        let chunks_fut =
            state.document_store.list_chunk_support_references_by_revision(revision_id);
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
                    block_kind: block.block_kind,
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
        state.application_services.knowledge.list_typed_technical_facts(state, revision_id).await
    }
}
