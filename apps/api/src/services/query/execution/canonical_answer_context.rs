use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::Context;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::query_ir::{QueryAct, QueryIR, QueryScope},
    infra::arangodb::document_store::{KnowledgeChunkRow, KnowledgeDocumentRow},
    services::query::effective_query::structured_current_question_segment,
    services::query::text_match::{near_token_match, normalized_alnum_tokens},
    shared::extraction::text_render::repair_technical_layout_noise,
};

use super::{
    CanonicalAnswerEvidence, RuntimeChunkScoreKind, RuntimeMatchedChunk,
    build_table_row_grounded_answer, canonical_document_revision_id,
    explicit_target_document_ids_from_values, focused_excerpt_for,
    load_initial_table_rows_for_documents, load_table_rows_for_documents,
    load_table_summary_chunks_for_documents, map_chunk_hit,
    merge_canonical_table_aggregation_chunks, merge_chunks, question_asks_table_aggregation,
    question_asks_table_value_inventory, render_canonical_chunk_section,
    render_canonical_technical_fact_section, render_prepared_segment_section,
    render_table_summary_chunk_section, requested_initial_table_row_count, score_desc_chunks,
    score_value, technical_literal_focus_keywords,
    technical_literals::technical_chunk_selection_score,
    technical_literals::{extract_explicit_path_literals, extract_parameter_literals},
};

const MAX_DIRECT_TABLE_ANALYTICS_ROWS: usize = 2_000;
const MAX_CANONICAL_ANSWER_TECHNICAL_FACTS: usize = 24;
const SOURCE_COVERAGE_DOCUMENT_LIMIT: usize = 3;
const SOURCE_COVERAGE_CHUNKS_PER_DOCUMENT: usize = 12;
const SOURCE_PROFILE_SCORE: f32 = 0.65;
const SOURCE_COVERAGE_SCORE_BASE: f32 = 0.95;
const SOURCE_COVERAGE_SCORE_STEP: f32 = 0.001;

pub(crate) async fn load_direct_targeted_table_answer(
    state: &AppState,
    question: &str,
    ir: Option<&crate::domains::query_ir::QueryIR>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> anyhow::Result<Option<String>> {
    let row_count = requested_initial_table_row_count(ir);
    let inventory_requested = question_asks_table_value_inventory(question, ir);
    if row_count.is_none() && !inventory_requested {
        return Ok(None);
    }
    let targeted_document_ids = explicit_target_document_ids_from_values(
        question,
        document_index.values().flat_map(|document| {
            [
                document.file_name.as_deref(),
                document.title.as_deref(),
                Some(document.external_key.as_str()),
            ]
            .into_iter()
            .flatten()
            .map(move |value| (document.document_id, value))
        }),
    );
    let Some(document_id) = targeted_document_ids.iter().next().copied() else {
        return Ok(None);
    };
    if targeted_document_ids.len() != 1 {
        return Ok(None);
    }
    let Some(document) = document_index.get(&document_id) else {
        return Ok(None);
    };
    let Some(revision_id) = document.readable_revision_id.or(document.active_revision_id) else {
        return Ok(None);
    };

    let plan_keywords = crate::services::query::planner::extract_keywords(question);
    let document_label = document
        .title
        .clone()
        .filter(|value: &String| !value.trim().is_empty())
        .or_else(|| document.file_name.clone())
        .unwrap_or_else(|| document.external_key.clone());
    let row_limit = row_count.unwrap_or(16);
    let initial_rows = state
        .arango_document_store
        .list_structured_blocks_by_revision(revision_id)
        .await
        .context("failed to load structured blocks for direct initial row answer")?
        .into_iter()
        .filter(|block| block.block_kind == "table_row")
        .take(row_limit)
        .enumerate()
        .map(|(ordinal, block)| RuntimeMatchedChunk {
            chunk_id: block.block_id,
            document_id,
            revision_id: block.revision_id,
            chunk_index: block.ordinal,
            chunk_kind: Some(block.block_kind.clone()),
            document_label: document_label.clone(),
            excerpt: focused_excerpt_for(&block.normalized_text, &plan_keywords, 280),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(10_000.0 - ordinal as f32),
            source_text: repair_technical_layout_noise(&block.normalized_text),
        })
        .collect::<Vec<_>>();
    if let Some(row_count) = row_count
        && initial_rows.len() < row_count
    {
        return Ok(None);
    }

    Ok(build_table_row_grounded_answer(question, ir, &initial_rows))
}

pub(crate) async fn load_canonical_answer_chunks(
    state: &AppState,
    execution_id: Uuid,
    question: &str,
    query_ir: &QueryIR,
    fallback_chunks: &[RuntimeMatchedChunk],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let document_values = document_index
        .values()
        .flat_map(|document| {
            [
                document.file_name.as_deref(),
                document.title.as_deref(),
                Some(document.external_key.as_str()),
            ]
            .into_iter()
            .flatten()
            .map(move |value| (document.document_id, value))
        })
        .collect::<Vec<_>>();
    let explicit_targeted_document_ids =
        explicit_target_document_ids_from_values(question, document_values.iter().copied());
    let focused_document_id = (explicit_targeted_document_ids.len() == 1)
        .then(|| explicit_targeted_document_ids.iter().next().copied())
        .flatten()
        .or_else(|| query_ir_canonical_context_document_id(query_ir, document_values));
    let aggregation_summary_chunks = if question_asks_table_aggregation(question, Some(query_ir))
        && let Some(document_id) = focused_document_id
    {
        let plan_keywords = crate::services::query::planner::extract_keywords(question);
        let targeted_document_ids = BTreeSet::from([document_id]);
        load_table_summary_chunks_for_documents(
            state,
            document_index,
            &targeted_document_ids,
            32,
            &plan_keywords,
        )
        .await
        .context("failed to load focused table summaries for canonical answer")?
    } else {
        Vec::new()
    };
    let aggregation_row_chunks = if question_asks_table_aggregation(question, Some(query_ir))
        && let Some(document_id) = focused_document_id
    {
        let plan_keywords = crate::services::query::planner::extract_keywords(question);
        let targeted_document_ids = BTreeSet::from([document_id]);
        load_table_rows_for_documents(
            state,
            document_index,
            &targeted_document_ids,
            MAX_DIRECT_TABLE_ANALYTICS_ROWS,
            &plan_keywords,
        )
        .await
        .context("failed to load focused table rows for canonical aggregate answer")?
    } else {
        Vec::new()
    };
    let explicit_initial_table_rows = if let Some(row_count) =
        requested_initial_table_row_count(Some(query_ir))
        && let Some(document_id) = focused_document_id
    {
        let plan_keywords = crate::services::query::planner::extract_keywords(question);
        let targeted_document_ids = BTreeSet::from([document_id]);
        let initial_rows = load_initial_table_rows_for_documents(
            state,
            document_index,
            &targeted_document_ids,
            row_count,
            &plan_keywords,
        )
        .await
        .context("failed to load direct initial table rows for canonical answer")?;
        (initial_rows.len() >= row_count).then_some(initial_rows)
    } else {
        None
    };
    if let Some(mut initial_rows) = explicit_initial_table_rows {
        if !aggregation_summary_chunks.is_empty() {
            let chunk_limit = initial_rows.len().saturating_add(32);
            initial_rows = merge_chunks(initial_rows, aggregation_summary_chunks, chunk_limit);
        }
        initial_rows.sort_by(score_desc_chunks);
        return Ok(initial_rows);
    }

    let Some(bundle_refs) = state
        .arango_context_store
        .get_bundle_reference_set_by_query_execution(execution_id)
        .await
        .with_context(|| {
            format!("failed to load context bundle for canonical answer chunks {execution_id}")
        })?
    else {
        if !aggregation_summary_chunks.is_empty() || !aggregation_row_chunks.is_empty() {
            let mut aggregate_chunks = merge_chunks(
                aggregation_summary_chunks,
                aggregation_row_chunks,
                MAX_DIRECT_TABLE_ANALYTICS_ROWS.saturating_add(32),
            );
            aggregate_chunks.sort_by(score_desc_chunks);
            return Ok(aggregate_chunks);
        }
        return augment_with_source_coverage_chunks(
            state,
            question,
            query_ir,
            focused_document_id,
            document_index,
            &crate::services::query::planner::extract_keywords(question),
            fallback_chunks.to_vec(),
        )
        .await;
    };
    let chunk_ids =
        bundle_refs.chunk_references.iter().map(|reference| reference.chunk_id).collect::<Vec<_>>();
    if chunk_ids.is_empty() {
        if !aggregation_summary_chunks.is_empty() || !aggregation_row_chunks.is_empty() {
            let mut aggregate_chunks = merge_chunks(
                aggregation_summary_chunks,
                aggregation_row_chunks,
                MAX_DIRECT_TABLE_ANALYTICS_ROWS.saturating_add(32),
            );
            aggregate_chunks.sort_by(score_desc_chunks);
            return Ok(aggregate_chunks);
        }
        return augment_with_source_coverage_chunks(
            state,
            question,
            query_ir,
            focused_document_id,
            document_index,
            &crate::services::query::planner::extract_keywords(question),
            fallback_chunks.to_vec(),
        )
        .await;
    }
    let plan_keywords = crate::services::query::planner::extract_keywords(question);
    let rows = state
        .arango_document_store
        .list_chunks_by_ids(&chunk_ids)
        .await
        .context("failed to load canonical answer chunks")?;
    let mut chunks: Vec<RuntimeMatchedChunk> = rows
        .into_iter()
        .filter_map(|chunk| map_chunk_hit(chunk, 1.0, document_index, &plan_keywords))
        .collect();
    merge_runtime_context_chunks(&mut chunks, fallback_chunks);
    if question_asks_table_aggregation(question, Some(query_ir))
        && let Some(document_id) = focused_document_id
    {
        chunks.retain(|chunk| chunk.document_id == document_id);
        chunks = merge_canonical_table_aggregation_chunks(
            chunks,
            aggregation_summary_chunks,
            aggregation_row_chunks,
            MAX_DIRECT_TABLE_ANALYTICS_ROWS.saturating_add(32),
        );
    }
    if chunks.is_empty() {
        if question_asks_table_aggregation(question, Some(query_ir))
            && focused_document_id.is_some()
        {
            return Ok(Vec::new());
        }
        return augment_with_source_coverage_chunks(
            state,
            question,
            query_ir,
            focused_document_id,
            document_index,
            &plan_keywords,
            fallback_chunks.to_vec(),
        )
        .await;
    }
    if let Some(row_count) = requested_initial_table_row_count(Some(query_ir))
        && let Some(document_id) = focused_document_id
    {
        let targeted_document_ids = BTreeSet::from([document_id]);
        let chunk_limit = chunks.len().max(row_count);
        let initial_rows = load_initial_table_rows_for_documents(
            state,
            document_index,
            &targeted_document_ids,
            row_count,
            &plan_keywords,
        )
        .await
        .context("failed to load focused initial table rows for canonical answer")?;
        chunks = merge_chunks(chunks, initial_rows, chunk_limit);
    }
    chunks = augment_with_source_coverage_chunks(
        state,
        question,
        query_ir,
        focused_document_id,
        document_index,
        &plan_keywords,
        chunks,
    )
    .await?;
    merge_runtime_context_chunks(&mut chunks, fallback_chunks);
    let image_revision_ids = load_image_revision_ids(state, &chunks).await.unwrap_or_default();
    deprioritize_image_source_chunks(&mut chunks, &image_revision_ids);
    chunks.sort_by(score_desc_chunks);
    Ok(chunks)
}

pub(crate) fn apply_runtime_chunk_overlays(
    chunks: &mut [RuntimeMatchedChunk],
    runtime_chunks: &[RuntimeMatchedChunk],
) {
    let runtime_by_chunk_id =
        runtime_chunks.iter().map(|chunk| (chunk.chunk_id, chunk)).collect::<HashMap<_, _>>();
    for chunk in chunks {
        let Some(runtime_chunk) = runtime_by_chunk_id.get(&chunk.chunk_id) else {
            continue;
        };
        if runtime_chunk.score_kind == RuntimeChunkScoreKind::GraphEvidence {
            chunk.score_kind = RuntimeChunkScoreKind::GraphEvidence;
            if runtime_chunk.score.is_some() {
                chunk.score = runtime_chunk.score;
            }
            if !runtime_chunk.source_text.trim().is_empty() {
                chunk.source_text = runtime_chunk.source_text.clone();
            }
            if !runtime_chunk.excerpt.trim().is_empty() {
                chunk.excerpt = runtime_chunk.excerpt.clone();
            }
            continue;
        }
        if runtime_chunk.score_kind != RuntimeChunkScoreKind::Relevance {
            chunk.score_kind = runtime_chunk.score_kind;
        }
        if runtime_chunk.score.is_some() {
            chunk.score = runtime_chunk.score;
        }
        if runtime_chunk.source_text.trim().chars().count()
            > chunk.source_text.trim().chars().count()
        {
            chunk.source_text = runtime_chunk.source_text.clone();
        }
        if runtime_chunk.excerpt.trim().chars().count() > chunk.excerpt.trim().chars().count()
            || runtime_chunk.score_kind != RuntimeChunkScoreKind::Relevance
        {
            chunk.excerpt = runtime_chunk.excerpt.clone();
        }
    }
}

pub(crate) fn merge_runtime_context_chunks(
    chunks: &mut Vec<RuntimeMatchedChunk>,
    runtime_chunks: &[RuntimeMatchedChunk],
) {
    apply_runtime_chunk_overlays(chunks, runtime_chunks);
    let mut seen_chunk_ids = chunks.iter().map(|chunk| chunk.chunk_id).collect::<HashSet<_>>();
    for runtime_chunk in runtime_chunks {
        if !matches!(
            runtime_chunk.score_kind,
            RuntimeChunkScoreKind::GraphEvidence
                | RuntimeChunkScoreKind::FocusedDocument
                | RuntimeChunkScoreKind::LatestVersion
                | RuntimeChunkScoreKind::SourceContext
        ) || !seen_chunk_ids.insert(runtime_chunk.chunk_id)
        {
            continue;
        }
        chunks.push(runtime_chunk.clone());
    }
}

async fn augment_with_source_coverage_chunks(
    state: &AppState,
    question: &str,
    query_ir: &QueryIR,
    focused_document_id: Option<Uuid>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    mut chunks: Vec<RuntimeMatchedChunk>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if !should_request_source_coverage_chunks(question, query_ir) {
        tracing::debug!(
            stage = "source_coverage_skip",
            reason = "predicate_false",
            ?query_ir.act,
            "source coverage augmentation skipped"
        );
        return Ok(chunks);
    }
    let initial_chunk_count = chunks.len();
    let mut document_ids = Vec::<Uuid>::new();
    let mut seen_document_ids = HashSet::<Uuid>::new();
    if let Some(document_id) = focused_document_id
        && seen_document_ids.insert(document_id)
    {
        document_ids.push(document_id);
    }
    let document_limit = source_coverage_document_limit(query_ir);
    for chunk in &chunks {
        if document_ids.len() >= document_limit {
            break;
        }
        if seen_document_ids.insert(chunk.document_id) {
            document_ids.push(chunk.document_id);
        }
    }
    if document_ids.is_empty() {
        return Ok(chunks);
    }

    let mut seen_chunk_ids = chunks.iter().map(|chunk| chunk.chunk_id).collect::<HashSet<_>>();
    let mut coverage_rank = 0_usize;
    for document_id in document_ids {
        let Some(document) = document_index.get(&document_id) else {
            continue;
        };
        let Some(revision_id) = canonical_document_revision_id(document) else {
            continue;
        };
        let rows =
            state.arango_document_store.list_chunks_by_revision(revision_id).await.with_context(
                || {
                    format!(
                        "failed to load source coverage chunks for document {} revision {}",
                        document_id, revision_id
                    )
                },
            )?;
        for row in select_source_coverage_chunk_rows(
            rows,
            SOURCE_COVERAGE_CHUNKS_PER_DOCUMENT,
            plan_keywords,
        ) {
            if !seen_chunk_ids.insert(row.chunk_id) {
                continue;
            }
            let is_source_profile = is_source_profile_chunk(&row);
            let score = if is_source_profile {
                SOURCE_PROFILE_SCORE
            } else {
                SOURCE_COVERAGE_SCORE_BASE - coverage_rank as f32 * SOURCE_COVERAGE_SCORE_STEP
            };
            coverage_rank = coverage_rank.saturating_add(1);
            if let Some(mut chunk) = map_chunk_hit(row, score, document_index, plan_keywords) {
                chunk.score = Some(score);
                chunks.push(chunk);
            }
        }
    }
    tracing::info!(
        stage = "source_coverage_augmented",
        initial_chunk_count,
        final_chunk_count = chunks.len(),
        added_chunk_count = chunks.len().saturating_sub(initial_chunk_count),
        focused_document_id = ?focused_document_id,
        coverage_rank,
        "source coverage augmentation finished"
    );
    Ok(chunks)
}

fn source_coverage_document_limit(query_ir: &QueryIR) -> usize {
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

fn should_request_source_coverage_chunks(_question: &str, query_ir: &QueryIR) -> bool {
    query_ir.requests_source_coverage_context()
        || query_ir.is_exact_literal_technical()
        || query_ir_requests_setup_source_coverage(query_ir)
        || query_ir_requests_inventory_source_coverage(query_ir)
}

#[cfg(test)]
mod runtime_context_merge_tests {
    use super::*;

    #[test]
    fn merge_runtime_context_chunks_keeps_focused_document_refs() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let chunk_id = Uuid::now_v7();
        let runtime_chunk = RuntimeMatchedChunk {
            chunk_id,
            document_id,
            revision_id,
            chunk_index: 0,
            chunk_kind: Some("code_block".to_string()),
            document_label: "focused.yaml".to_string(),
            excerpt: "focused evidence".to_string(),
            score_kind: RuntimeChunkScoreKind::FocusedDocument,
            score: Some(10.0),
            source_text: "focused evidence".to_string(),
        };

        let mut chunks = Vec::new();
        merge_runtime_context_chunks(&mut chunks, &[runtime_chunk]);

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_id, chunk_id);
        assert_eq!(chunks[0].score_kind, RuntimeChunkScoreKind::FocusedDocument);
    }
}

fn query_ir_requests_setup_source_coverage(query_ir: &QueryIR) -> bool {
    let act_signals_setup = matches!(
        query_ir.act,
        crate::domains::query_ir::QueryAct::ConfigureHow
            | crate::domains::query_ir::QueryAct::Describe
            | crate::domains::query_ir::QueryAct::RetrieveValue
    );
    if !act_signals_setup {
        return false;
    }

    let has_config_target = query_ir.target_types.iter().any(|target_type| {
        matches!(
            target_type.trim().to_ascii_lowercase().as_str(),
            "configuration_file" | "config_key"
        )
    });
    let has_package_or_parameter_target = query_ir.target_types.iter().any(|target_type| {
        matches!(target_type.trim().to_ascii_lowercase().as_str(), "package" | "parameter")
    });

    // Original gate: configuration_file/config_key + package/parameter
    // both present (matches a high-confidence "configure this parameter
    // inside this config file" intent).
    if has_config_target && has_package_or_parameter_target {
        return true;
    }

    query_ir.document_focus.is_some()
}

fn query_ir_requests_inventory_source_coverage(query_ir: &QueryIR) -> bool {
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

fn select_source_coverage_chunk_rows(
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

    let mut selected = BTreeSet::<usize>::new();
    for (index, row) in rows.iter().enumerate() {
        if is_source_profile_chunk(row) {
            selected.insert(index);
        }
    }
    let mut focus_rows = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {
            let score = source_coverage_focus_row_score(row, plan_keywords);
            (score > 0).then_some((score, index))
        })
        .collect::<Vec<_>>();
    focus_rows.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    for (_, index) in focus_rows.into_iter().take(limit.saturating_div(2).clamp(2, 6)) {
        selected.insert(index);
    }
    for index in 0..rows.len().min(2) {
        selected.insert(index);
    }
    if rows.len() > 4 {
        let middle = rows.len() / 2;
        selected.insert(middle.saturating_sub(1));
        selected.insert(middle);
    }
    if rows.len() > 2 {
        selected.insert(rows.len() - 2);
        selected.insert(rows.len() - 1);
    }
    // Greedy max-min coverage fill: after the forced anchors (head,
    // middle, tail), the remaining slots used to come from a fixed
    // `slot * (rows.len()-1) / (limit-1)` stride. That stride enumerated
    // candidate indices from 0 upward and stopped on the first
    // `selected.len() >= limit`, which on long documents produced a
    // tight cluster of head-side indices and an arbitrary gap somewhere
    // in the middle or tail.
    //
    // Worked example for a 27-chunk source document at limit=12:
    // forced anchors select indices {0, 1, 12, 13, 25, 26}; the old
    // stride then added {2, 4, 7, 9, 11, 14} and stopped, leaving
    // indices 15-24 entirely unrepresented. For configuration-style
    // documents whose canonical content sits in that range, that
    // gap pushed the answering model into honest-but-incomplete
    // responses.
    //
    // The greedy fill below picks the unused index that is farthest
    // from any already-selected index, breaking ties toward the smaller
    // index. Each addition shrinks the largest remaining gap, so the
    // final selection covers the document's full index range — the
    // canonical "maximum dispersion" sampler. Pure data-driven, no
    // language or dataset assumptions; deterministic for unit tests.
    while selected.len() < limit && selected.len() < rows.len() {
        let mut best_index: Option<usize> = None;
        let mut best_distance: usize = 0;
        for candidate in 0..rows.len() {
            if selected.contains(&candidate) {
                continue;
            }
            let nearest = selected
                .iter()
                .map(|chosen| candidate.abs_diff(*chosen))
                .min()
                .unwrap_or(usize::MAX);
            if nearest > best_distance || best_index.is_none() {
                best_distance = nearest;
                best_index = Some(candidate);
            }
        }
        match best_index {
            Some(index) => {
                selected.insert(index);
            }
            None => break,
        }
    }

    selected.into_iter().take(limit).filter_map(|index| rows.get(index).cloned()).collect()
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

fn is_source_profile_chunk(row: &KnowledgeChunkRow) -> bool {
    super::source_profile::is_source_profile_chunk_row(row)
}

pub(crate) async fn load_canonical_answer_evidence(
    state: &AppState,
    execution_id: Uuid,
) -> anyhow::Result<CanonicalAnswerEvidence> {
    let Some(bundle_refs) = state
        .arango_context_store
        .get_bundle_reference_set_by_query_execution(execution_id)
        .await
        .with_context(|| {
            format!("failed to load context bundle for answer evidence {execution_id}")
        })?
    else {
        return Ok(CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        });
    };

    let chunk_ids =
        bundle_refs.chunk_references.iter().map(|reference| reference.chunk_id).collect::<Vec<_>>();
    let evidence_rows = state
        .arango_graph_store
        .list_evidence_by_ids(
            &bundle_refs
                .evidence_references
                .iter()
                .map(|reference| reference.evidence_id)
                .collect::<Vec<_>>(),
        )
        .await
        .context("failed to load evidence rows for canonical answer context")?;
    let chunk_rows = state
        .arango_document_store
        .list_chunks_by_ids(&chunk_ids)
        .await
        .context("failed to load chunks for canonical answer context")?;
    let chunk_supported_facts =
        state.arango_document_store.list_technical_facts_by_chunk_ids(&chunk_ids).await.context(
            "failed to load chunk-supported technical facts for canonical answer context",
        )?;
    let mut fact_ids = selected_fact_ids_for_canonical_evidence(
        &bundle_refs.bundle.selected_fact_ids,
        &evidence_rows,
        &chunk_supported_facts,
    );
    for evidence in &evidence_rows {
        if let Some(fact_id) = evidence.fact_id
            && !fact_ids.contains(&fact_id)
            && fact_ids.len() < MAX_CANONICAL_ANSWER_TECHNICAL_FACTS
        {
            fact_ids.push(fact_id);
        }
    }
    let mut technical_facts = state
        .arango_document_store
        .list_technical_facts_by_ids(&fact_ids)
        .await
        .context("failed to load technical facts for canonical answer context")?;
    let mut seen_fact_ids = technical_facts.iter().map(|fact| fact.fact_id).collect::<HashSet<_>>();
    for fact in chunk_supported_facts {
        if fact_ids.contains(&fact.fact_id) && seen_fact_ids.insert(fact.fact_id) {
            technical_facts.push(fact);
        }
    }
    technical_facts.sort_by(|left, right| {
        left.fact_kind.cmp(&right.fact_kind).then_with(|| left.fact_id.cmp(&right.fact_id))
    });
    let mut block_ids =
        evidence_rows.iter().filter_map(|evidence| evidence.block_id).collect::<Vec<_>>();
    for chunk in &chunk_rows {
        for block_id in &chunk.support_block_ids {
            if !block_ids.contains(block_id) {
                block_ids.push(*block_id);
            }
        }
    }
    for fact in &technical_facts {
        for block_id in &fact.support_block_ids {
            if !block_ids.contains(block_id) {
                block_ids.push(*block_id);
            }
        }
    }
    let structured_blocks = state
        .arango_document_store
        .list_structured_blocks_by_ids(&block_ids)
        .await
        .context("failed to load structured blocks for canonical answer context")?;
    Ok(CanonicalAnswerEvidence {
        bundle: Some(bundle_refs.bundle),
        chunk_rows,
        structured_blocks,
        technical_facts,
    })
}

pub(crate) fn selected_fact_ids_for_canonical_evidence(
    selected_fact_ids: &[Uuid],
    evidence_rows: &[crate::infra::arangodb::graph_store::KnowledgeEvidenceRow],
    chunk_supported_facts: &[crate::infra::arangodb::document_store::KnowledgeTechnicalFactRow],
) -> Vec<Uuid> {
    let mut fact_ids = selected_fact_ids.to_vec();
    for evidence in evidence_rows {
        let Some(fact_id) = evidence.fact_id else {
            continue;
        };
        if fact_ids.len() >= MAX_CANONICAL_ANSWER_TECHNICAL_FACTS {
            break;
        }
        if !fact_ids.contains(&fact_id) {
            fact_ids.push(fact_id);
        }
    }
    if fact_ids.is_empty() {
        for fact in chunk_supported_facts {
            if fact_ids.len() >= MAX_CANONICAL_ANSWER_TECHNICAL_FACTS {
                break;
            }
            if !fact_ids.contains(&fact.fact_id) {
                fact_ids.push(fact.fact_id);
            }
        }
    }
    fact_ids.truncate(MAX_CANONICAL_ANSWER_TECHNICAL_FACTS);
    fact_ids
}

pub(crate) fn build_canonical_answer_context(
    question: &str,
    query_ir: &crate::domains::query_ir::QueryIR,
    technical_literals_text: Option<&str>,
    evidence: &CanonicalAnswerEvidence,
    canonical_answer_chunks: &[RuntimeMatchedChunk],
    graph_evidence_context_lines: &[String],
) -> String {
    let focused_document_id =
        canonical_context_document_id(question, query_ir, canonical_answer_chunks);
    let focused_document_label = focused_document_id.and_then(|document_id| {
        canonical_answer_chunks
            .iter()
            .find(|chunk| chunk.document_id == document_id)
            .map(|chunk| chunk.document_label.clone())
    });
    let filtered_technical_facts = focused_document_id.map_or_else(
        || evidence.technical_facts.clone(),
        |document_id| {
            evidence
                .technical_facts
                .iter()
                .filter(|fact| fact.document_id == document_id)
                .cloned()
                .collect::<Vec<_>>()
        },
    );
    let filtered_structured_blocks = focused_document_id.map_or_else(
        || evidence.structured_blocks.clone(),
        |document_id| {
            evidence
                .structured_blocks
                .iter()
                .filter(|block| block.document_id == document_id)
                .cloned()
                .collect::<Vec<_>>()
        },
    );
    let filtered_chunks = focused_document_id.map_or_else(
        || canonical_answer_chunks.to_vec(),
        |document_id| {
            canonical_answer_chunks
                .iter()
                .filter(|chunk| chunk.document_id == document_id)
                .cloned()
                .collect::<Vec<_>>()
        },
    );
    let mut sections = Vec::<String>::new();

    if let Some(technical_literals_text) = technical_literals_text
        && !technical_literals_text.trim().is_empty()
    {
        sections.push(technical_literals_text.trim().to_string());
    }

    if let Some(document_label) = focused_document_label.as_deref() {
        sections.push(format!("Focused grounded document\n- {document_label}"));
        sections.push(
            "When a document summary is available in the context, use it to frame the answer."
                .to_string(),
        );
    }

    let graph_evidence_section = render_graph_evidence_context_lines_for_focus(
        question,
        graph_evidence_context_lines,
        focused_document_label.as_deref(),
        query_ir,
    );
    if !graph_evidence_section.is_empty() {
        sections.push(graph_evidence_section);
    }

    let table_summary_section =
        render_table_summary_chunk_section(question, Some(query_ir), &filtered_chunks);
    let suppress_tabular_detail = question_asks_table_aggregation(question, Some(query_ir))
        && !table_summary_section.is_empty();
    if !table_summary_section.is_empty() {
        sections.push(table_summary_section);
    }

    if !suppress_tabular_detail {
        let technical_fact_section =
            render_canonical_technical_fact_section(&filtered_technical_facts);
        if !technical_fact_section.is_empty() {
            sections.push(technical_fact_section);
        }
    }

    let prepared_segment_section = render_prepared_segment_section(
        question,
        Some(query_ir),
        &filtered_structured_blocks,
        suppress_tabular_detail,
    );
    if !prepared_segment_section.is_empty() {
        sections.push(prepared_segment_section);
    }

    let chunk_section = render_canonical_chunk_section(
        question,
        query_ir,
        &filtered_chunks,
        suppress_tabular_detail,
    );
    if !chunk_section.is_empty() {
        sections.push(chunk_section);
    }

    if let Some(bundle) = evidence.bundle.as_ref() {
        sections.insert(
            0,
            format!(
                "Canonical query bundle\n- Strategy: {}\n- Requested mode: {}\n- Resolved mode: {}",
                bundle.bundle_strategy, bundle.requested_mode, bundle.resolved_mode
            ),
        );
    }

    sections.join("\n\n")
}

fn render_graph_evidence_context_lines(graph_evidence_context_lines: &[String]) -> String {
    let mut lines = Vec::<String>::new();
    let mut seen = BTreeSet::<String>::new();
    for line in graph_evidence_context_lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.to_string()) {
            lines.push(trimmed.to_string());
        }
    }
    if lines.is_empty() {
        String::new()
    } else {
        format!("Retrieved graph evidence\n{}", lines.join("\n"))
    }
}

fn render_graph_evidence_context_lines_for_focus(
    question: &str,
    graph_evidence_context_lines: &[String],
    focused_document_label: Option<&str>,
    query_ir: &QueryIR,
) -> String {
    let Some(focused_document_label) = focused_document_label else {
        if low_confidence_short_technical_context(query_ir, question) {
            let short_keywords = short_technical_focus_keywords(question, query_ir);
            let focused_lines = graph_evidence_context_lines
                .iter()
                .filter(|line| line_contains_any_focus_keyword(line, &short_keywords))
                .cloned()
                .collect::<Vec<_>>();
            return render_graph_evidence_context_lines(&focused_lines);
        }
        return render_graph_evidence_context_lines(graph_evidence_context_lines);
    };
    if !matches!(query_ir.scope, QueryScope::SingleDocument) {
        return render_graph_evidence_context_lines(graph_evidence_context_lines);
    }
    let focus_tokens = query_ir_document_focus_tokens(query_ir)
        .unwrap_or_else(|| normalized_alnum_tokens(focused_document_label, 3));
    if focus_tokens.is_empty() {
        return render_graph_evidence_context_lines(graph_evidence_context_lines);
    }
    let focused_lines = graph_evidence_context_lines
        .iter()
        .filter(|line| {
            let line_tokens = normalized_alnum_tokens(line, 3);
            focus_token_overlap_count(&focus_tokens, &line_tokens) > 0
        })
        .cloned()
        .collect::<Vec<_>>();
    render_graph_evidence_context_lines(&focused_lines)
}

fn low_confidence_short_technical_context(query_ir: &QueryIR, question: &str) -> bool {
    query_ir.confidence <= 0.3
        && matches!(query_ir.scope, QueryScope::SingleDocument)
        && matches!(query_ir.act, QueryAct::Describe | QueryAct::ConfigureHow)
        && query_ir.source_slice.is_none()
        && query_ir.document_focus.is_none()
        && query_ir.target_types.is_empty()
        && query_ir.target_entities.is_empty()
        && query_ir.literal_constraints.is_empty()
        && !short_technical_focus_keywords(question, query_ir).is_empty()
}

fn short_technical_focus_keywords(question: &str, query_ir: &QueryIR) -> Vec<String> {
    technical_literal_focus_keywords(question, Some(query_ir))
        .into_iter()
        .filter(|keyword| keyword.chars().count() < 4)
        .collect()
}

fn line_contains_any_focus_keyword(line: &str, keywords: &[String]) -> bool {
    if keywords.is_empty() {
        return false;
    }
    let lower = line.to_lowercase();
    keywords.iter().any(|keyword| lower.contains(keyword))
}

fn canonical_context_document_id(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<Uuid> {
    if contextual_low_confidence_setup_context_should_stay_broad(question, query_ir, chunks) {
        return None;
    }
    let document_ids = explicit_target_document_ids_from_values(
        question,
        chunks.iter().map(|chunk| (chunk.document_id, chunk.document_label.as_str())),
    );
    (document_ids.len() == 1).then(|| document_ids.iter().next().copied()).flatten().or_else(|| {
        query_ir_canonical_context_document_id(
            query_ir,
            chunks.iter().map(|chunk| (chunk.document_id, chunk.document_label.as_str())),
        )
        .or_else(|| dominant_single_document_context_id(query_ir, chunks))
    })
}

fn dominant_single_document_context_id(
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<Uuid> {
    if !matches!(query_ir.scope, QueryScope::SingleDocument) || chunks.is_empty() {
        return None;
    }
    if !dominant_single_document_context_allowed(query_ir, chunks) {
        return None;
    }
    let mut by_document = HashMap::<Uuid, (f32, usize)>::new();
    for chunk in chunks {
        let score = score_value(chunk.score).max(0.0);
        by_document
            .entry(chunk.document_id)
            .and_modify(|entry| {
                entry.0 += score;
                entry.1 = entry.1.saturating_add(1);
            })
            .or_insert((score, 1));
    }
    let mut ranked = by_document.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .1
            .0
            .total_cmp(&left.1.0)
            .then_with(|| right.1.1.cmp(&left.1.1))
            .then_with(|| left.0.cmp(&right.0))
    });
    let (best_document_id, (best_score, best_count)) = ranked.first().copied()?;
    let runner_score = ranked.get(1).map(|(_, (score, _))| *score).unwrap_or(0.0);
    (best_count >= 2 && best_score >= runner_score.mul_add(1.15, 1.0)).then_some(best_document_id)
}

fn dominant_single_document_context_allowed(
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> bool {
    if query_ir.document_focus.is_some() {
        return true;
    }
    let document_count = chunks.iter().map(|chunk| chunk.document_id).collect::<HashSet<_>>().len();
    if document_count <= 1 {
        return true;
    }
    if matches!(query_ir.act, QueryAct::RetrieveValue | QueryAct::Enumerate | QueryAct::Compare) {
        return false;
    }
    let target_types = query_ir
        .target_types
        .iter()
        .map(|target_type| {
            crate::services::query::execution::question_intent::canonical_target_type_tag(
                target_type,
            )
        })
        .collect::<HashSet<_>>();
    !target_types.contains("table_row") && !target_types.contains("table_summary")
}

fn contextual_low_confidence_setup_context_should_stay_broad(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> bool {
    structured_current_question_segment(question).is_some()
        && low_confidence_unfocused_descriptive_query(query_ir)
        && setup_like_document_count(chunks) > 1
}

fn low_confidence_unfocused_descriptive_query(query_ir: &QueryIR) -> bool {
    query_ir.confidence <= 0.3
        && matches!(query_ir.act, QueryAct::Describe | QueryAct::ConfigureHow)
        && query_ir.source_slice.is_none()
        && query_ir.document_focus.is_none()
        && query_ir.target_types.is_empty()
        && query_ir.target_entities.is_empty()
        && query_ir.literal_constraints.is_empty()
}

fn setup_like_document_count(chunks: &[RuntimeMatchedChunk]) -> usize {
    chunks
        .iter()
        .filter(|chunk| {
            setup_like_text_signal(&chunk.excerpt) || setup_like_text_signal(&chunk.source_text)
        })
        .map(|chunk| chunk.document_id)
        .collect::<HashSet<_>>()
        .len()
}

fn setup_like_text_signal(text: &str) -> bool {
    setup_like_configuration_path_count(text) > 0
        || setup_like_assignment_count(text) > 0
        || setup_like_section_count(text) > 0
        || extract_parameter_literals(text, 8).len() >= 2
}

fn setup_like_configuration_path_count(text: &str) -> usize {
    extract_explicit_path_literals(text, 8)
        .into_iter()
        .filter(|path| {
            let lowered = path.to_ascii_lowercase();
            [".conf", ".ini", ".cfg", ".properties", ".yaml", ".yml", ".toml", ".json"]
                .iter()
                .any(|extension| lowered.ends_with(extension))
        })
        .count()
}

fn setup_like_assignment_count(text: &str) -> usize {
    text.split_whitespace()
        .filter(|token| {
            let Some((name, _)) = token.split_once('=') else {
                return false;
            };
            let name = name.trim_matches(|ch: char| {
                matches!(ch, '`' | '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}')
            });
            let Some(first) = name.chars().next() else {
                return false;
            };
            first.is_ascii_alphabetic()
                && name.chars().any(|ch| ch.is_ascii_alphabetic())
                && name
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        })
        .take(8)
        .count()
}

fn setup_like_section_count(text: &str) -> usize {
    text.split_whitespace()
        .filter(|token| {
            let cleaned = token.trim_matches(|ch: char| {
                matches!(ch, '`' | '"' | '\'' | ',' | ';' | ':' | '.' | '(' | ')' | '{' | '}')
            });
            cleaned.len() > 2 && cleaned.starts_with('[') && cleaned.ends_with(']')
        })
        .take(8)
        .count()
}

fn query_ir_canonical_context_document_id<'a, I>(
    query_ir: &QueryIR,
    document_values: I,
) -> Option<Uuid>
where
    I: IntoIterator<Item = (Uuid, &'a str)>,
{
    if !matches!(query_ir.scope, QueryScope::SingleDocument) || query_ir.document_focus.is_none() {
        return None;
    }
    let focus_tokens = query_ir_document_focus_tokens(query_ir)?;

    let mut best_scores = HashMap::<Uuid, usize>::new();
    for (document_id, value) in document_values {
        let value_tokens = normalized_alnum_tokens(value, 3);
        let overlap = focus_token_overlap_count(&focus_tokens, &value_tokens);
        if overlap == 0 {
            continue;
        }
        best_scores
            .entry(document_id)
            .and_modify(|score| *score = (*score).max(overlap))
            .or_insert(overlap);
    }
    let max_score = best_scores.values().copied().max().unwrap_or_default();
    if max_score == 0 {
        return None;
    }
    let best_document_ids = best_scores
        .into_iter()
        .filter_map(|(document_id, score)| (score == max_score).then_some(document_id))
        .collect::<BTreeSet<_>>();
    (best_document_ids.len() == 1).then(|| best_document_ids.iter().next().copied()).flatten()
}

pub(crate) fn query_ir_document_focus_tokens(query_ir: &QueryIR) -> Option<BTreeSet<String>> {
    let tokens = query_ir
        .document_focus
        .as_ref()
        .map(|document_focus| normalized_alnum_tokens(document_focus.hint.trim(), 3))
        .unwrap_or_default();
    (!tokens.is_empty()).then_some(tokens)
}

pub(crate) fn focus_token_overlap_count(
    focus_tokens: &BTreeSet<String>,
    value_tokens: &BTreeSet<String>,
) -> usize {
    focus_tokens
        .iter()
        .filter(|focus_token| {
            value_tokens.iter().any(|value_token| {
                focus_token == &value_token
                    || near_token_match(focus_token, value_token)
                    || focus_token_is_value_prefix(focus_token, value_token)
            })
        })
        .count()
}

fn focus_token_is_value_prefix(focus_token: &str, value_token: &str) -> bool {
    let focus_len = focus_token.chars().count();
    let value_len = value_token.chars().count();
    focus_len >= 3 && value_len > focus_len && value_token.starts_with(focus_token)
}

/// Collect the set of revision_ids that belong to image-source documents
/// (`source_format == "image"`) by batch-fetching the structured revision
/// records for every distinct revision referenced by the given chunks.
async fn load_image_revision_ids(
    state: &AppState,
    chunks: &[RuntimeMatchedChunk],
) -> anyhow::Result<HashSet<Uuid>> {
    let revision_ids: Vec<Uuid> = {
        let mut seen = HashSet::new();
        chunks.iter().filter(|c| seen.insert(c.revision_id)).map(|c| c.revision_id).collect()
    };
    if revision_ids.is_empty() {
        return Ok(HashSet::new());
    }
    let revisions = state
        .arango_document_store
        .list_structured_revisions_by_revision_ids(&revision_ids)
        .await
        .context("failed to load structured revisions for image deprioritization")?;
    Ok(revisions
        .into_iter()
        .filter(|r| r.source_format == "image")
        .map(|r| r.revision_id)
        .collect())
}

/// Push chunks whose revision is an image-source document to the tail of the
/// list, preserving relative order within each group.  If every chunk in the
/// list is an image-source chunk (e.g. an image-only document query), the
/// order is left unchanged so image OCR stubs still surface.
pub(crate) fn deprioritize_image_source_chunks(
    chunks: &mut [RuntimeMatchedChunk],
    image_revision_ids: &HashSet<Uuid>,
) {
    if image_revision_ids.is_empty() {
        return;
    }
    let all_image = chunks.iter().all(|c| image_revision_ids.contains(&c.revision_id));
    if all_image {
        return;
    }
    // stable_partition: non-image first, image last
    chunks.sort_by_key(|c| image_revision_ids.contains(&c.revision_id));
}

#[cfg(test)]
mod source_coverage_tests {
    use super::*;
    use crate::domains::query_ir::{
        EntityMention, EntityRole, LiteralKind, LiteralSpan, QueryAct, QueryIR, QueryLanguage,
        QueryScope,
    };

    fn chunk_row(chunk_index: i32, text: &str) -> KnowledgeChunkRow {
        let chunk_id = Uuid::now_v7();
        KnowledgeChunkRow {
            key: chunk_id.to_string(),
            arango_id: None,
            arango_rev: None,
            chunk_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index,
            chunk_kind: if text.contains("[source_profile ") {
                Some("source_profile".to_string())
            } else {
                Some("paragraph".to_string())
            },
            content_text: text.to_string(),
            normalized_text: text.to_string(),
            span_start: None,
            span_end: None,
            token_count: None,
            support_block_ids: Vec::new(),
            section_path: Vec::new(),
            heading_trail: Vec::new(),
            literal_digest: None,
            chunk_state: "ready".to_string(),
            text_generation: Some(1),
            vector_generation: Some(1),
            quality_score: Some(1.0),
            window_text: None,
            raptor_level: None,
            occurred_at: None,
            occurred_until: None,
        }
    }

    fn exact_literal_query_ir() -> QueryIR {
        QueryIR {
            act: QueryAct::RetrieveValue,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec!["config_key".to_string()],
            target_entities: Vec::new(),
            literal_constraints: vec![LiteralSpan {
                text: "route_map".to_string(),
                kind: LiteralKind::Identifier,
            }],
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence: 1.0,
        }
    }

    fn low_confidence_short_token_ir() -> QueryIR {
        QueryIR {
            act: QueryAct::Describe,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: Vec::new(),
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence: 0.25,
        }
    }

    #[test]
    fn low_confidence_short_token_graph_evidence_keeps_only_focused_lines() {
        let lines = vec![
            "[graph-evidence target=\"QX\"] QX alphaFlag = true".to_string(),
            "[graph-evidence target=\"ZZ\"] ZZ betaFlag = true".to_string(),
        ];

        let rendered = render_graph_evidence_context_lines_for_focus(
            "QX settings",
            &lines,
            None,
            &low_confidence_short_token_ir(),
        );

        assert!(rendered.contains("QX alphaFlag"));
        assert!(!rendered.contains("ZZ betaFlag"));
    }

    #[test]
    fn contextual_low_confidence_setup_context_keeps_related_parameter_chunks() {
        let setup_document_id = Uuid::now_v7();
        let parameter_document_id = Uuid::now_v7();
        let setup_revision_id = Uuid::now_v7();
        let parameter_revision_id = Uuid::now_v7();
        let question = "scope: Provider Alpha setup was selected earlier\nliteral anchors: `https://alpha.local/api`\nquestion: provider_alpha_setup.md Provider Alpha module configuration all parameters url";
        let chunks = vec![
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: setup_revision_id,
                chunk_index: 0,
                chunk_kind: None,
                document_id: setup_document_id,
                document_label: "provider_alpha_setup.md".to_string(),
                excerpt: "Install alpha-module and edit /opt/alpha/alpha.conf.".to_string(),
                score_kind: RuntimeChunkScoreKind::Relevance,
                score: Some(0.96),
                source_text:
                    "Install alpha-module. Edit /opt/alpha/alpha.conf in [Main]. url = https://alpha.local/api."
                        .to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                revision_id: parameter_revision_id,
                chunk_index: 0,
                chunk_kind: None,
                document_id: parameter_document_id,
                document_label: "provider_alpha_change_notes.md".to_string(),
                excerpt: "Provider Alpha parameter table.".to_string(),
                score_kind: RuntimeChunkScoreKind::Relevance,
                score: Some(0.95),
                source_text:
                    "| alphaPrintSlip | boolean | true false | Print the slip | | alphaFillDetails | boolean | true false | Fill detail fields |"
                        .to_string(),
            },
        ];
        let context = build_canonical_answer_context(
            question,
            &low_confidence_short_token_ir(),
            None,
            &CanonicalAnswerEvidence {
                bundle: None,
                chunk_rows: Vec::new(),
                structured_blocks: Vec::new(),
                technical_facts: Vec::new(),
            },
            &chunks,
            &[],
        );

        assert!(context.contains("/opt/alpha/alpha.conf"), "{context}");
        assert!(context.contains("alphaPrintSlip"), "{context}");
        assert!(context.contains("alphaFillDetails"), "{context}");
    }

    #[test]
    fn source_coverage_is_enabled_for_exact_literal_queries() {
        assert!(should_request_source_coverage_chunks("route_map", &exact_literal_query_ir()));
        assert!(should_request_source_coverage_chunks(
            "configure package",
            &QueryIR {
                act: QueryAct::ConfigureHow,
                scope: QueryScope::SingleDocument,
                language: QueryLanguage::Auto,
                target_types: vec![
                    "package".to_string(),
                    "configuration_file".to_string(),
                    "config_key".to_string(),
                ],
                target_entities: Vec::new(),
                literal_constraints: Vec::new(),
                temporal_constraints: Vec::new(),
                comparison: None,
                document_focus: None,
                conversation_refs: Vec::new(),
                needs_clarification: None,
                source_slice: None,
                retrieval_query: None,
                confidence: 1.0,
            }
        ));
        assert!(!should_request_source_coverage_chunks(
            "how to update Sample Target",
            &QueryIR {
                act: QueryAct::ConfigureHow,
                scope: QueryScope::SingleDocument,
                language: QueryLanguage::Auto,
                target_types: vec!["procedure".to_string(), "concept".to_string()],
                target_entities: Vec::new(),
                literal_constraints: Vec::new(),
                temporal_constraints: Vec::new(),
                comparison: None,
                document_focus: None,
                conversation_refs: Vec::new(),
                needs_clarification: None,
                source_slice: None,
                retrieval_query: Some("how to update Sample Target".to_string()),
                confidence: 0.9,
            }
        ));
        assert!(!should_request_source_coverage_chunks(
            "compare these documents",
            &QueryIR {
                act: QueryAct::Compare,
                scope: QueryScope::MultiDocument,
                language: QueryLanguage::Auto,
                target_types: Vec::new(),
                target_entities: Vec::new(),
                literal_constraints: Vec::new(),
                temporal_constraints: Vec::new(),
                comparison: None,
                document_focus: None,
                conversation_refs: Vec::new(),
                needs_clarification: None,
                source_slice: None,
                retrieval_query: None,
                confidence: 1.0,
            }
        ));
    }

    #[test]
    fn source_coverage_is_enabled_for_bounded_inventory_queries() {
        assert!(should_request_source_coverage_chunks(
            "what values are exposed?",
            &QueryIR {
                act: QueryAct::Describe,
                scope: QueryScope::SingleDocument,
                language: QueryLanguage::Auto,
                target_types: vec!["attribute".to_string(), "record".to_string()],
                target_entities: Vec::new(),
                literal_constraints: Vec::new(),
                temporal_constraints: Vec::new(),
                comparison: None,
                document_focus: None,
                conversation_refs: Vec::new(),
                needs_clarification: None,
                source_slice: None,
                retrieval_query: None,
                confidence: 1.0,
            }
        ));
        assert!(should_request_source_coverage_chunks(
            "what entries are defined?",
            &QueryIR {
                act: QueryAct::Describe,
                scope: QueryScope::SingleDocument,
                language: QueryLanguage::Auto,
                target_types: Vec::new(),
                target_entities: vec![EntityMention {
                    label: "Subject Alpha".to_string(),
                    role: EntityRole::Subject,
                }],
                literal_constraints: Vec::new(),
                temporal_constraints: Vec::new(),
                comparison: None,
                document_focus: None,
                conversation_refs: Vec::new(),
                needs_clarification: None,
                source_slice: None,
                retrieval_query: None,
                confidence: 1.0,
            }
        ));
        assert!(should_request_source_coverage_chunks(
            "which exact value is defined?",
            &QueryIR {
                act: QueryAct::Describe,
                scope: QueryScope::SingleDocument,
                language: QueryLanguage::Auto,
                target_types: Vec::new(),
                target_entities: Vec::new(),
                literal_constraints: vec![LiteralSpan {
                    text: "alpha_key".to_string(),
                    kind: LiteralKind::Identifier,
                }],
                temporal_constraints: Vec::new(),
                comparison: None,
                document_focus: None,
                conversation_refs: Vec::new(),
                needs_clarification: None,
                source_slice: None,
                retrieval_query: None,
                confidence: 0.25,
            }
        ));
        assert!(!query_ir_requests_inventory_source_coverage(&QueryIR {
            act: QueryAct::Describe,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: Vec::new(),
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence: 0.25,
        }));
        assert!(!query_ir_requests_inventory_source_coverage(&QueryIR {
            act: QueryAct::Meta,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec!["facet".to_string()],
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence: 1.0,
        }));
        assert!(!query_ir_requests_inventory_source_coverage(&QueryIR {
            act: QueryAct::Describe,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: Vec::new(),
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence: 0.25,
        }));
    }

    #[test]
    fn source_coverage_selection_keeps_profile_edges_and_middle() {
        let rows = (0..10)
            .map(|index| {
                if index == 5 {
                    chunk_row(index, "[source_profile source_format=record_jsonl unit_count=42]")
                } else {
                    chunk_row(index, &format!("chunk {index}"))
                }
            })
            .collect::<Vec<_>>();

        let selected = select_source_coverage_chunk_rows(rows, 8, &[]);
        let selected_indexes = selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>();

        assert!(selected_indexes.contains(&0));
        assert!(selected_indexes.contains(&1));
        assert!(selected_indexes.contains(&4));
        assert!(selected_indexes.contains(&5));
        assert!(selected_indexes.contains(&8));
        assert!(selected_indexes.contains(&9));
        assert!(selected.iter().any(is_source_profile_chunk));
    }

    /// Regression guard for the long-document configuration retrieval
    /// gap. A 27-chunk source document at limit=12 used to produce a
    /// gap from index 14 to index 25 because the stride fill stopped
    /// once it accumulated 12 indices counting the forced head/middle/
    /// tail anchors. On real data this skipped exactly the chunk
    /// holding the canonical INI block, so the model truthfully
    /// reported the context as incomplete.
    ///
    /// With greedy max-min coverage the selector must hit at least
    /// one index in every quartile of the document, so a long-doc
    /// config query covers the full index range.
    #[test]
    fn select_source_coverage_chunk_rows_covers_long_document_without_quartile_gap() {
        let total = 27_usize;
        let limit = 12;
        let rows = (0..total)
            .map(|index| chunk_row(i32::try_from(index).expect("index in i32 range"), "body"))
            .collect::<Vec<_>>();

        let selected = select_source_coverage_chunk_rows(rows, limit, &[]);
        let mut selected_indexes = selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>();
        selected_indexes.sort();

        assert_eq!(selected_indexes.len(), limit);

        let quartile = total / 4;
        let in_q1 = selected_indexes.iter().any(|index| (*index as usize) < quartile);
        let in_q2 = selected_indexes
            .iter()
            .any(|index| (*index as usize) >= quartile && (*index as usize) < 2 * quartile);
        let in_q3 = selected_indexes
            .iter()
            .any(|index| (*index as usize) >= 2 * quartile && (*index as usize) < 3 * quartile);
        let in_q4 = selected_indexes.iter().any(|index| (*index as usize) >= 3 * quartile);

        assert!(in_q1, "first quartile must be represented: {selected_indexes:?}");
        assert!(in_q2, "second quartile must be represented: {selected_indexes:?}");
        assert!(
            in_q3,
            "third quartile must be represented (regression of stride-fill gap that skipped this range): {selected_indexes:?}"
        );
        assert!(in_q4, "fourth quartile must be represented: {selected_indexes:?}");

        // Maximum gap between consecutive selected indices must stay
        // bounded — on a 27-chunk document at limit=12 no run of
        // unselected indices should exceed `total / (limit - 1) + 2`
        // which is roughly the spacing of an even partition.
        let max_gap = selected_indexes.windows(2).map(|pair| pair[1] - pair[0]).max().unwrap_or(0);
        let upper_bound = (total / (limit - 1) + 2) as i32;
        assert!(
            max_gap <= upper_bound,
            "max gap {max_gap} must stay within {upper_bound} for total={total} limit={limit}: {selected_indexes:?}"
        );
    }

    #[test]
    fn source_coverage_selection_prioritizes_focused_keyword_rows() {
        let rows = (0..18)
            .map(|index| {
                if index == 12 {
                    chunk_row(index, "resource threshold RATE_LIMIT_REQUESTS exact anchor")
                } else {
                    chunk_row(index, &format!("chunk {index}"))
                }
            })
            .collect::<Vec<_>>();
        let plan_keywords = vec!["RATE_LIMIT_REQUESTS".to_string()];

        let selected = select_source_coverage_chunk_rows(rows, 8, &plan_keywords);
        let selected_indexes = selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>();

        assert!(
            selected_indexes.contains(&12),
            "focused exact keyword row must be retained: {selected_indexes:?}"
        );
    }

    #[test]
    fn source_coverage_selection_prefers_late_structural_anchor_rows() {
        let rows = (0..24)
            .map(|index| match index {
                2 => chunk_row(index, "generic cloudwatch alarms and threshold overview"),
                19 => chunk_row(
                    index,
                    "resource aws_cloudwatch_metric_alarm ecs_cpu_high metric_name CPUUtilization threshold 85",
                ),
                _ => chunk_row(index, &format!("chunk {index}")),
            })
            .collect::<Vec<_>>();
        let plan_keywords =
            vec!["cloudwatch".to_string(), "cpu".to_string(), "CPUUtilization".to_string()];

        let selected = select_source_coverage_chunk_rows(rows, 8, &plan_keywords);
        let selected_indexes = selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>();

        assert!(
            selected_indexes.contains(&19),
            "late exact structural row must be retained: {selected_indexes:?}"
        );
    }
}

#[cfg(test)]
mod image_deprioritization_tests {
    use super::*;

    fn make_chunk(revision_id: Uuid, chunk_index: i32) -> RuntimeMatchedChunk {
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id,
            chunk_index,
            chunk_kind: Some("paragraph".to_string()),
            document_label: "doc".to_string(),
            excerpt: "excerpt".to_string(),
            score_kind: RuntimeChunkScoreKind::Relevance,
            score: Some(1.0),
            source_text: "text".to_string(),
        }
    }

    /// Case 1: document has 1 text chunk + 5 image stubs → text chunk ends up
    /// before all image-source chunks in the output.
    #[test]
    fn text_chunk_before_image_stubs_when_mixed() {
        let text_revision = Uuid::now_v7();
        let image_revision = Uuid::now_v7();
        let mut image_revision_ids = HashSet::new();
        image_revision_ids.insert(image_revision);

        let mut chunks = vec![
            make_chunk(image_revision, 0),
            make_chunk(image_revision, 1),
            make_chunk(text_revision, 2),
            make_chunk(image_revision, 3),
            make_chunk(image_revision, 4),
            make_chunk(image_revision, 5),
        ];

        deprioritize_image_source_chunks(&mut chunks, &image_revision_ids);

        // Text chunk must appear before all image chunks
        let text_pos = chunks.iter().position(|c| c.revision_id == text_revision).unwrap();
        let all_image_after = chunks
            .iter()
            .enumerate()
            .filter(|(_, c)| c.revision_id == image_revision)
            .all(|(pos, _)| pos > text_pos);
        assert!(all_image_after, "all image chunks should follow the text chunk");
        // All 6 chunks must still be present
        assert_eq!(chunks.len(), 6);
    }

    /// Case 2: document has ONLY image stubs → order is unchanged (all survive).
    #[test]
    fn image_only_context_is_not_reordered() {
        let image_revision = Uuid::now_v7();
        let mut image_revision_ids = HashSet::new();
        image_revision_ids.insert(image_revision);

        let original_chunk_indices: Vec<i32> = (0..5).collect();
        let mut chunks: Vec<RuntimeMatchedChunk> =
            original_chunk_indices.iter().map(|&i| make_chunk(image_revision, i)).collect();

        deprioritize_image_source_chunks(&mut chunks, &image_revision_ids);

        let result_indices: Vec<i32> = chunks.iter().map(|c| c.chunk_index).collect();
        assert_eq!(result_indices, original_chunk_indices, "image-only list must not be reordered");
        assert_eq!(chunks.len(), 5);
    }

    /// Case 3: two documents — one with text+image stubs, one with only image
    /// stubs.  The second document's image chunks should be preserved
    /// (deprioritized relative to the first doc's text chunk, not dropped).
    #[test]
    fn image_only_doc_preserved_alongside_mixed_doc() {
        let text_revision = Uuid::now_v7();
        let mixed_image_revision = Uuid::now_v7();
        let pure_image_revision = Uuid::now_v7();
        let mut image_revision_ids = HashSet::new();
        image_revision_ids.insert(mixed_image_revision);
        image_revision_ids.insert(pure_image_revision);

        let mut chunks = vec![
            make_chunk(mixed_image_revision, 0), // image stub from mixed doc
            make_chunk(text_revision, 1),        // text chunk from mixed doc
            make_chunk(pure_image_revision, 2),  // image from image-only doc
            make_chunk(pure_image_revision, 3),  // image from image-only doc
        ];

        deprioritize_image_source_chunks(&mut chunks, &image_revision_ids);

        // Text chunk must come before all image chunks
        let text_pos = chunks.iter().position(|c| c.revision_id == text_revision).unwrap();
        let image_positions: Vec<usize> = chunks
            .iter()
            .enumerate()
            .filter(|(_, c)| image_revision_ids.contains(&c.revision_id))
            .map(|(i, _)| i)
            .collect();
        assert!(
            image_positions.iter().all(|&p| p > text_pos),
            "all image chunks should follow the text chunk"
        );
        // Pure-image doc's chunks are still present (not dropped)
        let pure_image_count =
            chunks.iter().filter(|c| c.revision_id == pure_image_revision).count();
        assert_eq!(pure_image_count, 2, "image-only doc chunks must not be dropped");
        assert_eq!(chunks.len(), 4);
    }
}
