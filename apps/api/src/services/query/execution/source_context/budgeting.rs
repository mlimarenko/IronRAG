use super::{
    QueryIR, RuntimeMatchedChunk,
    candidates::{
        requests_expanded_source_context, requests_fallback_structured_source_context,
        requests_structured_source_unit_inventory_context,
    },
    loading::{
        fallback_structured_source_context_chunk_floor, procedural_source_context_chunk_floor,
    },
    source_slice::source_slice_count,
    types::{
        FOCUSED_DECLARATION_CHUNK_KIND, SOURCE_SLICE_CONTEXT_CHARS_PER_UNIT,
        SOURCE_SLICE_CONTEXT_MAX_CHARS, SOURCE_UNIT_CHUNK_KIND,
    },
};

pub(crate) fn source_slice_context_top_k(query_ir: &QueryIR, base_top_k: usize) -> usize {
    let Some(slice) = query_ir.source_slice.as_ref() else {
        return base_top_k;
    };
    base_top_k.max(source_slice_count(slice).saturating_add(1))
}

#[must_use]
pub(crate) fn structured_source_context_top_k(query_ir: &QueryIR, base_top_k: usize) -> usize {
    let top_k = super::source_slice_context_top_k(query_ir, base_top_k);
    if !requests_expanded_source_context(query_ir) {
        return top_k;
    }
    top_k.max(procedural_source_context_chunk_floor())
}

#[must_use]
pub(crate) fn structured_source_context_top_k_for_chunks(
    query_ir: &QueryIR,
    base_top_k: usize,
    chunks: &[RuntimeMatchedChunk],
) -> usize {
    let top_k = super::structured_source_context_top_k(query_ir, base_top_k);
    if !requests_fallback_structured_source_context(Some(query_ir), chunks)
        && !requests_structured_source_unit_inventory_context(query_ir)
    {
        return top_k;
    }
    top_k.max(fallback_structured_source_context_chunk_floor())
}

#[must_use]
pub(crate) fn source_slice_context_budget_chars(query_ir: &QueryIR, base_budget: usize) -> usize {
    let Some(slice) = query_ir.source_slice.as_ref() else {
        return base_budget;
    };
    let requested_units = source_slice_count(slice).saturating_add(1);
    base_budget
        .max(requested_units.saturating_mul(SOURCE_SLICE_CONTEXT_CHARS_PER_UNIT))
        .min(SOURCE_SLICE_CONTEXT_MAX_CHARS)
}

#[must_use]
pub(crate) fn source_slice_requested_count(query_ir: &QueryIR) -> Option<usize> {
    query_ir.source_slice.as_ref().map(source_slice_count)
}

pub(crate) fn is_source_unit_runtime_chunk(chunk: &RuntimeMatchedChunk) -> bool {
    chunk.chunk_kind.as_deref() == Some(SOURCE_UNIT_CHUNK_KIND)
}

pub(crate) fn is_focused_declaration_context_chunk(chunk: &RuntimeMatchedChunk) -> bool {
    chunk.chunk_kind.as_deref() == Some(FOCUSED_DECLARATION_CHUNK_KIND)
}
