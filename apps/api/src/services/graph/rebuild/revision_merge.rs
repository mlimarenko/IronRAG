use super::*;

#[derive(Debug, Default)]
struct ChunkMergeOutcome {
    contribution: usize,
    node_ids: Vec<Uuid>,
    edge_ids: Vec<Uuid>,
}

/// One unchanged chunk carried across an append: the parent chunk id and the
/// id the new revision gave the same content at the same index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct KeptChunkPair {
    pub(super) previous_chunk_id: Uuid,
    pub(super) revision_chunk_id: Uuid,
}

/// A pure append: the new revision repeats every parent chunk at the same
/// index with the same text checksum and only appends chunks after them.
///
/// Chunk ids themselves are revision-scoped, so the stable link between the
/// two revisions is `(chunk_index, text_checksum)`, not the id. The chunker is
/// a forward greedy scan whose flushed windows read only earlier blocks, and
/// near-duplicate marking is forward-only, so an append cannot rewrite an
/// earlier window's `normalized_text`; `window_text` may change on the last
/// carried chunk and is intentionally not part of the identity.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct PureAppendDelta {
    pub(super) kept: Vec<KeptChunkPair>,
    pub(super) added_chunk_ids: BTreeSet<Uuid>,
}

impl PureAppendDelta {
    pub(super) fn kept_previous_chunk_ids(&self) -> Vec<Uuid> {
        self.kept.iter().map(|pair| pair.previous_chunk_id).collect()
    }

    pub(super) fn kept_revision_chunk_ids(&self) -> Vec<Uuid> {
        self.kept.iter().map(|pair| pair.revision_chunk_id).collect()
    }
}

/// Classifies a revision against its previous active revision.
///
/// Returns `None` for anything that is not a pure append (edited, removed or
/// reordered chunks), so the caller keeps the full teardown path unchanged.
pub(super) fn classify_pure_append_chunks(
    previous_chunks: &[content_chunk_repository::ContentChunkRow],
    revision_chunks: &[content_chunk_repository::ContentChunkRow],
) -> Option<PureAppendDelta> {
    if previous_chunks.is_empty() || revision_chunks.len() < previous_chunks.len() {
        return None;
    }
    let mut kept = Vec::with_capacity(previous_chunks.len());
    for (previous, revision) in previous_chunks.iter().zip(revision_chunks.iter()) {
        if previous.chunk_index != revision.chunk_index
            || previous.text_checksum != revision.text_checksum
        {
            return None;
        }
        kept.push(KeptChunkPair { previous_chunk_id: previous.id, revision_chunk_id: revision.id });
    }
    let added_chunk_ids = revision_chunks[previous_chunks.len()..]
        .iter()
        .map(|chunk| chunk.id)
        .collect::<BTreeSet<_>>();
    Some(PureAppendDelta { kept, added_chunk_ids })
}

pub(super) struct RevisionGraphMergeSummary {
    pub(super) graph_contribution_count: usize,
    pub(super) changed_node_ids: Vec<Uuid>,
    pub(super) changed_edge_ids: Vec<Uuid>,
}

fn summarize_revision_graph_merges(
    merge_results: Vec<ChunkMergeOutcome>,
    cancellation_token: &CancellationToken,
) -> Result<RevisionGraphMergeSummary, GraphServiceError> {
    let mut graph_contribution_count = 0usize;
    let mut changed_node_ids = BTreeSet::new();
    let mut changed_edge_ids = BTreeSet::new();
    for outcome in merge_results {
        ensure_not_cancelled(cancellation_token)?;
        graph_contribution_count = graph_contribution_count.saturating_add(outcome.contribution);
        changed_node_ids.extend(outcome.node_ids);
        changed_edge_ids.extend(outcome.edge_ids);
    }
    Ok(RevisionGraphMergeSummary {
        graph_contribution_count,
        changed_node_ids: changed_node_ids.into_iter().collect(),
        changed_edge_ids: changed_edge_ids.into_iter().collect(),
    })
}

pub(super) async fn collect_revision_graph_targets_with_connection(
    connection: &mut sqlx::PgConnection,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    delete_query_references: bool,
    cancellation_token: &CancellationToken,
) -> Result<(BTreeSet<Uuid>, BTreeSet<Uuid>), GraphServiceError> {
    ensure_not_cancelled(cancellation_token)?;
    if delete_query_references {
        repositories::query_repository::delete_query_execution_references_by_content_revision_with_executor(
            &mut *connection,
            library_id,
            document_id,
            revision_id,
        )
        .await
        .with_context(|| {
            format!(
                "failed to delete superseded query references for document {document_id} revision {revision_id}"
            )
        })?;
    }
    let targets =
        repositories::deactivate_runtime_graph_evidence_by_content_revision_with_executor(
            &mut *connection,
            library_id,
            document_id,
            revision_id,
        )
        .await
        .with_context(|| {
            format!(
                "failed to replace graph evidence for document {document_id} revision {revision_id}"
            )
        })?;
    ensure_not_cancelled(cancellation_token)?;
    Ok(partition_graph_target_ids(targets))
}

/// Records and rows for one revision's graph merge pass.
///
/// Groups the mutable record/row maps with the merge scope and document so the
/// entry point stays within the argument budget; iteration order is unchanged.
pub(super) struct RevisionMergeRecordsInput {
    pub(super) document: DocumentRow,
    pub(super) chunk_rows_by_id: BTreeMap<Uuid, ChunkRow>,
    pub(super) merge_scope: GraphMergeScope,
    pub(super) records: BTreeMap<Uuid, repositories::RuntimeGraphExtractionRecordRow>,
}

pub(super) async fn merge_revision_graph_records_with_connection(
    connection: &mut sqlx::PgConnection,
    state: &AppState,
    input: RevisionMergeRecordsInput,
    cancellation_token: &CancellationToken,
) -> Result<RevisionGraphMergeSummary, GraphServiceError> {
    let RevisionMergeRecordsInput { document, chunk_rows_by_id, merge_scope, records } = input;
    let mut outcomes = Vec::with_capacity(records.len());
    for mut record in records.into_values() {
        ensure_not_cancelled(cancellation_token)?;
        let chunk_id = record.chunk_id;
        let Some(chunk_row) = chunk_rows_by_id.get(&chunk_id) else {
            continue;
        };
        if !is_graph_reconcile_chunk_text_eligible(&chunk_row.content) {
            continue;
        }
        let normalized = std::mem::take(&mut record.normalized_output_json);
        let recovery = extraction_recovery_summary_from_record(&record);
        let candidates = tokio::task::spawn_blocking(move || {
            repaired_graph_extraction_candidates(normalized)
        })
        .await
        .context("normalized graph extraction decode task panicked")?
        .with_context(|| {
            format!(
                "failed to decode normalized graph extraction for document {} chunk {chunk_id}",
                document.id
            )
        })?;
        ensure_not_cancelled(cancellation_token)?;
        if candidates.entities.is_empty() && candidates.relations.is_empty() {
            outcomes.push(ChunkMergeOutcome::default());
            continue;
        }
        let merge_outcome = merge_chunk_graph_candidates_with_connection(
            connection,
            &state.bulk_ingest_hardening_services.graph_quality_guard,
            &merge_scope,
            MergeChunkInput {
                document: &document,
                chunk: chunk_row,
                candidates: &candidates,
                extraction_recovery: recovery.as_ref(),
            },
        )
        .await?;
        outcomes.push(ChunkMergeOutcome {
            contribution: merge_outcome.nodes.len() + merge_outcome.edges.len(),
            node_ids: merge_outcome.summary_refresh_node_ids(),
            edge_ids: merge_outcome.summary_refresh_edge_ids(),
        });
    }
    let summary = summarize_revision_graph_merges(outcomes, cancellation_token)?;
    repositories::recalculate_runtime_graph_support_counts_by_ids_with_executor(
        connection,
        merge_scope.library_id,
        merge_scope.projection_version,
        &summary.changed_node_ids,
        &summary.changed_edge_ids,
    )
    .await
    .context("failed to reconcile graph support counts during authorized revision reconcile")?;
    ensure_not_cancelled(cancellation_token)?;
    Ok(summary)
}

fn partition_graph_target_ids(
    targets: Vec<repositories::RuntimeGraphEvidenceTargetRow>,
) -> (BTreeSet<Uuid>, BTreeSet<Uuid>) {
    let mut node_ids = BTreeSet::new();
    let mut edge_ids = BTreeSet::new();
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
    (node_ids, edge_ids)
}

pub(super) fn select_latest_revision_extraction_records(
    records: Vec<repositories::RuntimeGraphExtractionRecordRow>,
    revision_chunk_ids: &BTreeSet<Uuid>,
    revision_id: Uuid,
    attempt_id: Uuid,
    cancellation_token: &CancellationToken,
) -> Result<BTreeMap<Uuid, repositories::RuntimeGraphExtractionRecordRow>, GraphServiceError> {
    let mut latest_records = BTreeMap::new();
    for record in records {
        ensure_not_cancelled(cancellation_token)?;
        if extraction_record_matches_revision(&record, revision_chunk_ids, revision_id, attempt_id)
        {
            latest_records.insert(record.chunk_id, record);
        }
    }
    Ok(latest_records)
}

fn extraction_record_matches_revision(
    record: &repositories::RuntimeGraphExtractionRecordRow,
    revision_chunk_ids: &BTreeSet<Uuid>,
    revision_id: Uuid,
    attempt_id: Uuid,
) -> bool {
    if record.status != ASYNC_OP_STATUS_READY || !revision_chunk_ids.contains(&record.chunk_id) {
        return false;
    }
    let lifecycle = extraction_lifecycle_from_record(record);
    lifecycle.revision_id == Some(revision_id)
        && lifecycle.activated_by_attempt_id == Some(attempt_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(
        id: u128,
        chunk_index: i32,
        text_checksum: &str,
    ) -> content_chunk_repository::ContentChunkRow {
        content_chunk_repository::ContentChunkRow {
            id: Uuid::from_u128(id),
            revision_id: Uuid::from_u128(0xAA),
            chunk_index,
            start_offset: 0,
            end_offset: 1,
            token_count: Some(1),
            normalized_text: format!("normalized-{text_checksum}"),
            text_checksum: text_checksum.to_string(),
            occurred_at: None,
            occurred_until: None,
        }
    }

    #[test]
    fn pure_append_keeps_parent_chunks_and_reports_only_appended() {
        let previous = vec![chunk(1, 0, "a"), chunk(2, 1, "b")];
        let revision = vec![chunk(11, 0, "a"), chunk(12, 1, "b"), chunk(13, 2, "c")];

        let delta = classify_pure_append_chunks(&previous, &revision).expect("pure append");

        assert_eq!(delta.kept.len(), 2);
        assert_eq!(delta.kept_previous_chunk_ids(), vec![Uuid::from_u128(1), Uuid::from_u128(2)]);
        assert_eq!(delta.kept_revision_chunk_ids(), vec![Uuid::from_u128(11), Uuid::from_u128(12)]);
        assert_eq!(delta.added_chunk_ids, BTreeSet::from([Uuid::from_u128(13)]));
    }

    #[test]
    fn edited_deleted_shrunk_and_reordered_revisions_fall_back_to_full_path() {
        let previous = vec![chunk(1, 0, "a"), chunk(2, 1, "b")];

        let edited = vec![chunk(11, 0, "a"), chunk(12, 1, "changed"), chunk(13, 2, "c")];
        assert!(classify_pure_append_chunks(&previous, &edited).is_none());

        let removed = vec![chunk(11, 0, "a")];
        assert!(classify_pure_append_chunks(&previous, &removed).is_none());

        let reordered = vec![chunk(11, 0, "b"), chunk(12, 1, "a"), chunk(13, 2, "c")];
        assert!(classify_pure_append_chunks(&previous, &reordered).is_none());

        let identical = vec![chunk(11, 0, "a"), chunk(12, 1, "b")];
        let delta = classify_pure_append_chunks(&previous, &identical).expect("identical prefix");
        assert!(delta.added_chunk_ids.is_empty());

        assert!(classify_pure_append_chunks(&[], &[chunk(11, 0, "a")]).is_none());
    }
}
