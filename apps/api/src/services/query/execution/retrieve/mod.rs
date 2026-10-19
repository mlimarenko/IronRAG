use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::{DateTime, Utc};
use futures::{future::join_all, stream};
use uuid::Uuid;

use super::chunk_support::chunk_answer_source_text;
pub(crate) use super::chunk_support::{
    chunk_is_setup_focus_command_path_anchor, command_dense_excerpt_for,
    current_document_revision_id, excerpt_for, focused_excerpt_for, map_chunk_hit,
};
use super::command_shape::{
    procedure_artifact_token_count,
    procedure_line_has_command_start as versioned_update_procedure_line_has_command_start,
    shellish_inline_token_starts_command, shellish_token_has_artifact_preparation_signal,
    shellish_token_has_external_artifact, shellish_token_is_local_artifact,
    shellish_tokens_from_text, strip_leading_procedure_order_marker,
};
use super::document_target::comparison_target_document_ids_from_values;
use super::fusion::{RrfFusionLane as RetrievalMergeLane, fuse_rrf_chunks, score_kind_priority};
use super::question_intent::{
    QuestionIntent, classify_query_ir_intents, has_question_intent,
    query_ir_has_focused_document_answer_intent, query_ir_has_setup_configuration_target,
    query_ir_requires_remediation_synthesis,
};
use super::retrieval_plan::{
    LaneResolution, LatestSelectionKind, RetrievalLane, RetrievalPlan as CompanionRetrievalPlan,
    RetrievalPlanningContext, explicit_content_anchor_requested, resolve_lane_result,
    resolve_text_search_config, reuse_or_execute_primary,
};
use super::source_profile::is_source_profile_chunk_row;
use super::technical_literals::{
    extract_config_assignment_literals, extract_explicit_path_literals,
    extract_package_command_literals, extract_parameter_literals,
    technical_literal_focus_keyword_segments,
};
use super::tuning::{DOCUMENT_IDENTITY_SCORE_FLOOR, FOCUS_BROADEN_MIN_CHUNKS};
use super::types::{
    QueryGraphIndex, RetrievalBundle, RuntimeChunkScoreKind, RuntimeMatchedChunk,
    RuntimeMatchedEntity, RuntimeMatchedRelationship, RuntimeVectorSearchContext,
};
use super::{
    GraphTargetEntityCoverageField, GraphTargetEntityCoverageFieldKind, GraphTargetEntityProfile,
    associative_edges_for_entities, focus_token_overlap_count, graph_target_entity_coverage_score,
    load_initial_table_rows_for_documents, load_table_rows_for_documents,
    load_table_section_sibling_chunks, load_table_summary_chunks_for_documents,
    merge_table_aggregation_chunks, query_ir_requests_table_section_siblings,
    query_relevant_graph_evidence_target_hits, question_asks_table_aggregation,
    requested_initial_table_row_count, resolve_scoped_target_document_ids,
};
use crate::{
    app::state::AppState,
    domains::{
        ai::AiBindingPurpose,
        content::{attachment_parent_page_id, source_page_id},
        query::RuntimeQueryMode,
        query_ir::{
            EntityMention, EntityRole, LiteralKind, QueryAct, QueryIR, QueryScope, QueryTargetKind,
            TemporalConstraint, literal_kind_has_exact_technical_shape,
        },
    },
    infra::{
        knowledge_plane::LexicalQueryOrigin,
        knowledge_rows::{
            KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeLibraryGenerationRow,
            KnowledgeRevisionRow,
        },
        repositories,
    },
    services::{
        content::document_hint::resolve_document_hint,
        knowledge::runtime_read::load_active_runtime_graph_projection,
        query::{
            effective_query::current_question_segment,
            error::QueryServiceError,
            latest_versions::{
                LATEST_VERSION_CHUNKS_PER_DOCUMENT, ReleaseSourceIdentity, compare_version_desc,
                extract_release_context_version, extract_semver_like_version,
                latest_version_chunk_score, latest_version_context_top_k,
                latest_version_scope_terms, query_requests_latest_versions,
                requested_latest_version_count, text_has_release_version_marker,
            },
            planner::{RuntimeQueryPlan, strip_leading_question_marker},
            text_match::{
                common_prefix_char_count, near_token_match, near_token_overlap_count,
                normalized_alnum_token_sequence, normalized_alnum_tokens,
                prefix_token_sequence_contains_tokens, short_acronym_identity_tokens,
                token_sequence_contains, token_sequence_contains_tokens,
            },
            vector_dimensions::{
                EmbeddingProfileIndexState, EmbeddingProfileInventoryVersion,
                ensure_active_embedding_profile_key,
                ensure_embedding_profile_inventory_version_current,
                ensure_library_embedding_profile_indexed, load_embedding_profile_inventory_version,
                validate_embedding_vector_dimensions,
            },
        },
    },
    shared::extraction::text_render::repair_technical_layout_noise,
};

mod chunk_budget;
mod chunk_reservations;
mod companion_lanes;
mod content_anchors;
mod document_expansion;
mod final_shaping;
mod fusion;
mod graph_evidence_context;
mod graph_evidence_loading;
mod graph_evidence_search;
mod indexes;
mod lane_execution;
mod latest_version_loading;
mod latest_version_selection;
mod primary_lanes;
mod query_builders;
mod query_focus;
mod retrieval_execution;
mod setup_candidates;
mod setup_focus;
mod vector_context;
mod versioned_update;

#[cfg(test)]
pub(crate) use chunk_budget::truncate_bundle;
pub(crate) use chunk_budget::truncate_bundle_with_semantic_chunk_ranks;
use chunk_budget::{
    best_versioned_update_procedure_runbook_anchor_chunk, structural_literal_inventory_score,
};
#[cfg(test)]
use chunk_budget::{
    query_ir_requests_setup_variant_anchor_reservation, truncate_chunks_for_context,
};
use chunk_reservations::{
    ensure_anchor_chunks_retained, ensure_content_anchor_chunks_retained,
    ensure_exact_literal_context_anchors_retained, ensure_setup_focus_anchor_retained,
    ensure_setup_variant_anchors_retained,
    ensure_versioned_update_procedure_runbook_anchor_retained,
    ensure_versioned_update_procedure_runbook_chunk_retained, query_multi_document_relevance_terms,
    setup_variant_anchor_family, setup_variant_anchor_family_model,
    source_context_reservation_count, truncate_chunks_for_multi_document_scope,
    truncate_chunks_with_source_context_reservation,
};
use companion_lanes::{load_initial_companion_chunks, merge_initial_companion_chunks};
#[cfg(test)]
use content_anchors::content_anchor_revision_ids;
#[cfg(test)]
pub(crate) use content_anchors::{RetrievalContentAnchorModel, content_anchor_row_score};
use content_anchors::{
    load_content_anchor_chunks, load_document_evidence_anchor_chunks, load_query_ir_focus_chunks,
};
#[cfg(test)]
pub(crate) use document_expansion::linked_anchor_hydration_target_filter;
#[cfg(test)]
use document_expansion::{
    artifact_sibling_source_chunk_score, artifact_sibling_source_document_ids,
    artifact_sibling_source_revision_ids,
};
use document_expansion::{load_artifact_sibling_source_chunks, load_linked_anchor_context_chunks};
use final_shaping::{apply_final_chunk_shaping, load_post_merge_companion_chunks};
#[cfg(test)]
pub(crate) use fusion::merge_query_ir_focus_chunks;
pub(crate) use fusion::{
    merge_chunks, merge_entity_bio_chunks, merge_graph_evidence_chunks,
    query_ir_promotes_graph_evidence, score_desc_chunks, score_value,
};
use fusion::{
    merge_query_ir_focus_chunks_for_query, merge_versioned_update_procedure_chunks_for_query,
};
#[cfg(test)]
pub(crate) use graph_evidence_context::graph_evidence_targets;
use graph_evidence_context::{
    GraphEvidenceContextCandidate, graph_evidence_context_candidate_fields,
    graph_evidence_context_fetch_cap, graph_evidence_context_focus_texts,
    graph_evidence_context_focus_tokens, graph_evidence_context_line_focus_keywords,
    score_graph_evidence_context_candidates,
};
pub(crate) use graph_evidence_context::{
    GraphEvidenceContextRequest, apply_graph_evidence_texts_to_chunks, graph_evidence_context_line,
    graph_evidence_targets_for_query, rank_graph_evidence_context_rows,
};
use graph_evidence_loading::{
    GraphEvidenceChunkHits, GraphEvidenceSourceDocumentCandidate, GraphEvidenceTextsByChunk,
    RankedGraphEvidenceRows, load_entity_bio_chunks,
};
pub(crate) use graph_evidence_loading::{
    content_anchor_chunk_score, graph_evidence_chunk_score, graph_evidence_context_top_k,
    load_graph_evidence_chunks_for_bundle, query_ir_focus_chunk_score,
    query_ir_focus_context_top_k,
};
#[cfg(test)]
pub(crate) use graph_evidence_loading::{
    entity_bio_chunk_score, entity_bio_target_mentions, graph_evidence_source_document_ids,
    graph_evidence_source_document_ids_with_priority,
};
pub(crate) use graph_evidence_search::GraphEvidenceScope;
pub(crate) use graph_evidence_search::graph_evidence_chunk_hits_from_rows;
use graph_evidence_search::load_ranked_graph_evidence_rows_for_query;
#[cfg(test)]
pub(crate) use graph_evidence_search::{
    graph_evidence_source_document_ids_from_scored_targets,
    graph_evidence_text_search_document_scope,
};
use indexes::{
    ARTIFACT_SIBLING_SOURCE_CHUNKS_PER_DOCUMENT, ARTIFACT_SIBLING_SOURCE_DOCUMENT_CAP,
    ARTIFACT_SIBLING_SOURCE_FOCUS_TERM_CAP, ARTIFACT_SIBLING_SOURCE_SCORE_BASE,
    ARTIFACT_SIBLING_SOURCE_SCORE_STEP, ARTIFACT_SIBLING_SOURCE_STRUCTURAL_CHUNKS_PER_DOCUMENT,
    CONTENT_ANCHOR_CHUNK_CAP, CONTENT_ANCHOR_CHUNK_SCORE_BASE, CONTENT_ANCHOR_CHUNK_SCORE_STEP,
    CONTENT_ANCHOR_CHUNKS_PER_REVISION, CONTENT_ANCHOR_CONTEXT_RESERVATION_LIMIT,
    CONTENT_ANCHOR_EVIDENCE_SCORE_STEP, CONTENT_ANCHOR_MIN_TOKEN_OVERLAP,
    CONTENT_ANCHOR_SEARCH_PREFIX_CHARS, CONTENT_ANCHOR_SEARCH_PREFIX_MIN_CHARS,
    CONTENT_ANCHOR_SEARCH_TERM_CAP, CONTENT_ANCHOR_SEQUENCE_CAP, CONTENT_ANCHOR_SEQUENCE_MIN_CHARS,
    CONTENT_ANCHOR_TOKEN_MIN_CHARS, DIRECT_TABLE_AGGREGATION_CHUNK_LIMIT,
    DIRECT_TABLE_AGGREGATION_ROW_LIMIT, DIRECT_TABLE_AGGREGATION_SUMMARY_LIMIT,
    DOCUMENT_EVIDENCE_ANCHOR_CANDIDATE_LIMIT, DOCUMENT_EVIDENCE_ANCHOR_CHUNKS_PER_DOCUMENT,
    DOCUMENT_EVIDENCE_CONTEXT_RESERVATION_LIMIT, DOCUMENT_IDENTITY_CHUNKS_PER_DOCUMENT,
    DOCUMENT_IDENTITY_FOCUS_PREFIX_CHARS, DOCUMENT_IDENTITY_FOCUSED_CHUNKS_PER_DOCUMENT,
    ENTITY_BIO_CHUNK_CAP, ENTITY_BIO_CHUNK_SCORE_BASE, ENTITY_BIO_CHUNK_SCORE_STEP,
    EXACT_LITERAL_CONTEXT_RESERVATION_LIMIT, GRAPH_EVIDENCE_CHUNK_CAP,
    GRAPH_EVIDENCE_CHUNK_SCORE_BASE, GRAPH_EVIDENCE_CHUNK_SCORE_STEP,
    GRAPH_EVIDENCE_CONTEXT_EVIDENCE_FIELD_WEIGHT, GRAPH_EVIDENCE_CONTEXT_FETCH_MULTIPLIER,
    GRAPH_EVIDENCE_CONTEXT_LINE_CAP, GRAPH_EVIDENCE_CONTEXT_LINE_CHARS,
    GRAPH_EVIDENCE_CONTEXT_SCORE_TOKEN_MIN_CHARS, GRAPH_EVIDENCE_CONTEXT_SOURCE_FIELD_WEIGHT,
    GRAPH_EVIDENCE_CONTEXT_TARGET_FIELD_WEIGHT, GRAPH_EVIDENCE_SOURCE_DOCUMENT_PRIORITY_CAP,
    GRAPH_EVIDENCE_TARGET_CAP, GRAPH_EVIDENCE_TEXTS_PER_CHUNK, LATEST_VERSION_SEMANTIC_CHUNK_CAP,
    LATEST_VERSION_SEMANTIC_CHUNK_SCAN_LIMIT, LATEST_VERSION_SEMANTIC_DEEP_CHUNK_SCAN_LIMIT,
    LATEST_VERSION_SEMANTIC_DEEP_DOCUMENT_CAP, LATEST_VERSION_SEMANTIC_DOCUMENT_CANDIDATE_CAP,
    LATEST_VERSION_SEMANTIC_UNSCOPED_IDENTITY_DOCUMENT_CAP_MULTIPLIER,
    LATEST_VERSION_SEMANTIC_UNSCOPED_ROWS_PER_DOCUMENT,
    LATEST_VERSION_STRUCTURAL_DENSITY_DOCUMENT_CAP, LATEST_VERSION_STRUCTURAL_DENSITY_MIN_ROWS,
    LATEST_VERSION_STRUCTURAL_DENSITY_REGEX, LATEST_VERSION_STRUCTURAL_MIN_DISTINCT_VERSIONS,
    LATEST_VERSION_STRUCTURAL_PROBE_DOCUMENT_CAP,
    LATEST_VERSION_STRUCTURAL_PROBE_SCOPED_DOCUMENT_CAP, LATEST_VERSION_WINDOW_FETCH_BATCH,
    LINKED_ANCHOR_CONTEXT_CHUNKS_PER_QUERY, LINKED_ANCHOR_CONTEXT_PREFIX_CHARS,
    LINKED_ANCHOR_CONTEXT_QUERY_CAP, LINKED_ANCHOR_CONTEXT_QUERY_PREFIX_CHARS,
    MAX_GRAPH_EVIDENCE_DB_TEXT_QUERIES, QUERY_IR_FOCUS_CHUNK_CAP, QUERY_IR_FOCUS_CHUNK_SCORE_BASE,
    QUERY_IR_FOCUS_CHUNK_SCORE_STEP, QUERY_IR_FOCUS_CHUNKS_PER_QUERY,
    QUERY_IR_FOCUS_SOURCE_CONTEXT_RESERVATION_LIMIT,
    RAW_SETUP_FOCUS_CANDIDATE_TOKEN_DOCUMENT_DIVISOR,
    RAW_SETUP_FOCUS_CANDIDATE_TOKEN_MAX_DOC_FREQUENCY_CAP,
    RAW_SETUP_FOCUS_CANDIDATE_TOKEN_MAX_DOC_FREQUENCY_FLOOR,
    RAW_SETUP_FOCUS_STRUCTURAL_DOCUMENT_SCORE_FLOOR, SETUP_FOCUS_CONFIG_PATH_EXTENSIONS,
    SETUP_FOCUS_DOCUMENT_CANDIDATE_CAP, SETUP_FOCUS_DOCUMENT_CHUNK_CAP,
    SETUP_FOCUS_DOCUMENT_FETCH_CONCURRENCY, SETUP_FOCUS_DOCUMENT_FORWARD_CHUNKS,
    SETUP_FOCUS_DOCUMENT_SCAN_CHUNKS, SETUP_FOCUS_DOCUMENT_SCORE_BASE, SETUP_VARIANT_CHUNK_CAP,
    SETUP_VARIANT_CHUNKS_PER_DOCUMENT, SETUP_VARIANT_DOCUMENT_CANDIDATE_CAP,
    SETUP_VARIANT_DOCUMENT_CAP, SETUP_VARIANT_DOCUMENT_FETCH_CAP,
    SETUP_VARIANT_DOCUMENT_FETCH_CONCURRENCY, TABLE_SECTION_SIBLING_CHUNK_LIMIT,
    TABLE_SECTION_SIBLING_LIMIT_PER_SECTION, VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT,
    VERSIONED_UPDATE_PROCEDURE_COMMAND_SIGNAL_SCORE_CAP,
    VERSIONED_UPDATE_PROCEDURE_CONTEXT_BACKWARD_CHUNKS,
    VERSIONED_UPDATE_PROCEDURE_CONTEXT_RESERVATION_LIMIT,
    VERSIONED_UPDATE_PROCEDURE_DOCUMENT_CANDIDATE_CAP,
    VERSIONED_UPDATE_PROCEDURE_DOCUMENT_EVIDENCE_RESERVATION_LIMIT,
    VERSIONED_UPDATE_PROCEDURE_DOCUMENT_FETCH_CONCURRENCY,
    VERSIONED_UPDATE_PROCEDURE_EVIDENCE_SEARCH_HIT_LIMIT,
    VERSIONED_UPDATE_PROCEDURE_EVIDENCE_SEARCH_QUERY_CAP,
    VERSIONED_UPDATE_PROCEDURE_EVIDENCE_STRUCTURAL_SCORE_FLOOR,
    VERSIONED_UPDATE_PROCEDURE_MATCH_PROBE_CHUNKS_PER_DOCUMENT,
    VERSIONED_UPDATE_PROCEDURE_RESERVED_NEIGHBORS_PER_ANCHOR,
    VERSIONED_UPDATE_PROCEDURE_SCORE_BASE, VERSIONED_UPDATE_PROCEDURE_SUBJECT_TITLE_RESERVE_CAP,
};
#[cfg(test)]
use indexes::{QueryDocumentIndexRow, query_document_index_row_to_knowledge_document_row};
pub(crate) use indexes::{
    load_document_index, load_graph_index, load_latest_library_generation, query_graph_status,
};
pub(crate) use lane_execution::{
    DocumentChunkRetrievalRequest, retrieve_document_chunks, should_broaden_focus,
};
use lane_execution::{
    FinalChunkShapingRequest, InitialCompanionChunks, InitialCompanionRequest,
    MergedCompanionChunks, PostMergeCompanionRequest, RetrievalExecutionRequest,
    RetrievalLaneContext, record_stage_elapsed, resolve_planned_chunk_lane, run_planned_chunk_lane,
    timed_stage,
};
use latest_version_loading::{
    LatestVersionSemanticRequest, load_latest_version_document_chunks,
    load_latest_version_semantic_document_chunks, select_latest_version_documents,
};
pub(crate) use latest_version_selection::{LatestVersionDocument, latest_version_documents};
use latest_version_selection::{
    LatestVersionSemanticDocument, LatestVersionSemanticRow,
    latest_version_semantic_candidate_documents, latest_version_semantic_rows_from_chunk_rows,
    latest_version_structural_density_candidate_documents,
    latest_version_structural_inventory_candidate_has_density,
    latest_version_structural_probe_candidate_documents, order_latest_version_semantic_rows,
};
#[cfg(test)]
use latest_version_selection::{
    dedupe_latest_version_documents, latest_version_context_versions,
    latest_version_document_identity_text, latest_version_identity_structural_score,
    latest_version_structural_inventory_text_score,
};
use primary_lanes::{
    ChunkRetrievalLaneOutcome, PrimaryRetrievalRequest, document_identity_chunk_score,
    execute_primary_retrieval, load_document_identity_chunks_for_targets,
    retain_entity_bio_candidates, versioned_update_procedure_chunk_score,
};
#[cfg(test)]
use primary_lanes::{combine_chunk_retrieval_lanes, document_identity_focus_terms};
use query_builders::{
    MAX_CHUNKS_PER_DOCUMENT, MAX_CONCURRENT_LEXICAL_QUERIES, diversify_chunks_by_document,
};
pub(crate) use query_builders::{
    build_graph_evidence_text_queries, build_lexical_queries, graph_evidence_db_text_queries,
};
use query_focus::{adjacent_query_ir_focus_compounds, is_usable_query_ir_focus};
pub(crate) use query_focus::{
    linked_anchor_focus_queries, query_ir_focus_search_queries, query_ir_lexical_focus_queries,
    request_safe_query, retain_current_document_head_chunks,
};
#[cfg(test)]
pub(crate) use retrieval_execution::retain_scoped_documents;
use retrieval_execution::{
    combine_lexical_query_results, combine_query_ir_focus_search_results,
    latest_version_scoped_document_ids, retrieve_document_chunks_with_targets,
};
use setup_candidates::{
    load_setup_focus_document_chunks, load_setup_variant_document_chunks,
    query_ir_has_specific_adjacent_entity_identity, question_requests_setup_variant_evidence,
    soft_token_match,
};
#[cfg(test)]
use setup_candidates::{
    setup_variant_candidate_document_ids, setup_variant_document_family,
    setup_variant_family_model, setup_variant_query_terms,
};
use setup_focus::{
    document_evidence_anchor_candidate_document_ids, document_evidence_anchor_focus_terms,
    normalize_document_identity_value, query_ir_has_setup_focus_identity,
    query_ir_has_setup_focus_target, query_ir_requests_setup_focus_document_candidates,
    raw_setup_focus_text_structural_score, select_setup_focus_document_chunks,
    select_setup_focus_document_rows, setup_focus_candidate_document_ids,
    setup_focus_document_candidate_score, setup_focus_document_chunk_score,
    setup_focus_document_identity_values, setup_focus_document_is_standalone_image,
    setup_focus_document_label_identity_score, setup_focus_identity_tokens,
};
#[cfg(test)]
use setup_focus::{setup_focus_query_identity_terms, setup_focus_row_score};
use vector_context::batch_hydrate_hits;
#[cfg(test)]
use vector_context::is_answer_driving_search_chunk_row;
pub(crate) use vector_context::{
    expanded_candidate_limit, prepare_runtime_vector_search_context, should_skip_vector_search,
    validate_runtime_vector_search_context,
};
pub(in crate::services::query::execution) use versioned_update::query_ir_requests_versioned_update_procedure_context;
pub(crate) use versioned_update::{
    chunk_is_versioned_update_instruction_title_anchor,
    versioned_update_exact_target_runbook_score, versioned_update_procedure_runbook_anchor_score,
};
use versioned_update::{
    load_versioned_update_procedure_chunks,
    load_versioned_update_procedure_source_local_runbook_chunks,
    question_requests_versioned_update_procedure_evidence,
    versioned_update_procedure_chunk_runbook_evidence, versioned_update_procedure_focus_model,
    versioned_update_procedure_focus_terms,
};
#[cfg(test)]
use versioned_update::{
    ordered_step_looks_like_sequential_record,
    versioned_update_procedure_evidence_supports_runbook, versioned_update_procedure_text_evidence,
    versioned_update_procedure_text_has_target_identity_sequence,
};

#[cfg(test)]
mod test_prelude {
    pub(super) use chrono::TimeZone;
    pub(super) use serde_json::json;

    pub(super) use super::super::graph_retrieval::graph_target_entity_profiles;
    pub(super) use super::super::{
        is_table_analytics_chunk, normalized_document_target_candidates, source_slice_context_top_k,
    };
    pub(super) use super::{
        ARTIFACT_SIBLING_SOURCE_SCORE_BASE, BTreeSet, DOCUMENT_IDENTITY_SCORE_FLOOR,
        GraphEvidenceContextRequest, HashMap, HashSet, KnowledgeChunkRow, KnowledgeDocumentRow,
        KnowledgeRevisionRow, LATEST_VERSION_SEMANTIC_CHUNK_CAP,
        LATEST_VERSION_SEMANTIC_DOCUMENT_CANDIDATE_CAP, LatestVersionDocument,
        LatestVersionSemanticRow, MAX_CHUNKS_PER_DOCUMENT, QueryDocumentIndexRow, QueryGraphIndex,
        RetrievalBundle, RetrievalContentAnchorModel, RuntimeChunkScoreKind, RuntimeMatchedChunk,
        RuntimeMatchedEntity, RuntimeMatchedRelationship, RuntimeQueryPlan,
        SETUP_FOCUS_DOCUMENT_CANDIDATE_CAP, SETUP_FOCUS_DOCUMENT_CHUNK_CAP,
        SETUP_FOCUS_DOCUMENT_SCORE_BASE, SETUP_VARIANT_CHUNKS_PER_DOCUMENT, Utc, Uuid,
        apply_graph_evidence_texts_to_chunks, artifact_sibling_source_chunk_score,
        artifact_sibling_source_document_ids, artifact_sibling_source_revision_ids,
        build_lexical_queries, chunk_answer_source_text, combine_chunk_retrieval_lanes,
        combine_lexical_query_results, combine_query_ir_focus_search_results,
        command_dense_excerpt_for, compare_version_desc, content_anchor_revision_ids,
        content_anchor_row_score, current_document_revision_id, dedupe_latest_version_documents,
        diversify_chunks_by_document, document_evidence_anchor_candidate_document_ids,
        document_identity_chunk_score, document_identity_focus_terms, entity_bio_chunk_score,
        entity_bio_target_mentions, extract_semver_like_version,
        graph_evidence_chunk_hits_from_rows, graph_evidence_chunk_score,
        graph_evidence_context_line, graph_evidence_source_document_ids,
        graph_evidence_source_document_ids_from_scored_targets,
        graph_evidence_source_document_ids_with_priority, graph_evidence_targets,
        graph_evidence_targets_for_query, graph_evidence_text_search_document_scope,
        is_answer_driving_search_chunk_row, latest_version_chunk_score,
        latest_version_context_top_k, latest_version_context_versions,
        latest_version_document_identity_text, latest_version_documents,
        latest_version_identity_structural_score, latest_version_scope_terms,
        latest_version_scoped_document_ids, latest_version_semantic_candidate_documents,
        latest_version_structural_inventory_candidate_has_density,
        latest_version_structural_inventory_text_score,
        latest_version_structural_probe_candidate_documents, linked_anchor_focus_queries,
        linked_anchor_hydration_target_filter, map_chunk_hit, merge_chunks,
        merge_entity_bio_chunks, merge_graph_evidence_chunks, merge_query_ir_focus_chunks,
        merge_table_aggregation_chunks, order_latest_version_semantic_rows,
        ordered_step_looks_like_sequential_record,
        query_document_index_row_to_knowledge_document_row, query_ir_focus_chunk_score,
        query_ir_focus_search_queries, query_ir_lexical_focus_queries,
        query_ir_promotes_graph_evidence, query_ir_requests_setup_focus_document_candidates,
        query_ir_requests_setup_variant_anchor_reservation,
        query_ir_requests_versioned_update_procedure_context, query_requests_latest_versions,
        question_requests_setup_variant_evidence,
        question_requests_versioned_update_procedure_evidence, rank_graph_evidence_context_rows,
        requested_initial_table_row_count, requested_latest_version_count,
        retain_current_document_head_chunks, retain_entity_bio_candidates, retain_scoped_documents,
        score_value, select_latest_version_documents, select_setup_focus_document_chunks,
        select_setup_focus_document_rows, setup_focus_candidate_document_ids,
        setup_focus_document_candidate_score, setup_focus_document_chunk_score,
        setup_focus_document_label_identity_score, setup_focus_query_identity_terms,
        setup_focus_row_score, setup_variant_candidate_document_ids, setup_variant_document_family,
        setup_variant_family_model, setup_variant_query_terms, should_skip_vector_search,
        text_has_release_version_marker, truncate_bundle,
        truncate_bundle_with_semantic_chunk_ranks, truncate_chunks_for_context,
        versioned_update_exact_target_runbook_score,
        versioned_update_procedure_evidence_supports_runbook,
        versioned_update_procedure_focus_model, versioned_update_procedure_text_evidence,
        versioned_update_procedure_text_has_target_identity_sequence,
    };
    pub(super) use crate::domains::query_ir::{
        ComparisonSpec, DocumentHint, EntityMention, EntityRole, LiteralKind, LiteralSpan,
        QueryAct, QueryIR, QueryLanguage, QueryScope, QueryTargetKind, SourceSliceDirection,
        SourceSliceFilter, SourceSliceSpec, UnresolvedRef,
    };
    pub(super) use crate::infra::repositories::{
        RuntimeGraphEvidenceRow, RuntimeGraphQueryNodeRow,
    };
    pub(super) use crate::services::knowledge::runtime_read::ActiveRuntimeGraphProjection;
    pub(super) use crate::services::query::planner::{QueryIntentProfile, build_query_plan};

    pub(super) use super::content_anchors_tests::{
        comparison_query_ir, exact_error_code_query_ir, release_query_ir,
        release_query_ir_with_source_slice, release_query_ir_with_source_slice_count,
        target_entities_query_ir, transport_inventory_query_ir,
    };
    pub(super) use super::fusion_tests::{
        latest_version_identity_chunk, runtime_chunk, runtime_chunk_for_document,
    };
    pub(super) use super::primary_lanes_tests::{
        graph_index_with_nodes, knowledge_chunk_row, runtime_graph_evidence_row,
        runtime_graph_node, sample_document_row,
    };
    pub(super) use super::runtime_chunk_test_support_tests::{
        latest_version_runtime_chunk, setup_focus_runtime_chunk,
    };
    pub(super) use super::test_support_tests::{
        answer_source_chunk, chunk_row, document_row, document_row_with_source,
        low_confidence_untyped_query_ir, revision_row, setup_query_ir,
    };
}

#[cfg(test)]
mod chunk_budget_source_tests;
#[cfg(test)]
mod chunk_budget_tests;
#[cfg(test)]
mod chunk_reservations_setup_tests;
#[cfg(test)]
mod chunk_reservations_tests;
#[cfg(test)]
mod chunk_source_tests;
#[cfg(test)]
mod content_anchors_tests;
#[cfg(test)]
mod document_expansion_tests;
#[cfg(test)]
mod fusion_score_tests;
#[cfg(test)]
mod fusion_tests;
#[cfg(test)]
mod graph_evidence_context_tests;
#[cfg(test)]
mod latest_version_inventory_tests;
#[cfg(test)]
mod latest_version_loading_tests;
#[cfg(test)]
mod latest_version_selection_tests;
#[cfg(test)]
mod primary_lanes_tests;
#[cfg(test)]
mod query_focus_tests;
#[cfg(test)]
mod runtime_chunk_test_support_tests;
#[cfg(test)]
mod setup_candidates_tests;
#[cfg(test)]
mod setup_focus_tests;
#[cfg(test)]
mod test_support_tests;
#[cfg(test)]
mod versioned_update_tests;
