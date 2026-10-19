use super::*;

/// The document rows a summary is assembled from: the document itself, its
/// content head and latest pipeline rows, and the revision-keyed prefetch.
/// Grouped so the summary builder reads as one named unit.
pub(super) struct DocumentSummaryInput<'a> {
    pub(super) document_row: KnowledgeDocumentRow,
    pub(super) content_head: Option<&'a content_document_head_repository::ContentDocumentHeadRow>,
    pub(super) latest_mutation: Option<ContentMutation>,
    pub(super) latest_job: Option<ContentDocumentPipelineJob>,
    pub(super) prefetched: &'a PrefetchedDocumentSummaryData,
}

impl ContentService {
    // --- Document summary construction ---

    pub(crate) async fn build_document_summary_from_knowledge(
        &self,
        state: &AppState,
        document_row: KnowledgeDocumentRow,
        content_head: Option<&content_document_head_repository::ContentDocumentHeadRow>,
        latest_mutation: Option<ContentMutation>,
        latest_job: Option<ContentDocumentPipelineJob>,
    ) -> Result<ContentDocumentSummary, ApiError> {
        // Fetch private operational failure state and the exact publication in
        // parallel. Structured counts and web provenance belong only to that
        // publication; an active candidate never owns public source metadata.
        let active_revision_id = document_row.active_revision_id;
        let readable_revision_id = document_row.readable_revision_id;
        let effective_readiness_revision_id = readable_revision_id;
        let active_fut = async {
            match active_revision_id {
                Some(id) => state.document_store.get_revision(id).await,
                None => Ok(None),
            }
        };
        let readable_fut = async {
            match readable_revision_id {
                Some(id) if Some(id) != active_revision_id => {
                    state.document_store.get_revision(id).await
                }
                _ => Ok(None),
            }
        };
        let counts_fut = async {
            match effective_readiness_revision_id {
                Some(id) => state.document_store.get_structured_revision_counts(id).await,
                None => Ok(None),
            }
        };
        let web_page_fut = async {
            match readable_revision_id {
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
        let effective_readiness_row = readable_revision_row.clone();
        // Slim count-only projection: inspector only needs
        // `prepared_segment_count` and `technical_fact_count`, not the
        // full `outline_json` blob (~4 MB on PDF-ingested docs).
        let prepared_revision_row = counts_res
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .and_then(|counts| {
                effective_readiness_row.as_ref().map(|readiness| {
                    crate::infra::knowledge_rows::KnowledgeStructuredRevisionRow {
                        revision_id: readiness.revision_id,
                        workspace_id: readiness.workspace_id,
                        library_id: readiness.library_id,
                        document_id: readiness.document_id,
                        preparation_state: "prepared".to_string(),
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
        // Only keep the web-page row when the published revision actually
        // came from a web capture; otherwise the PG lookup is wasted.
        // We still fired it speculatively to keep the fan-out flat, but
        // drop the result if the revision kind disagrees.
        let web_page_row = web_page_res
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .flatten()
            .filter(|_| {
                readable_revision_row
                    .as_ref()
                    .is_some_and(|revision| revision.revision_kind == "web_page")
            });

        let mut revisions_by_id = HashMap::new();
        if let Some(revision) = active_revision_row {
            revisions_by_id.insert(revision.revision_id, revision);
        }
        if let Some(revision) = readable_revision_row {
            revisions_by_id.entry(revision.revision_id).or_insert(revision);
        }
        if let Some(revision) = effective_readiness_row {
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

        // Publication stores the summary with its exact locked revision/item;
        // changing that owner clears it. The selected read snapshot retains
        // those same owner IDs through its final publication check.
        let document_summary = content_head
            .and_then(|row| row.document_summary.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);

        let mut summary = self.build_document_summary_from_prefetched(
            state,
            DocumentSummaryInput {
                document_row,
                content_head,
                latest_mutation,
                latest_job,
                prefetched: &PrefetchedDocumentSummaryData {
                    revisions_by_id,
                    structured_revisions_by_revision_id,
                    web_pages_by_result_revision_id,
                },
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
            .document_store
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
            .document_store
            .list_structured_revisions_by_revision_ids(&effective_revision_ids)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .into_iter()
            .map(|row| (row.revision_id, row))
            .collect::<HashMap<_, _>>();

        let web_page_revision_ids = documents
            .iter()
            .filter_map(|document| document.readable_revision_id)
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
        input: DocumentSummaryInput<'_>,
    ) -> ContentDocumentSummary {
        let DocumentSummaryInput {
            mut document_row,
            content_head,
            latest_mutation,
            latest_job,
            prefetched,
        } = input;
        let document_external_key = document_row.external_key.clone();
        let deleted_document =
            document_row.document_state == "deleted" || document_row.deleted_at.is_some();
        let active_revision_row = document_row
            .active_revision_id
            .and_then(|revision_id| prefetched.revisions_by_id.get(&revision_id).cloned());
        let display_revision_row = self.resolve_effective_readiness_row(
            &document_row,
            active_revision_row.as_ref(),
            &prefetched.revisions_by_id,
        );
        let effective_readiness_row =
            (!deleted_document).then_some(display_revision_row.clone()).flatten();
        let operational_revision_failed = document_row.readable_revision_id.is_none()
            && active_revision_row.as_ref().is_some_and(|revision| {
                revision.document_id == document_row.document_id
                    && revision.workspace_id == document_row.workspace_id
                    && revision.library_id == document_row.library_id
                    && (revision.text_state == "failed" || revision.vector_state == "failed")
            });
        // Knowledge active state is private progress evidence, never a source
        // publication. Only the selected published row enters public metadata.
        document_row.active_revision_id = document_row.readable_revision_id;
        document_row.title = display_revision_row.as_ref().and_then(|row| row.title.clone());
        let active_revision = display_revision_row.clone().map(map_knowledge_revision_row);
        let prepared_revision = effective_readiness_row
            .as_ref()
            .and_then(|revision| {
                prefetched.structured_revisions_by_revision_id.get(&revision.revision_id)
            })
            .cloned()
            .map(map_structured_revision_row);
        let head = Some(ContentDocumentHead {
            visibility_intent_id: content_head.and_then(|row| row.visibility_intent_id),
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
            state.application_services.ops.derive_document_readiness_summary(
                state,
                crate::services::ops::service::DocumentReadinessInputs {
                    document_id: document_row.document_id,
                    active_revision_id: document_row.active_revision_id,
                    effective_readiness_row: effective_readiness_row.as_ref(),
                    operational_revision_failed,
                    prepared_revision: prepared_revision.as_ref(),
                    latest_mutation: latest_mutation.as_ref(),
                    latest_job: latest_job.as_ref(),
                    created_at: document_row.created_at,
                },
            )
        });
        let source_descriptor = effective_readiness_row.as_ref().map(|revision| {
            describe_content_source(
                revision.document_id,
                Some(revision.revision_id),
                crate::services::content::source_access::ContentSourceInputs {
                    content_source_kind: &revision.revision_kind,
                    source_uri: revision.source_uri.as_deref(),
                    storage_key: revision.storage_ref.as_deref(),
                    title: revision.title.as_deref(),
                    external_key: &document_external_key,
                },
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
                            preferred_url: revision.source_uri.clone(),
                        })
                    })
            });

        ContentDocumentSummary {
            document: map_knowledge_document_row(&document_row),
            file_name: document_row.file_name.clone(),
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
        let revision_id = document_row.readable_revision_id?;
        active_revision_row
            .filter(|row| row.revision_id == revision_id)
            .or_else(|| revisions_by_id.get(&revision_id))
            .filter(|row| {
                row.document_id == document_row.document_id
                    && row.workspace_id == document_row.workspace_id
                    && row.library_id == document_row.library_id
            })
            .cloned()
    }
}
