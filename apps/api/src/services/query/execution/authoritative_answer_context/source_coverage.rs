use super::{
    AppState, BTreeSet, Context, HashMap, HashSet, KnowledgeBundleChunkReferenceRow,
    KnowledgeChunkRow, KnowledgeDocumentRow, QueryAct, QueryIR, QueryScope, QueryTargetKind,
    RuntimeMatchedChunk, SOURCE_COVERAGE_CANDIDATE_CHUNKS_PER_DOCUMENT,
    SOURCE_COVERAGE_CHUNKS_PER_DOCUMENT, SOURCE_COVERAGE_DOCUMENT_LIMIT,
    SOURCE_COVERAGE_MAX_ADDED_CHUNKS, SOURCE_COVERAGE_SCORE_BASE, SOURCE_COVERAGE_SCORE_STEP,
    SOURCE_PROFILE_SCORE, Uuid, current_document_revision_id, map_chunk_hit, score_desc_chunks,
    technical_chunk_selection_score,
};

/// Read-only inputs for the source-coverage augmentation stage.
pub(super) struct SourceCoverageInputs<'a> {
    pub(super) state: &'a AppState,
    pub(super) question: &'a str,
    pub(super) query_ir: &'a QueryIR,
    pub(super) focused_document_id: Option<Uuid>,
    pub(super) document_index: &'a HashMap<Uuid, KnowledgeDocumentRow>,
    pub(super) plan_keywords: &'a [String],
    pub(super) ranked_document_ids: &'a [Uuid],
}

/// Mutable accumulators threaded through the per-document append step.
struct SourceCoverageAppendTargets<'a> {
    seen_chunk_ids: &'a mut HashSet<Uuid>,
    coverage_rank: &'a mut usize,
    chunks: &'a mut Vec<RuntimeMatchedChunk>,
}

pub(super) async fn augment_with_source_coverage_chunks(
    inputs: SourceCoverageInputs<'_>,
    mut chunks: Vec<RuntimeMatchedChunk>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if !should_request_source_coverage_chunks(inputs.question, inputs.query_ir) {
        log_source_coverage_skip(inputs.query_ir);
        return Ok(chunks);
    }
    let initial_chunk_count = chunks.len();
    let coverage_plan = source_coverage_plan(
        inputs.focused_document_id,
        inputs.ranked_document_ids,
        initial_chunk_count,
        source_coverage_document_limit(inputs.query_ir),
    );
    if coverage_plan.is_empty() {
        return Ok(chunks);
    }

    let mut seen_chunk_ids = chunks.iter().map(|chunk| chunk.chunk_id).collect::<HashSet<_>>();
    let mut coverage_rank = 0_usize;
    let coverage_rows_by_revision = load_source_coverage_rows_by_revision(
        inputs.state,
        &coverage_plan,
        inputs.document_index,
        inputs.plan_keywords,
    )
    .await?;
    for (document_id, chunk_limit) in coverage_plan {
        append_source_coverage_document(
            &inputs,
            &coverage_rows_by_revision,
            document_id,
            chunk_limit,
            SourceCoverageAppendTargets {
                seen_chunk_ids: &mut seen_chunk_ids,
                coverage_rank: &mut coverage_rank,
                chunks: &mut chunks,
            },
        );
    }
    log_source_coverage_augmented(
        inputs.focused_document_id,
        initial_chunk_count,
        &chunks,
        coverage_rank,
    );
    Ok(chunks)
}

fn log_source_coverage_skip(query_ir: &QueryIR) {
    tracing::debug!(
        stage = "source_coverage_skip",
        reason = "predicate_false",
        ?query_ir.act,
        "source coverage augmentation skipped"
    );
}

fn log_source_coverage_augmented(
    focused_document_id: Option<Uuid>,
    initial_chunk_count: usize,
    chunks: &[RuntimeMatchedChunk],
    coverage_rank: usize,
) {
    tracing::info!(
        stage = "source_coverage_augmented",
        initial_chunk_count,
        final_chunk_count = chunks.len(),
        added_chunk_count = chunks.len().saturating_sub(initial_chunk_count),
        focused_document_id = ?focused_document_id,
        coverage_rank,
        "source coverage augmentation finished"
    );
}

fn append_source_coverage_document(
    inputs: &SourceCoverageInputs<'_>,
    coverage_rows_by_revision: &HashMap<Uuid, Vec<KnowledgeChunkRow>>,
    document_id: Uuid,
    chunk_limit: usize,
    targets: SourceCoverageAppendTargets<'_>,
) {
    let Some(document) = inputs.document_index.get(&document_id) else {
        return;
    };
    let Some(revision_id) = current_document_revision_id(document) else {
        return;
    };
    let Some(rows) = coverage_rows_by_revision.get(&revision_id) else {
        return;
    };
    append_source_coverage_chunks(
        select_source_coverage_chunk_rows(rows.clone(), chunk_limit, inputs.plan_keywords),
        inputs.document_index,
        inputs.plan_keywords,
        targets.seen_chunk_ids,
        targets.coverage_rank,
        targets.chunks,
    );
}

pub(super) fn ranked_document_ids_from_chunks(chunks: &[RuntimeMatchedChunk]) -> Vec<Uuid> {
    let mut seen = HashSet::new();
    chunks
        .iter()
        .map(|chunk| chunk.document_id)
        .filter(|document_id| seen.insert(*document_id))
        .collect()
}

pub(super) fn align_chunks_to_bundle_ranking(
    chunks: &mut [RuntimeMatchedChunk],
    references: &[KnowledgeBundleChunkReferenceRow],
) {
    let ranking = references
        .iter()
        .map(|reference| (reference.chunk_id, (reference.rank, reference.score)))
        .collect::<HashMap<_, _>>();
    for chunk in chunks.iter_mut() {
        if let Some((_, score)) = ranking.get(&chunk.chunk_id) {
            chunk.score = Some(*score as f32);
        }
    }
    chunks.sort_by(|left, right| {
        let left_rank = ranking.get(&left.chunk_id).map(|(rank, _)| *rank).unwrap_or(i32::MAX);
        let right_rank = ranking.get(&right.chunk_id).map(|(rank, _)| *rank).unwrap_or(i32::MAX);
        left_rank
            .cmp(&right_rank)
            .then_with(|| score_desc_chunks(left, right))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
}

pub(super) fn source_coverage_plan(
    focused_document_id: Option<Uuid>,
    ranked_document_ids: &[Uuid],
    initial_chunk_count: usize,
    document_limit: usize,
) -> Vec<(Uuid, usize)> {
    let candidates = focused_document_id.into_iter().chain(ranked_document_ids.iter().copied());
    let mut seen = HashSet::new();
    let document_ids = candidates
        .filter(|document_id| seen.insert(*document_id))
        .take(document_limit)
        .collect::<Vec<_>>();
    if document_ids.is_empty() {
        return Vec::new();
    }

    let total_budget = initial_chunk_count
        .saturating_mul(2)
        .clamp(SOURCE_COVERAGE_CHUNKS_PER_DOCUMENT, SOURCE_COVERAGE_MAX_ADDED_CHUNKS);
    let base_limit = total_budget / document_ids.len();
    let remainder = total_budget % document_ids.len();
    document_ids
        .into_iter()
        .enumerate()
        .map(|(index, document_id)| {
            let rank_share = base_limit.saturating_add(usize::from(index < remainder));
            (document_id, rank_share.min(SOURCE_COVERAGE_CHUNKS_PER_DOCUMENT))
        })
        .filter(|(_, chunk_limit)| *chunk_limit > 0)
        .collect()
}

async fn load_source_coverage_rows_by_revision(
    state: &AppState,
    coverage_plan: &[(Uuid, usize)],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
) -> anyhow::Result<HashMap<Uuid, Vec<KnowledgeChunkRow>>> {
    let mut revision_ids = Vec::new();
    let mut seen_revisions = HashSet::new();
    for (document_id, _) in coverage_plan {
        let Some(document) = document_index.get(document_id) else {
            continue;
        };
        let Some(revision_id) = current_document_revision_id(document) else {
            continue;
        };
        if seen_revisions.insert(revision_id) {
            revision_ids.push(revision_id);
        }
    }
    let focused_by_revision =
        load_source_coverage_matching_terms(state, &revision_ids, plan_keywords).await?;

    let mut rows_by_revision = HashMap::new();
    for revision_id in revision_ids {
        let mut rows = state
            .document_store
            .list_head_chunks_by_revision(
                revision_id,
                SOURCE_COVERAGE_CANDIDATE_CHUNKS_PER_DOCUMENT,
            )
            .await
            .with_context(|| {
                format!("failed to load source coverage sample for revision {revision_id}")
            })?;
        let tail_rows = state
            .document_store
            .list_tail_chunks_by_revision(revision_id, SOURCE_COVERAGE_CHUNKS_PER_DOCUMENT)
            .await
            .with_context(|| {
                format!("failed to load source coverage tail for revision {revision_id}")
            })?;
        extend_unique_source_coverage_rows(&mut rows, tail_rows);
        if let Some(focused_rows) = focused_by_revision.get(&revision_id) {
            extend_unique_source_coverage_rows(&mut rows, focused_rows.clone());
        }
        rows_by_revision.insert(revision_id, rows);
    }
    Ok(rows_by_revision)
}

async fn load_source_coverage_matching_terms(
    state: &AppState,
    revision_ids: &[Uuid],
    plan_keywords: &[String],
) -> anyhow::Result<HashMap<Uuid, Vec<KnowledgeChunkRow>>> {
    if plan_keywords.is_empty() || revision_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let focused_rows = state
        .document_store
        .list_chunks_by_revisions_matching_terms(
            revision_ids,
            plan_keywords,
            SOURCE_COVERAGE_CANDIDATE_CHUNKS_PER_DOCUMENT,
        )
        .await
        .context("failed to load focused source coverage chunks")?;
    let mut by_revision = HashMap::<Uuid, Vec<KnowledgeChunkRow>>::new();
    for row in focused_rows {
        by_revision.entry(row.revision_id).or_default().push(row);
    }
    Ok(by_revision)
}

fn extend_unique_source_coverage_rows(
    rows: &mut Vec<KnowledgeChunkRow>,
    additional_rows: Vec<KnowledgeChunkRow>,
) {
    let mut row_ids = rows.iter().map(|row| row.chunk_id).collect::<HashSet<_>>();
    rows.extend(additional_rows.into_iter().filter(|row| row_ids.insert(row.chunk_id)));
}

fn append_source_coverage_chunks(
    rows: Vec<KnowledgeChunkRow>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    seen_chunk_ids: &mut HashSet<Uuid>,
    coverage_rank: &mut usize,
    chunks: &mut Vec<RuntimeMatchedChunk>,
) {
    for row in rows {
        if !seen_chunk_ids.insert(row.chunk_id) {
            continue;
        }
        let score = source_coverage_row_score(&row, *coverage_rank);
        *coverage_rank = coverage_rank.saturating_add(1);
        if let Some(mut chunk) = map_chunk_hit(row, score, document_index, plan_keywords) {
            chunk.score = Some(score);
            chunks.push(chunk);
        }
    }
}

fn source_coverage_row_score(row: &KnowledgeChunkRow, coverage_rank: usize) -> f32 {
    if is_source_profile_chunk(row) {
        SOURCE_PROFILE_SCORE
    } else {
        SOURCE_COVERAGE_SCORE_BASE - coverage_rank as f32 * SOURCE_COVERAGE_SCORE_STEP
    }
}

pub(super) fn broad_unfocused_procedure_source_coverage(query_ir: &QueryIR) -> bool {
    query_ir.requests_broad_procedure_variant_coverage()
}

pub(super) fn source_coverage_document_limit(query_ir: &QueryIR) -> usize {
    if broad_unfocused_procedure_source_coverage(query_ir) {
        return 5;
    }
    let requested_sources =
        query_ir.target_entities.len().saturating_add(query_ir.literal_constraints.len()).max(
            match query_ir.scope {
                QueryScope::MultiDocument => 4,
                QueryScope::CrossLibrary | QueryScope::LibraryMeta => 5,
                QueryScope::SingleDocument => SOURCE_COVERAGE_DOCUMENT_LIMIT,
            },
        );
    if matches!(query_ir.act, QueryAct::Compare) {
        requested_sources.saturating_add(1).clamp(SOURCE_COVERAGE_DOCUMENT_LIMIT, 6)
    } else {
        requested_sources.clamp(SOURCE_COVERAGE_DOCUMENT_LIMIT, 5)
    }
}

pub(super) fn should_request_source_coverage_chunks(_question: &str, query_ir: &QueryIR) -> bool {
    query_ir.requests_source_coverage_context()
        || query_ir.is_exact_literal_technical()
        || query_ir_requests_setup_source_coverage(query_ir)
        || query_ir_requests_inventory_source_coverage(query_ir)
}

pub(super) fn query_ir_requests_setup_source_coverage(query_ir: &QueryIR) -> bool {
    let act_signals_setup = matches!(
        query_ir.act,
        crate::domains::query_ir::QueryAct::ConfigureHow
            | crate::domains::query_ir::QueryAct::Describe
            | crate::domains::query_ir::QueryAct::RetrieveValue
    );
    if !act_signals_setup {
        return false;
    }

    if broad_unfocused_procedure_source_coverage(query_ir) {
        return true;
    }
    let has_config_target =
        query_ir.targets_any(&[QueryTargetKind::ConfigurationFile, QueryTargetKind::ConfigKey]);
    let has_package_or_parameter_target =
        query_ir.targets_any(&[QueryTargetKind::Package, QueryTargetKind::Parameter]);

    // Original gate: configuration_file/config_key + package/parameter
    // both present (matches a high-confidence "configure this parameter
    // inside this config file" intent).
    if has_config_target && has_package_or_parameter_target {
        return true;
    }

    query_ir.document_focus.is_some()
}

pub(super) fn query_ir_requests_inventory_source_coverage(query_ir: &QueryIR) -> bool {
    if !matches!(
        query_ir.act,
        crate::domains::query_ir::QueryAct::Describe
            | crate::domains::query_ir::QueryAct::Enumerate
            | crate::domains::query_ir::QueryAct::Compare
            | crate::domains::query_ir::QueryAct::RetrieveValue
    ) {
        return false;
    }
    if query_ir.source_slice.is_some() {
        return false;
    }
    if !query_ir.target_types.is_empty() || !query_ir.target_entities.is_empty() {
        return true;
    }
    if !query_ir.literal_constraints.is_empty()
        && matches!(query_ir.scope, QueryScope::SingleDocument)
    {
        return true;
    }

    false
}

pub(super) fn select_source_coverage_chunk_rows(
    mut rows: Vec<KnowledgeChunkRow>,
    limit: usize,
    plan_keywords: &[String],
) -> Vec<KnowledgeChunkRow> {
    if rows.len() <= limit {
        return rows;
    }
    rows.sort_by(|left, right| {
        left.chunk_index.cmp(&right.chunk_index).then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });

    let mut selected = source_coverage_anchor_indices(&rows, limit, plan_keywords);
    fill_source_coverage_indices(&mut selected, rows.len(), limit);
    selected.into_iter().take(limit).filter_map(|index| rows.get(index).cloned()).collect()
}

fn source_coverage_anchor_indices(
    rows: &[KnowledgeChunkRow],
    limit: usize,
    plan_keywords: &[String],
) -> BTreeSet<usize> {
    let mut selected = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| is_source_profile_chunk(row).then_some(index))
        .collect::<BTreeSet<_>>();
    let mut focus_rows = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {
            let score = source_coverage_focus_row_score(row, plan_keywords);
            (score > 0).then_some((score, index))
        })
        .collect::<Vec<_>>();
    focus_rows.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    selected.extend(
        focus_rows.into_iter().take(limit.saturating_div(2).clamp(2, 6)).map(|(_, index)| index),
    );
    selected.extend(0..rows.len().min(2));
    if rows.len() > 4 {
        let middle = rows.len() / 2;
        selected.extend([middle.saturating_sub(1), middle]);
    }
    if rows.len() > 2 {
        selected.extend([rows.len() - 2, rows.len() - 1]);
    }
    selected
}

fn fill_source_coverage_indices(selected: &mut BTreeSet<usize>, row_count: usize, limit: usize) {
    while selected.len() < limit && selected.len() < row_count {
        let Some(index) = farthest_unselected_index(selected, row_count) else {
            break;
        };
        selected.insert(index);
    }
}

fn farthest_unselected_index(selected: &BTreeSet<usize>, row_count: usize) -> Option<usize> {
    (0..row_count)
        .filter(|candidate| !selected.contains(candidate))
        .map(|candidate| {
            let nearest = selected
                .iter()
                .map(|chosen| candidate.abs_diff(*chosen))
                .min()
                .unwrap_or(usize::MAX);
            (candidate, nearest)
        })
        .max_by(|left, right| left.1.cmp(&right.1).then_with(|| right.0.cmp(&left.0)))
        .map(|(candidate, _)| candidate)
}

fn source_coverage_focus_row_score(row: &KnowledgeChunkRow, plan_keywords: &[String]) -> usize {
    if plan_keywords.is_empty() {
        return 0;
    }
    let haystack = format!("{}\n{}", row.content_text, row.normalized_text).to_lowercase();
    let technical_score = technical_chunk_selection_score(&haystack, plan_keywords, false)
        .try_into()
        .unwrap_or(0_usize);
    technical_score
        + plan_keywords
            .iter()
            .map(|keyword| keyword.trim().to_lowercase())
            .filter(|keyword| keyword.chars().count() >= 2)
            .map(|keyword| {
                let exact = haystack.matches(keyword.as_str()).count();
                let stem = keyword.chars().take(5).collect::<String>();
                let shape_bonus = usize::from(
                    keyword.chars().any(|ch| ch == '_' || ch == '/' || ch.is_ascii_digit())
                        && haystack.contains(keyword.as_str()),
                ) * 32;
                shape_bonus
                    + exact.saturating_mul(4)
                    + usize::from(stem.chars().count() >= 4 && haystack.contains(stem.as_str()))
            })
            .sum::<usize>()
}

pub(super) fn is_source_profile_chunk(row: &KnowledgeChunkRow) -> bool {
    super::super::source_profile::is_source_profile_chunk_row(row)
}
