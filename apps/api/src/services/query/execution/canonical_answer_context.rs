use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::Context;
use uuid::Uuid;

use crate::{
    app::state::AppState, infra::arangodb::document_store::KnowledgeDocumentRow,
    shared::extraction::text_render::repair_technical_layout_noise,
};

use super::{
    CanonicalAnswerEvidence, RuntimeMatchedChunk, build_table_row_grounded_answer,
    explicit_target_document_ids_from_values, focused_answer_document_id, focused_excerpt_for,
    load_initial_table_rows_for_documents, load_table_rows_for_documents,
    load_table_summary_chunks_for_documents, map_chunk_hit,
    merge_canonical_table_aggregation_chunks, merge_chunks, question_asks_table_aggregation,
    question_asks_table_value_inventory, render_canonical_chunk_section,
    render_canonical_technical_fact_section, render_prepared_segment_section,
    render_table_summary_chunk_section, requested_initial_table_row_count, score_desc_chunks,
};

const MAX_DIRECT_TABLE_ANALYTICS_ROWS: usize = 2_000;
const MAX_CANONICAL_ANSWER_TECHNICAL_FACTS: usize = 24;

pub(crate) async fn load_direct_targeted_table_answer(
    state: &AppState,
    question: &str,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> anyhow::Result<Option<String>> {
    let row_count = requested_initial_table_row_count(question);
    let inventory_requested = question_asks_table_value_inventory(question);
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
            document_label: document_label.clone(),
            excerpt: focused_excerpt_for(&block.normalized_text, &plan_keywords, 280),
            score: Some(10_000.0 - ordinal as f32),
            source_text: repair_technical_layout_noise(&block.normalized_text),
        })
        .collect::<Vec<_>>();
    if let Some(row_count) = row_count
        && initial_rows.len() < row_count
    {
        return Ok(None);
    }

    Ok(build_table_row_grounded_answer(question, &initial_rows))
}

pub(crate) async fn load_canonical_answer_chunks(
    state: &AppState,
    execution_id: Uuid,
    question: &str,
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
    let focused_document_id = if explicit_targeted_document_ids.len() == 1 {
        explicit_targeted_document_ids.iter().next().copied()
    } else {
        focused_answer_document_id(question, fallback_chunks)
    };
    let aggregation_summary_chunks = if question_asks_table_aggregation(question)
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
    let aggregation_row_chunks = if question_asks_table_aggregation(question)
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
        return Ok(fallback_chunks.to_vec());
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
        return Ok(fallback_chunks.to_vec());
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
    if question_asks_table_aggregation(question)
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
        if question_asks_table_aggregation(question) && focused_document_id.is_some() {
            return Ok(Vec::new());
        }
        return Ok(fallback_chunks.to_vec());
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
    chunks.sort_by(score_desc_chunks);
    Ok(chunks)
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

pub(crate) async fn search_community_summaries(
    state: &AppState,
    library_id: Uuid,
    question: &str,
    limit: usize,
) -> Vec<(i32, String, String)> {
    let communities = sqlx::query_as::<_, (i32, Option<String>, Vec<String>, i32)>(
        "SELECT community_id, summary, top_entities, node_count
         FROM runtime_graph_community
         WHERE library_id = $1 AND summary IS NOT NULL
         ORDER BY node_count DESC",
    )
    .bind(library_id)
    .fetch_all(&state.persistence.postgres)
    .await
    .unwrap_or_default();

    let question_lower = question.to_ascii_lowercase();
    let question_words: Vec<&str> = question_lower.split_whitespace().collect();

    let mut scored: Vec<_> = communities
        .into_iter()
        .filter_map(|(cid, summary, entities, _)| {
            let summary = summary?;
            let summary_lower = summary.to_ascii_lowercase();
            let entity_text = entities.join(" ").to_ascii_lowercase();

            let score: usize = question_words
                .iter()
                .filter(|w| {
                    w.len() > 2 && (summary_lower.contains(**w) || entity_text.contains(**w))
                })
                .count();

            if score > 0 { Some((score, cid, summary, entities.join(", "))) } else { None }
        })
        .collect();

    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.truncate(limit);

    scored.into_iter().map(|(_, cid, summary, entities)| (cid, summary, entities)).collect()
}

pub(crate) fn format_community_context(matches: &[(i32, String, String)]) -> Option<String> {
    if matches.is_empty() {
        return None;
    }
    let lines: Vec<String> = matches
        .iter()
        .map(|(_, summary, entities)| format!("- {summary} (key entities: {entities})"))
        .collect();
    Some(format!("Knowledge graph communities:\n{}", lines.join("\n")))
}

pub(crate) fn build_canonical_answer_context(
    question: &str,
    technical_literals_text: Option<&str>,
    evidence: &CanonicalAnswerEvidence,
    canonical_answer_chunks: &[RuntimeMatchedChunk],
    fallback_context: &str,
    community_context: Option<&str>,
) -> String {
    let focused_document_id = focused_answer_document_id(question, canonical_answer_chunks);
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

    let table_summary_section = render_table_summary_chunk_section(question, &filtered_chunks);
    let suppress_tabular_detail =
        question_asks_table_aggregation(question) && !table_summary_section.is_empty();
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

    if let Some(community_text) = community_context
        && !community_text.is_empty()
    {
        sections.push(community_text.to_string());
    }

    let prepared_segment_section = render_prepared_segment_section(
        question,
        &filtered_structured_blocks,
        suppress_tabular_detail,
    );
    if !prepared_segment_section.is_empty() {
        sections.push(prepared_segment_section);
    }

    let chunk_section =
        render_canonical_chunk_section(question, &filtered_chunks, suppress_tabular_detail);
    if !chunk_section.is_empty() {
        sections.push(chunk_section);
    }

    if sections.is_empty() {
        return fallback_context.trim().to_string();
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
