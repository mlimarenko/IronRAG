use std::collections::{HashMap, HashSet};

use anyhow::Context;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::query_ir::QueryIR,
    infra::knowledge_rows::{KnowledgeDocumentRow, KnowledgeStructuredBlockRow},
};

use super::super::{
    CanonicalAnswerEvidence, RuntimeMatchedChunk,
    retrieve::canonical_document_revision_id,
    technical_literals::{extract_package_command_literals, extract_parameter_literals},
};
use super::{
    query_ir_requests_broad_procedure_variant_coverage,
    query_ir_requests_low_confidence_setup_preflight, query_ir_requests_setup_literal_context,
    select_setup_literal_document_id, select_setup_literal_document_ids,
    setup_literal_assignment_count, setup_literal_configuration_path_count,
    setup_literal_section_count,
};

const SETUP_PREFLIGHT_STRUCTURED_BLOCK_LIMIT: usize = 96;
const SETUP_PREFLIGHT_DOCUMENT_LIMIT: usize = 5;

pub(super) async fn augment_setup_preflight_structured_blocks(
    state: &AppState,
    question: &str,
    query_ir: &QueryIR,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    preflight_answer_chunks: &[RuntimeMatchedChunk],
    scoped_document_ids: Option<&HashSet<Uuid>>,
    preflight_evidence: &mut CanonicalAnswerEvidence,
) -> anyhow::Result<()> {
    if !query_ir_requests_setup_literal_context(query_ir)
        && !query_ir_requests_low_confidence_setup_preflight(query_ir, preflight_answer_chunks)
    {
        return Ok(());
    }
    let document_ids = setup_preflight_document_ids(
        question,
        query_ir,
        preflight_answer_chunks,
        scoped_document_ids,
    );
    if document_ids.is_empty() {
        return Ok(());
    }

    let document_count = document_ids.len();
    let per_document_block_limit =
        SETUP_PREFLIGHT_STRUCTURED_BLOCK_LIMIT.checked_div(document_count).unwrap_or(0).max(1);
    let mut loaded_block_count = 0usize;
    let mut added_block_count = 0usize;
    for document_id in document_ids {
        let remaining_block_budget =
            SETUP_PREFLIGHT_STRUCTURED_BLOCK_LIMIT.saturating_sub(added_block_count);
        if remaining_block_budget == 0 {
            break;
        }
        let Some(revision_id) =
            setup_preflight_revision_id(document_id, preflight_answer_chunks, document_index)
        else {
            continue;
        };
        let candidate_limit =
            remaining_block_budget.min(per_document_block_limit).saturating_mul(4).max(1);
        let revision_blocks = state
            .document_store
            .list_setup_structured_blocks_by_revision(revision_id, candidate_limit, candidate_limit)
            .await
            .context("failed to load setup structured blocks for canonical preflight")?;
        loaded_block_count = loaded_block_count.saturating_add(revision_blocks.len());
        added_block_count =
            added_block_count.saturating_add(merge_setup_preflight_structured_blocks(
                preflight_evidence,
                document_id,
                revision_blocks,
                remaining_block_budget.min(per_document_block_limit),
            ));
    }
    if added_block_count > 0 {
        tracing::info!(
            stage = "answer.preflight.setup_structured_blocks",
            loaded_block_count,
            added_block_count,
            structured_block_count = preflight_evidence.structured_blocks.len(),
            "setup structured blocks added to canonical preflight evidence"
        );
    }
    Ok(())
}

pub(in crate::services::query::execution) fn setup_preflight_document_ids(
    question: &str,
    query_ir: &QueryIR,
    preflight_answer_chunks: &[RuntimeMatchedChunk],
    scoped_document_ids: Option<&HashSet<Uuid>>,
) -> Vec<Uuid> {
    if let Some(document_ids) = scoped_document_ids
        && document_ids.len() == 1
    {
        return document_ids.iter().copied().collect();
    }
    if query_ir_requests_broad_procedure_variant_coverage(query_ir, preflight_answer_chunks) {
        return select_setup_literal_document_ids(
            question,
            query_ir,
            preflight_answer_chunks,
            SETUP_PREFLIGHT_DOCUMENT_LIMIT,
        );
    }
    select_setup_literal_document_id(question, query_ir, preflight_answer_chunks)
        .into_iter()
        .collect()
}

fn setup_preflight_revision_id(
    document_id: Uuid,
    preflight_answer_chunks: &[RuntimeMatchedChunk],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> Option<Uuid> {
    preflight_answer_chunks
        .iter()
        .find(|chunk| chunk.document_id == document_id)
        .map(|chunk| chunk.revision_id)
        .or_else(|| document_index.get(&document_id).and_then(canonical_document_revision_id))
}

pub(in crate::services::query::execution) fn merge_setup_preflight_structured_blocks(
    preflight_evidence: &mut CanonicalAnswerEvidence,
    document_id: Uuid,
    revision_blocks: Vec<KnowledgeStructuredBlockRow>,
    limit: usize,
) -> usize {
    if limit == 0 {
        return 0;
    }
    let mut selected = revision_blocks
        .into_iter()
        .filter(|block| block.document_id == document_id)
        .filter_map(|block| {
            let score = setup_preflight_structured_block_score(&block);
            (score > 0).then_some((score, block.ordinal, block.block_id, block))
        })
        .collect::<Vec<_>>();
    selected.sort_by(|left, right| {
        right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)).then_with(|| left.2.cmp(&right.2))
    });
    selected.truncate(limit);
    selected.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.2.cmp(&right.2)));

    let mut seen_block_ids = preflight_evidence
        .structured_blocks
        .iter()
        .map(|block| block.block_id)
        .collect::<HashSet<_>>();
    let before = preflight_evidence.structured_blocks.len();
    preflight_evidence.structured_blocks.extend(
        selected
            .into_iter()
            .map(|(_, _, _, block)| block)
            .filter(|block| seen_block_ids.insert(block.block_id)),
    );
    preflight_evidence.structured_blocks.len().saturating_sub(before)
}

fn setup_preflight_structured_block_score(block: &KnowledgeStructuredBlockRow) -> usize {
    let text = if block.normalized_text == block.text {
        block.text.clone()
    } else {
        format!("{}\n{}", block.text, block.normalized_text)
    };
    let package_count = extract_package_command_literals(&text, 4).len();
    let path_count = setup_literal_configuration_path_count(&text);
    let assignment_count = setup_literal_assignment_count(&text);
    let section_count = setup_literal_section_count(&text);
    let parameter_count = extract_parameter_literals(&text, 32).len();
    let block_kind = block.block_kind.as_str();
    let kind_score: usize = if block_kind.contains("table_row") {
        32
    } else if block_kind.contains("table") {
        18
    } else if block_kind.contains("code") {
        24
    } else {
        0
    };
    let has_structured_parameter = parameter_count > 0 && kind_score > 0;
    let has_setup_signal =
        package_count > 0 || path_count > 0 || assignment_count > 0 || section_count > 0;
    if !has_setup_signal && !has_structured_parameter {
        return 0;
    }
    kind_score
        .saturating_add(package_count.saturating_mul(16))
        .saturating_add(path_count.saturating_mul(24))
        .saturating_add(assignment_count.saturating_mul(10))
        .saturating_add(section_count.saturating_mul(8))
        .saturating_add(parameter_count.saturating_mul(3))
}
