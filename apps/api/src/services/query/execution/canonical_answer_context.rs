use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::Context;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::query_ir::QueryIR,
    infra::arangodb::document_store::{KnowledgeChunkRow, KnowledgeDocumentRow},
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
};

const MAX_DIRECT_TABLE_ANALYTICS_ROWS: usize = 2_000;
const MAX_CANONICAL_ANSWER_TECHNICAL_FACTS: usize = 24;
const SOURCE_COVERAGE_DOCUMENT_LIMIT: usize = 3;
const SOURCE_COVERAGE_CHUNKS_PER_DOCUMENT: usize = 8;
const SOURCE_PROFILE_SCORE: f32 = 1.25;
const SOURCE_COVERAGE_SCORE_BASE: f32 = 0.95;
const SOURCE_COVERAGE_SCORE_STEP: f32 = 0.001;

pub(crate) async fn load_direct_targeted_table_answer(
    state: &AppState,
    question: &str,
    ir: Option<&crate::domains::query_ir::QueryIR>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> anyhow::Result<Option<String>> {
    let row_count = requested_initial_table_row_count(question);
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
    let explicit_targeted_document_ids = explicit_target_document_ids_from_values(
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
    let focused_document_id = (explicit_targeted_document_ids.len() == 1)
        .then(|| explicit_targeted_document_ids.iter().next().copied())
        .flatten();
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
        requested_initial_table_row_count(question)
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
    apply_runtime_chunk_overlays(&mut chunks, fallback_chunks);
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
            query_ir,
            focused_document_id,
            document_index,
            &plan_keywords,
            fallback_chunks.to_vec(),
        )
        .await;
    }
    if let Some(row_count) = requested_initial_table_row_count(question)
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
        query_ir,
        focused_document_id,
        document_index,
        &plan_keywords,
        chunks,
    )
    .await?;
    apply_runtime_chunk_overlays(&mut chunks, fallback_chunks);
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

async fn augment_with_source_coverage_chunks(
    state: &AppState,
    query_ir: &QueryIR,
    focused_document_id: Option<Uuid>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    mut chunks: Vec<RuntimeMatchedChunk>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if !query_ir.requests_source_coverage_context() {
        return Ok(chunks);
    }
    let mut document_ids = Vec::<Uuid>::new();
    let mut seen_document_ids = HashSet::<Uuid>::new();
    if let Some(document_id) = focused_document_id
        && seen_document_ids.insert(document_id)
    {
        document_ids.push(document_id);
    }
    for chunk in &chunks {
        if document_ids.len() >= SOURCE_COVERAGE_DOCUMENT_LIMIT {
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
        for row in select_source_coverage_chunk_rows(rows, SOURCE_COVERAGE_CHUNKS_PER_DOCUMENT) {
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
    Ok(chunks)
}

fn select_source_coverage_chunk_rows(
    mut rows: Vec<KnowledgeChunkRow>,
    limit: usize,
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
    if selected.len() < limit && rows.len() > 1 {
        for slot in 0..limit {
            let index = slot * (rows.len() - 1) / (limit - 1);
            selected.insert(index);
            if selected.len() >= limit {
                break;
            }
        }
    }

    selected.into_iter().take(limit).filter_map(|index| rows.get(index).cloned()).collect()
}

fn is_source_profile_chunk(row: &KnowledgeChunkRow) -> bool {
    super::source_profile::is_source_profile_chunk_row(row)
}

#[cfg(test)]
mod source_coverage_tests {
    use super::*;

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

        let selected = select_source_coverage_chunk_rows(rows, 8);
        let selected_indexes = selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>();

        assert!(selected_indexes.contains(&0));
        assert!(selected_indexes.contains(&1));
        assert!(selected_indexes.contains(&4));
        assert!(selected_indexes.contains(&5));
        assert!(selected_indexes.contains(&8));
        assert!(selected_indexes.contains(&9));
        assert!(selected.iter().any(is_source_profile_chunk));
    }
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
        explicit_canonical_context_document_id(question, canonical_answer_chunks);
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

    let graph_evidence_section = render_graph_evidence_context_lines(graph_evidence_context_lines);
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

fn explicit_canonical_context_document_id(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Option<Uuid> {
    let document_ids = explicit_target_document_ids_from_values(
        question,
        chunks.iter().map(|chunk| (chunk.document_id, chunk.document_label.as_str())),
    );
    (document_ids.len() == 1).then(|| document_ids.iter().next().copied()).flatten()
}
