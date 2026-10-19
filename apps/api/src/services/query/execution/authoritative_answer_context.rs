use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::Context;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::query_ir::{QueryAct, QueryIR, QueryScope, QueryTargetKind},
    infra::knowledge_rows::{
        KnowledgeBundleChunkReferenceRow, KnowledgeChunkRow, KnowledgeDocumentRow,
    },
    services::query::effective_query::structured_current_question_segment,
    services::query::text_match::{near_token_match, normalized_alnum_tokens},
    shared::extraction::text_render::repair_technical_layout_noise,
};

use super::{
    AuthoritativeAnswerEvidence, RuntimeChunkScoreKind, RuntimeMatchedChunk,
    build_table_row_grounded_answer, current_document_revision_id,
    explicit_target_document_ids_from_values, focused_excerpt_for,
    load_initial_table_rows_for_documents, load_table_rows_for_documents,
    load_table_summary_chunks_for_documents, map_chunk_hit, merge_chunks,
    merge_table_aggregation_chunks, question_asks_table_aggregation,
    question_asks_table_value_inventory, render_authoritative_technical_fact_section,
    render_prepared_segment_section, render_primary_chunk_section,
    render_table_summary_chunk_section, requested_initial_table_row_count, score_desc_chunks,
    score_value,
    technical_literals::technical_chunk_selection_score,
    technical_literals::{extract_explicit_path_literals, extract_parameter_literals},
};

const MAX_DIRECT_TABLE_ANALYTICS_ROWS: usize = 2_000;
const MAX_AUTHORITATIVE_ANSWER_TECHNICAL_FACTS: usize = 24;
const SOURCE_COVERAGE_DOCUMENT_LIMIT: usize = 3;
const SOURCE_COVERAGE_CHUNKS_PER_DOCUMENT: usize = 12;
const SOURCE_COVERAGE_CANDIDATE_CHUNKS_PER_DOCUMENT: usize = 24;
const SOURCE_COVERAGE_MAX_ADDED_CHUNKS: usize = 24;
const SOURCE_PROFILE_SCORE: f32 = 0.65;
const SOURCE_COVERAGE_SCORE_BASE: f32 = 0.95;
const SOURCE_COVERAGE_SCORE_STEP: f32 = 0.001;

mod answer_context;
mod source_coverage;

#[cfg(test)]
use answer_context::CURRENT_READABLE_PREPARED_SCOPES_SQL;
#[cfg(test)]
pub(crate) use answer_context::selected_fact_ids_for_authoritative_evidence;
pub(crate) use answer_context::{
    build_authoritative_answer_context, deprioritize_image_source_chunks,
    focus_token_overlap_count, load_authoritative_answer_evidence, query_ir_document_focus_tokens,
};
use answer_context::{load_image_revision_ids, query_ir_focus_document_id};
use source_coverage::{
    SourceCoverageInputs, align_chunks_to_bundle_ranking, augment_with_source_coverage_chunks,
    ranked_document_ids_from_chunks,
};

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
    let Some(revision_id) = document.readable_revision_id else {
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
        .document_store
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

struct TableAggregationChunks {
    summaries: Vec<RuntimeMatchedChunk>,
    rows: Vec<RuntimeMatchedChunk>,
    initial_rows: Option<Vec<RuntimeMatchedChunk>>,
}

async fn load_table_aggregation_chunks(
    state: &AppState,
    question: &str,
    query_ir: &QueryIR,
    focused_document_id: Option<Uuid>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
) -> anyhow::Result<TableAggregationChunks> {
    let Some(document_id) = focused_document_id else {
        return Ok(TableAggregationChunks {
            summaries: Vec::new(),
            rows: Vec::new(),
            initial_rows: None,
        });
    };
    let targeted_document_ids = BTreeSet::from([document_id]);
    let (summaries, rows) = if question_asks_table_aggregation(question, Some(query_ir)) {
        let summaries = load_table_summary_chunks_for_documents(
            state,
            document_index,
            &targeted_document_ids,
            32,
            plan_keywords,
        )
        .await
        .context("failed to load focused table summaries for authoritative answer")?;
        let rows = load_table_rows_for_documents(
            state,
            document_index,
            &targeted_document_ids,
            MAX_DIRECT_TABLE_ANALYTICS_ROWS,
            plan_keywords,
        )
        .await
        .context("failed to load focused table rows for computed aggregate answer")?;
        (summaries, rows)
    } else {
        (Vec::new(), Vec::new())
    };
    let initial_rows = load_requested_initial_table_rows(
        state,
        query_ir,
        document_index,
        &targeted_document_ids,
        plan_keywords,
    )
    .await?;
    Ok(TableAggregationChunks { summaries, rows, initial_rows })
}

async fn load_requested_initial_table_rows(
    state: &AppState,
    query_ir: &QueryIR,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    targeted_document_ids: &BTreeSet<Uuid>,
    plan_keywords: &[String],
) -> anyhow::Result<Option<Vec<RuntimeMatchedChunk>>> {
    let Some(row_count) = requested_initial_table_row_count(Some(query_ir)) else {
        return Ok(None);
    };
    let initial_rows = load_initial_table_rows_for_documents(
        state,
        document_index,
        targeted_document_ids,
        row_count,
        plan_keywords,
    )
    .await
    .context("failed to load direct initial table rows for authoritative answer")?;
    Ok((initial_rows.len() >= row_count).then_some(initial_rows))
}

pub(crate) async fn load_primary_answer_chunks(
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
        .or_else(|| query_ir_focus_document_id(query_ir, document_values));
    let plan_keywords = crate::services::query::planner::extract_keywords(question);
    let mut table_chunks = load_table_aggregation_chunks(
        state,
        question,
        query_ir,
        focused_document_id,
        document_index,
        &plan_keywords,
    )
    .await?;
    if let Some(mut initial_rows) = table_chunks.initial_rows.take() {
        if !table_chunks.summaries.is_empty() {
            let chunk_limit = initial_rows.len().saturating_add(32);
            initial_rows = merge_chunks(initial_rows, table_chunks.summaries, chunk_limit);
        }
        initial_rows.sort_by(score_desc_chunks);
        return Ok(initial_rows);
    }

    let bundle_refs = state
        .context_store
        .get_bundle_reference_set_by_query_execution(execution_id)
        .await
        .with_context(|| {
            format!("failed to load context bundle for authoritative answer chunks {execution_id}")
        })?;
    let chunk_references =
        bundle_refs.as_ref().map(|refs| refs.chunk_references.clone()).unwrap_or_default();
    if chunk_references.is_empty() {
        return primary_chunks_without_bundle(
            PrimaryChunkLoadContext {
                state,
                question,
                query_ir,
                focused_document_id,
                document_index,
                plan_keywords: &plan_keywords,
                fallback_chunks,
            },
            table_chunks.summaries,
            table_chunks.rows,
        )
        .await;
    }
    load_primary_bundle_chunks(
        PrimaryChunkLoadContext {
            state,
            question,
            query_ir,
            focused_document_id,
            document_index,
            plan_keywords: &plan_keywords,
            fallback_chunks,
        },
        chunk_references,
        table_chunks.summaries,
        table_chunks.rows,
    )
    .await
}

/// Primary-chunk loader inputs: the query turn, its scoped document and the
/// fallback chunks a missing bundle is answered from.
pub(super) struct PrimaryChunkLoadContext<'a> {
    pub(super) state: &'a AppState,
    pub(super) question: &'a str,
    pub(super) query_ir: &'a QueryIR,
    pub(super) focused_document_id: Option<Uuid>,
    pub(super) document_index: &'a HashMap<Uuid, KnowledgeDocumentRow>,
    pub(super) plan_keywords: &'a [String],
    pub(super) fallback_chunks: &'a [RuntimeMatchedChunk],
}

async fn primary_chunks_without_bundle(
    context: PrimaryChunkLoadContext<'_>,
    aggregation_summary_chunks: Vec<RuntimeMatchedChunk>,
    aggregation_row_chunks: Vec<RuntimeMatchedChunk>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let PrimaryChunkLoadContext {
        state,
        question,
        query_ir,
        focused_document_id,
        document_index,
        plan_keywords,
        fallback_chunks,
    } = context;
    if !aggregation_summary_chunks.is_empty() || !aggregation_row_chunks.is_empty() {
        let mut aggregate_chunks = merge_chunks(
            aggregation_summary_chunks,
            aggregation_row_chunks,
            MAX_DIRECT_TABLE_ANALYTICS_ROWS.saturating_add(32),
        );
        aggregate_chunks.sort_by(score_desc_chunks);
        return Ok(aggregate_chunks);
    }
    let ranked_document_ids = ranked_document_ids_from_chunks(fallback_chunks);
    augment_with_source_coverage_chunks(
        SourceCoverageInputs {
            state,
            question,
            query_ir,
            focused_document_id,
            document_index,
            plan_keywords,
            ranked_document_ids: &ranked_document_ids,
        },
        fallback_chunks.to_vec(),
    )
    .await
}

async fn load_primary_bundle_chunks(
    context: PrimaryChunkLoadContext<'_>,
    chunk_references: Vec<KnowledgeBundleChunkReferenceRow>,
    aggregation_summary_chunks: Vec<RuntimeMatchedChunk>,
    aggregation_row_chunks: Vec<RuntimeMatchedChunk>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let PrimaryChunkLoadContext {
        state,
        question,
        query_ir,
        focused_document_id,
        document_index,
        plan_keywords,
        fallback_chunks,
    } = context;
    let chunk_ids = chunk_references.iter().map(|reference| reference.chunk_id).collect::<Vec<_>>();
    let rows = state
        .document_store
        .list_chunks_by_ids(&chunk_ids)
        .await
        .context("failed to load authoritative answer chunks")?;
    let mut chunks = rows
        .into_iter()
        .filter_map(|chunk| map_chunk_hit(chunk, 1.0, document_index, plan_keywords))
        .collect::<Vec<_>>();
    align_chunks_to_bundle_ranking(&mut chunks, &chunk_references);
    let ranked_document_ids = ranked_document_ids_from_chunks(&chunks);
    merge_runtime_context_chunks(&mut chunks, fallback_chunks);
    if question_asks_table_aggregation(question, Some(query_ir))
        && let Some(document_id) = focused_document_id
    {
        chunks.retain(|chunk| chunk.document_id == document_id);
        chunks = merge_table_aggregation_chunks(
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
        let fallback_document_ids = ranked_document_ids_from_chunks(fallback_chunks);
        return augment_with_source_coverage_chunks(
            SourceCoverageInputs {
                state,
                question,
                query_ir,
                focused_document_id,
                document_index,
                plan_keywords,
                ranked_document_ids: &fallback_document_ids,
            },
            fallback_chunks.to_vec(),
        )
        .await;
    }
    chunks = merge_additional_initial_rows(
        state,
        query_ir,
        focused_document_id,
        document_index,
        plan_keywords,
        chunks,
    )
    .await?;
    chunks = augment_with_source_coverage_chunks(
        SourceCoverageInputs {
            state,
            question,
            query_ir,
            focused_document_id,
            document_index,
            plan_keywords,
            ranked_document_ids: &ranked_document_ids,
        },
        chunks,
    )
    .await?;
    merge_runtime_context_chunks(&mut chunks, fallback_chunks);
    let image_revision_ids =
        load_image_revision_ids(state, &chunks).await.unwrap_or_else(|error| {
            tracing::warn!(
                %error,
                "failed to load image revision ids for chunk deprioritization; skipping"
            );
            HashSet::new()
        });
    deprioritize_image_source_chunks(&mut chunks, &image_revision_ids);
    chunks.sort_by(score_desc_chunks);
    Ok(chunks)
}

async fn merge_additional_initial_rows(
    state: &AppState,
    query_ir: &QueryIR,
    focused_document_id: Option<Uuid>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    chunks: Vec<RuntimeMatchedChunk>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let Some(row_count) = requested_initial_table_row_count(Some(query_ir)) else {
        return Ok(chunks);
    };
    let Some(document_id) = focused_document_id else {
        return Ok(chunks);
    };
    let targeted_document_ids = BTreeSet::from([document_id]);
    let chunk_limit = chunks.len().max(row_count);
    let initial_rows = load_initial_table_rows_for_documents(
        state,
        document_index,
        &targeted_document_ids,
        row_count,
        plan_keywords,
    )
    .await
    .context("failed to load focused initial table rows for authoritative answer")?;
    Ok(merge_chunks(chunks, initial_rows, chunk_limit))
}

pub(crate) fn apply_runtime_chunk_overlays(
    chunks: &mut [RuntimeMatchedChunk],
    runtime_chunks: &[RuntimeMatchedChunk],
) {
    let runtime_by_chunk_id =
        runtime_chunks.iter().map(|chunk| (chunk.chunk_id, chunk)).collect::<HashMap<_, _>>();
    for chunk in chunks {
        if let Some(runtime_chunk) = runtime_by_chunk_id.get(&chunk.chunk_id) {
            apply_runtime_chunk_overlay(chunk, runtime_chunk);
        }
    }
}

fn apply_runtime_chunk_overlay(
    chunk: &mut RuntimeMatchedChunk,
    runtime_chunk: &RuntimeMatchedChunk,
) {
    if runtime_chunk.score_kind == RuntimeChunkScoreKind::GraphEvidence {
        apply_graph_evidence_overlay(chunk, runtime_chunk);
        return;
    }
    if runtime_chunk.score_kind != RuntimeChunkScoreKind::Relevance {
        chunk.score_kind = runtime_chunk.score_kind;
    }
    if runtime_chunk.score.is_some() {
        chunk.score = runtime_chunk.score;
    }
    if runtime_chunk.source_text.trim().chars().count() > chunk.source_text.trim().chars().count() {
        chunk.source_text = runtime_chunk.source_text.clone();
    }
    if runtime_chunk.excerpt.trim().chars().count() > chunk.excerpt.trim().chars().count()
        || runtime_chunk.score_kind != RuntimeChunkScoreKind::Relevance
    {
        chunk.excerpt = runtime_chunk.excerpt.clone();
    }
}

fn apply_graph_evidence_overlay(
    chunk: &mut RuntimeMatchedChunk,
    runtime_chunk: &RuntimeMatchedChunk,
) {
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

#[cfg(test)]
mod runtime_context_merge_tests;

#[cfg(test)]
#[path = "authoritative_answer_source_coverage_tests.rs"]
mod source_coverage_tests;

#[cfg(test)]
mod image_deprioritization_tests;
