use std::collections::{BTreeSet, HashMap};

use uuid::Uuid;

use crate::domains::query_ir::{
    EntityRole, LiteralKind, QueryAct, QueryIR, QueryScope, QueryTargetKind,
    literal_text_is_identifier_shaped,
};
use crate::infra::knowledge_rows::KnowledgeDocumentRow;
use crate::services::query::effective_query::{
    current_question_segment, structured_current_question_segment,
};
use crate::services::query::text_match::{
    build_related_token_candidates, common_prefix_char_count, near_token_match,
    near_token_overlap_count, normalized_alnum_tokens,
    select_related_overlap_tokens_from_candidates,
};

use super::{
    question_intent::{
        classify_query_ir_intents, query_ir_has_focused_document_answer_intent,
        query_ir_targets_graph_entities_or_relationships,
    },
    retrieve::score_value,
    types::RuntimeMatchedChunk,
};

mod comparison;
mod focused_answer;
mod matching;
mod scope_resolution;

pub(crate) use comparison::{
    comparison_target_document_ids_from_values, promote_comparison_target_chunks,
    question_requests_multi_document_scope,
};
#[cfg(test)]
pub(crate) use focused_answer::document_focus_marker_hits;
pub(crate) use focused_answer::{concise_document_subject_label, focused_answer_document_id};
pub(crate) use matching::{
    explicit_document_reference_literal_is_present, explicit_document_reference_literals,
    explicit_target_document_ids_from_values, normalize_document_target_text,
    normalized_document_target_candidates,
};
pub(crate) use scope_resolution::{
    query_ir_allows_document_focus_scope, resolve_scoped_target_document_ids,
};

#[cfg(test)]
#[path = "document_target_tests.rs"]
mod tests;
