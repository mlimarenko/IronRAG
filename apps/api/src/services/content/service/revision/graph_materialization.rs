use super::*;

use crate::shared::cancellation::AbortOnDrop;

fn should_sync_stage_unit_progress(completed_units: usize, total_units: usize) -> bool {
    if total_units <= 20 || completed_units >= total_units {
        return true;
    }
    let stride = (total_units / 20).max(1);
    completed_units.is_multiple_of(stride)
}

async fn sync_extract_graph_stage_progress(
    state: &AppState,
    attempt_id: Uuid,
    completed_units: usize,
    total_units: usize,
) {
    if total_units == 0 {
        return;
    }
    let command = RecordStageUnitProgressCommand {
        attempt_id,
        stage_name: INGEST_STAGE_EXTRACT_GRAPH.to_string(),
        completed_units: u32::try_from(completed_units).unwrap_or(u32::MAX),
        total_units: u32::try_from(total_units).unwrap_or(u32::MAX),
        details_json: serde_json::json!({}),
    };
    if let Err(error) =
        state.application_services.ingest.record_stage_unit_progress(state, command).await
    {
        warn!(
            attempt_id = %attempt_id,
            ?error,
            "failed to sync extract_graph stage progress"
        );
    }
}

impl ContentService {
    async fn load_graph_materialization_sources(
        &self,
        state: &AppState,
        command: &MaterializeRevisionGraphCandidatesCommand,
        cancellation_token: &CancellationToken,
    ) -> Result<GraphMaterializationSources, ContentServiceError> {
        ensure_not_cancelled(cancellation_token)?;
        let graph_runtime_context = resolve_runtime_task_execution_context(
            state,
            command.authority.library_id,
            &GraphExtractTask::spec(),
        )
        .await
        .map_err(|error| {
            ApiError::BadRequest(format!(
                "active extract_graph binding is required for graph extraction: {error:#}"
            ))
        })?;
        let revision = state
            .document_store
            .get_revision(command.authority.revision_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| {
                ApiError::resource_not_found("knowledge_revision", command.authority.revision_id)
            })?;
        ensure_not_cancelled(cancellation_token)?;
        let document = state
            .document_store
            .get_document(revision.document_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| {
                ApiError::resource_not_found("knowledge_document", revision.document_id)
            })?;
        if revision.workspace_id != command.authority.workspace_id
            || revision.library_id != command.authority.library_id
            || document.workspace_id != command.authority.workspace_id
            || document.library_id != command.authority.library_id
            || document.document_id != revision.document_id
        {
            return Err(ApiError::Conflict(
                "graph materialization authority does not match the revision scope".to_string(),
            )
            .into());
        }
        ensure_not_cancelled(cancellation_token)?;
        let all_chunks = state
            .document_store
            .list_chunks_by_revision(command.authority.revision_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        ensure_not_cancelled(cancellation_token)?;
        let structured_revision = state
            .document_store
            .get_structured_revision(command.authority.revision_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        ensure_not_cancelled(cancellation_token)?;
        let revision_facts = state
            .application_services
            .knowledge
            .list_typed_technical_facts(state, command.authority.revision_id)
            .await?;
        ensure_not_cancelled(cancellation_token)?;
        let structured_blocks = state
            .document_store
            .list_structured_blocks_by_revision(command.authority.revision_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        ensure_not_cancelled(cancellation_token)?;
        let table_graph_context = build_revision_table_graph_context(&structured_blocks);
        let library_extraction_prompt = catalog_repository::get_library_by_id(
            &state.persistence.postgres,
            command.authority.library_id,
        )
        .await
        .ok()
        .flatten()
        .and_then(|row| row.extraction_prompt);
        let sub_type_hints =
            load_sub_type_hints_for_extraction(state, command.authority.library_id).await;
        Ok(GraphMaterializationSources {
            graph_runtime_context,
            revision,
            document,
            all_chunks,
            structured_revision,
            revision_facts,
            table_graph_context,
            library_extraction_prompt,
            sub_type_hints,
        })
    }

    pub async fn materialize_revision_graph_candidates(
        &self,
        state: &AppState,
        command: MaterializeRevisionGraphCandidatesCommand,
        cancellation_token: &CancellationToken,
    ) -> Result<
        IngestAttemptMutationOutcome<RevisionGraphCandidateMaterialization>,
        ContentServiceError,
    > {
        let sources =
            self.load_graph_materialization_sources(state, &command, cancellation_token).await?;
        let structured_revision = sources.structured_revision.as_ref().ok_or_else(|| {
            ApiError::Conflict(
                "graph materialization requires a prepared structured revision".to_string(),
            )
        })?;
        if structured_revision.preparation_state != "prepared"
            || structured_revision.workspace_id != command.authority.workspace_id
            || structured_revision.library_id != command.authority.library_id
            || structured_revision.revision_id != command.authority.revision_id
            || structured_revision.document_id != sources.document.document_id
            || usize::try_from(structured_revision.chunk_count).ok()
                != Some(sources.all_chunks.len())
        {
            return Err(ApiError::Conflict(
                "graph materialization structured artifact is incomplete or outside authority scope"
                    .to_string(),
            )
            .into());
        }
        let preparation =
            repositories::runtime_graph_repository::prepare_revision_graph_materialization_fenced(
                &state.persistence.postgres,
                command.authority,
                sources.document.document_id,
                "graph extraction restarted before this chunk completed",
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let IngestAttemptMutationOutcome::Applied(canceled_orphans) = preparation else {
            return Ok(IngestAttemptMutationOutcome::AuthorityLost);
        };
        log_canceled_graph_orphans(&command, &sources.document, canceled_orphans);
        let selection = graph_materialization_selection(&sources);
        log_graph_materialization_selection(&command, &selection);
        let binding =
            resolve_graph_materialization_binding(state, command.authority.library_id).await?;
        let cache = match self
            .resolve_graph_materialization_cache(GraphMaterializationCacheInput {
                state,
                command: &command,
                sources: &sources,
                selection: &selection,
                binding: &binding,
                cancellation_token,
            })
            .await?
        {
            IngestAttemptMutationOutcome::Applied(cache) => cache,
            IngestAttemptMutationOutcome::AuthorityLost => {
                return Ok(IngestAttemptMutationOutcome::AuthorityLost);
            }
        };
        log_graph_materialization_cache(&command, &selection, &cache);
        let extraction = prepare_graph_chunk_extraction(selection, sources.all_chunks, cache);
        sync_extract_graph_stage_progress(
            state,
            command.authority.attempt_id,
            extraction.result_seed.reused_chunk_count,
            extraction.progress_total,
        )
        .await;
        let PreparedGraphChunkExtraction {
            result_seed,
            chunks,
            graph_chunk_policy,
            progress_total,
        } = extraction;
        let document_id = sources.document.document_id;
        let aggregate = match extract_remaining_graph_chunks(GraphChunkExtractionBatch {
            state,
            command: &command,
            graph_runtime_context: sources.graph_runtime_context,
            document: sources.document,
            revision: sources.revision,
            revision_facts: sources.revision_facts,
            library_extraction_prompt: sources.library_extraction_prompt,
            sub_type_hints: sources.sub_type_hints,
            table_graph_context: sources.table_graph_context,
            graph_chunk_policy,
            chunks,
            progress_total,
            progress_completed: result_seed.reused_chunk_count,
            cancellation_token,
        })
        .await?
        {
            IngestAttemptMutationOutcome::Applied(aggregate) => aggregate,
            IngestAttemptMutationOutcome::AuthorityLost => {
                return Ok(IngestAttemptMutationOutcome::AuthorityLost);
            }
        };
        let retirement = repositories::runtime_graph_repository::retire_superseded_revision_graph_extractions_fenced(
            &state.persistence.postgres,
            command.authority,
            document_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        if matches!(retirement, IngestAttemptMutationOutcome::AuthorityLost) {
            return Ok(IngestAttemptMutationOutcome::AuthorityLost);
        }
        Ok(IngestAttemptMutationOutcome::Applied(graph_materialization_result(
            result_seed,
            aggregate,
            binding,
        )))
    }
}

pub(super) struct GraphMaterializationBinding {
    pub(super) runtime_binding: ResolvedRuntimeBinding,
    pub(super) provider_kind: String,
    pub(super) model_name: String,
    pub(super) request_size_soft_limit_bytes: usize,
}

struct GraphMaterializationResultSeed {
    chunk_count: usize,
    selected_graph_chunk_count: usize,
    record_stream_source_units_skipped: usize,
    reused_chunk_count: usize,
    reused_entity_count: usize,
    reused_relation_count: usize,
}

struct PreparedGraphChunkExtraction {
    result_seed: GraphMaterializationResultSeed,
    chunks: Vec<KnowledgeChunkRow>,
    graph_chunk_policy: GraphExtractionChunkPolicy,
    progress_total: usize,
}

struct GraphChunkExtractionBatch<'a> {
    state: &'a AppState,
    command: &'a MaterializeRevisionGraphCandidatesCommand,
    graph_runtime_context: crate::services::ingest::runtime::RuntimeTaskExecutionContext,
    document: KnowledgeDocumentRow,
    revision: KnowledgeRevisionRow,
    revision_facts: Vec<TypedTechnicalFact>,
    library_extraction_prompt: Option<String>,
    sub_type_hints: GraphExtractionSubTypeHints,
    table_graph_context: RevisionTableGraphContext,
    graph_chunk_policy: GraphExtractionChunkPolicy,
    chunks: Vec<KnowledgeChunkRow>,
    progress_total: usize,
    progress_completed: usize,
    cancellation_token: &'a CancellationToken,
}

async fn resolve_graph_materialization_binding(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<GraphMaterializationBinding> {
    let runtime_binding = state
        .application_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::ExtractGraph)
        .await?
        .ok_or_else(|| {
            ApiError::BadRequest(
                "active extract_graph binding is required for graph extraction".to_string(),
            )
        })?;
    Ok(GraphMaterializationBinding {
        provider_kind: runtime_binding.provider_kind.clone(),
        model_name: runtime_binding.model_name.clone(),
        request_size_soft_limit_bytes: state
            .resolve_settle_blockers_services
            .provider_failure_classification
            .request_size_soft_limit_bytes(),
        runtime_binding,
    })
}

fn prepare_graph_chunk_extraction(
    selection: GraphMaterializationSelection,
    chunks: Vec<KnowledgeChunkRow>,
    cache: GraphExtractionCachePlan,
) -> PreparedGraphChunkExtraction {
    let remaining_chunks = chunks
        .into_iter()
        .filter(|chunk| !cache.reused_chunk_ids.contains(&chunk.chunk_id))
        .collect::<Vec<_>>();
    let reused_chunk_count = cache.reused_chunk_ids.len();
    let remaining_graph_chunk_count =
        selection.selected_graph_chunk_count.saturating_sub(reused_chunk_count);
    PreparedGraphChunkExtraction {
        result_seed: GraphMaterializationResultSeed {
            chunk_count: selection.chunk_count,
            selected_graph_chunk_count: selection.selected_graph_chunk_count,
            record_stream_source_units_skipped: selection.record_stream_source_units_skipped,
            reused_chunk_count,
            reused_entity_count: cache.reused_entity_count,
            reused_relation_count: cache.reused_relation_count,
        },
        chunks: remaining_chunks,
        graph_chunk_policy: selection.graph_chunk_policy,
        progress_total: reused_chunk_count.saturating_add(remaining_graph_chunk_count),
    }
}

async fn extract_remaining_graph_chunks(
    batch: GraphChunkExtractionBatch<'_>,
) -> anyhow::Result<IngestAttemptMutationOutcome<ChunkExtractAggregate>> {
    let GraphChunkExtractionBatch {
        state,
        command,
        graph_runtime_context,
        document,
        revision,
        revision_facts,
        library_extraction_prompt,
        sub_type_hints,
        table_graph_context,
        graph_chunk_policy,
        chunks,
        progress_total,
        progress_completed,
        cancellation_token,
    } = batch;
    let total_to_extract = chunks.len();
    let graph_extract_parallelism =
        state.settings.ingestion_graph_extract_parallelism_per_doc.max(1);
    let document = std::sync::Arc::new(document);
    let revision = std::sync::Arc::new(revision);
    let revision_facts = std::sync::Arc::new(revision_facts);
    let library_extraction_prompt = std::sync::Arc::new(library_extraction_prompt);
    let sub_type_hints = std::sync::Arc::new(sub_type_hints);
    let table_graph_context = std::sync::Arc::new(table_graph_context);
    let graph_chunk_policy = std::sync::Arc::new(graph_chunk_policy);
    let per_chunk_stream = stream::iter(chunks.into_iter().map(|chunk| {
        let task = GraphChunkExtractionTask {
            state: state.clone(),
            command: command.clone(),
            graph_runtime_context: graph_runtime_context.clone(),
            document: std::sync::Arc::clone(&document),
            revision: std::sync::Arc::clone(&revision),
            revision_facts: std::sync::Arc::clone(&revision_facts),
            library_extraction_prompt: std::sync::Arc::clone(&library_extraction_prompt),
            sub_type_hints: std::sync::Arc::clone(&sub_type_hints),
            table_graph_context: std::sync::Arc::clone(&table_graph_context),
            graph_chunk_policy: std::sync::Arc::clone(&graph_chunk_policy),
            cancellation_token: cancellation_token.clone(),
        };
        async move { Box::pin(extract_graph_chunk(task, chunk)).await }
    }));
    let progress_counter = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    // The guard aborts the reporter on drop, so every exit path — success,
    // error, cancellation or panic — stops it. It used to be aborted only on
    // the success path, which leaked the task (and its 30s progress logs) for
    // the lifetime of the process when the calling future was dropped.
    let _progress_reporter = spawn_graph_progress_reporter(
        std::sync::Arc::clone(&progress_counter),
        command.authority.library_id,
        command.authority.revision_id,
        total_to_extract,
    );
    let idle_timeout = std::time::Duration::from_secs(
        state.settings.runtime_graph_extract_idle_timeout_seconds.max(1),
    );
    let (activity, idle_watchdog) =
        crate::services::graph::extract::GraphExtractionIdleWatchdog::new(idle_timeout);
    activity
        .scope(fold_graph_chunk_extractions(GraphChunkFold {
            state,
            command,
            per_chunk_stream,
            graph_extract_parallelism,
            idle_timeout,
            idle_watchdog,
            progress_counter,
            progress_total,
            progress_completed,
            cancellation_token,
        }))
        .await
}

// The `extract_graph` chunk-progress reporter is owned by a shared
// [`AbortOnDrop`] guard, not a bespoke type: it must never outlive the
// extraction it reports on, and a `JoinHandle` dropped on its own only detaches
// the task. See `crates/shared/cancellation.rs` for the class.

struct GraphChunkExtractionTask {
    state: AppState,
    command: MaterializeRevisionGraphCandidatesCommand,
    graph_runtime_context: crate::services::ingest::runtime::RuntimeTaskExecutionContext,
    document: std::sync::Arc<KnowledgeDocumentRow>,
    revision: std::sync::Arc<KnowledgeRevisionRow>,
    revision_facts: std::sync::Arc<Vec<TypedTechnicalFact>>,
    library_extraction_prompt: std::sync::Arc<Option<String>>,
    sub_type_hints: std::sync::Arc<GraphExtractionSubTypeHints>,
    table_graph_context: std::sync::Arc<RevisionTableGraphContext>,
    graph_chunk_policy: std::sync::Arc<GraphExtractionChunkPolicy>,
    cancellation_token: CancellationToken,
}

async fn extract_graph_chunk(
    task: GraphChunkExtractionTask,
    chunk: KnowledgeChunkRow,
) -> anyhow::Result<IngestAttemptMutationOutcome<ChunkExtractAggregate>> {
    ensure_not_cancelled(&task.cancellation_token)?;
    let Some(chunk_content) = build_graph_chunk_content(
        &chunk,
        task.table_graph_context.profile_for_chunk(&chunk),
        task.table_graph_context.requires_row_only_graph(),
        &task.graph_chunk_policy,
    ) else {
        return Ok(IngestAttemptMutationOutcome::Applied(ChunkExtractAggregate::default()));
    };
    let chunk_facts = task
        .revision_facts
        .iter()
        .filter(|fact| typed_fact_supports_chunk(fact, &chunk))
        .cloned()
        .collect::<Vec<_>>();
    ensure_not_cancelled(&task.cancellation_token)?;
    let response = extract_chunk_graph_candidates(
        &task.state,
        &task.graph_runtime_context,
        task.command.authority,
        &build_graph_extraction_request(GraphExtractionRequestInput {
            document: &task.document,
            revision: &task.revision,
            chunk: &chunk,
            chunk_content,
            technical_facts: &chunk_facts,
            attempt_id: Some(task.command.authority.attempt_id),
            library_extraction_prompt: (*task.library_extraction_prompt).clone(),
            sub_type_hints: (*task.sub_type_hints).clone(),
        }),
        &task.cancellation_token,
    )
    .await
    .map_err(|error| graph_chunk_extraction_error(chunk.chunk_id, error))?;
    let response = match response {
        IngestAttemptMutationOutcome::Applied(response) => response,
        IngestAttemptMutationOutcome::AuthorityLost => {
            return Ok(IngestAttemptMutationOutcome::AuthorityLost);
        }
    };
    ensure_not_cancelled(&task.cancellation_token)?;
    capture_graph_chunk_billing(&task, &chunk, &response).await?;
    Ok(IngestAttemptMutationOutcome::Applied(graph_chunk_aggregate(&response)))
}

fn graph_chunk_extraction_error(
    chunk_id: Uuid,
    error: GraphExtractionExecutionError,
) -> anyhow::Error {
    if error.cancelled {
        return anyhow::Error::new(crate::services::ingest::cancellation::StageError::Cancelled);
    }
    if let Some(admission_error) = error.provider_admission_error {
        // `provider_admission_error` is boxed on the error struct; unwrap the
        // box so the anyhow source is `ProviderBudgetError` itself and stays
        // downcastable by the ingest worker's admission classifier.
        return anyhow::Error::new(*admission_error)
            .context(format!("graph extraction failed for chunk {chunk_id}"));
    }
    if let Some(rate_limit) = graph_provider_rate_limit(error.provider_failure.as_deref()) {
        // Preserve the operational signal across the service boundary: a bare
        // anyhow message would let the worker's failure classifier read a
        // transient 429 as an unclassified content fault and burn the retry
        // budget until the job is permanently failed.
        return anyhow::Error::new(rate_limit)
            .context(format!("graph extraction failed for chunk {chunk_id}"));
    }
    anyhow::anyhow!("graph extraction failed for chunk {chunk_id}: {}", error.message)
}

/// Reconstructs the typed operational rate-limit signal from the sanitized
/// provider-failure snapshot. Only an HTTP 429 (rate limit or quota) qualifies:
/// the same request succeeds once the provider window resets or quota is topped
/// up, so the owning job must be deferred rather than terminally failed.
///
/// The decision reads `upstream_status`, never the derived `failure_class`: the
/// classifier forces that class to `InternalRequestInvalid` when the request
/// exceeds the size soft limit *before* it consults the status, so keying on the
/// class drops the 429 on exactly the oversized-chunk path that produced the
/// incident. The raw status is the operational fact and survives that rewrite.
fn graph_provider_rate_limit(
    failure: Option<&crate::domains::runtime_ingestion::RuntimeProviderFailureDetail>,
) -> Option<crate::integrations::retry::ProviderRateLimit> {
    let failure = failure?;
    if failure.upstream_status.as_deref() != Some("429") {
        return None;
    }
    Some(crate::integrations::retry::ProviderRateLimit::new(
        failure.provider_kind.clone().unwrap_or_else(|| "unknown".to_string()),
        failure.retry_after_seconds.map(std::time::Duration::from_secs),
        "provider rate limited; response details were redacted".to_string(),
    ))
}

async fn capture_graph_chunk_billing(
    task: &GraphChunkExtractionTask,
    chunk: &KnowledgeChunkRow,
    response: &GraphExtractionOutcome,
) -> anyhow::Result<()> {
    let graph_extraction_id = response.graph_extraction_id.ok_or_else(|| {
        anyhow::anyhow!("graph extraction response is missing graph_extraction_id")
    })?;
    let runtime_execution_id = response.runtime_execution_id.ok_or_else(|| {
        anyhow::anyhow!("graph extraction response is missing active runtime_execution_id")
    })?;
    if let Err(error) = task
        .state
        .application_services
        .billing
        .capture_graph_extraction(
            &task.state,
            CaptureGraphExtractionBillingCommand {
                workspace_id: task.command.authority.workspace_id,
                library_id: task.command.authority.library_id,
                graph_extraction_id,
                runtime_execution_id,
                binding_id: None,
                provider_kind: response.provider_kind.clone(),
                model_name: response.model_name.clone(),
                usage_json: response.usage_json.clone(),
            },
        )
        .await
    {
        warn!(
            revision_id = %task.command.authority.revision_id,
            chunk_id = %chunk.chunk_id,
            graph_extraction_id = %graph_extraction_id,
            runtime_execution_id = %runtime_execution_id,
            ?error,
            "graph extraction billing capture failed; continuing graph admission",
        );
    }
    Ok(())
}

fn graph_chunk_aggregate(response: &GraphExtractionOutcome) -> ChunkExtractAggregate {
    ChunkExtractAggregate {
        processed_graph_chunks: 1,
        extracted_entities: response.normalized.entities.len(),
        extracted_relations: response.normalized.relations.len(),
        prompt_tokens: usage_token_count(&response.usage_json, "prompt_tokens"),
        completion_tokens: usage_token_count(&response.usage_json, "completion_tokens"),
        total_tokens: usage_token_count(&response.usage_json, "total_tokens"),
    }
}

fn usage_token_count(usage_json: &serde_json::Value, field: &str) -> i64 {
    usage_json.get(field).and_then(serde_json::Value::as_i64).unwrap_or(0)
}

fn spawn_graph_progress_reporter(
    counter: std::sync::Arc<std::sync::atomic::AtomicU64>,
    library_id: Uuid,
    revision_id: Uuid,
    total: usize,
) -> AbortOnDrop<()> {
    AbortOnDrop::new(tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            let done = counter.load(std::sync::atomic::Ordering::Relaxed) as usize;
            if done >= total {
                break;
            }
            tracing::info!(
                library_id = %library_id,
                revision_id = %revision_id,
                progress_pct = done * 100 / total.max(1),
                done,
                total,
                "extract_graph: chunk progress"
            );
        }
    }))
}

struct GraphChunkFold<'a, S> {
    state: &'a AppState,
    command: &'a MaterializeRevisionGraphCandidatesCommand,
    per_chunk_stream: S,
    graph_extract_parallelism: usize,
    idle_timeout: std::time::Duration,
    idle_watchdog: crate::services::graph::extract::GraphExtractionIdleWatchdog,
    progress_counter: std::sync::Arc<std::sync::atomic::AtomicU64>,
    progress_total: usize,
    progress_completed: usize,
    cancellation_token: &'a CancellationToken,
}

async fn fold_graph_chunk_extractions<S, F>(
    fold: GraphChunkFold<'_, S>,
) -> anyhow::Result<IngestAttemptMutationOutcome<ChunkExtractAggregate>>
where
    S: futures::Stream<Item = F>,
    F: std::future::Future<
            Output = anyhow::Result<IngestAttemptMutationOutcome<ChunkExtractAggregate>>,
        >,
{
    let buffered = fold.per_chunk_stream.buffer_unordered(fold.graph_extract_parallelism);
    futures::pin_mut!(buffered);
    let stalled = fold.idle_watchdog.wait_for_stall();
    tokio::pin!(stalled);
    let mut aggregate = ChunkExtractAggregate::default();
    let mut progress_completed = fold.progress_completed;
    loop {
        let item = tokio::select! {
            biased;
            result = buffered.try_next() => match result {
                Ok(Some(item)) => item,
                Ok(None) => break,
                Err(error) => return Err(error),
            },
            () = &mut stalled => {
                return Err(graph_extraction_idle_timeout(
                    fold.command.authority.revision_id,
                    fold.idle_timeout,
                ));
            }
        };
        crate::services::graph::extract::GraphExtractionActivity::record_current_chunk_completion();
        ensure_not_cancelled(fold.cancellation_token)?;
        let item = match item {
            IngestAttemptMutationOutcome::Applied(item) => item,
            IngestAttemptMutationOutcome::AuthorityLost => {
                return Ok(IngestAttemptMutationOutcome::AuthorityLost);
            }
        };
        aggregate.add(&item);
        progress_completed = sync_graph_chunk_progress(
            fold.state,
            fold.command,
            progress_completed,
            fold.progress_total,
            item.processed_graph_chunks,
        )
        .await;
        fold.progress_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(IngestAttemptMutationOutcome::Applied(aggregate))
}

fn graph_extraction_idle_timeout(
    revision_id: Uuid,
    idle_timeout: std::time::Duration,
) -> anyhow::Error {
    anyhow::anyhow!(
        "graph extraction idle timeout: no chunk completed for revision {} within {}s",
        revision_id,
        idle_timeout.as_secs()
    )
}

async fn sync_graph_chunk_progress(
    state: &AppState,
    command: &MaterializeRevisionGraphCandidatesCommand,
    progress_completed: usize,
    progress_total: usize,
    processed_graph_chunks: usize,
) -> usize {
    if processed_graph_chunks == 0 {
        return progress_completed;
    }
    let completed = progress_completed.saturating_add(processed_graph_chunks);
    if should_sync_stage_unit_progress(completed, progress_total) {
        sync_extract_graph_stage_progress(
            state,
            command.authority.attempt_id,
            completed,
            progress_total,
        )
        .await;
    }
    completed
}

fn graph_materialization_result(
    result_seed: GraphMaterializationResultSeed,
    aggregate: ChunkExtractAggregate,
    binding: GraphMaterializationBinding,
) -> RevisionGraphCandidateMaterialization {
    RevisionGraphCandidateMaterialization {
        chunk_count: result_seed.chunk_count,
        selected_graph_chunks: result_seed.selected_graph_chunk_count,
        extracted_entities: aggregate
            .extracted_entities
            .saturating_add(result_seed.reused_entity_count),
        extracted_relations: aggregate
            .extracted_relations
            .saturating_add(result_seed.reused_relation_count),
        provider_kind: Some(binding.provider_kind),
        model_name: Some(binding.model_name),
        usage_json: serde_json::json!({
            "prompt_tokens": aggregate.prompt_tokens,
            "completion_tokens": aggregate.completion_tokens,
            "total_tokens": aggregate.total_tokens,
        }),
        reused_chunks: result_seed.reused_chunk_count,
        reused_entities: result_seed.reused_entity_count,
        reused_relations: result_seed.reused_relation_count,
        record_stream_source_units_skipped: result_seed.record_stream_source_units_skipped,
    }
}

pub(super) struct GraphMaterializationSources {
    pub(super) graph_runtime_context: crate::services::ingest::runtime::RuntimeTaskExecutionContext,
    pub(super) revision: KnowledgeRevisionRow,
    pub(super) document: KnowledgeDocumentRow,
    pub(super) all_chunks: Vec<KnowledgeChunkRow>,
    pub(super) structured_revision: Option<KnowledgeStructuredRevisionRow>,
    pub(super) revision_facts: Vec<TypedTechnicalFact>,
    pub(super) table_graph_context: RevisionTableGraphContext,
    pub(super) library_extraction_prompt: Option<String>,
    pub(super) sub_type_hints: GraphExtractionSubTypeHints,
}

fn log_canceled_graph_orphans(
    command: &MaterializeRevisionGraphCandidatesCommand,
    document: &KnowledgeDocumentRow,
    canceled_orphans: u64,
) {
    if canceled_orphans == 0 {
        return;
    }
    info!(
        library_id = %command.authority.library_id,
        revision_id = %command.authority.revision_id,
        document_id = %document.document_id,
        canceled_orphans,
        "graph extraction resume canceled orphaned processing chunk rows"
    );
}

/// Per-chunk graph extraction outcome: only the small fields the caller
/// aggregates, keeping ~96 bytes per chunk in flight.
#[derive(Debug, Default)]
struct ChunkExtractAggregate {
    processed_graph_chunks: usize,
    extracted_entities: usize,
    extracted_relations: usize,
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
}

impl ChunkExtractAggregate {
    fn add(&mut self, item: &Self) {
        self.processed_graph_chunks =
            self.processed_graph_chunks.saturating_add(item.processed_graph_chunks);
        self.extracted_entities = self.extracted_entities.saturating_add(item.extracted_entities);
        self.extracted_relations =
            self.extracted_relations.saturating_add(item.extracted_relations);
        self.prompt_tokens += item.prompt_tokens;
        self.completion_tokens += item.completion_tokens;
        self.total_tokens += item.total_tokens;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::cancellation::AbortOnDrop;

    use crate::{
        domains::{
            graph_quality::{ExtractionOutcomeStatus, ExtractionRecoverySummary},
            runtime_ingestion::{RuntimeProviderFailureClass, RuntimeProviderFailureDetail},
        },
        infra::knowledge_rows::KnowledgeChunkRow,
        integrations::{provider_budget::ProviderBudgetError, retry::ProviderRateLimit},
        services::graph::extract::GraphExtractionResumeState,
    };

    /// The durable checkpoint is the `ready` `runtime_graph_extraction` row.
    /// `prepare_graph_chunk_extraction` is the exact boundary that turns the
    /// cache plan into the provider batch, so `prepared.chunks.len()` is the
    /// number of provider calls `extract_remaining_graph_chunks` can make.
    fn chunk(chunk_id: Uuid) -> KnowledgeChunkRow {
        KnowledgeChunkRow {
            chunk_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            content_text: String::new(),
            normalized_text: String::new(),
            span_start: None,
            span_end: None,
            token_count: None,
            support_block_ids: Vec::new(),
            section_path: Vec::new(),
            heading_trail: Vec::new(),
            literal_digest: None,
            chunk_state: "ready".to_string(),
            text_generation: None,
            vector_generation: None,
            quality_score: None,
            window_text: None,
            occurred_at: None,
            occurred_until: None,
        }
    }

    fn selection(chunk_count: usize) -> GraphMaterializationSelection {
        GraphMaterializationSelection {
            chunk_count,
            selected_graph_chunk_count: chunk_count,
            record_stream_source_units_skipped: 0,
            graph_chunk_policy: GraphExtractionChunkPolicy::standard(),
        }
    }

    /// A provider-failure snapshot for the boundary tests. `retry_after` is in
    /// whole seconds, matching the wire field the sanitized detail carries.
    fn provider_failure(
        failure_class: RuntimeProviderFailureClass,
        upstream_status: &str,
        retry_after_seconds: Option<u64>,
    ) -> RuntimeProviderFailureDetail {
        RuntimeProviderFailureDetail {
            failure_class,
            provider_kind: Some("provider-alpha".to_string()),
            model_name: None,
            request_shape_key: None,
            request_size_bytes: None,
            chunk_count: None,
            upstream_status: Some(upstream_status.to_string()),
            retry_after_seconds,
            elapsed_ms: None,
            retry_decision: None,
            usage_visible: true,
        }
    }

    /// The execution error `graph_chunk_extraction_error` consumes, with both
    /// failure channels left to the caller. Every field no test varies is fixed
    /// here so the boundary tests cannot drift apart.
    fn graph_execution_error(
        provider_failure: Option<RuntimeProviderFailureDetail>,
        provider_admission_error: Option<ProviderBudgetError>,
        request_size_bytes: usize,
    ) -> GraphExtractionExecutionError {
        GraphExtractionExecutionError {
            message: "graph extraction failed".to_string(),
            request_shape_key: "shape".to_string(),
            request_size_bytes,
            provider_failure: provider_failure.map(Box::new),
            provider_admission_error: provider_admission_error.map(Box::new),
            recovery_summary: Box::new(ExtractionRecoverySummary {
                status: ExtractionOutcomeStatus::Failed,
                second_pass_applied: false,
                warning: None,
            }),
            recovery_attempts: Vec::new(),
            resume_state: Box::new(GraphExtractionResumeState {
                resumed_from_checkpoint: false,
                replay_count: 0,
                downgrade_level: 0,
            }),
            cancelled: false,
        }
    }

    #[test]
    fn retry_reuses_every_ready_exact_key_chunk_without_a_provider_call() {
        // Attempt 1 persisted ready extractions for all three chunks; a
        // downstream reconcile/publication step then failed, so attempt 2 runs
        // materialization again and rebuilds the same cache plan from those
        // ready records.
        let chunks = vec![chunk(Uuid::now_v7()), chunk(Uuid::now_v7()), chunk(Uuid::now_v7())];
        let mut cache = GraphExtractionCachePlan::default();
        for chunk in &chunks {
            cache.reused_chunk_ids.insert(chunk.chunk_id);
        }
        cache.reused_entity_count = 5;
        cache.reused_relation_count = 3;

        let prepared = prepare_graph_chunk_extraction(selection(chunks.len()), chunks, cache);

        // Zero provider calls: the retry pays nothing for the already-extracted
        // chunks and still reports the reused counts.
        assert_eq!(prepared.chunks.len(), 0);
        assert_eq!(prepared.result_seed.reused_chunk_count, 3);
        assert_eq!(prepared.result_seed.reused_entity_count, 5);
        assert_eq!(prepared.result_seed.reused_relation_count, 3);
        assert_eq!(prepared.progress_total, 3);
    }

    #[test]
    fn a_chunk_without_a_ready_exact_key_record_still_reaches_the_provider() {
        let reused = chunk(Uuid::now_v7());
        let missing = chunk(Uuid::now_v7());
        let mut cache = GraphExtractionCachePlan::default();
        cache.reused_chunk_ids.insert(reused.chunk_id);

        let prepared =
            prepare_graph_chunk_extraction(selection(2), vec![reused, missing.clone()], cache);

        assert_eq!(prepared.chunks.len(), 1);
        assert_eq!(prepared.chunks[0].chunk_id, missing.chunk_id);
        assert_eq!(prepared.result_seed.reused_chunk_count, 1);
        assert_eq!(prepared.progress_total, 2);
    }

    /// `classify_content_ingest_failure` (ingest worker) decides
    /// `provider_concurrency_unavailable` by downcasting the anyhow chain to
    /// `ProviderBudgetError`. `provider_admission_error` is boxed on the
    /// execution error, so this boundary must unwrap the box before converting:
    /// a `Box<_>` source is not downcastable and the same refusal would fall
    /// through to `worker_error`.
    #[test]
    fn graph_admission_error_is_unboxed_for_the_ingest_classifier() {
        let converted = graph_chunk_extraction_error(
            Uuid::now_v7(),
            graph_execution_error(None, Some(ProviderBudgetError::LimiterClosed), 1),
        );

        assert!(
            converted.downcast_ref::<ProviderBudgetError>().is_some(),
            "the graph admission conversion left the ProviderBudgetError boxed, so the ingest \
             classifier cannot see it and would classify the failure as worker_error"
        );

        // The exact regression this guards: keeping the box makes the source
        // `Box<ProviderBudgetError>`, which is not downcastable to the inner
        // error and therefore classifies as `worker_error`.
        let boxed = anyhow::Error::new(Box::new(ProviderBudgetError::LimiterClosed));
        assert!(
            boxed.downcast_ref::<ProviderBudgetError>().is_none(),
            "a boxed admission error is not downcastable to ProviderBudgetError"
        );
    }

    /// A provider 429 on the graph path must cross the boundary as a typed
    /// `ProviderRateLimit`, not a bare message: the ingest worker defers the job
    /// for the provider's cooldown, where a lost signal would instead burn the
    /// attempt budget until the revision failed permanently.
    #[test]
    fn graph_rate_limit_error_is_preserved_for_the_ingest_classifier() {
        let rate_limited = graph_chunk_extraction_error(
            Uuid::now_v7(),
            graph_execution_error(
                Some(provider_failure(
                    RuntimeProviderFailureClass::UpstreamRejection,
                    "429",
                    Some(30),
                )),
                None,
                1,
            ),
        );
        let preserved = rate_limited
            .downcast_ref::<ProviderRateLimit>()
            .expect("a 429 must cross the boundary as a typed ProviderRateLimit");
        assert_eq!(preserved.retry_after, Some(std::time::Duration::from_secs(30)));

        let other_rejection = graph_chunk_extraction_error(
            Uuid::now_v7(),
            graph_execution_error(
                Some(provider_failure(RuntimeProviderFailureClass::UpstreamRejection, "400", None)),
                None,
                1,
            ),
        );
        assert!(
            other_rejection.downcast_ref::<ProviderRateLimit>().is_none(),
            "a non-429 rejection must not be reported as a provider rate limit"
        );
    }

    /// The incident this reconstructor exists for: a chunk larger than the
    /// request-size soft limit got HTTP 429. `classify_failure` forces the
    /// derived `failure_class` to `InternalRequestInvalid` from the size
    /// BEFORE it consults the status, so the snapshot reads
    /// `InternalRequestInvalid` while the operational fact — `upstream_status`
    /// 429 — is intact. Keying the reconstruction on the derived class drops
    /// the signal on exactly this path and burns the attempt budget.
    #[test]
    fn graph_rate_limit_survives_a_size_limit_forced_internal_class() {
        use crate::services::ops::provider_failure::{
            ProviderFailureClassificationService, ProviderFailureObservation,
        };
        use reqwest::{StatusCode, header::HeaderMap};

        let headers = HeaderMap::new();
        let error = anyhow::Error::new(crate::integrations::retry::provider_http_status_error(
            "provider-alpha",
            StatusCode::TOO_MANY_REQUESTS,
            &headers,
            r#"{"error":{"message":"rate limited"}}"#,
        ));
        let classifier = ProviderFailureClassificationService::new(1024);
        let failure = classifier
            .classify_failure(
                &error,
                None,
                ProviderFailureObservation {
                    provider_kind: Some("provider-alpha".to_string()),
                    request_size_bytes: Some(2_048),
                    ..ProviderFailureObservation::default()
                },
            )
            .expect("a 429 must classify");
        assert_eq!(
            failure.failure_class,
            RuntimeProviderFailureClass::InternalRequestInvalid,
            "the size soft limit must have forced the derived class off UpstreamRejection"
        );
        assert_eq!(failure.upstream_status.as_deref(), Some("429"));

        let converted = graph_chunk_extraction_error(
            Uuid::now_v7(),
            graph_execution_error(Some(failure), None, 2_048),
        );
        assert!(
            converted.downcast_ref::<ProviderRateLimit>().is_some(),
            "a 429 must cross the boundary as a typed ProviderRateLimit even when the size \
             soft limit forced the derived failure class to InternalRequestInvalid"
        );
    }

    /// Dropping the guard must abort the reporter task. A `JoinHandle` dropped
    /// on its own merely detaches the task, which is how a reporter survived
    /// its parent for 33 minutes and kept logging every 30s.
    #[tokio::test]
    async fn progress_reporter_guard_aborts_its_task_on_drop() {
        struct DropMarker(std::sync::Arc<std::sync::atomic::AtomicBool>);
        impl Drop for DropMarker {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }

        let future_dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let guard = {
            let marker = DropMarker(std::sync::Arc::clone(&future_dropped));
            AbortOnDrop::new(tokio::spawn(async move {
                let _marker = marker;
                std::future::pending::<()>().await;
            }))
        };
        assert!(
            !future_dropped.load(std::sync::atomic::Ordering::Relaxed),
            "the reporter future must be alive before the guard is dropped",
        );

        drop(guard);

        let aborted = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !future_dropped.load(std::sync::atomic::Ordering::Relaxed) {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(aborted.is_ok(), "dropping the reporter guard must abort its task");
    }

    /// The real reporter returned to the pipeline carries the same abort-on-drop
    /// contract, so a dropped ingest future cannot leak a 30s progress task.
    #[tokio::test]
    async fn graph_progress_reporter_is_aborted_when_its_guard_drops() {
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let mut reporter =
            spawn_graph_progress_reporter(counter, Uuid::now_v7(), Uuid::now_v7(), 4);
        let probe = reporter
            .handle_mut()
            .expect("the reporter guard is armed before its drop")
            .abort_handle();
        assert!(!probe.is_finished(), "the reporter must be running before the guard drops");

        drop(reporter);

        for _ in 0..500 {
            if probe.is_finished() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        panic!("dropping the reporter guard must abort the reporter task");
    }
}
