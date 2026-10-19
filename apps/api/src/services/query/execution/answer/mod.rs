use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use uuid::Uuid;

use crate::services::query::text_match::{
    label_terms, normalized_alnum_token_sequence, normalized_alnum_tokens,
    token_sequence_contains_tokens, token_sequence_exact_or_contains_tokens,
};
use crate::{
    domains::query_ir::{EntityRole, LiteralKind, QueryAct, QueryIR, QueryScope, QueryTargetKind},
    infra::knowledge_rows::{KnowledgeStructuredBlockRow, KnowledgeTechnicalFactRow},
    services::query::i18n::{self},
    shared::extraction::{
        record_jsonl::focused_record_unit_excerpt, table_summary::parse_table_column_summary,
        technical_facts::TechnicalFactKind,
    },
};

use super::consolidation::query_has_multi_document_setup_anchors;
use super::endpoint_answer::{
    build_multi_document_endpoint_answer_from_facts, build_single_endpoint_answer_from_facts,
};
pub(crate) use super::focused_document_answer::build_focused_document_answer;
use super::port_answer::{build_port_and_protocol_answer_from_facts, build_port_answer_from_facts};
use super::question_intent::{
    QuestionIntent, classify_question_or_ir_intents, query_ir_allows_procedure_runbook_target,
    query_ir_has_setup_configuration_target, query_ir_is_unambiguous_versioned_procedure,
    query_ir_requires_remediation_synthesis,
};
use crate::services::query::completion_policy::AnswerCompletionContract;
use crate::services::query::effective_query::current_question_segment;
use crate::shared::extraction::text_render::repair_technical_layout_noise;

use super::chunk_support::{
    chunk_is_setup_focus_command_path_anchor, command_dense_excerpt_for, excerpt_for,
    focused_excerpt_for,
};
use super::command_shape::{
    procedure_line_has_command_start, procedure_line_has_list_marker,
    shellish_line_has_source_syntax, shellish_token_file_artifact_name,
    shellish_token_has_artifact_preparation_signal, shellish_token_has_executable_name_shape,
    shellish_token_is_command_argument_signal, shellish_token_is_invocable_head,
    shellish_token_is_local_artifact, shellish_token_is_path_command_start,
    shellish_tokens_start_command, split_concatenated_local_artifact_token,
    strip_leading_numeric_order_marker,
};
use super::source_excerpt::{salient_source_excerpt_for, structured_literal_excerpt_for};
use super::technical_answer::build_exact_technical_literal_answer;
#[cfg(test)]
use super::technical_literals::technical_chunk_selection_score;
use super::technical_literals::{
    DocumentBalancedChunkSelection, extract_config_assignment_literals,
    extract_explicit_path_literals, extract_http_methods, extract_package_command_literals,
    extract_parameter_literals, extract_prefix_literals, extract_url_literals,
    select_document_balanced_chunks, technical_literal_focus_keywords,
};
use super::types::{AuthoritativeAnswerEvidence, RuntimeChunkScoreKind, RuntimeMatchedChunk};
use super::{
    MAX_ANSWER_BLOCKS, MAX_CHUNKS_PER_DOCUMENT, MIN_CHUNKS_PER_DOCUMENT, SOURCE_UNIT_CHUNK_KIND,
    build_table_row_grounded_answer, build_table_summary_grounded_answer,
    focus_token_overlap_count, focused_answer_document_id, query_ir_document_focus_tokens,
    question_asks_table_aggregation, source_context, source_profile, source_slice_requested_count,
};
use crate::services::query::latest_versions::{
    ReleaseSourceIdentity, compare_version_desc, extract_release_context_version,
    extract_semver_like_version, is_version_token_continuation, query_requests_latest_versions,
    requested_latest_version_count,
};

mod comparison_context;
mod context_rendering;
mod deterministic;
mod procedure_intent;
mod procedure_parsing;
mod procedure_projection;
mod procedure_selection;
mod prompt;
mod setup_configuration;
mod setup_literals;
mod source_slice;
mod source_unit_focus;
mod source_unit_inventory;
mod structural_identifiers;
mod structured_list;
mod version_changes;

use comparison_context::comparison_declaration_chunks;
use context_rendering::{
    ParsedSourceUnitText, indent_source_unit_body, is_source_profile_runtime_chunk,
    is_structured_source_unit_runtime_chunk, parse_source_unit_text,
};
use procedure_intent::{
    UpdateProcedureFocusModel, update_procedure_focus_model,
    update_procedure_text_has_bound_target_identity_runbook,
    update_procedure_text_target_identity_priority,
};
use procedure_parsing::{
    UpdateProcedureLine, line_has_command_signal, line_has_order_marker,
    split_dense_procedure_line, strip_leading_order_marker, update_procedure_line_blocks,
    update_procedure_line_has_version, update_procedure_step_from_line,
};
use procedure_projection::{
    command_token_values, structural_command_head_token, token_has_local_command_artifact,
    trim_command_boundary_token_decorations, update_procedure_block_has_setup_script_signature,
    update_procedure_chunk_text, update_procedure_command_head,
    update_procedure_command_heads_match, update_procedure_evidence_anchors,
    update_procedure_extract_is_typed_evidence, update_procedure_extracts_from_text,
    update_procedure_focused_structural_score, update_procedure_step_is_structural,
    update_procedure_text_focus_score,
};
use procedure_selection::{
    UpdateProcedureExtract, update_procedure_capped_command_score,
    update_procedure_command_candidate_bonus, update_procedure_normalized_step,
    update_procedure_steps_from_chunks,
};
use setup_configuration::SetupConfigurationParameterRow;
use setup_literals::{
    SetupConfigurationLiterals, extract_configuration_path_literals,
    extract_configuration_section_literals, extract_setup_configuration_command_literals,
    extract_setup_configuration_parameter_rows, filter_setup_configuration_parameters,
    push_unique_parameter_rows, push_unique_values,
    setup_configuration_default_value_is_assignment_scalar, setup_configuration_label_focus_score,
    setup_configuration_literal_key, setup_configuration_variant_score,
};
use source_slice::score_value;
use source_unit_focus::{
    StructuredSourceUnitFocus, expand_structured_source_unit_sibling_fields,
    source_unit_inventory_focused_document_ids,
    structured_source_unit_compact_root_inventory_score,
    structured_source_unit_field_overlap_score, structured_source_unit_inventory_focus_terms,
    structured_source_unit_inventory_matches_specific_terms,
    structured_source_unit_inventory_too_broad,
};
#[cfg(test)]
use source_unit_inventory::query_ir_allows_structured_source_unit_inventory;
use source_unit_inventory::{SourceUnitField, query_ir_has_typed_table_column_inventory_intent};
use structured_list::{
    DeterministicAnswerLabels, build_structured_list_grounded_answer, deterministic_answer_labels,
};

#[cfg(test)]
use context_rendering::{render_evidence_chunk_lines, render_ordered_source_slice_unit_section};
#[cfg(test)]
use procedure_intent::render_update_procedure_step;
#[cfg(test)]
use setup_configuration::question_requests_lifecycle_update_procedure;
#[cfg(test)]
use source_slice::build_ordered_source_units_answer;
#[cfg(test)]
use source_unit_focus::build_structured_source_unit_field_answer;
#[cfg(test)]
use structured_list::{
    StructuredListCandidate, extract_comment_list_candidates,
    structured_list_source_addresses_question_focus,
};

pub(crate) use context_rendering::{
    render_authoritative_technical_fact_section, render_prepared_segment_section,
    render_primary_chunk_section, render_targeted_evidence_chunk_section,
};
pub(super) use deterministic::augment_deterministic_grounded_answer_with_evidence;
pub(crate) use deterministic::build_deterministic_grounded_answer;
pub(super) use procedure_intent::build_update_procedure_sequence_answer;
#[cfg(test)]
pub(crate) use prompt::build_answer_prompt;
#[cfg(test)]
pub(super) use setup_configuration::build_setup_configuration_anchor_answer;
pub(super) use setup_configuration::{
    SetupConfigurationAnchorCandidate, build_setup_configuration_anchor_candidate,
};
pub(crate) use source_slice::build_ordered_source_slice_answer;
pub(super) use source_unit_inventory::build_structured_source_unit_inventory_answer;
pub(crate) use structural_identifiers::{
    answer_omits_structural_identifier_mapping_target, build_structural_identifier_mapping_answer,
};
pub(crate) use version_changes::build_exact_version_change_summary_answer;

#[cfg(test)]
mod context_rendering_tests;
#[cfg(test)]
mod document_label_tests;
#[cfg(test)]
mod procedure_intent_tests;
#[cfg(test)]
mod procedure_parsing_tests;
#[cfg(test)]
mod procedure_syntax_tests;
#[cfg(test)]
mod setup_configuration_tests;
#[cfg(test)]
mod source_slice_tests;
#[cfg(test)]
mod source_unit_inventory_tests;
#[cfg(test)]
mod structured_list_tests;
#[cfg(test)]
#[path = "test_support_tests.rs"]
mod test_support;
#[cfg(test)]
mod version_changes_tests;
