use std::collections::{HashMap, HashSet};

use anyhow::Context;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::query_ir::{
        LiteralKind, QueryAct, QueryIR, QueryScope, QueryTargetKind, SourceSliceDirection,
        SourceSliceFilter, SourceSliceSpec,
    },
    infra::{
        knowledge_plane::LexicalQueryOrigin,
        knowledge_rows::{KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeStructuredBlockRow},
    },
    shared::extraction::{
        record_jsonl::focused_record_unit_excerpt, text_render::repair_technical_layout_noise,
    },
};

use super::{
    RuntimeMatchedChunk, question_asks_table_aggregation,
    question_intent::{QuestionIntent, classify_query_ir_intents, has_question_intent},
    retrieve::{
        current_document_revision_id, excerpt_for, focused_excerpt_for, map_chunk_hit, score_value,
    },
    source_excerpt::{
        line_has_key_value_literal_surface, source_local_evidence_line_score,
        structured_literal_excerpt_for, structured_literal_line_score,
    },
    source_focus_selection::SourceFocusLimits,
    source_profile::{
        SOURCE_PROFILE_CHUNK_KIND, is_record_stream_source_profile_row,
        is_source_profile_chunk_row, is_source_profile_runtime_chunk,
    },
    technical_literals::{
        detect_technical_literal_intent_from_query_ir, extract_config_assignment_literals,
        extract_config_section_literals, extract_explicit_path_literals,
        extract_package_command_literals, extract_parameter_literals,
        technical_chunk_selection_score, technical_literal_focus_keywords,
    },
};

mod budgeting;
mod candidates;
mod companions;
mod loading;
mod orchestration;
mod rendering;
mod selection;
mod source_slice;
#[cfg(test)]
mod tests;
mod types;

pub(crate) use budgeting::{
    is_focused_declaration_context_chunk, is_source_unit_runtime_chunk,
    source_slice_context_budget_chars, source_slice_context_top_k, source_slice_requested_count,
    structured_source_context_top_k, structured_source_context_top_k_for_chunks,
};
pub(crate) use orchestration::{
    StructuredSourceContextAugmentRequest, augment_structured_source_context,
};
pub(crate) use types::{
    SOURCE_SLICE_DEFAULT_COUNT, SOURCE_SLICE_MAX_COUNT, SOURCE_UNIT_CHUNK_KIND,
    StructuredSourceContextDiagnostics, source_anchor_window,
};

#[cfg(test)]
use candidates::{
    collect_source_context_candidates, collect_source_context_candidates_with_limit,
    merge_graph_evidence_source_context_candidates, push_unique_source_context_anchor,
    requests_expanded_source_context, requests_fallback_structured_source_context,
    requests_structured_source_unit_inventory_context, seed_document_head_source_context_anchors,
    source_context_candidate_document_limit, source_context_candidate_document_limit_for_request,
    source_context_neighbor_span, source_context_neighbor_span_for_request,
    unique_candidate_revision_ids,
};
#[cfg(test)]
use loading::{
    code_pattern_candidate_document_ids, code_pattern_literal_terms, code_pattern_query_terms,
    fallback_structured_source_context_chunk_floor,
    preserve_primary_context_on_code_pattern_failure,
    preserve_primary_context_on_transport_pattern_failure, procedural_source_context_chunk_floor,
    requests_code_pattern_source_context, requests_path_source_context,
    requests_table_row_source_context, transport_pattern_candidate_document_ids,
};
#[cfg(test)]
use rendering::{
    apply_structured_source_companions, map_companion_chunk,
    structured_inventory_source_profile_score,
};
#[cfg(test)]
use selection::{
    procedural_structured_sibling_windows, select_fallback_structured_search_rows,
    select_path_source_rows, select_procedural_structured_sibling_rows,
    select_query_focused_source_rows, select_table_structured_sibling_rows,
    source_context_best_neighbor_score, source_context_focus_keywords,
    source_context_neighbor_score, source_context_neighbor_windows,
};
#[cfg(test)]
use source_slice::{map_source_unit_block, top_ranked_source_context_candidate};
#[cfg(test)]
use types::{
    SOURCE_CONTEXT_ANCHOR_LIMIT_PER_DOCUMENT, SOURCE_CONTEXT_DEFAULT_NEIGHBOR_BACKWARD,
    SOURCE_CONTEXT_DEFAULT_NEIGHBOR_FORWARD, SOURCE_CONTEXT_DOCUMENT_HEAD_CHUNK_INDEX,
    SOURCE_CONTEXT_DOCUMENT_LIMIT, SOURCE_CONTEXT_FALLBACK_DOCUMENT_LIMIT,
    SOURCE_CONTEXT_PROCEDURAL_NEIGHBOR_BACKWARD, SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_FORWARD,
    SOURCE_CONTEXT_PROFILE_ANCHOR_SCORE_CAP, SOURCE_CONTEXT_STRUCTURED_PROFILE_SCORE_BONUS,
    SourceContextAnchor, SourceContextCandidate, SourceContextNeighborSpan,
    StructuredSourceCompanion, StructuredSourceCompanionKind,
};
