use crate::{
    domains::query_ir::{QueryIR, SourceSliceDirection},
    services::query::latest_versions::query_requests_latest_versions,
};

use super::super::{
    retrieve::{command_dense_excerpt_for, excerpt_for, focused_excerpt_for, score_value},
    source_context::is_source_unit_runtime_chunk,
    source_excerpt::structured_literal_excerpt_for,
    source_profile::is_source_profile_runtime_chunk,
    source_slice_requested_count,
    types::{RuntimeChunkScoreKind, RuntimeMatchedChunk},
};

const BOUNDED_SOURCE_UNIT_CONTEXT_CHARS: usize = 4_000;
const BOUNDED_ORDINARY_CONTEXT_CHARS: usize = 1_200;

pub(super) fn bounded_context_document_block(
    chunk: &RuntimeMatchedChunk,
    ordinary_keywords: &[String],
) -> String {
    if chunk.score_kind == RuntimeChunkScoreKind::GraphEvidence {
        return graph_evidence_document_block(chunk, ordinary_keywords);
    }
    if is_structured_source_unit_context_chunk(chunk) {
        return structured_document_block(chunk, ordinary_keywords, "source_unit");
    }
    if chunk.score_kind == RuntimeChunkScoreKind::SourceContext
        && !is_source_profile_runtime_chunk(chunk)
    {
        return structured_document_block(chunk, ordinary_keywords, "source_context");
    }
    if let Some(block_kind) = identity_document_block_kind(chunk) {
        return identity_document_block(chunk, block_kind);
    }
    let text = query_focused_chunk_context_text(chunk, ordinary_keywords);
    format!("[document] {}: {}", chunk.document_label, text.trim())
}

pub(super) fn preferred_chunk_text(chunk: &RuntimeMatchedChunk) -> &str {
    let source_text = chunk.source_text.trim();
    if source_text.is_empty() { chunk.excerpt.trim() } else { source_text }
}

fn graph_evidence_document_block(
    chunk: &RuntimeMatchedChunk,
    ordinary_keywords: &[String],
) -> String {
    let text = preferred_chunk_text(chunk);
    let focused = focused_excerpt_for(text, ordinary_keywords, BOUNDED_SOURCE_UNIT_CONTEXT_CHARS);
    let excerpt = if focused.trim().is_empty() {
        excerpt_for(text, BOUNDED_SOURCE_UNIT_CONTEXT_CHARS)
    } else {
        focused
    };
    format!(
        "[document graph_evidence document=\"{}\" ordinal={}]\n{}",
        context_label(&chunk.document_label),
        chunk.chunk_index,
        excerpt
    )
}

fn structured_document_block(
    chunk: &RuntimeMatchedChunk,
    ordinary_keywords: &[String],
    block_kind: &str,
) -> String {
    let text = preferred_chunk_text(chunk);
    let excerpt =
        structured_literal_excerpt_for(text, ordinary_keywords, BOUNDED_SOURCE_UNIT_CONTEXT_CHARS)
            .unwrap_or_else(|| excerpt_for(text, BOUNDED_SOURCE_UNIT_CONTEXT_CHARS));
    format!(
        "[document {block_kind} ordinal={} document=\"{}\"]\n{}",
        chunk.chunk_index,
        context_label(&chunk.document_label),
        excerpt
    )
}

fn identity_document_block_kind(chunk: &RuntimeMatchedChunk) -> Option<&'static str> {
    match chunk.score_kind {
        RuntimeChunkScoreKind::LatestVersion => Some("latest_version"),
        RuntimeChunkScoreKind::DocumentIdentity => Some("document_identity"),
        _ => None,
    }
}

fn identity_document_block(chunk: &RuntimeMatchedChunk, block_kind: &str) -> String {
    format!(
        "[document {block_kind} ordinal={} document=\"{}\"]\n{}",
        chunk.chunk_index,
        context_label(&chunk.document_label),
        excerpt_for(preferred_chunk_text(chunk), BOUNDED_SOURCE_UNIT_CONTEXT_CHARS)
    )
}

fn query_focused_chunk_context_text(
    chunk: &RuntimeMatchedChunk,
    ordinary_keywords: &[String],
) -> String {
    if ordinary_keywords.is_empty() {
        return chunk.excerpt.trim().to_string();
    }
    let source_text = chunk.source_text.trim();
    if source_text.is_empty() {
        return chunk.excerpt.trim().to_string();
    }
    if let Some(excerpt) = command_dense_excerpt_for(source_text, BOUNDED_SOURCE_UNIT_CONTEXT_CHARS)
    {
        return excerpt;
    }
    focused_excerpt_for(source_text, ordinary_keywords, BOUNDED_ORDINARY_CONTEXT_CHARS)
}

fn is_structured_source_unit_context_chunk(chunk: &RuntimeMatchedChunk) -> bool {
    is_source_unit_runtime_chunk(chunk)
        || chunk.source_text.lines().map(str::trim_start).any(|line| line.starts_with("[unit_id="))
}

pub(super) fn assemble_ordered_source_slice_context(
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
    budget_chars: usize,
) -> Option<String> {
    let slice = query_ir.source_slice.as_ref()?;
    let mut profile_blocks = chunks
        .iter()
        .filter(|chunk| is_source_profile_runtime_chunk(chunk))
        .map(|chunk| {
            format!(
                "[SOURCE_PROFILE document=\"{}\"]\n{}",
                context_label(&chunk.document_label),
                source_profile_text_for_source_slice(chunk)
            )
        })
        .collect::<Vec<_>>();
    let mut content_chunks =
        chunks.iter().filter(|chunk| !is_source_profile_runtime_chunk(chunk)).collect::<Vec<_>>();
    if content_chunks.iter().any(|chunk| is_source_unit_runtime_chunk(chunk)) {
        content_chunks.retain(|chunk| is_source_unit_runtime_chunk(chunk));
    }
    if content_chunks.is_empty() {
        return None;
    }
    let latest_version_context = query_requests_latest_versions(query_ir);
    if latest_version_context {
        let identity_chunks = content_chunks
            .iter()
            .copied()
            .filter(|chunk| {
                matches!(
                    chunk.score_kind,
                    RuntimeChunkScoreKind::DocumentIdentity | RuntimeChunkScoreKind::LatestVersion
                )
            })
            .collect::<Vec<_>>();
        if !identity_chunks.is_empty() {
            content_chunks = identity_chunks;
        }
        content_chunks.sort_by(latest_version_source_slice_chunk_order);
    } else {
        content_chunks.sort_by_key(|chunk| (chunk.document_label.clone(), chunk.chunk_index));
    }
    let mut content_blocks = content_chunks
        .iter()
        .map(|chunk| {
            format!(
                "[SOURCE_SLICE_UNIT direction={} requested_count={} document=\"{}\" ordinal={} coverage=ordered]\n{}",
                source_slice_direction_label(slice.direction),
                source_slice_requested_count(query_ir).unwrap_or_default(),
                context_label(&chunk.document_label),
                chunk.chunk_index,
                chunk_text_for_source_slice(chunk)
            )
        })
        .collect::<Vec<_>>();
    let header = format!(
        "Context\nSOURCE_SLICE blocks below are the ordered source slice selected by the runtime for this question. Treat them as ordered source records, not sampled excerpts.\n- direction: {}\n- requested_count: {}\n- returned_unit_count: {}",
        source_slice_direction_label(slice.direction),
        source_slice_requested_count(query_ir).unwrap_or_default(),
        content_blocks.len()
    );
    let prefix_len =
        header.len() + profile_blocks.iter().map(|block| block.len() + 2).sum::<usize>() + 2;
    let remaining_budget = budget_chars.saturating_sub(prefix_len);
    content_blocks = if latest_version_context {
        select_blocks_for_budget_in_order(content_blocks, remaining_budget)
    } else {
        select_source_slice_blocks_for_budget(content_blocks, remaining_budget, slice.direction)
    };
    if content_blocks.is_empty() {
        return None;
    }
    let mut sections = Vec::new();
    sections.push(header);
    sections.append(&mut profile_blocks);
    sections.append(&mut content_blocks);
    Some(sections.join("\n\n"))
}

fn latest_version_source_slice_chunk_order(
    left: &&RuntimeMatchedChunk,
    right: &&RuntimeMatchedChunk,
) -> std::cmp::Ordering {
    score_value(right.score)
        .total_cmp(&score_value(left.score))
        .then_with(|| left.chunk_index.cmp(&right.chunk_index))
        .then_with(|| left.document_label.cmp(&right.document_label))
}

fn select_blocks_for_budget_in_order(blocks: Vec<String>, budget_chars: usize) -> Vec<String> {
    let mut selected = Vec::<String>::new();
    let mut used = 0usize;
    for block in blocks {
        let projected = used.saturating_add(block.len()).saturating_add(2);
        if projected > budget_chars && !selected.is_empty() {
            break;
        }
        used = projected;
        selected.push(block);
    }
    selected
}

fn select_source_slice_blocks_for_budget(
    blocks: Vec<String>,
    budget_chars: usize,
    direction: SourceSliceDirection,
) -> Vec<String> {
    let mut selected = Vec::<String>::new();
    let mut used = 0usize;
    let iter: Box<dyn Iterator<Item = String>> = match direction {
        SourceSliceDirection::Tail => Box::new(blocks.into_iter().rev()),
        SourceSliceDirection::Head | SourceSliceDirection::All => Box::new(blocks.into_iter()),
    };
    for block in iter {
        let projected = used.saturating_add(block.len()).saturating_add(2);
        if projected > budget_chars && !selected.is_empty() {
            break;
        }
        used = projected;
        selected.push(block);
    }
    if direction == SourceSliceDirection::Tail {
        selected.reverse();
    }
    selected
}

fn chunk_text_for_source_slice(chunk: &RuntimeMatchedChunk) -> String {
    let source = chunk.source_text.trim();
    if !source.is_empty() {
        return source.to_string();
    }
    chunk.excerpt.trim().to_string()
}

fn source_profile_text_for_source_slice(chunk: &RuntimeMatchedChunk) -> String {
    chunk
        .source_text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .filter(|line| !line.is_empty())
        .unwrap_or_else(|| chunk.excerpt.trim())
        .to_string()
}

fn source_slice_direction_label(direction: SourceSliceDirection) -> &'static str {
    match direction {
        SourceSliceDirection::Head => "head",
        SourceSliceDirection::Tail => "tail",
        SourceSliceDirection::All => "all",
    }
}

pub(super) fn context_label(label: &str) -> String {
    label.replace('\\', "\\\\").replace('"', "\\\"")
}
