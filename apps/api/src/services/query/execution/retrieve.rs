use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use anyhow::Context;
use chrono::{DateTime, Datelike, Utc};
use futures::{StreamExt, future::join_all, stream};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        ai::AiBindingPurpose,
        content::{attachment_parent_page_id, revision_text_state_is_readable, source_page_id},
        provider_profiles::EffectiveProviderProfile,
        query::RuntimeQueryMode,
        query_ir::{
            EntityRole, LiteralKind, QueryAct, QueryIR, QueryScope, TemporalConstraint,
            literal_kind_has_exact_technical_shape,
        },
    },
    infra::{
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
            effective_query::{current_question_segment, structured_current_question_segment},
            latest_versions::{
                LATEST_VERSION_CHUNKS_PER_DOCUMENT, compare_version_desc,
                extract_release_context_version, extract_semver_like_version,
                latest_version_chunk_score, latest_version_context_top_k,
                latest_version_family_key, latest_version_scope_terms,
                query_requests_latest_versions, requested_latest_version_count,
                text_has_release_version_marker,
            },
            planner::{RuntimeQueryPlan, strip_leading_question_marker},
            text_match::{
                add_label_terms_with_acronyms, common_prefix_char_count, near_token_match,
                near_token_overlap_count, normalized_alnum_token_sequence, normalized_alnum_tokens,
                token_sequence_contains, token_sequence_contains_tokens,
            },
            vector_dimensions::{
                library_vector_index_dimensions, validate_embedding_vector_dimensions,
            },
        },
    },
    shared::extraction::{
        record_jsonl::focused_record_unit_excerpt, text_render::repair_technical_layout_noise,
    },
};

use super::question_intent::{
    QuestionIntent, canonical_target_type_tag, classify_query_ir_intents, has_question_intent,
    query_ir_allows_procedure_runbook_target, query_ir_has_focused_document_answer_intent,
    query_ir_has_setup_configuration_target, query_ir_is_unambiguous_versioned_procedure,
};
use super::source_profile::is_source_profile_chunk_row;
use super::technical_literals::{
    extract_config_assignment_literals, extract_config_section_literals,
    extract_explicit_path_literals, extract_package_command_literals, extract_parameter_literals,
    technical_literal_focus_keyword_segments,
};
use super::tuning::{DOCUMENT_IDENTITY_SCORE_FLOOR, FOCUS_BROADEN_MIN_CHUNKS};
use super::types::*;
use super::{
    GraphTargetEntityCoverageField, GraphTargetEntityCoverageFieldKind, GraphTargetEntityProfile,
    associative_edges_for_entities, focus_token_overlap_count, graph_target_entity_coverage_score,
    load_initial_table_rows_for_documents, load_table_rows_for_documents,
    load_table_section_sibling_chunks, load_table_summary_chunks_for_documents,
    merge_canonical_table_aggregation_chunks, query_ir_requests_table_section_siblings,
    query_relevant_graph_evidence_target_hits, question_asks_table_aggregation,
    requested_initial_table_row_count, resolve_scoped_target_document_ids,
};

const DIRECT_TABLE_AGGREGATION_SUMMARY_LIMIT: usize = 32;
const DIRECT_TABLE_AGGREGATION_ROW_LIMIT: usize = 24;
const DIRECT_TABLE_AGGREGATION_CHUNK_LIMIT: usize = 32;
const TABLE_SECTION_SIBLING_LIMIT_PER_SECTION: usize = 32;
const TABLE_SECTION_SIBLING_CHUNK_LIMIT: usize = 40;
const DOCUMENT_EVIDENCE_ANCHOR_CANDIDATE_LIMIT: usize = 3;
const DOCUMENT_EVIDENCE_ANCHOR_CHUNKS_PER_DOCUMENT: usize = 3;
const DOCUMENT_EVIDENCE_CONTEXT_RESERVATION_LIMIT: usize = 2;
const DOCUMENT_IDENTITY_CHUNKS_PER_DOCUMENT: usize = 3;
const DOCUMENT_IDENTITY_FOCUSED_CHUNKS_PER_DOCUMENT: usize = 4;
const EXACT_LITERAL_CONTEXT_RESERVATION_LIMIT: usize = 3;
const FALLBACK_LATEST_VERSION_DEFAULT_COUNT: usize = FALLBACK_LATEST_VERSION_MAX_COUNT;
const FALLBACK_LATEST_VERSION_MAX_COUNT: usize = 10;
const DOCUMENT_IDENTITY_FOCUS_PREFIX_CHARS: usize = 6;
const LINKED_ANCHOR_CONTEXT_QUERY_CAP: usize = 4;
const LINKED_ANCHOR_CONTEXT_CHUNKS_PER_QUERY: usize = 4;
const LINKED_ANCHOR_CONTEXT_PREFIX_CHARS: usize = 6;
const LINKED_ANCHOR_CONTEXT_QUERY_PREFIX_CHARS: usize = 7;
const GRAPH_EVIDENCE_CONTEXT_FETCH_MULTIPLIER: usize = 3;
const GRAPH_EVIDENCE_CONTEXT_SCORE_TOKEN_MIN_CHARS: usize = 4;
const GRAPH_EVIDENCE_CONTEXT_EVIDENCE_FIELD_WEIGHT: usize = 4;
const GRAPH_EVIDENCE_CONTEXT_TARGET_FIELD_WEIGHT: usize = 2;
const GRAPH_EVIDENCE_CONTEXT_SOURCE_FIELD_WEIGHT: usize = 1;
const GRAPH_EVIDENCE_CONTEXT_LINE_CHARS: usize = 1600;
const GRAPH_EVIDENCE_SOURCE_DOCUMENT_PRIORITY_CAP: usize = 12;
const MAX_GRAPH_EVIDENCE_DB_TEXT_QUERIES: usize = 5;

pub(crate) async fn load_graph_index(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<QueryGraphIndex> {
    let projection = load_active_runtime_graph_projection(state, library_id)
        .await
        .context("failed to load active runtime graph projection for query")?;
    let mut all_node_positions = HashMap::with_capacity(projection.nodes.len());
    for (position, node) in projection.nodes.iter().enumerate() {
        all_node_positions.insert(node.id, position);
    }

    let mut edge_positions = HashMap::with_capacity(projection.edges.len());
    let mut connected_node_ids = HashSet::with_capacity(projection.edges.len().saturating_mul(2));
    for (position, edge) in projection.edges.iter().enumerate() {
        let Some(&from_position) = all_node_positions.get(&edge.from_node_id) else {
            continue;
        };
        let Some(&to_position) = all_node_positions.get(&edge.to_node_id) else {
            continue;
        };
        let from_node_key = projection.nodes[from_position].canonical_key.as_str();
        let to_node_key = projection.nodes[to_position].canonical_key.as_str();
        if !state.bulk_ingest_hardening_services.graph_quality_guard.allows_relation(
            from_node_key,
            to_node_key,
            &edge.relation_type,
        ) {
            continue;
        }
        edge_positions.insert(edge.id, position);
        connected_node_ids.insert(edge.from_node_id);
        connected_node_ids.insert(edge.to_node_id);
    }
    let node_positions = projection
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(position, node)| {
            (node.node_type == "document" || connected_node_ids.contains(&node.id))
                .then_some((node.id, position))
        })
        .collect();

    Ok(QueryGraphIndex::new(projection, node_positions, edge_positions))
}

pub(crate) async fn load_latest_library_generation(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<Option<KnowledgeLibraryGenerationRow>> {
    state
        .canonical_services
        .knowledge
        .derive_library_generation_rows(state, library_id)
        .await
        .map(|rows| rows.into_iter().next())
        .map_err(|error| {
            anyhow::anyhow!("failed to derive library generations for runtime query: {error}")
        })
}

pub(crate) fn query_graph_status(
    generation: Option<&KnowledgeLibraryGenerationRow>,
) -> &'static str {
    match generation {
        Some(row) if row.active_graph_generation > 0 && row.degraded_state == "ready" => "current",
        Some(row) if row.active_graph_generation > 0 => "partial",
        _ => "empty",
    }
}

pub(crate) async fn load_document_index(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<HashMap<Uuid, KnowledgeDocumentRow>> {
    let rows = sqlx::query_as::<_, QueryDocumentIndexRow>(
        "select
            d.id as document_id,
            d.workspace_id,
            d.library_id,
            d.external_key,
            r.title,
            r.source_uri,
            r.document_hint,
            d.document_state::text as document_state,
            h.active_revision_id,
            h.readable_revision_id,
            latest_revision.latest_revision_no,
            d.parent_document_id,
            d.document_role,
            d.created_at,
            coalesce(h.head_updated_at, d.created_at) as updated_at,
            d.deleted_at
         from content_document d
         left join content_document_head h on h.document_id = d.id
         left join content_revision r on r.id = coalesce(h.readable_revision_id, h.active_revision_id)
         left join lateral (
            select max(revision_number)::bigint as latest_revision_no
            from content_revision revision
            where revision.document_id = d.id
         ) latest_revision on true
         where d.library_id = $1
           and d.document_state = 'active'
           and d.deleted_at is null
         order by coalesce(h.head_updated_at, d.created_at) desc, d.id desc",
    )
    .bind(library_id)
    .fetch_all(&state.persistence.postgres)
    .await
    .context("failed to load runtime query document index from canonical content heads")?;
    Ok(rows
        .into_iter()
        .map(query_document_index_row_to_knowledge_document_row)
        .map(|row| (row.document_id, row))
        .collect())
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct QueryDocumentIndexRow {
    document_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    external_key: String,
    title: Option<String>,
    source_uri: Option<String>,
    document_hint: Option<String>,
    document_state: String,
    active_revision_id: Option<Uuid>,
    readable_revision_id: Option<Uuid>,
    latest_revision_no: Option<i64>,
    parent_document_id: Option<Uuid>,
    document_role: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    deleted_at: Option<DateTime<Utc>>,
}

fn query_document_index_row_to_knowledge_document_row(
    row: QueryDocumentIndexRow,
) -> KnowledgeDocumentRow {
    let file_name = Some(row.external_key.clone());
    KnowledgeDocumentRow {
        document_id: row.document_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        external_key: row.external_key,
        file_name,
        title: row.title,
        source_uri: row.source_uri,
        document_hint: row.document_hint,
        document_state: row.document_state,
        active_revision_id: row.active_revision_id,
        readable_revision_id: row.readable_revision_id,
        latest_revision_no: row.latest_revision_no,
        parent_document_id: row.parent_document_id,
        document_role: row.document_role,
        created_at: row.created_at,
        updated_at: row.updated_at,
        deleted_at: row.deleted_at,
    }
}

/// Ceiling on chunks pulled by the entity-bio fan-out. Bounded so
/// the concat with vector + lexical hits does not drown the context
/// window on entities that appear across dozens of documents.
const ENTITY_BIO_CHUNK_CAP: usize = 24;
const GRAPH_EVIDENCE_CHUNK_CAP: usize = 24;
const GRAPH_EVIDENCE_TARGET_CAP: usize = 48;
const QUERY_IR_FOCUS_CHUNK_CAP: usize = 32;
const QUERY_IR_FOCUS_SOURCE_CONTEXT_RESERVATION_LIMIT: usize = 8;
const QUERY_IR_FOCUS_CHUNKS_PER_QUERY: usize = 12;
const SETUP_FOCUS_DOCUMENT_CANDIDATE_CAP: usize = 128;
const SETUP_FOCUS_DOCUMENT_SCAN_CHUNKS: i32 = 32;
const SETUP_FOCUS_DOCUMENT_FORWARD_CHUNKS: i32 = 24;
const SETUP_FOCUS_DOCUMENT_CHUNK_CAP: usize = 24;
const SETUP_FOCUS_DOCUMENT_SCORE_BASE: f32 = DOCUMENT_IDENTITY_SCORE_FLOOR * 20.0;
const SETUP_VARIANT_DOCUMENT_CANDIDATE_CAP: usize = 64;
const SETUP_VARIANT_DOCUMENT_CAP: usize = 12;
const SETUP_VARIANT_DOCUMENT_FETCH_CAP: usize = SETUP_VARIANT_DOCUMENT_CAP * 2;
const SETUP_VARIANT_CHUNKS_PER_DOCUMENT: usize = 4;
const SETUP_VARIANT_CHUNK_CAP: usize = 48;
const SETUP_VARIANT_DOCUMENT_FETCH_CONCURRENCY: usize = 8;
const LATEST_VERSION_SEMANTIC_DOCUMENT_CANDIDATE_CAP: usize = 128;
const LATEST_VERSION_STRUCTURAL_PROBE_DOCUMENT_CAP: usize = 1024;
const LATEST_VERSION_STRUCTURAL_PROBE_SCOPED_DOCUMENT_CAP: usize = 256;
const LATEST_VERSION_SEMANTIC_CHUNK_SCAN_LIMIT: usize = 48;
const LATEST_VERSION_SEMANTIC_DEEP_DOCUMENT_CAP: usize = 1024;
const LATEST_VERSION_SEMANTIC_DEEP_CHUNK_SCAN_LIMIT: i32 = 512;
const LATEST_VERSION_SEMANTIC_CHUNK_CAP: usize = 40;
const LATEST_VERSION_SEMANTIC_UNSCOPED_IDENTITY_DOCUMENT_CAP_MULTIPLIER: usize = 2;
const LATEST_VERSION_SEMANTIC_UNSCOPED_ROWS_PER_DOCUMENT: usize = 10;
const LATEST_VERSION_STRUCTURAL_MIN_DISTINCT_VERSIONS: usize = 2;
const LATEST_VERSION_STRUCTURAL_DENSITY_DOCUMENT_CAP: usize = 96;
const LATEST_VERSION_STRUCTURAL_DENSITY_MIN_ROWS: i64 = 2;
const LATEST_VERSION_STRUCTURAL_DENSITY_REGEX: &str =
    r"(^|[^0-9.])[0-9]+\.[0-9]+(\.[0-9]+)?([^0-9.]|$)";
const VERSIONED_UPDATE_PROCEDURE_DOCUMENT_CANDIDATE_CAP: usize = 12;
const VERSIONED_UPDATE_PROCEDURE_TITLE_SCAN_CANDIDATE_CAP: usize =
    VERSIONED_UPDATE_PROCEDURE_DOCUMENT_CANDIDATE_CAP * 4;
const VERSIONED_UPDATE_PROCEDURE_ACTION_TITLE_RESERVE_CAP: usize = 3;
const VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT: usize = 8;
const VERSIONED_UPDATE_PROCEDURE_MATCH_PROBE_CHUNKS_PER_DOCUMENT: usize =
    VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT * 4;
const VERSIONED_UPDATE_PROCEDURE_CONTEXT_BACKWARD_CHUNKS: i32 = 1;
const VERSIONED_UPDATE_PROCEDURE_PROBE_SEED_ROWS_PER_DOCUMENT: usize = 3;
const VERSIONED_UPDATE_PROCEDURE_SCORE_PRIORITY_MARGIN: f32 = 32.0;
const VERSIONED_UPDATE_PROCEDURE_SCORE_BASE: f32 = SETUP_FOCUS_DOCUMENT_SCORE_BASE
    + DOCUMENT_IDENTITY_SCORE_FLOOR
    + VERSIONED_UPDATE_PROCEDURE_SCORE_PRIORITY_MARGIN;
const VERSIONED_UPDATE_PROCEDURE_CONTEXT_RESERVATION_LIMIT: usize =
    VERSIONED_UPDATE_PROCEDURE_DOCUMENT_CANDIDATE_CAP
        * VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT;
const VERSIONED_UPDATE_PROCEDURE_DOCUMENT_EVIDENCE_RESERVATION_LIMIT: usize = 8;
const VERSIONED_UPDATE_PROCEDURE_RESERVED_NEIGHBORS_PER_ANCHOR: usize = 2;
const VERSIONED_UPDATE_PROCEDURE_DOCUMENT_FETCH_CONCURRENCY: usize = 4;
const VERSIONED_UPDATE_PROCEDURE_EVIDENCE_DOCUMENT_CANDIDATE_CAP: usize = 24;
const VERSIONED_UPDATE_PROCEDURE_EVIDENCE_SEARCH_QUERY_CAP: usize = 6;
const VERSIONED_UPDATE_PROCEDURE_EVIDENCE_SEARCH_HIT_LIMIT: usize = 96;
const VERSIONED_UPDATE_PROCEDURE_REFERENCE_SEARCH_QUERY_CAP: usize = 4;
const VERSIONED_UPDATE_PROCEDURE_REFERENCE_DOCUMENT_CANDIDATE_CAP: usize = 32;
const VERSIONED_UPDATE_PROCEDURE_REFERENCE_CHUNK_CAP: usize = 24;
const VERSIONED_UPDATE_PROCEDURE_STRUCTURAL_SOURCE_DOCUMENT_CAP: usize = 8;
const VERSIONED_UPDATE_PROCEDURE_CONTEXTUAL_SOURCE_DOCUMENT_CAP: usize = 8;
const VERSIONED_UPDATE_PROCEDURE_STRUCTURAL_SOURCE_SCAN_CHUNKS: i32 = 96;
const VERSIONED_UPDATE_PROCEDURE_STRUCTURAL_SOURCE_CHUNKS_PER_DOCUMENT: usize = 4;
const VERSIONED_UPDATE_PROCEDURE_STRUCTURAL_SOURCE_CHUNK_CAP: usize = 24;
const VERSIONED_UPDATE_PROCEDURE_SOURCE_LOCAL_RUNBOOK_DOCUMENT_CAP: usize = 8;
const VERSIONED_UPDATE_PROCEDURE_SOURCE_LOCAL_RUNBOOK_ANCHORS_PER_DOCUMENT: usize = 4;
const VERSIONED_UPDATE_PROCEDURE_SOURCE_LOCAL_RUNBOOK_CONTEXT_LIMIT: usize = 16;
const VERSIONED_UPDATE_PROCEDURE_EVIDENCE_EXPANSION_DOCUMENT_CAP: usize = 24;
const VERSIONED_UPDATE_PROCEDURE_EVIDENCE_SEEDS_PER_DOCUMENT: usize = 3;
const VERSIONED_UPDATE_PROCEDURE_EVIDENCE_NEIGHBOR_BACKWARD_CHUNKS: i32 = 1;
const VERSIONED_UPDATE_PROCEDURE_EVIDENCE_NEIGHBOR_FORWARD_CHUNKS: i32 = 2;
const VERSIONED_UPDATE_PROCEDURE_BOUND_TARGET_LINE_WINDOW_MAX_CHARS: usize = 480;
const VERSIONED_UPDATE_PROCEDURE_BOUND_TARGET_LINE_WINDOW_MAX_LINES: usize = 6;
const VERSIONED_UPDATE_PROCEDURE_EVIDENCE_PRIORITY_BONUS: usize = 1_000_000;
const VERSIONED_UPDATE_PROCEDURE_TITLE_ALIGNMENT_PRIORITY_BONUS: usize =
    VERSIONED_UPDATE_PROCEDURE_EVIDENCE_PRIORITY_BONUS.saturating_mul(2);
const VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_IDENTITY_PRIORITY_BONUS: usize =
    VERSIONED_UPDATE_PROCEDURE_TITLE_ALIGNMENT_PRIORITY_BONUS.saturating_mul(4);
const VERSIONED_UPDATE_PROCEDURE_SEEDED_EXACT_PRIORITY_BONUS: usize =
    VERSIONED_UPDATE_PROCEDURE_TITLE_ALIGNMENT_PRIORITY_BONUS;
const VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_CHUNK_SCORE_BONUS: f32 = 5_000.0;
const VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_RESERVE_CAP: usize = 6;
const VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_RUNBOOK_SCAN_CAP: usize =
    VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_RESERVE_CAP * 4;
const VERSIONED_UPDATE_PROCEDURE_EXACT_TARGET_RUNBOOK_SCAN_CAP: usize =
    VERSIONED_UPDATE_PROCEDURE_DOCUMENT_CANDIDATE_CAP * 8;
const VERSIONED_UPDATE_PROCEDURE_EXACT_ACTION_TITLE_RESERVE_CAP: usize = 4;
const VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_RUNBOOK_CHUNK_SCORE_BONUS: f32 =
    VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_CHUNK_SCORE_BONUS * 5.0;
const VERSIONED_UPDATE_PROCEDURE_SUBJECT_TITLE_RESERVE_CAP: usize = 2;
const VERSIONED_UPDATE_PROCEDURE_STRONG_SUBJECT_IDENTITY_MIN: usize = 2;
const VERSIONED_UPDATE_PROCEDURE_EVIDENCE_STRUCTURAL_SCORE_FLOOR: usize = 3;
const VERSIONED_UPDATE_PROCEDURE_COMMAND_SIGNAL_SCORE_CAP: usize = 8;
const VERSIONED_UPDATE_PROCEDURE_TARGET_IDENTITY_PREFIX_MIN_CHARS: usize = 5;
const SETUP_FOCUS_CONFIG_PATH_EXTENSIONS: [&str; 8] =
    [".conf", ".ini", ".cfg", ".properties", ".yaml", ".yml", ".toml", ".json"];
const RAW_SETUP_FOCUS_CANDIDATE_TOKEN_MAX_DOC_FREQUENCY_FLOOR: usize = 8;
const RAW_SETUP_FOCUS_CANDIDATE_TOKEN_MAX_DOC_FREQUENCY_CAP: usize = 512;
const RAW_SETUP_FOCUS_CANDIDATE_TOKEN_DOCUMENT_DIVISOR: usize = 50;
const RAW_SETUP_FOCUS_PRIMARY_SUPPORT_CHUNK_LIMIT: usize = 16;
const RAW_SETUP_FOCUS_PRIMARY_LABEL_MIN_OVERLAP: usize = 2;
const RAW_SETUP_FOCUS_STRUCTURAL_DOCUMENT_SCORE_FLOOR: usize = 24;
const RAW_SETUP_FOCUS_STRUCTURAL_DOCUMENT_CHUNK_FLOOR: usize = 2;
const ARTIFACT_SIBLING_SOURCE_DOCUMENT_CAP: usize = 3;
const ARTIFACT_SIBLING_SOURCE_CHUNKS_PER_DOCUMENT: i32 = 4;
const ARTIFACT_SIBLING_SOURCE_STRUCTURAL_CHUNKS_PER_DOCUMENT: usize = 8;
const ARTIFACT_SIBLING_SOURCE_REVISION_CAP: usize = 8;
const ARTIFACT_SIBLING_SOURCE_FOCUS_TERM_CAP: usize = 24;
const ARTIFACT_SIBLING_SOURCE_SCORE_BASE: f32 = 1.1;
const ARTIFACT_SIBLING_SOURCE_SCORE_STEP: f32 = 0.01;

const ENTITY_BIO_CHUNK_SCORE_BASE: f32 = 1.0;
const ENTITY_BIO_CHUNK_SCORE_STEP: f32 = 0.001;
const GRAPH_EVIDENCE_CHUNK_SCORE_BASE: f32 = 1.25;
const GRAPH_EVIDENCE_CHUNK_SCORE_STEP: f32 = 0.001;
const QUERY_IR_FOCUS_CHUNK_SCORE_BASE: f32 = 1.5;
const QUERY_IR_FOCUS_CHUNK_SCORE_STEP: f32 = 0.001;
const CONTENT_ANCHOR_CHUNK_CAP: usize = 24;
const CONTENT_ANCHOR_CHUNKS_PER_REVISION: usize = 2;
const CONTENT_ANCHOR_SEARCH_TERM_CAP: usize = 24;
const CONTENT_ANCHOR_SEARCH_PREFIX_MIN_CHARS: usize = 6;
const CONTENT_ANCHOR_SEARCH_PREFIX_CHARS: usize = 5;
const CONTENT_ANCHOR_SEQUENCE_CAP: usize = 32;
const CONTENT_ANCHOR_TOKEN_MIN_CHARS: usize = 4;
const CONTENT_ANCHOR_SEQUENCE_MIN_CHARS: usize = 3;
const CONTENT_ANCHOR_MIN_TOKEN_OVERLAP: usize = 2;
const CONTENT_ANCHOR_CONTEXT_RESERVATION_LIMIT: usize = 4;
const CONTENT_ANCHOR_CHUNK_SCORE_BASE: f32 = 1.75;
const CONTENT_ANCHOR_CHUNK_SCORE_STEP: f32 = 0.001;
const CONTENT_ANCHOR_EVIDENCE_SCORE_STEP: f32 = 0.000_001;
const GRAPH_EVIDENCE_TEXTS_PER_CHUNK: usize = 4;
const GRAPH_EVIDENCE_CONTEXT_LINE_CAP: usize = 24;

#[derive(Debug, Clone)]
pub(crate) struct RuntimeGraphEvidenceRetrieval {
    pub(crate) chunks: Vec<RuntimeMatchedChunk>,
    pub(crate) context_lines: Vec<String>,
    pub(crate) source_document_ids: Vec<Uuid>,
}

pub(crate) fn entity_bio_chunk_score(rank: usize) -> f32 {
    ENTITY_BIO_CHUNK_SCORE_BASE - (rank as f32 * ENTITY_BIO_CHUNK_SCORE_STEP)
}

pub(crate) fn graph_evidence_chunk_score(rank: usize) -> f32 {
    GRAPH_EVIDENCE_CHUNK_SCORE_BASE - (rank as f32 * GRAPH_EVIDENCE_CHUNK_SCORE_STEP)
}

pub(crate) fn query_ir_focus_chunk_score(rank: usize) -> f32 {
    QUERY_IR_FOCUS_CHUNK_SCORE_BASE - (rank as f32 * QUERY_IR_FOCUS_CHUNK_SCORE_STEP)
}

pub(crate) fn content_anchor_chunk_score(rank: usize, evidence_score: usize) -> f32 {
    CONTENT_ANCHOR_CHUNK_SCORE_BASE - (rank as f32 * CONTENT_ANCHOR_CHUNK_SCORE_STEP)
        + (evidence_score.min(4096) as f32 * CONTENT_ANCHOR_EVIDENCE_SCORE_STEP)
}

pub(crate) fn graph_evidence_context_top_k(base_limit: usize) -> usize {
    base_limit.saturating_add(GRAPH_EVIDENCE_CHUNK_CAP)
}

pub(crate) fn query_ir_focus_context_top_k(base_limit: usize) -> usize {
    base_limit.saturating_add(QUERY_IR_FOCUS_CHUNK_CAP)
}

/// For entity-describe questions (`QueryAct::Describe` with at least one
/// target entity) the vector + lexical lanes can miss the full picture.
/// This helper fans out over the graph instead: match the entity label
/// against the admitted runtime graph, then load every chunk of evidence
/// attached to that node (capped at `ENTITY_BIO_CHUNK_CAP`). The caller
/// merges the result into the main retrieval set so the answer model sees
/// all mentions of the entity, not just the top-scored one.
async fn load_entity_bio_chunks(
    state: &AppState,
    library_id: Uuid,
    query_ir: Option<&QueryIR>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    targeted_document_ids: &BTreeSet<Uuid>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let Some(ir) = query_ir else {
        return Ok(Vec::new());
    };
    if ir.target_entities.is_empty() {
        return Ok(Vec::new());
    }

    let snapshot =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .context("failed to load graph projection snapshot for entity-bio retrieval")?;
    let Some(snapshot) = snapshot else {
        return Ok(Vec::new());
    };

    // Entity-bio is a proper-name fan-out. When the IR contains at least
    // one capitalized mention, restrict to capitalized ones; otherwise keep
    // the whole set so lower-case entity queries still work.
    let has_capitalized = ir.target_entities.iter().any(|m| {
        m.label.trim().chars().find(|c| c.is_alphabetic()).is_some_and(char::is_uppercase)
    });
    let proper_name_mentions: Vec<&_> = ir
        .target_entities
        .iter()
        .filter(|m| {
            if !has_capitalized {
                return true;
            }
            m.label.trim().chars().find(|c| c.is_alphabetic()).is_some_and(char::is_uppercase)
        })
        .collect();
    if proper_name_mentions.is_empty() {
        return Ok(Vec::new());
    }

    let mut seen_nodes: HashSet<Uuid> = HashSet::new();
    let mut evidence_targets = Vec::<(String, Uuid)>::new();
    let target_kind = "node".to_string();
    for mention in &proper_name_mentions {
        if mention.label.trim().is_empty() {
            continue;
        }
        let nodes = repositories::search_admitted_runtime_graph_entities_by_query_text(
            &state.persistence.postgres,
            library_id,
            snapshot.projection_version,
            &mention.label,
            4,
        )
        .await
        .context("failed to search graph entities by label for entity-bio retrieval")?;
        for node in nodes {
            if seen_nodes.insert(node.id) {
                evidence_targets.push((target_kind.clone(), node.id));
            }
        }
    }

    let mut all_evidence_chunk_ids: Vec<Uuid> = Vec::new();
    if !evidence_targets.is_empty() {
        let evidence_limit =
            evidence_targets.len().saturating_mul(ENTITY_BIO_CHUNK_CAP).min(i64::MAX as usize)
                as i64;
        let evidence = repositories::list_runtime_graph_evidence_by_targets(
            &state.persistence.postgres,
            library_id,
            &evidence_targets,
            evidence_limit,
        )
        .await
        .context("failed to list graph evidence for entity-bio retrieval")?;
        for row in evidence {
            if all_evidence_chunk_ids.len() >= ENTITY_BIO_CHUNK_CAP {
                break;
            }
            if let Some(chunk_id) = row.chunk_id
                && !all_evidence_chunk_ids.contains(&chunk_id)
            {
                all_evidence_chunk_ids.push(chunk_id);
            }
        }
    }

    // Graph-evidence is bounded by what the `extract_graph` stage
    // captured — low-confidence or oblique-case mentions often miss
    // that pass. Complement the graph lookup with a dedicated lexical
    // search over the entity label itself so every chunk where the
    // label appears as plain text contributes, not just the ones that
    // became evidence rows.
    let mut lexical_chunk_ids: Vec<Uuid> = Vec::new();
    for mention in &proper_name_mentions {
        if mention.label.trim().is_empty() {
            continue;
        }
        let remaining = ENTITY_BIO_CHUNK_CAP
            .saturating_sub(all_evidence_chunk_ids.len() + lexical_chunk_ids.len());
        if remaining == 0 {
            break;
        }
        let hits = state
            .search_store
            .search_chunks(library_id, mention.label.trim(), remaining.max(4), None, None)
            .await
            .context("failed to run lexical entity-label search for entity-bio retrieval")?;
        for hit in hits {
            if lexical_chunk_ids.len() + all_evidence_chunk_ids.len() >= ENTITY_BIO_CHUNK_CAP {
                break;
            }
            if all_evidence_chunk_ids.contains(&hit.chunk_id)
                || lexical_chunk_ids.contains(&hit.chunk_id)
            {
                continue;
            }
            lexical_chunk_ids.push(hit.chunk_id);
        }
    }

    if all_evidence_chunk_ids.is_empty() && lexical_chunk_ids.is_empty() {
        return Ok(Vec::new());
    }

    let evidence_chunk_id_set: HashSet<Uuid> = all_evidence_chunk_ids.iter().copied().collect();
    let mut all_ids = all_evidence_chunk_ids;
    all_ids.extend(lexical_chunk_ids.iter().copied());
    let candidate_total = all_ids.len();
    let hits: Vec<(Uuid, f32)> = all_ids
        .into_iter()
        .enumerate()
        .map(|(rank, id)| (id, entity_bio_chunk_score(rank)))
        .collect();
    let candidates =
        batch_hydrate_hits(state, hits, document_index, plan_keywords, targeted_document_ids)
            .await?;
    // Post-filter: full-text BM25 stems tokens, so a surname like
    // "Foster" can retrieve chunks mentioning "forest" that share a
    // stem but have nothing to do with the target person. Similarly,
    // a graph entity whose label contains the mention as substring may
    // attach evidence chunks that do not carry the name as plain text.
    // Keep only chunks whose raw text actually contains one of the
    // mention labels as a case-insensitive substring — this is the
    // literal grounding the answer model needs.
    let label_tokens: Vec<String> = proper_name_mentions
        .iter()
        .map(|m| m.label.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    let mut chunks =
        retain_entity_bio_candidates(candidates, &evidence_chunk_id_set, &label_tokens);
    for chunk in &mut chunks {
        chunk.score_kind = RuntimeChunkScoreKind::EntityBio;
    }
    tracing::info!(
        stage = "retrieval.entity_bio",
        entity_label_count = ir.target_entities.len(),
        evidence_node_count = seen_nodes.len(),
        lexical_extra_count = lexical_chunk_ids.len(),
        candidate_chunk_count = candidate_total,
        evidence_chunk_count = chunks.len(),
        "entity-bio fan-out loaded extra chunks for Describe-intent query",
    );
    Ok(chunks)
}

pub(crate) async fn load_graph_evidence_chunks_for_bundle(
    state: &AppState,
    library_id: Uuid,
    question: &str,
    entities: &[RuntimeMatchedEntity],
    relationships: &[RuntimeMatchedRelationship],
    plan: &RuntimeQueryPlan,
    query_ir: Option<&QueryIR>,
    target_entity_profiles: &[GraphTargetEntityProfile],
    graph_index: &QueryGraphIndex,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    targeted_document_ids: &BTreeSet<Uuid>,
    plan_keywords: &[String],
) -> anyhow::Result<RuntimeGraphEvidenceRetrieval> {
    let ranked_evidence = load_ranked_graph_evidence_rows_for_query(
        state,
        library_id,
        question,
        entities,
        relationships,
        plan,
        query_ir,
        target_entity_profiles,
        graph_index,
        targeted_document_ids,
        graph_evidence_context_fetch_cap(),
    )
    .await
    .context("failed to load ranked graph evidence rows for chunk hydration")?;
    let query_ir_focus_queries = query_ir.map(query_ir_lexical_focus_queries).unwrap_or_default();
    let text_queries =
        build_graph_evidence_text_queries(question, plan, &query_ir_focus_queries, query_ir);
    let context_focus_keywords =
        graph_evidence_context_line_focus_keywords(question, &text_queries);
    let context_source_labels = load_graph_evidence_context_source_labels(
        state,
        library_id,
        &ranked_evidence.rows,
        document_index,
    )
    .await;
    let context_lines = graph_evidence_context_lines_from_rows(
        &ranked_evidence.rows,
        graph_index,
        &context_source_labels,
        &context_focus_keywords,
    );
    let source_document_ids = graph_evidence_source_document_ids_with_priority(
        &ranked_evidence.target_source_document_ids,
        &ranked_evidence.rows,
    );
    let (hits, evidence_texts_by_chunk) =
        graph_evidence_chunk_hits_from_rows(&ranked_evidence.rows);
    if hits.is_empty() {
        tracing::info!(
            stage = "retrieval.graph_evidence",
            graph_target_count = ranked_evidence.graph_target_count,
            text_query_count = ranked_evidence.text_query_count,
            text_query_executed_count = ranked_evidence.text_query_executed_count,
            text_evidence_count = ranked_evidence.text_evidence_count,
            target_evidence_count = ranked_evidence.target_evidence_count,
            ranked_evidence_count = ranked_evidence.rows.len(),
            graph_evidence_line_count = context_lines.len(),
            evidence_chunk_count = 0usize,
            "graph evidence rows loaded without hydratable chunks",
        );
        return Ok(RuntimeGraphEvidenceRetrieval {
            chunks: Vec::new(),
            context_lines,
            source_document_ids,
        });
    }

    let mut chunks =
        batch_hydrate_hits(state, hits, document_index, plan_keywords, targeted_document_ids)
            .await
            .context("failed to hydrate graph evidence chunks")?;
    apply_graph_evidence_texts_to_chunks(
        &mut chunks,
        &evidence_texts_by_chunk,
        plan_keywords,
        &context_focus_keywords,
    );
    for chunk in &mut chunks {
        chunk.score_kind = RuntimeChunkScoreKind::GraphEvidence;
    }
    tracing::info!(
        stage = "retrieval.graph_evidence",
        graph_target_count = ranked_evidence.graph_target_count,
        text_query_count = ranked_evidence.text_query_count,
        text_query_executed_count = ranked_evidence.text_query_executed_count,
        text_evidence_count = ranked_evidence.text_evidence_count,
        target_evidence_count = ranked_evidence.target_evidence_count,
        ranked_evidence_count = ranked_evidence.rows.len(),
        graph_evidence_line_count = context_lines.len(),
        evidence_chunk_count = chunks.len(),
        "graph evidence chunks hydrated from ranked graph evidence rows",
    );
    Ok(RuntimeGraphEvidenceRetrieval { chunks, context_lines, source_document_ids })
}

pub(crate) fn graph_evidence_source_document_ids(
    rows: &[repositories::RuntimeGraphEvidenceRow],
) -> Vec<Uuid> {
    let mut seen = HashSet::new();
    rows.iter()
        .filter_map(|row| row.document_id)
        .filter(|document_id| seen.insert(*document_id))
        .collect()
}

pub(crate) fn graph_evidence_source_document_ids_with_priority(
    priority_document_ids: &[Uuid],
    rows: &[repositories::RuntimeGraphEvidenceRow],
) -> Vec<Uuid> {
    let mut seen = HashSet::new();
    let ranked_document_ids = graph_evidence_source_document_ids(rows);
    priority_document_ids
        .iter()
        .copied()
        .chain(ranked_document_ids)
        .filter(|document_id| seen.insert(*document_id))
        .collect()
}

async fn load_graph_evidence_context_source_labels(
    state: &AppState,
    library_id: Uuid,
    rows: &[repositories::RuntimeGraphEvidenceRow],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> HashMap<Uuid, String> {
    let mut revision_ids = Vec::new();
    let mut seen_revisions = HashSet::new();
    for row in rows {
        let Some(document_id) = row.document_id else {
            continue;
        };
        let Some(revision_id) =
            document_index.get(&document_id).and_then(canonical_document_revision_id)
        else {
            continue;
        };
        if seen_revisions.insert(revision_id) {
            revision_ids.push(revision_id);
        }
    }
    if revision_ids.is_empty() {
        return HashMap::new();
    }

    let library_setting = repositories::catalog_repository::get_library_by_id(
        &state.persistence.postgres,
        library_id,
    )
    .await
    .ok()
    .flatten()
    .map(|library| library.include_document_hint_in_mcp_answers)
    .unwrap_or(true);

    state
        .document_store
        .list_revisions_by_ids(&revision_ids)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|revision| {
            let document = document_index.get(&revision.document_id)?;
            let document_title = document.title.as_deref().or(Some(document.external_key.as_str()));
            let source_label = resolve_document_hint(
                &revision.revision_kind,
                revision.source_uri.as_deref(),
                revision.document_hint.as_deref(),
                document_title,
                library_setting,
            )?;
            let source_label = source_label.trim().to_string();
            (!source_label.is_empty()).then_some((revision.document_id, source_label))
        })
        .collect()
}

fn graph_evidence_context_lines_from_rows(
    rows: &[repositories::RuntimeGraphEvidenceRow],
    graph_index: &QueryGraphIndex,
    source_labels_by_document_id: &HashMap<Uuid, String>,
    focus_keywords: &[String],
) -> Vec<String> {
    let mut lines = Vec::with_capacity(rows.len());
    for row in rows.iter().take(GRAPH_EVIDENCE_CONTEXT_LINE_CAP) {
        let Some(line) = graph_evidence_context_line(
            row,
            graph_index,
            source_labels_by_document_id,
            focus_keywords,
        ) else {
            continue;
        };
        lines.push(line);
    }
    lines
}

#[derive(Debug)]
struct RankedGraphEvidenceRows {
    rows: Vec<repositories::RuntimeGraphEvidenceRow>,
    target_source_document_ids: Vec<Uuid>,
    graph_target_count: usize,
    text_query_count: usize,
    text_query_executed_count: usize,
    text_evidence_count: usize,
    target_evidence_count: usize,
}

#[derive(Debug, Clone)]
struct GraphEvidenceSourceDocumentCandidate {
    document_id: Uuid,
    best_score: usize,
    total_score: usize,
    first_ordinal: usize,
    best_confidence: f64,
    latest_created_at: DateTime<Utc>,
}

type GraphEvidenceChunkHits = Vec<(Uuid, f32)>;
type GraphEvidenceTextsByChunk = HashMap<Uuid, Vec<String>>;

async fn load_ranked_graph_evidence_rows_for_query(
    state: &AppState,
    library_id: Uuid,
    question: &str,
    entities: &[RuntimeMatchedEntity],
    relationships: &[RuntimeMatchedRelationship],
    plan: &RuntimeQueryPlan,
    query_ir: Option<&QueryIR>,
    target_entity_profiles: &[GraphTargetEntityProfile],
    graph_index: &QueryGraphIndex,
    targeted_document_ids: &BTreeSet<Uuid>,
    limit: usize,
) -> anyhow::Result<RankedGraphEvidenceRows> {
    let started = std::time::Instant::now();
    let targets = graph_evidence_targets_for_query(
        entities,
        relationships,
        plan,
        query_ir,
        target_entity_profiles,
        graph_index,
    );
    let target_build_elapsed_ms = started.elapsed().as_millis();
    let query_build_started = std::time::Instant::now();
    let query_ir_focus_queries = query_ir.map(query_ir_lexical_focus_queries).unwrap_or_default();
    let text_queries =
        build_graph_evidence_text_queries(question, plan, &query_ir_focus_queries, query_ir);
    let db_text_queries = graph_evidence_db_text_queries(&text_queries);
    let query_build_elapsed_ms = query_build_started.elapsed().as_millis();

    let target_search_started = std::time::Instant::now();
    let target_evidence = if targets.is_empty() {
        Vec::new()
    } else {
        repositories::list_runtime_graph_evidence_by_targets(
            &state.persistence.postgres,
            library_id,
            &targets,
            graph_evidence_context_fetch_cap() as i64,
        )
        .await
        .context("failed to list graph evidence context for retrieved graph targets")?
    };
    let target_search_elapsed_ms = target_search_started.elapsed().as_millis();
    let target_filter_started = std::time::Instant::now();
    let target_evidence = if targeted_document_ids.is_empty() {
        target_evidence
    } else {
        filter_graph_evidence_rows_for_target_documents(
            state,
            target_evidence,
            targeted_document_ids,
        )
        .await?
    };
    let target_filter_elapsed_ms = target_filter_started.elapsed().as_millis();

    let target_source_document_ids = graph_evidence_source_document_ids_from_scored_targets(
        &target_evidence,
        question,
        &text_queries,
        graph_index,
        target_entity_profiles,
    );
    let text_search_document_ids = graph_evidence_text_search_document_scope(
        targeted_document_ids,
        &target_source_document_ids,
    );

    let text_started = std::time::Instant::now();
    let text_evidence = repositories::search_runtime_graph_evidence_by_text(
        &state.persistence.postgres,
        library_id,
        &db_text_queries,
        &text_search_document_ids,
        graph_evidence_context_fetch_cap() as i64,
    )
    .await
    .context("failed to search graph evidence context by evidence text")?;
    let text_search_elapsed_ms = text_started.elapsed().as_millis();
    let text_filter_started = std::time::Instant::now();
    let text_evidence = if targeted_document_ids.is_empty() {
        text_evidence
    } else {
        filter_graph_evidence_rows_for_target_documents(state, text_evidence, targeted_document_ids)
            .await?
    };
    let text_filter_elapsed_ms = text_filter_started.elapsed().as_millis();

    let rank_started = std::time::Instant::now();
    let rows = rank_graph_evidence_context_rows(
        &text_evidence,
        &target_evidence,
        question,
        &text_queries,
        graph_index,
        &target_entity_profiles,
        limit,
    );
    let rank_elapsed_ms = rank_started.elapsed().as_millis();
    tracing::info!(
        stage = "retrieval.graph_evidence_breakdown",
        graph_target_count = targets.len(),
        text_query_count = text_queries.len(),
        text_query_executed_count = db_text_queries.len(),
        text_evidence_count = text_evidence.len(),
        target_evidence_count = target_evidence.len(),
        ranked_evidence_count = rows.len(),
        target_build_elapsed_ms,
        target_profile_count = target_entity_profiles.len(),
        query_build_elapsed_ms,
        text_search_elapsed_ms,
        text_filter_elapsed_ms,
        target_search_elapsed_ms,
        target_filter_elapsed_ms,
        text_search_document_scope_count = text_search_document_ids.len(),
        target_document_scope_count = targeted_document_ids.len(),
        target_source_document_count = target_source_document_ids.len(),
        rank_elapsed_ms,
        total_elapsed_ms = started.elapsed().as_millis(),
        "graph evidence retrieval substeps completed",
    );

    Ok(RankedGraphEvidenceRows {
        rows,
        target_source_document_ids,
        graph_target_count: targets.len(),
        text_query_count: text_queries.len(),
        text_query_executed_count: db_text_queries.len(),
        text_evidence_count: text_evidence.len(),
        target_evidence_count: target_evidence.len(),
    })
}

pub(crate) fn graph_evidence_text_search_document_scope(
    targeted_document_ids: &BTreeSet<Uuid>,
    target_source_document_ids: &[Uuid],
) -> Vec<Uuid> {
    if !targeted_document_ids.is_empty() {
        return targeted_document_ids.iter().copied().collect();
    }
    let mut seen = BTreeSet::new();
    target_source_document_ids
        .iter()
        .copied()
        .filter(|document_id| seen.insert(*document_id))
        .collect()
}

pub(crate) fn graph_evidence_source_document_ids_from_scored_targets(
    target_evidence: &[repositories::RuntimeGraphEvidenceRow],
    question: &str,
    text_queries: &[String],
    graph_index: &QueryGraphIndex,
    target_entity_profiles: &[GraphTargetEntityProfile],
) -> Vec<Uuid> {
    let focus_texts = graph_evidence_context_focus_texts(question, text_queries);
    let focus_tokens = graph_evidence_context_focus_tokens(&focus_texts);
    if target_evidence.is_empty() || focus_tokens.is_empty() {
        return Vec::new();
    }

    let mut candidates = target_evidence
        .iter()
        .enumerate()
        .map(|(ordinal, row)| {
            let fields = graph_evidence_context_candidate_fields(row, graph_index);
            let tokens = fields
                .iter()
                .flat_map(|field| field.tokens.iter().cloned())
                .collect::<BTreeSet<_>>();
            GraphEvidenceContextCandidate { row: row.clone(), ordinal, fields, tokens, score: 0 }
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Vec::new();
    }

    let mut token_frequencies = HashMap::<String, usize>::new();
    for candidate in &candidates {
        for token in &candidate.tokens {
            *token_frequencies.entry(token.clone()).or_default() += 1;
        }
    }
    let candidate_count = candidates.len();
    for candidate in &mut candidates {
        candidate.score = graph_evidence_context_relevance_score(
            &candidate.fields,
            &focus_tokens,
            &token_frequencies,
            candidate_count,
            target_entity_profiles,
        );
    }

    let mut documents = HashMap::<Uuid, GraphEvidenceSourceDocumentCandidate>::new();
    for candidate in candidates.into_iter().filter(|candidate| candidate.score > 0) {
        let Some(document_id) = candidate.row.document_id else {
            continue;
        };
        let confidence = candidate.row.confidence_score.unwrap_or(f64::NEG_INFINITY);
        documents
            .entry(document_id)
            .and_modify(|document| {
                document.best_score = document.best_score.max(candidate.score);
                document.total_score = document.total_score.saturating_add(candidate.score);
                document.first_ordinal = document.first_ordinal.min(candidate.ordinal);
                document.best_confidence = document.best_confidence.max(confidence);
                document.latest_created_at =
                    document.latest_created_at.max(candidate.row.created_at);
            })
            .or_insert(GraphEvidenceSourceDocumentCandidate {
                document_id,
                best_score: candidate.score,
                total_score: candidate.score,
                first_ordinal: candidate.ordinal,
                best_confidence: confidence,
                latest_created_at: candidate.row.created_at,
            });
    }

    let mut documents = documents.into_values().collect::<Vec<_>>();
    documents.sort_by(|left, right| {
        right
            .best_score
            .cmp(&left.best_score)
            .then_with(|| right.total_score.cmp(&left.total_score))
            .then_with(|| left.first_ordinal.cmp(&right.first_ordinal))
            .then_with(|| {
                right
                    .best_confidence
                    .partial_cmp(&left.best_confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| right.latest_created_at.cmp(&left.latest_created_at))
            .then_with(|| right.document_id.cmp(&left.document_id))
    });
    documents
        .into_iter()
        .take(GRAPH_EVIDENCE_SOURCE_DOCUMENT_PRIORITY_CAP)
        .map(|document| document.document_id)
        .collect()
}

async fn filter_graph_evidence_rows_for_target_documents(
    state: &AppState,
    rows: Vec<repositories::RuntimeGraphEvidenceRow>,
    targeted_document_ids: &BTreeSet<Uuid>,
) -> anyhow::Result<Vec<repositories::RuntimeGraphEvidenceRow>> {
    if rows.is_empty() || targeted_document_ids.is_empty() {
        return Ok(rows);
    }

    let mut selected = Vec::with_capacity(rows.len());
    let mut unresolved = Vec::with_capacity(rows.len());
    let mut fallback_chunk_ids = BTreeSet::new();

    for (position, row) in rows.into_iter().enumerate() {
        if let Some(document_id) = row.document_id {
            if targeted_document_ids.contains(&document_id) {
                selected.push((position, row));
            }
            continue;
        }

        let Some(chunk_id) = row.chunk_id else {
            continue;
        };
        unresolved.push((position, row, chunk_id));
        fallback_chunk_ids.insert(chunk_id);
    }

    if unresolved.is_empty() {
        selected.sort_unstable_by_key(|(position, _)| *position);
        return Ok(selected.into_iter().map(|(_, row)| row).collect());
    }

    let chunk_rows = state
        .document_store
        .list_chunks_by_ids(&fallback_chunk_ids.iter().copied().collect::<Vec<_>>())
        .await
        .context("failed to resolve chunk document ids for scoped graph evidence filtering")?;
    let chunk_documents = chunk_rows
        .into_iter()
        .map(|chunk| (chunk.chunk_id, chunk.document_id))
        .collect::<HashMap<_, _>>();

    for (position, row, chunk_id) in unresolved {
        if chunk_documents
            .get(&chunk_id)
            .is_some_and(|document_id| targeted_document_ids.contains(document_id))
        {
            selected.push((position, row));
        }
    }

    selected.sort_unstable_by_key(|(position, _)| *position);
    Ok(selected.into_iter().map(|(_, row)| row).collect())
}

pub(crate) fn graph_evidence_chunk_hits_from_rows(
    rows: &[repositories::RuntimeGraphEvidenceRow],
) -> (GraphEvidenceChunkHits, GraphEvidenceTextsByChunk) {
    let mut seen_chunks = HashSet::<Uuid>::new();
    let mut seen_texts = HashSet::<(Uuid, String)>::new();
    let mut evidence_texts_by_chunk = HashMap::<Uuid, Vec<String>>::new();
    let mut hits = Vec::new();
    for row in rows {
        let Some(chunk_id) = row.chunk_id else {
            continue;
        };
        if !seen_chunks.contains(&chunk_id) {
            if hits.len() >= GRAPH_EVIDENCE_CHUNK_CAP {
                continue;
            }
            seen_chunks.insert(chunk_id);
            let score = graph_evidence_chunk_score(hits.len());
            hits.push((chunk_id, score));
        }
        let evidence_text = repair_technical_layout_noise(row.evidence_text.trim());
        if !evidence_text.is_empty() && seen_texts.insert((chunk_id, evidence_text.clone())) {
            let texts = evidence_texts_by_chunk.entry(chunk_id).or_default();
            if texts.len() < GRAPH_EVIDENCE_TEXTS_PER_CHUNK {
                texts.push(evidence_text);
            }
        }
    }
    (hits, evidence_texts_by_chunk)
}

#[must_use]
fn graph_evidence_context_fetch_cap() -> usize {
    GRAPH_EVIDENCE_CONTEXT_LINE_CAP * GRAPH_EVIDENCE_CONTEXT_FETCH_MULTIPLIER
}

#[derive(Debug, Clone)]
struct GraphEvidenceContextCandidate {
    row: repositories::RuntimeGraphEvidenceRow,
    ordinal: usize,
    fields: Vec<GraphEvidenceContextCandidateField>,
    tokens: BTreeSet<String>,
    score: usize,
}

#[derive(Debug, Clone)]
struct GraphEvidenceContextCandidateField {
    text: String,
    tokens: BTreeSet<String>,
    weight: usize,
    coverage_kind: GraphTargetEntityCoverageFieldKind,
}

pub(crate) fn rank_graph_evidence_context_rows(
    text_evidence: &[repositories::RuntimeGraphEvidenceRow],
    target_evidence: &[repositories::RuntimeGraphEvidenceRow],
    question: &str,
    text_queries: &[String],
    graph_index: &QueryGraphIndex,
    target_entity_profiles: &[GraphTargetEntityProfile],
    limit: usize,
) -> Vec<repositories::RuntimeGraphEvidenceRow> {
    if limit == 0 {
        return Vec::new();
    }

    let focus_texts = graph_evidence_context_focus_texts(question, text_queries);
    let focus_tokens = graph_evidence_context_focus_tokens(&focus_texts);
    let mut candidates = Vec::new();
    let mut seen_row_ids = BTreeSet::new();

    for (source_ordinal, rows) in [&text_evidence, &target_evidence].into_iter().enumerate() {
        for (row_ordinal, row) in rows.iter().enumerate() {
            if !seen_row_ids.insert(row.id) {
                continue;
            }
            let fields = graph_evidence_context_candidate_fields(row, graph_index);
            let tokens = fields
                .iter()
                .flat_map(|field| field.tokens.iter().cloned())
                .collect::<BTreeSet<_>>();
            candidates.push(GraphEvidenceContextCandidate {
                row: row.clone(),
                ordinal: source_ordinal.saturating_mul(graph_evidence_context_fetch_cap())
                    + row_ordinal,
                fields,
                tokens,
                score: 0,
            });
        }
    }

    if candidates.is_empty() {
        return Vec::new();
    }

    let mut token_frequencies = HashMap::<String, usize>::new();
    for candidate in &candidates {
        for token in &candidate.tokens {
            *token_frequencies.entry(token.clone()).or_default() += 1;
        }
    }
    let candidate_count = candidates.len();
    for candidate in &mut candidates {
        candidate.score = graph_evidence_context_relevance_score(
            &candidate.fields,
            &focus_tokens,
            &token_frequencies,
            candidate_count,
            target_entity_profiles,
        );
    }

    candidates.sort_by(|left, right| {
        let left_confidence = left.row.confidence_score.unwrap_or(f64::NEG_INFINITY);
        let right_confidence = right.row.confidence_score.unwrap_or(f64::NEG_INFINITY);
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.ordinal.cmp(&right.ordinal))
            .then_with(|| {
                right_confidence.partial_cmp(&left_confidence).unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| right.row.created_at.cmp(&left.row.created_at))
            .then_with(|| right.row.id.cmp(&left.row.id))
    });

    let mut selected = Vec::new();
    let mut seen_bodies = BTreeSet::new();
    for candidate in candidates {
        if selected.len() >= limit {
            break;
        }
        let body_key = graph_evidence_context_body_key(&candidate.row.evidence_text);
        if body_key.is_empty() || !seen_bodies.insert(body_key) {
            continue;
        }
        selected.push(candidate.row);
    }
    selected
}

fn graph_evidence_context_focus_texts(question: &str, text_queries: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut focus_texts = Vec::new();
    let mut push_focus = |value: &str, focus_texts: &mut Vec<String>| {
        let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized.is_empty() || !seen.insert(normalized.to_lowercase()) {
            return;
        }
        focus_texts.push(normalized);
    };

    for text_query in text_queries {
        push_focus(text_query, &mut focus_texts);
    }
    push_focus(question, &mut focus_texts);

    focus_texts
}

fn graph_evidence_context_focus_tokens(focus_texts: &[String]) -> Vec<(String, BTreeSet<String>)> {
    focus_texts
        .iter()
        .filter_map(|focus_text| {
            let tokens = normalized_alnum_token_sequence(
                focus_text,
                GRAPH_EVIDENCE_CONTEXT_SCORE_TOKEN_MIN_CHARS,
            )
            .into_iter()
            .collect::<BTreeSet<_>>();
            (!tokens.is_empty()).then(|| (focus_text.clone(), tokens))
        })
        .collect()
}

fn graph_evidence_context_candidate_fields(
    row: &repositories::RuntimeGraphEvidenceRow,
    graph_index: &QueryGraphIndex,
) -> Vec<GraphEvidenceContextCandidateField> {
    let mut fields = Vec::new();
    push_graph_evidence_context_candidate_field(
        &mut fields,
        row.evidence_text.clone(),
        GRAPH_EVIDENCE_CONTEXT_EVIDENCE_FIELD_WEIGHT,
        GraphTargetEntityCoverageFieldKind::Evidence,
    );
    if let Some(target_label) =
        graph_evidence_target_label(&row.target_kind, row.target_id, graph_index)
    {
        push_graph_evidence_context_candidate_field(
            &mut fields,
            target_label,
            GRAPH_EVIDENCE_CONTEXT_TARGET_FIELD_WEIGHT,
            GraphTargetEntityCoverageFieldKind::Label,
        );
    }
    let mut source_parts = Vec::new();
    if let Some(source) =
        row.source_file_name.as_deref().map(str::trim).filter(|value| !value.is_empty())
    {
        source_parts.push(source.to_string());
    }
    if let Some(page) = row.page_ref.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
        source_parts.push(page.to_string());
    }
    if !source_parts.is_empty() {
        push_graph_evidence_context_candidate_field(
            &mut fields,
            source_parts.join(" "),
            GRAPH_EVIDENCE_CONTEXT_SOURCE_FIELD_WEIGHT,
            GraphTargetEntityCoverageFieldKind::Summary,
        );
    }
    fields
}

fn push_graph_evidence_context_candidate_field(
    fields: &mut Vec<GraphEvidenceContextCandidateField>,
    text: String,
    weight: usize,
    coverage_kind: GraphTargetEntityCoverageFieldKind,
) {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return;
    }
    let tokens = normalized_alnum_tokens(&normalized, GRAPH_EVIDENCE_CONTEXT_SCORE_TOKEN_MIN_CHARS);
    fields.push(GraphEvidenceContextCandidateField {
        text: normalized,
        tokens,
        weight,
        coverage_kind,
    });
}

fn graph_evidence_context_relevance_score(
    candidate_fields: &[GraphEvidenceContextCandidateField],
    focus_tokens: &[(String, BTreeSet<String>)],
    token_frequencies: &HashMap<String, usize>,
    candidate_count: usize,
    target_entity_profiles: &[GraphTargetEntityProfile],
) -> usize {
    if candidate_fields.is_empty() || focus_tokens.is_empty() {
        return 0;
    }

    let mut score = 0usize;
    for (ordinal, (focus_text, tokens)) in focus_tokens.iter().enumerate() {
        let weight = focus_tokens.len().saturating_sub(ordinal).max(1);
        let mut overlap_count = 0usize;
        let mut overlap_score = 0usize;
        for token in tokens {
            let field_weight = candidate_fields
                .iter()
                .filter(|field| field.tokens.contains(token))
                .map(|field| field.weight)
                .max()
                .unwrap_or_default();
            if field_weight > 0 {
                overlap_count += 1;
                let frequency = token_frequencies.get(token).copied().unwrap_or(candidate_count);
                overlap_score += candidate_count
                    .saturating_sub(frequency)
                    .saturating_add(1)
                    .saturating_mul(field_weight);
            }
        }
        if overlap_count == 0 {
            continue;
        }
        score += overlap_score.saturating_mul(weight);
        if overlap_count == tokens.len() {
            score += (16usize.saturating_mul(weight)).saturating_add(overlap_score);
        }
        for field in candidate_fields {
            if token_sequence_contains(
                &field.text,
                focus_text,
                GRAPH_EVIDENCE_CONTEXT_SCORE_TOKEN_MIN_CHARS,
            ) {
                score += 32usize.saturating_mul(weight).saturating_mul(field.weight);
            }
        }
    }
    let coverage_fields = candidate_fields
        .iter()
        .map(|field| GraphTargetEntityCoverageField {
            text: field.text.as_str(),
            kind: field.coverage_kind,
        })
        .collect::<Vec<_>>();
    score.saturating_add(graph_target_entity_coverage_score(
        &coverage_fields,
        target_entity_profiles,
    ))
}

fn graph_evidence_context_body_key(evidence_text: &str) -> String {
    graph_evidence_text_for_context(evidence_text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub(crate) fn graph_evidence_context_line(
    row: &repositories::RuntimeGraphEvidenceRow,
    graph_index: &QueryGraphIndex,
    source_labels_by_document_id: &HashMap<Uuid, String>,
    focus_keywords: &[String],
) -> Option<String> {
    let evidence_text = graph_evidence_text_for_context(&row.evidence_text);
    if evidence_text.is_empty() {
        return None;
    }
    let evidence_text = focused_graph_evidence_context_text(&evidence_text, focus_keywords);
    if evidence_text.is_empty() {
        return None;
    }

    let mut attrs = Vec::new();
    if let Some(target_label) =
        graph_evidence_target_label(&row.target_kind, row.target_id, graph_index)
    {
        attrs.push(("target", target_label));
    }
    if let Some(source) = graph_evidence_context_source_label(row, source_labels_by_document_id) {
        attrs.push(("source", source));
    }
    if let Some(page) = row.page_ref.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
        attrs.push(("page", page.to_string()));
    }

    let attr_text = attrs
        .into_iter()
        .map(|(key, value)| format!("{key}=\"{}\"", context_attribute_value(&value)))
        .collect::<Vec<_>>()
        .join(" ");
    let header = if attr_text.is_empty() {
        "[graph-evidence]".to_string()
    } else {
        format!("[graph-evidence {attr_text}]")
    };
    Some(format!("{header}\n{evidence_text}"))
}

fn graph_evidence_context_source_label(
    row: &repositories::RuntimeGraphEvidenceRow,
    source_labels_by_document_id: &HashMap<Uuid, String>,
) -> Option<String> {
    if let Some(document_id) = row.document_id
        && let Some(source_label) = source_labels_by_document_id.get(&document_id)
        && let Some(source_label) = trimmed_non_empty(Some(source_label.as_str()))
    {
        return Some(source_label);
    }

    trimmed_non_empty(row.source_file_name.as_deref())
}

fn trimmed_non_empty(value: Option<&str>) -> Option<String> {
    value.map(str::trim).filter(|value| !value.is_empty()).map(str::to_string)
}

fn graph_evidence_context_line_focus_keywords(
    question: &str,
    text_queries: &[String],
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut keywords = Vec::new();
    for focus_text in graph_evidence_context_focus_texts(question, text_queries) {
        for token in normalized_alnum_token_sequence(
            &focus_text,
            GRAPH_EVIDENCE_CONTEXT_SCORE_TOKEN_MIN_CHARS,
        ) {
            if seen.insert(token.clone()) {
                keywords.push(token);
            }
        }
    }
    keywords
}

fn focused_graph_evidence_context_text(evidence_text: &str, focus_keywords: &[String]) -> String {
    if evidence_text.chars().count() <= GRAPH_EVIDENCE_CONTEXT_LINE_CHARS {
        return evidence_text.to_string();
    }
    let excerpt =
        focused_excerpt_for(evidence_text, focus_keywords, GRAPH_EVIDENCE_CONTEXT_LINE_CHARS);
    if excerpt.trim().is_empty() {
        excerpt_for(evidence_text, GRAPH_EVIDENCE_CONTEXT_LINE_CHARS)
    } else {
        excerpt
    }
}

fn graph_evidence_target_label(
    target_kind: &str,
    target_id: Uuid,
    graph_index: &QueryGraphIndex,
) -> Option<String> {
    match target_kind {
        "node" => {
            graph_index.node(target_id).map(|node| format!("{} ({})", node.label, node.node_type))
        }
        "edge" => graph_index.edge(target_id).map(|edge| {
            let from_label = graph_index
                .node(edge.from_node_id)
                .map(|node| node.label.as_str())
                .unwrap_or("<unknown>");
            let to_label = graph_index
                .node(edge.to_node_id)
                .map(|node| node.label.as_str())
                .unwrap_or("<unknown>");
            format!("{from_label} --{}--> {to_label}", edge.relation_type)
        }),
        _ => None,
    }
}

fn context_attribute_value(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

pub(crate) fn apply_graph_evidence_texts_to_chunks(
    chunks: &mut [RuntimeMatchedChunk],
    evidence_texts_by_chunk: &HashMap<Uuid, Vec<String>>,
    plan_keywords: &[String],
    focus_keywords: &[String],
) {
    for chunk in chunks {
        let Some(evidence_texts) = evidence_texts_by_chunk.get(&chunk.chunk_id) else {
            continue;
        };
        let evidence_source_text =
            graph_evidence_source_text(&chunk.source_text, evidence_texts, focus_keywords);
        if evidence_source_text.trim().is_empty() {
            continue;
        }
        chunk.source_text = evidence_source_text;
        chunk.excerpt = focused_excerpt_for(&chunk.source_text, plan_keywords, 280);
    }
}

fn graph_evidence_source_text(
    chunk_source_text: &str,
    evidence_texts: &[String],
    focus_keywords: &[String],
) -> String {
    let mut parts = Vec::new();
    let mut seen = BTreeSet::new();
    for text in evidence_texts {
        let normalized = graph_evidence_text_for_context(text);
        if normalized.is_empty() || !seen.insert(normalized.to_lowercase()) {
            continue;
        }
        parts.push(focused_graph_evidence_context_text(&normalized, focus_keywords));
    }
    if parts.is_empty() {
        return chunk_source_text.trim().to_string();
    }

    let evidence_text = parts.join("\n\n");
    let chunk_text = chunk_source_text.trim();
    if chunk_text.is_empty() || chunk_text.contains(&evidence_text) {
        evidence_text
    } else {
        format!("{evidence_text}\n\nSource chunk:\n{chunk_text}")
    }
}

fn graph_evidence_text_for_context(value: &str) -> String {
    let normalized = repair_technical_layout_noise(value.trim());
    if normalized.is_empty() {
        return String::new();
    }
    let fields = normalized
        .split(" | ")
        .map(str::trim)
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>();
    if fields.len() < 2 {
        return normalized;
    }
    fields.into_iter().map(|field| format!("- {field}")).collect::<Vec<_>>().join("\n")
}

pub(crate) fn graph_evidence_targets(
    entities: &[RuntimeMatchedEntity],
    relationships: &[RuntimeMatchedRelationship],
) -> Vec<(String, Uuid)> {
    let mut seen = HashSet::<(String, Uuid)>::new();
    let mut targets = Vec::with_capacity(entities.len() + relationships.len());
    for entity in entities {
        let key = ("node".to_string(), entity.node_id);
        if seen.insert(key.clone()) {
            targets.push(key);
        }
    }
    for relationship in relationships {
        let key = ("edge".to_string(), relationship.edge_id);
        if seen.insert(key.clone()) {
            targets.push(key);
        }
    }
    targets
}

pub(crate) fn graph_evidence_targets_for_query(
    entities: &[RuntimeMatchedEntity],
    relationships: &[RuntimeMatchedRelationship],
    plan: &RuntimeQueryPlan,
    query_ir: Option<&QueryIR>,
    target_entity_profiles: &[GraphTargetEntityProfile],
    graph_index: &QueryGraphIndex,
) -> Vec<(String, Uuid)> {
    let mut seen = HashSet::<(String, Uuid)>::new();
    let mut targets = Vec::new();
    let retrieved_targets = graph_evidence_targets(entities, relationships);

    let query_entities = query_relevant_graph_evidence_target_hits(
        plan,
        query_ir,
        target_entity_profiles,
        graph_index,
        GRAPH_EVIDENCE_TARGET_CAP / 2,
    );
    let query_relationships = associative_edges_for_entities(
        &query_entities,
        graph_index,
        plan,
        query_ir,
        GRAPH_EVIDENCE_TARGET_CAP / 2,
    );
    let query_targets = graph_evidence_targets(&query_entities, &query_relationships);

    if !target_entity_profiles.is_empty() {
        append_graph_evidence_targets(&mut targets, &mut seen, query_targets);
        append_graph_evidence_targets(&mut targets, &mut seen, retrieved_targets);
    } else {
        append_graph_evidence_targets(&mut targets, &mut seen, retrieved_targets);
        append_graph_evidence_targets(&mut targets, &mut seen, query_targets);
    }
    targets
}

fn append_graph_evidence_targets(
    targets: &mut Vec<(String, Uuid)>,
    seen: &mut HashSet<(String, Uuid)>,
    candidates: Vec<(String, Uuid)>,
) {
    for target in candidates {
        if targets.len() >= GRAPH_EVIDENCE_TARGET_CAP {
            return;
        }
        if seen.insert(target.clone()) {
            targets.push(target);
        }
    }
}

async fn load_query_ir_focus_chunks(
    state: &AppState,
    library_id: Uuid,
    question: &str,
    focus_queries: &[String],
    targeted_document_ids: &BTreeSet<Uuid>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    temporal_start: Option<DateTime<Utc>>,
    temporal_end: Option<DateTime<Utc>>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let search_queries = query_ir_focus_search_queries(question, focus_queries);
    if search_queries.is_empty() {
        return Ok(Vec::new());
    }

    let per_query_futures = search_queries.iter().cloned().map(|focus_query| async move {
        state
            .search_store
            .search_chunks(
                library_id,
                &focus_query,
                QUERY_IR_FOCUS_CHUNKS_PER_QUERY,
                temporal_start,
                temporal_end,
            )
            .await
            .map(|rows| {
                rows.into_iter().map(|row| (row.chunk_id, row.score as f32)).collect::<Vec<_>>()
            })
            .with_context(|| format!("failed to run query-IR focus chunk search: {focus_query}"))
    });
    let per_query_results: Vec<Result<Vec<_>, anyhow::Error>> = join_all(per_query_futures).await;
    let hits = combine_query_ir_focus_search_results(per_query_results, search_queries.len())?;
    if hits.is_empty() {
        return Ok(Vec::new());
    }

    let mut chunks =
        batch_hydrate_hits(state, hits, document_index, plan_keywords, targeted_document_ids)
            .await
            .context("failed to hydrate query-IR focus chunks")?;
    for chunk in &mut chunks {
        chunk.score_kind = RuntimeChunkScoreKind::QueryIrFocus;
    }
    tracing::info!(
        stage = "retrieval.query_ir_focus",
        focus_query_count = search_queries.len(),
        focus_chunk_count = chunks.len(),
        "query-IR focus chunks loaded for rare exact retrieval signals",
    );
    Ok(chunks)
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RetrievalContentAnchorModel {
    search_terms: Vec<String>,
    focus_tokens: BTreeSet<String>,
    phrase_sequences: Vec<Vec<String>>,
}

impl RetrievalContentAnchorModel {
    pub(crate) fn new(question: &str, query_ir: Option<&QueryIR>) -> Self {
        let mut values = Vec::new();
        collect_content_anchor_text_value(&mut values, question);
        if let Some(query_ir) = query_ir {
            if let Some(retrieval_query) = query_ir.retrieval_query.as_deref() {
                collect_content_anchor_text_value(&mut values, retrieval_query);
            }
            if let Some(document_focus) = query_ir.document_focus.as_ref() {
                collect_content_anchor_text_value(&mut values, &document_focus.hint);
            }
            for entity in &query_ir.target_entities {
                collect_content_anchor_text_value(&mut values, &entity.label);
            }
            for literal in &query_ir.literal_constraints {
                collect_content_anchor_text_value(&mut values, &literal.text);
            }
        }

        let mut focus_tokens = BTreeSet::new();
        let mut phrase_sequences = Vec::new();
        let mut seen_sequences = BTreeSet::new();
        for value in &values {
            for token in normalized_alnum_tokens(value, CONTENT_ANCHOR_TOKEN_MIN_CHARS) {
                focus_tokens.insert(token);
            }
            for sequence in quoted_content_anchor_sequences(value) {
                push_content_anchor_sequence(&mut phrase_sequences, &mut seen_sequences, sequence);
            }
            for sequence in adjacent_content_anchor_sequences(value) {
                push_content_anchor_sequence(&mut phrase_sequences, &mut seen_sequences, sequence);
            }
        }

        let search_terms = content_anchor_search_terms(&focus_tokens);
        Self { search_terms, focus_tokens, phrase_sequences }
    }

    fn is_empty(&self) -> bool {
        self.search_terms.is_empty()
            && self.focus_tokens.is_empty()
            && self.phrase_sequences.is_empty()
    }
}

fn content_anchor_search_terms(focus_tokens: &BTreeSet<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut terms = Vec::new();
    for token in focus_tokens {
        push_content_anchor_search_term(&mut terms, &mut seen, token.clone());
        let token_len = token.chars().count();
        if token_len >= CONTENT_ANCHOR_SEARCH_PREFIX_MIN_CHARS {
            let prefix = token.chars().take(CONTENT_ANCHOR_SEARCH_PREFIX_CHARS).collect::<String>();
            if prefix.chars().count() >= CONTENT_ANCHOR_TOKEN_MIN_CHARS && prefix != *token {
                push_content_anchor_search_term(&mut terms, &mut seen, prefix);
            }
        }
        if terms.len() >= CONTENT_ANCHOR_SEARCH_TERM_CAP {
            break;
        }
    }
    terms
}

fn push_content_anchor_search_term(
    terms: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    term: String,
) {
    if terms.len() >= CONTENT_ANCHOR_SEARCH_TERM_CAP {
        return;
    }
    if seen.insert(term.clone()) {
        terms.push(term);
    }
}

fn collect_content_anchor_text_value(values: &mut Vec<String>, value: &str) {
    let current = current_question_segment(value).trim();
    if !current.is_empty() {
        values.push(current.to_string());
    }
}

fn quoted_content_anchor_sequences(value: &str) -> Vec<Vec<String>> {
    let mut sequences = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in value.chars() {
        if content_anchor_quote_matches(quote, ch) {
            let sequence =
                normalized_alnum_token_sequence(&current, CONTENT_ANCHOR_SEQUENCE_MIN_CHARS);
            if sequence.len() >= 2 {
                sequences.push(sequence);
            }
            current.clear();
            quote = None;
            continue;
        }
        if quote.is_some() {
            current.push(ch);
            continue;
        }
        if content_anchor_opening_quote(ch) {
            quote = Some(ch);
            current.clear();
        }
    }
    sequences
}

fn content_anchor_opening_quote(ch: char) -> bool {
    matches!(ch, '"' | '\'' | '`' | '\u{00ab}' | '\u{201c}' | '\u{2018}')
}

fn content_anchor_quote_matches(opening: Option<char>, closing: char) -> bool {
    matches!(
        (opening, closing),
        (Some('"'), '"')
            | (Some('\''), '\'')
            | (Some('`'), '`')
            | (Some('\u{00ab}'), '\u{00bb}')
            | (Some('\u{201c}'), '\u{201d}')
            | (Some('\u{2018}'), '\u{2019}')
    )
}

fn adjacent_content_anchor_sequences(value: &str) -> Vec<Vec<String>> {
    let tokens = normalized_alnum_token_sequence(value, CONTENT_ANCHOR_SEQUENCE_MIN_CHARS);
    let mut sequences = Vec::new();
    for window_size in 2..=4 {
        for window in tokens.windows(window_size) {
            sequences.push(window.to_vec());
            if sequences.len() >= CONTENT_ANCHOR_SEQUENCE_CAP {
                return sequences;
            }
        }
    }
    sequences
}

fn push_content_anchor_sequence(
    sequences: &mut Vec<Vec<String>>,
    seen: &mut BTreeSet<Vec<String>>,
    sequence: Vec<String>,
) {
    if sequences.len() >= CONTENT_ANCHOR_SEQUENCE_CAP || sequence.len() < 2 {
        return;
    }
    if seen.insert(sequence.clone()) {
        sequences.push(sequence);
    }
}

fn content_anchor_revision_ids(
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    targeted_document_ids: &BTreeSet<Uuid>,
) -> Vec<Uuid> {
    let mut revision_ids = document_index
        .values()
        .filter(|document| {
            targeted_document_ids.is_empty()
                || targeted_document_ids.contains(&document.document_id)
        })
        .filter_map(|document| {
            canonical_document_revision_id(document).map(|id| (document.document_id, id))
        })
        .collect::<Vec<_>>();
    revision_ids.sort_by_key(|(document_id, revision_id)| (*revision_id, *document_id));
    revision_ids.dedup_by_key(|(_, revision_id)| *revision_id);
    revision_ids.into_iter().map(|(_, revision_id)| revision_id).collect()
}

pub(crate) fn content_anchor_row_score(
    row: &KnowledgeChunkRow,
    model: &RetrievalContentAnchorModel,
) -> usize {
    if is_source_profile_chunk_row(row)
        || matches!(row.chunk_kind.as_deref(), Some("DocumentIdentity" | "LatestVersion"))
    {
        return 0;
    }
    let mut text = String::new();
    if !row.heading_trail.is_empty() {
        text.push_str(&row.heading_trail.join(" "));
        text.push('\n');
    }
    if !row.section_path.is_empty() {
        text.push_str(&row.section_path.join(" "));
        text.push('\n');
    }
    text.push_str(&row.content_text);
    if !row.normalized_text.trim().is_empty() {
        text.push('\n');
        text.push_str(&row.normalized_text);
    }
    if let Some(window_text) = row.window_text.as_deref().filter(|value| !value.trim().is_empty()) {
        text.push('\n');
        text.push_str(window_text);
    }
    let text = repair_technical_layout_noise(&text);
    let text_sequence = normalized_alnum_token_sequence(&text, CONTENT_ANCHOR_SEQUENCE_MIN_CHARS);
    let text_tokens = normalized_alnum_tokens(&text, CONTENT_ANCHOR_TOKEN_MIN_CHARS)
        .into_iter()
        .collect::<BTreeSet<_>>();

    let phrase_hits = model
        .phrase_sequences
        .iter()
        .filter(|sequence| token_sequence_contains_tokens(&text_sequence, sequence))
        .count();
    let token_overlap = model
        .focus_tokens
        .iter()
        .filter(|focus| {
            text_tokens.contains(*focus)
                || text_tokens.iter().any(|token| near_token_match(focus, token))
        })
        .count();
    if phrase_hits == 0 && token_overlap < CONTENT_ANCHOR_MIN_TOKEN_OVERLAP {
        return 0;
    }
    let longest_sequence = model
        .phrase_sequences
        .iter()
        .filter(|sequence| token_sequence_contains_tokens(&text_sequence, sequence))
        .map(Vec::len)
        .max()
        .unwrap_or(0);
    phrase_hits
        .saturating_mul(1024)
        .saturating_add(token_overlap.saturating_mul(128))
        .saturating_add(longest_sequence.saturating_mul(16))
}

async fn load_content_anchor_chunks(
    state: &AppState,
    question: &str,
    query_ir: Option<&QueryIR>,
    targeted_document_ids: &BTreeSet<Uuid>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    temporal_start: Option<DateTime<Utc>>,
    temporal_end: Option<DateTime<Utc>>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if temporal_start.is_some() || temporal_end.is_some() {
        return Ok(Vec::new());
    }
    let model = RetrievalContentAnchorModel::new(question, query_ir);
    if model.is_empty() {
        return Ok(Vec::new());
    }
    let revision_ids = content_anchor_revision_ids(document_index, targeted_document_ids);
    if revision_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = state
        .document_store
        .list_chunks_by_revisions_matching_terms(
            &revision_ids,
            &model.search_terms,
            CONTENT_ANCHOR_CHUNKS_PER_REVISION,
        )
        .await
        .context("failed to load content-anchor chunks")?;
    let mut scored_rows = rows
        .into_iter()
        .filter_map(|row| {
            let score = content_anchor_row_score(&row, &model);
            (score > 0).then_some((score, row))
        })
        .collect::<Vec<_>>();
    scored_rows.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    let mut seen = BTreeSet::new();
    let mut chunks = Vec::new();
    for (rank, (evidence_score, row)) in scored_rows.into_iter().enumerate() {
        if chunks.len() >= CONTENT_ANCHOR_CHUNK_CAP {
            break;
        }
        if !seen.insert(row.chunk_id) {
            continue;
        }
        if let Some(mut chunk) = map_chunk_hit(
            row,
            content_anchor_chunk_score(rank, evidence_score),
            document_index,
            plan_keywords,
        ) {
            chunk.score_kind = RuntimeChunkScoreKind::ContentAnchor;
            chunks.push(chunk);
        }
    }
    tracing::info!(
        stage = "retrieval.content_anchor",
        content_anchor_chunk_count = chunks.len(),
        content_anchor_search_term_count = model.search_terms.len(),
        "content-anchor chunks loaded from body evidence"
    );
    Ok(chunks)
}

async fn load_document_evidence_anchor_chunks(
    state: &AppState,
    question: &str,
    query_ir: Option<&QueryIR>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    temporal_start: Option<DateTime<Utc>>,
    temporal_end: Option<DateTime<Utc>>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if temporal_start.is_some() || temporal_end.is_some() {
        return Ok(Vec::new());
    }
    let candidate_document_ids = document_evidence_anchor_candidate_document_ids(
        question,
        query_ir,
        document_index,
        DOCUMENT_EVIDENCE_ANCHOR_CANDIDATE_LIMIT,
    );
    if candidate_document_ids.is_empty() {
        return Ok(Vec::new());
    }
    let focus_terms = document_evidence_anchor_focus_terms(question, query_ir);
    let mut chunks = Vec::new();
    for (document_rank, document_id) in candidate_document_ids.iter().enumerate() {
        let Some(document) = document_index.get(document_id) else {
            continue;
        };
        let Some(revision_id) = canonical_document_revision_id(document) else {
            continue;
        };
        let rows = state
            .document_store
            .list_chunks_by_revision_matching_terms(
                revision_id,
                &focus_terms,
                DOCUMENT_EVIDENCE_ANCHOR_CHUNKS_PER_DOCUMENT,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to load document-evidence anchor chunks for document {} revision {}",
                    document_id, revision_id
                )
            })?;
        let rows = if rows.is_empty() {
            state
                .document_store
                .list_chunks_by_revision_range(
                    revision_id,
                    0,
                    DOCUMENT_EVIDENCE_ANCHOR_CHUNKS_PER_DOCUMENT.saturating_sub(1) as i32,
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to load fallback document-evidence anchor chunks for document {} revision {}",
                        document_id, revision_id
                    )
                })?
        } else {
            rows
        };
        for (chunk_rank, row) in rows.into_iter().enumerate() {
            let score = document_identity_chunk_score(document_rank, chunk_rank);
            if let Some(mut chunk) = map_chunk_hit(row, score, document_index, plan_keywords) {
                chunk.score = Some(score);
                chunk.score_kind = RuntimeChunkScoreKind::FocusedDocument;
                chunks.push(chunk);
            }
        }
    }
    if !chunks.is_empty() {
        tracing::info!(
            stage = "retrieval.document_evidence_anchor",
            anchor_document_count = candidate_document_ids.len(),
            anchor_chunk_count = chunks.len(),
            "document identity evidence anchored into answer context"
        );
    }
    Ok(chunks)
}

async fn load_setup_focus_document_chunks(
    state: &AppState,
    question: &str,
    query_ir: Option<&QueryIR>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    temporal_start: Option<DateTime<Utc>>,
    temporal_end: Option<DateTime<Utc>>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let Some(query_ir) = query_ir else {
        return Ok(Vec::new());
    };
    if temporal_start.is_some() || temporal_end.is_some() {
        return Ok(Vec::new());
    }
    let typed_setup_focus = query_ir_requests_setup_focus_document_candidates(query_ir);
    let structural_setup_focus = query_ir_warrants_structural_setup_focus_fallback(query_ir);
    let raw_setup_focus = setup_focus_uses_raw_question_fallback(query_ir);
    if !typed_setup_focus && !structural_setup_focus && !raw_setup_focus {
        return Ok(Vec::new());
    }
    let raw_question_tokens = (typed_setup_focus || structural_setup_focus || raw_setup_focus)
        .then(|| raw_question_setup_focus_tokens(question));
    let mut candidate_document_ids = if typed_setup_focus || structural_setup_focus {
        let candidate_document_ids = setup_focus_candidate_document_ids(
            query_ir,
            document_index,
            SETUP_FOCUS_DOCUMENT_CANDIDATE_CAP,
        );
        if candidate_document_ids.is_empty() {
            raw_question_setup_focus_candidate_document_ids(
                raw_question_tokens.as_ref(),
                document_index,
                SETUP_FOCUS_DOCUMENT_CANDIDATE_CAP,
            )
        } else {
            candidate_document_ids
        }
    } else {
        raw_question_setup_focus_candidate_document_ids(
            raw_question_tokens.as_ref(),
            document_index,
            SETUP_FOCUS_DOCUMENT_CANDIDATE_CAP,
        )
    };
    let mut seen_candidate_document_ids = HashSet::new();
    candidate_document_ids.retain(|document_id| seen_candidate_document_ids.insert(*document_id));
    if candidate_document_ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut document_candidates = Vec::<(usize, Uuid, Vec<RuntimeMatchedChunk>)>::new();
    for document_id in candidate_document_ids {
        let Some(document) = document_index.get(&document_id) else {
            continue;
        };
        let Some(revision_id) = canonical_document_revision_id(document) else {
            continue;
        };
        let rows = state
            .document_store
            .list_chunks_by_revision_range(revision_id, 0, SETUP_FOCUS_DOCUMENT_SCAN_CHUNKS)
            .await
            .with_context(|| {
                format!(
                    "failed to load setup-focused document chunks for document {} revision {}",
                    document_id, revision_id
                )
            })?;
        let selected_rows = select_setup_focus_document_rows(&rows);
        if selected_rows.is_empty() {
            continue;
        }
        let mut document_score = setup_focus_document_candidate_score(&selected_rows, query_ir)
            .saturating_add(
                setup_focus_document_label_identity_score(document, query_ir).saturating_mul(4096),
            );
        if let Some(tokens) = raw_question_tokens.as_ref() {
            document_score = document_score.saturating_add(
                raw_question_setup_focus_document_score(tokens, document).saturating_mul(64),
            );
        }
        let mut document_chunks = Vec::new();
        for row in selected_rows {
            if let Some(mut chunk) = map_chunk_hit(row, 0.0, document_index, plan_keywords) {
                chunk.score = Some(setup_focus_document_chunk_score(&chunk));
                chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
                document_chunks.push(chunk);
            }
        }
        if !document_chunks.is_empty() {
            document_candidates.push((document_score, document_id, document_chunks));
        }
    }
    let chunks = select_setup_focus_document_chunks(document_candidates);
    if !chunks.is_empty() {
        tracing::info!(
            stage = "retrieval.setup_focus_document",
            setup_focus_chunk_count = chunks.len(),
            "setup-focused document chunks loaded from title-matched documents",
        );
    }
    Ok(chunks)
}

async fn load_setup_variant_document_chunks(
    state: &AppState,
    question: &str,
    query_ir: Option<&QueryIR>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    temporal_start: Option<DateTime<Utc>>,
    temporal_end: Option<DateTime<Utc>>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if temporal_start.is_some()
        || temporal_end.is_some()
        || !question_requests_setup_variant_evidence(question, query_ir)
    {
        return Ok(Vec::new());
    }
    let query_terms = setup_variant_query_terms(question, query_ir);
    let candidate_document_ids = setup_variant_candidate_document_ids(
        question,
        query_ir,
        document_index,
        SETUP_VARIANT_DOCUMENT_CANDIDATE_CAP,
    );
    if candidate_document_ids.is_empty() {
        return Ok(Vec::new());
    }
    let family_model =
        setup_variant_family_model(&candidate_document_ids, document_index, &query_terms);

    let fetch_inputs = candidate_document_ids
        .iter()
        .take(SETUP_VARIANT_DOCUMENT_FETCH_CAP)
        .enumerate()
        .filter_map(|(document_rank, document_id)| {
            let document = document_index.get(document_id)?;
            let revision_id = canonical_document_revision_id(document)?;
            Some((document_rank, *document_id, revision_id))
        })
        .collect::<Vec<_>>();
    let fetched_results = stream::iter(fetch_inputs.into_iter().map(
        |(document_rank, document_id, revision_id)| async move {
            let rows = state
                .document_store
                .list_chunks_by_revision_range(revision_id, 0, SETUP_FOCUS_DOCUMENT_SCAN_CHUNKS)
                .await
                .with_context(|| {
                    format!(
                        "failed to load setup-variant chunks for document {} revision {}",
                        document_id, revision_id
                    )
                })?;
            Ok::<_, anyhow::Error>((document_rank, document_id, rows))
        },
    ))
    .buffer_unordered(SETUP_VARIANT_DOCUMENT_FETCH_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;
    let mut fetched = fetched_results.into_iter().collect::<anyhow::Result<Vec<_>>>()?;
    fetched.sort_by_key(|(document_rank, _, _)| *document_rank);

    let mut selected_document_count = 0usize;
    let mut selected_families = BTreeSet::<String>::new();
    let mut chunks = Vec::new();
    for (_, document_id, rows) in fetched {
        let Some(document) = document_index.get(&document_id) else {
            continue;
        };
        let selected_rows = select_setup_focus_document_rows(&rows);
        if selected_rows.is_empty() {
            continue;
        }
        let structural_score = selected_rows
            .iter()
            .map(|row| raw_setup_focus_text_structural_score(&row.content_text))
            .sum::<usize>();
        if structural_score < RAW_SETUP_FOCUS_STRUCTURAL_DOCUMENT_SCORE_FLOOR {
            continue;
        }
        let family = setup_variant_document_family(document, &query_terms, &family_model);
        if !selected_families.insert(family) {
            continue;
        }
        selected_document_count = selected_document_count.saturating_add(1);
        for (chunk_rank, row) in
            selected_rows.into_iter().take(SETUP_VARIANT_CHUNKS_PER_DOCUMENT).enumerate()
        {
            let score = setup_variant_document_chunk_score(selected_document_count - 1, chunk_rank);
            if let Some(mut chunk) = map_chunk_hit(row, score, document_index, plan_keywords) {
                chunk.score = Some(score);
                chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
                chunks.push(chunk);
                if chunks.len() >= SETUP_VARIANT_CHUNK_CAP {
                    break;
                }
            }
        }
        if selected_document_count >= SETUP_VARIANT_DOCUMENT_CAP
            || chunks.len() >= SETUP_VARIANT_CHUNK_CAP
        {
            break;
        }
    }
    if !chunks.is_empty() {
        tracing::info!(
            stage = "retrieval.setup_variant",
            setup_variant_document_count = selected_document_count,
            setup_variant_chunk_count = chunks.len(),
            "setup variant evidence anchored into answer context"
        );
    }
    Ok(chunks)
}

fn setup_variant_document_chunk_score(document_rank: usize, chunk_rank: usize) -> f32 {
    SETUP_FOCUS_DOCUMENT_SCORE_BASE - document_rank as f32 * 10.0 - chunk_rank as f32
}

fn question_requests_setup_variant_evidence(question: &str, query_ir: Option<&QueryIR>) -> bool {
    let Some(query_ir) = query_ir else {
        return false;
    };
    if !matches!(query_ir.act, QueryAct::ConfigureHow | QueryAct::Describe)
        || query_ir.source_slice.is_some()
        || query_ir_has_exact_non_focus_literal_constraint(query_ir)
        || query_ir_requests_versioned_update_procedure_context(question, query_ir)
        || query_ir_has_specific_adjacent_entity_identity(query_ir)
    {
        return false;
    }
    let raw_question_is_short =
        normalized_alnum_tokens(strip_leading_question_marker(question), 3).len() <= 4;
    raw_question_is_short
        || (query_ir_has_setup_focus_target(query_ir)
            && query_ir_has_setup_focus_identity(query_ir))
}

fn query_ir_has_exact_non_focus_literal_constraint(query_ir: &QueryIR) -> bool {
    query_ir.literal_constraints.iter().any(|literal| {
        literal_kind_has_exact_technical_shape(literal.kind, &literal.text)
            && !query_ir_literal_matches_named_focus(query_ir, &literal.text)
    })
}

fn query_ir_literal_matches_named_focus(query_ir: &QueryIR, literal: &str) -> bool {
    let literal_key = normalize_document_identity_value(literal);
    if literal_key.is_empty() {
        return false;
    }
    if query_ir
        .document_focus
        .as_ref()
        .is_some_and(|focus| normalize_document_identity_value(&focus.hint) == literal_key)
    {
        return true;
    }
    query_ir
        .target_entities
        .iter()
        .any(|entity| normalize_document_identity_value(&entity.label) == literal_key)
}

fn query_ir_has_specific_adjacent_entity_identity(query_ir: &QueryIR) -> bool {
    if !matches!(query_ir.scope, QueryScope::SingleDocument) {
        return false;
    }
    if query_ir.comparison.is_some()
        || !query_ir.temporal_constraints.is_empty()
        || !query_ir.conversation_refs.is_empty()
        || !query_ir.literal_constraints.is_empty()
    {
        return false;
    }
    if !query_ir.target_types.iter().any(|target_type| {
        matches!(
            canonical_target_type_tag(target_type).as_str(),
            "artifact" | "document" | "entity"
        )
    }) {
        return false;
    }
    let entity_values = query_ir
        .target_entities
        .iter()
        .filter(|entity| matches!(entity.role, EntityRole::Subject))
        .filter_map(|entity| {
            let normalized = entity.label.split_whitespace().collect::<Vec<_>>().join(" ");
            is_usable_query_ir_focus(&normalized).then_some(normalized)
        })
        .collect::<Vec<_>>();
    if entity_values.len() < 2 {
        return false;
    }
    adjacent_query_ir_focus_compounds(&entity_values)
        .into_iter()
        .any(|compound| setup_focus_identity_tokens(&compound).len() >= 2)
}

fn setup_variant_candidate_document_ids(
    question: &str,
    query_ir: Option<&QueryIR>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    limit: usize,
) -> Vec<Uuid> {
    let query_terms = setup_variant_query_terms(question, query_ir);
    if query_terms.is_empty() || limit == 0 {
        return Vec::new();
    }
    let subject_terms = setup_variant_subject_terms(query_ir);
    let identity_document_count = document_index
        .values()
        .filter(|document| document.document_role == crate::domains::content::DOCUMENT_ROLE_PRIMARY)
        .filter(|document| !setup_focus_document_is_standalone_image(document))
        .count();
    let query_term_frequencies =
        setup_variant_query_term_document_frequency(&query_terms, document_index);
    let mut candidates = document_index
        .values()
        .filter(|document| document.document_role == crate::domains::content::DOCUMENT_ROLE_PRIMARY)
        .filter(|document| !setup_focus_document_is_standalone_image(document))
        .filter_map(|document| {
            let values = setup_focus_document_identity_values(document);
            let best_score = values
                .iter()
                .map(|value| {
                    let value_tokens = normalized_alnum_tokens(value, 3);
                    let subject_overlap = soft_token_overlap_count(&subject_terms, &value_tokens);
                    let overlap = soft_token_overlap_count(&query_terms, &value_tokens);
                    if overlap == 0 {
                        return 0usize;
                    }
                    let weighted_overlap = setup_variant_weighted_query_overlap_score(
                        &query_terms,
                        &value_tokens,
                        &query_term_frequencies,
                        identity_document_count,
                    );
                    subject_overlap
                        .saturating_mul(384)
                        .saturating_add(weighted_overlap)
                        .saturating_add(overlap.saturating_mul(16))
                        .saturating_add(value_tokens.len().min(24))
                })
                .max()
                .unwrap_or(0);
            (best_score > 0).then_some((
                best_score,
                document.title.clone().unwrap_or_else(|| document.external_key.clone()),
                document.document_id,
            ))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)).then_with(|| left.2.cmp(&right.2))
    });
    candidates.into_iter().take(limit).map(|(_, _, document_id)| document_id).collect()
}

fn setup_variant_query_term_document_frequency(
    query_terms: &BTreeSet<String>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> BTreeMap<String, usize> {
    let mut frequencies = BTreeMap::<String, usize>::new();
    if query_terms.is_empty() {
        return frequencies;
    }
    for document in document_index.values() {
        if document.document_role != crate::domains::content::DOCUMENT_ROLE_PRIMARY
            || setup_focus_document_is_standalone_image(document)
        {
            continue;
        }
        let mut matched = BTreeSet::<String>::new();
        for value in setup_focus_document_identity_values(document) {
            let value_tokens = normalized_alnum_tokens(&value, 3);
            for query_term in query_terms {
                if value_tokens.iter().any(|value_token| soft_token_match(query_term, value_token))
                {
                    matched.insert(query_term.clone());
                }
            }
        }
        for token in matched {
            *frequencies.entry(token).or_default() += 1;
        }
    }
    frequencies
}

fn setup_variant_weighted_query_overlap_score(
    query_terms: &BTreeSet<String>,
    value_tokens: &BTreeSet<String>,
    frequencies: &BTreeMap<String, usize>,
    document_count: usize,
) -> usize {
    query_terms
        .iter()
        .filter(|query_term| {
            value_tokens.iter().any(|value_token| soft_token_match(query_term, value_token))
        })
        .map(|query_term| {
            let frequency = frequencies.get(query_term).copied().unwrap_or(document_count).max(1);
            let rarity = document_count.saturating_div(frequency).clamp(1, 64);
            rarity
                .saturating_mul(48)
                .saturating_add(usize::from(query_term.chars().count() <= 4).saturating_mul(48))
        })
        .sum()
}

fn setup_variant_subject_terms(query_ir: Option<&QueryIR>) -> BTreeSet<String> {
    let mut terms = BTreeSet::<String>::new();
    if let Some(query_ir) = query_ir {
        for entity in &query_ir.target_entities {
            terms.extend(normalized_alnum_tokens(&entity.label, 3));
        }
        if let Some(document_focus) = query_ir.document_focus.as_ref() {
            terms.extend(normalized_alnum_tokens(&document_focus.hint, 3));
        }
    }
    terms
}

#[derive(Default)]
struct SetupVariantFamilyModel {
    document_count: usize,
    token_document_frequency: BTreeMap<String, usize>,
}

fn setup_variant_family_model(
    candidate_document_ids: &[Uuid],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    query_terms: &BTreeSet<String>,
) -> SetupVariantFamilyModel {
    let mut token_document_frequency = BTreeMap::<String, usize>::new();
    let mut document_count = 0usize;
    for document_id in candidate_document_ids {
        let Some(document) = document_index.get(document_id) else {
            continue;
        };
        let tokens = setup_variant_document_family_tokens(document, query_terms);
        if tokens.is_empty() {
            continue;
        }
        document_count = document_count.saturating_add(1);
        for token in tokens {
            *token_document_frequency.entry(token).or_default() += 1;
        }
    }
    SetupVariantFamilyModel { document_count, token_document_frequency }
}

fn setup_variant_document_family(
    document: &KnowledgeDocumentRow,
    query_terms: &BTreeSet<String>,
    family_model: &SetupVariantFamilyModel,
) -> String {
    let tokens = setup_variant_document_family_tokens(document, query_terms);
    if let Some(token) = tokens.iter().find(|token| {
        family_model
            .token_document_frequency
            .get(*token)
            .is_some_and(|frequency| *frequency < family_model.document_count)
    }) {
        return token.clone();
    }
    if !tokens.is_empty() {
        return tokens.join(" ");
    }
    document.document_id.to_string()
}

fn setup_variant_document_family_tokens(
    document: &KnowledgeDocumentRow,
    query_terms: &BTreeSet<String>,
) -> Vec<String> {
    let title = document
        .title
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&document.external_key);
    normalized_alnum_tokens(title, 3)
        .into_iter()
        .filter(|token| !query_terms.iter().any(|query_token| soft_token_match(query_token, token)))
        .collect::<Vec<_>>()
}

fn setup_variant_query_terms(question: &str, query_ir: Option<&QueryIR>) -> BTreeSet<String> {
    let mut terms: BTreeSet<String> =
        normalized_alnum_tokens(strip_leading_question_marker(question), 3).into_iter().collect();
    if let Some(query_ir) = query_ir {
        for entity in &query_ir.target_entities {
            terms.extend(normalized_alnum_tokens(&entity.label, 3));
        }
        if let Some(document_focus) = query_ir.document_focus.as_ref() {
            terms.extend(normalized_alnum_tokens(&document_focus.hint, 3));
        }
        if let Some(retrieval_query) = query_ir.retrieval_query.as_deref() {
            terms.extend(normalized_alnum_tokens(retrieval_query, 3));
        }
    }
    terms
}

fn soft_token_overlap_count(left: &BTreeSet<String>, right: &BTreeSet<String>) -> usize {
    left.iter()
        .filter(|left_token| {
            right.iter().any(|right_token| soft_token_match(left_token, right_token))
        })
        .count()
}

fn strict_token_overlap_count(left: &BTreeSet<String>, right: &BTreeSet<String>) -> usize {
    left.iter()
        .filter(|left_token| {
            right.iter().any(|right_token| near_token_match(left_token, right_token))
        })
        .count()
}

fn soft_token_match(left: &str, right: &str) -> bool {
    if near_token_match(left, right) {
        return true;
    }
    let left_len = left.chars().count();
    let right_len = right.chars().count();
    if left_len < 5 || right_len < 5 {
        return false;
    }
    let common_prefix = common_prefix_char_count(left, right);
    common_prefix >= 4 && common_prefix.saturating_mul(2) >= left_len.min(right_len)
}

async fn load_versioned_update_procedure_chunks(
    state: &AppState,
    library_id: Uuid,
    question: &str,
    query_ir: Option<&QueryIR>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    temporal_start: Option<DateTime<Utc>>,
    temporal_end: Option<DateTime<Utc>>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if temporal_start.is_some()
        || temporal_end.is_some()
        || !question_requests_versioned_update_procedure_evidence(question, query_ir)
    {
        return Ok(Vec::new());
    }
    let mut title_candidates = versioned_update_procedure_candidate_documents(
        question,
        query_ir,
        document_index,
        VERSIONED_UPDATE_PROCEDURE_TITLE_SCAN_CANDIDATE_CAP,
    );
    let mut instruction_title_scan_candidates =
        versioned_update_procedure_instruction_title_candidates(
            question,
            query_ir,
            document_index,
            VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_RUNBOOK_SCAN_CAP,
        );
    let exact_target_runbook_scan_candidates =
        versioned_update_procedure_exact_target_runbook_scan_candidates(
            question,
            query_ir,
            document_index,
            VERSIONED_UPDATE_PROCEDURE_EXACT_TARGET_RUNBOOK_SCAN_CAP,
        );
    let exact_action_title_candidates = versioned_update_procedure_exact_action_title_candidates(
        question,
        query_ir,
        document_index,
        VERSIONED_UPDATE_PROCEDURE_EXACT_ACTION_TITLE_RESERVE_CAP,
    );
    ensure_reserved_versioned_update_procedure_title_candidates(
        &mut instruction_title_scan_candidates,
        exact_action_title_candidates.clone(),
        VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_RUNBOOK_SCAN_CAP,
    );
    ensure_reserved_versioned_update_procedure_title_candidates(
        &mut instruction_title_scan_candidates,
        exact_target_runbook_scan_candidates.clone(),
        VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_RUNBOOK_SCAN_CAP,
    );
    let instruction_title_candidates = instruction_title_scan_candidates
        .iter()
        .take(VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_RESERVE_CAP)
        .cloned()
        .collect::<Vec<_>>();
    let instruction_title_anchor_candidates = instruction_title_candidates.clone();
    ensure_reserved_versioned_update_procedure_title_candidates(
        &mut title_candidates,
        instruction_title_candidates.clone(),
        VERSIONED_UPDATE_PROCEDURE_TITLE_SCAN_CANDIDATE_CAP,
    );
    let exact_title_candidate_document_ids = title_candidates
        .iter()
        .filter(|candidate| candidate.exact_title_identity)
        .map(|candidate| candidate.document_id)
        .collect::<BTreeSet<_>>();
    let mut reserved_title_candidates = instruction_title_candidates;
    reserved_title_candidates.extend(exact_action_title_candidates.clone());
    reserved_title_candidates.extend(
        title_candidates
            .iter()
            .filter(|candidate| {
                candidate.exact_title_identity
                    || versioned_update_procedure_candidate_has_strong_subject_title(candidate)
            })
            .take(VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_RESERVE_CAP)
            .cloned(),
    );
    let procedure_term_model = versioned_update_procedure_term_model(question, query_ir);
    let evidence_candidates = load_versioned_update_procedure_evidence_candidates(
        state,
        library_id,
        question,
        &procedure_term_model,
        document_index,
        plan_keywords,
        temporal_start,
        temporal_end,
    )
    .await?;
    let mut candidate_documents = merge_versioned_update_procedure_document_candidates(
        title_candidates,
        evidence_candidates,
        VERSIONED_UPDATE_PROCEDURE_DOCUMENT_CANDIDATE_CAP,
    );
    ensure_reserved_versioned_update_procedure_title_candidates(
        &mut candidate_documents,
        reserved_title_candidates,
        VERSIONED_UPDATE_PROCEDURE_DOCUMENT_CANDIDATE_CAP,
    );
    if candidate_documents.is_empty() {
        return Ok(Vec::new());
    }
    let candidate_document_count = candidate_documents.len();
    let seeded_runbook_candidates =
        versioned_update_procedure_seeded_runbook_candidates(&candidate_documents);
    let structural_source_candidates =
        versioned_update_procedure_structural_source_candidates(&candidate_documents);
    let focus_terms = versioned_update_procedure_focus_terms(question, query_ir, plan_keywords);
    let procedure_focus_terms =
        versioned_update_procedure_terms_as_focus_terms(&procedure_term_model.procedure_terms);
    let fetch_inputs = candidate_documents
        .into_iter()
        .enumerate()
        .filter(|(_, candidate)| {
            !versioned_update_procedure_candidate_is_source_local_anchor_only(candidate)
        })
        .filter_map(|(document_rank, candidate)| {
            let document = document_index.get(&candidate.document_id)?;
            let revision_id = canonical_document_revision_id(document)?;
            Some((document_rank, candidate.clone(), revision_id))
        })
        .collect::<Vec<_>>();
    let fetched_results = stream::iter(fetch_inputs.into_iter().map(
        |(document_rank, candidate, revision_id)| {
            let focus_terms = if candidate.requires_action_text_match {
                procedure_focus_terms.clone()
            } else {
                focus_terms.clone()
            };
            let procedure_term_model = procedure_term_model.clone();
            async move {
                let mut rows = if candidate.seed_chunk_indices.is_empty() {
                    state
                        .document_store
                        .list_chunks_by_revision_matching_terms(
                            revision_id,
                            &focus_terms,
                            VERSIONED_UPDATE_PROCEDURE_MATCH_PROBE_CHUNKS_PER_DOCUMENT,
                        )
                        .await
                            .with_context(|| {
                                format!(
                                    "failed to load versioned update procedure chunks for document {} revision {}",
                                    candidate.document_id, revision_id
                                )
                            })?
                } else {
                    let seed_rows = load_versioned_update_procedure_seed_context_rows(
                        state,
                        revision_id,
                        &candidate.seed_chunk_indices,
                        VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT,
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "failed to load seeded versioned update procedure chunks for document {} revision {}",
                            candidate.document_id, revision_id
                        )
                    })?;
                    let probe_rows = state
                        .document_store
                        .list_chunks_by_revision_matching_terms(
                            revision_id,
                            &focus_terms,
                            VERSIONED_UPDATE_PROCEDURE_MATCH_PROBE_CHUNKS_PER_DOCUMENT,
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "failed to probe seeded versioned update procedure chunks for document {} revision {}",
                                candidate.document_id, revision_id
                            )
                        })?;
                    let probe_rows = select_versioned_update_procedure_probe_seed_rows(
                        probe_rows,
                        &candidate,
                        &procedure_term_model,
                        document_index,
                        &focus_terms,
                        VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT,
                    );
                    merge_versioned_update_procedure_context_rows(
                        probe_rows,
                        seed_rows,
                        VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT,
                    )
                };
                if candidate.seed_chunk_indices.is_empty() {
                    rows = select_versioned_update_procedure_probe_seed_rows(
                        rows,
                        &candidate,
                        &procedure_term_model,
                        document_index,
                        &focus_terms,
                        VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT,
                    );
                }
                if versioned_update_procedure_candidate_has_strong_subject_title(&candidate) {
                    let head_rows = state
                        .document_store
                        .list_chunks_by_revision_range(
                            revision_id,
                            0,
                            VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT.saturating_sub(1) as i32,
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "failed to load title-aligned versioned update procedure chunks for document {} revision {}",
                                candidate.document_id, revision_id
                            )
                        })?;
                    rows = if versioned_update_procedure_candidate_prefers_head_window(&candidate) {
                        merge_versioned_update_procedure_context_rows(
                            head_rows,
                            rows,
                            VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT,
                        )
                    } else {
                        merge_versioned_update_procedure_context_rows(
                            rows,
                            head_rows,
                            VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT,
                        )
                    };
                }
                if !rows.is_empty() {
                    rows = expand_versioned_update_procedure_context_rows(
                        state,
                        revision_id,
                        rows,
                        VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT,
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "failed to expand versioned update procedure chunks for document {} revision {}",
                            candidate.document_id, revision_id
                        )
                    })?;
                }
                if rows.is_empty() && candidate.allow_head_fallback {
                    rows = state
                        .document_store
                        .list_chunks_by_revision_range(
                            revision_id,
                            0,
                            VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT.saturating_sub(1) as i32,
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "failed to load fallback versioned update procedure chunks for document {} revision {}",
                                candidate.document_id, revision_id
                            )
                        })?;
                }
                Ok::<_, anyhow::Error>((document_rank, candidate, rows))
            }
        },
    ))
    .buffer_unordered(VERSIONED_UPDATE_PROCEDURE_DOCUMENT_FETCH_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;
    let mut fetched = fetched_results.into_iter().collect::<anyhow::Result<Vec<_>>>()?;
    fetched.sort_by_key(|(document_rank, _, _)| *document_rank);

    let mut chunks = Vec::new();
    for (document_rank, candidate, rows) in fetched {
        for (chunk_rank, row) in rows.into_iter().enumerate() {
            let score = versioned_update_procedure_candidate_chunk_score(
                &candidate,
                document_rank,
                chunk_rank,
            );
            if let Some(mut chunk) = map_chunk_hit(row, score, document_index, &focus_terms) {
                if !versioned_update_procedure_chunk_satisfies_candidate_text_requirements(
                    &chunk,
                    &candidate,
                    &procedure_term_model,
                ) {
                    continue;
                }
                chunk.score = Some(score);
                chunk.score_kind =
                    if versioned_update_procedure_candidate_prefers_head_window(&candidate) {
                        RuntimeChunkScoreKind::DocumentIdentity
                    } else {
                        RuntimeChunkScoreKind::FocusedDocument
                    };
                chunks.push(chunk);
            }
        }
    }
    let instruction_title_chunks = load_versioned_update_procedure_instruction_title_anchor_chunks(
        state,
        &instruction_title_anchor_candidates,
        document_index,
        &focus_terms,
    )
    .await?;
    if !instruction_title_chunks.is_empty() {
        chunks = merge_versioned_update_procedure_chunks_for_query(
            chunks,
            instruction_title_chunks,
            VERSIONED_UPDATE_PROCEDURE_CONTEXT_RESERVATION_LIMIT,
            query_ir,
        );
    }
    let exact_action_runbook_chunks = load_versioned_update_procedure_exact_target_runbook_chunks(
        state,
        &exact_action_title_candidates,
        document_index,
        &focus_terms,
        question,
        query_ir,
    )
    .await?;
    if !exact_action_runbook_chunks.is_empty() {
        chunks = merge_versioned_update_procedure_chunks_for_query(
            chunks,
            exact_action_runbook_chunks,
            VERSIONED_UPDATE_PROCEDURE_CONTEXT_RESERVATION_LIMIT,
            query_ir,
        );
    }
    let exact_target_runbook_chunks = load_versioned_update_procedure_exact_target_runbook_chunks(
        state,
        &exact_target_runbook_scan_candidates,
        document_index,
        &focus_terms,
        question,
        query_ir,
    )
    .await?;
    if !exact_target_runbook_chunks.is_empty() {
        chunks = merge_versioned_update_procedure_chunks_for_query(
            chunks,
            exact_target_runbook_chunks,
            VERSIONED_UPDATE_PROCEDURE_CONTEXT_RESERVATION_LIMIT,
            query_ir,
        );
    }
    let seeded_runbook_chunks = load_versioned_update_procedure_exact_target_runbook_chunks(
        state,
        &seeded_runbook_candidates,
        document_index,
        &focus_terms,
        question,
        query_ir,
    )
    .await?;
    if !seeded_runbook_chunks.is_empty() {
        chunks = merge_versioned_update_procedure_chunks_for_query(
            chunks,
            seeded_runbook_chunks,
            VERSIONED_UPDATE_PROCEDURE_CONTEXT_RESERVATION_LIMIT,
            query_ir,
        );
    }
    let source_local_runbook_chunks = load_versioned_update_procedure_source_local_runbook_chunks(
        state,
        &chunks,
        document_index,
        &focus_terms,
        question,
        query_ir,
    )
    .await?;
    if !source_local_runbook_chunks.is_empty() {
        chunks = merge_versioned_update_procedure_chunks_for_query(
            chunks,
            source_local_runbook_chunks,
            VERSIONED_UPDATE_PROCEDURE_CONTEXT_RESERVATION_LIMIT,
            query_ir,
        );
    }
    let referenced_document_chunks = load_versioned_update_procedure_reference_document_chunks(
        state,
        &chunks,
        &procedure_term_model,
        document_index,
        &focus_terms,
        question,
        query_ir,
    )
    .await?;
    if !referenced_document_chunks.is_empty() {
        chunks = merge_versioned_update_procedure_chunks_for_query(
            chunks,
            referenced_document_chunks,
            VERSIONED_UPDATE_PROCEDURE_CONTEXT_RESERVATION_LIMIT,
            query_ir,
        );
    }
    let referenced_chunks = load_versioned_update_procedure_reference_chunks(
        state,
        library_id,
        &chunks,
        &procedure_term_model,
        document_index,
        plan_keywords,
        temporal_start,
        temporal_end,
    )
    .await?;
    if !referenced_chunks.is_empty() {
        chunks = merge_versioned_update_procedure_chunks_for_query(
            chunks,
            referenced_chunks,
            VERSIONED_UPDATE_PROCEDURE_CONTEXT_RESERVATION_LIMIT,
            query_ir,
        );
    }
    let contextual_structural_source_candidates =
        versioned_update_procedure_structural_source_candidates_from_chunks(
            &chunks,
            &procedure_term_model,
            document_index,
            VERSIONED_UPDATE_PROCEDURE_CONTEXTUAL_SOURCE_DOCUMENT_CAP,
        );
    let structural_source_chunks = load_versioned_update_procedure_structural_source_chunks(
        state,
        &structural_source_candidates,
        &procedure_term_model,
        document_index,
        &focus_terms,
    )
    .await?;
    if !structural_source_chunks.is_empty() {
        chunks = merge_versioned_update_procedure_chunks_for_query(
            chunks,
            structural_source_chunks,
            VERSIONED_UPDATE_PROCEDURE_CONTEXT_RESERVATION_LIMIT,
            query_ir,
        );
    }
    let contextual_structural_source_chunks =
        load_versioned_update_procedure_structural_source_chunks(
            state,
            &contextual_structural_source_candidates,
            &procedure_term_model,
            document_index,
            &focus_terms,
        )
        .await?;
    if !contextual_structural_source_chunks.is_empty() {
        chunks = merge_versioned_update_procedure_chunks_for_query(
            chunks,
            contextual_structural_source_chunks,
            VERSIONED_UPDATE_PROCEDURE_CONTEXT_RESERVATION_LIMIT,
            query_ir,
        );
    }
    if !chunks.is_empty() {
        tracing::info!(
            stage = "retrieval.versioned_update_procedure",
            procedure_document_count = candidate_document_count,
            procedure_chunk_count = chunks.len(),
            exact_title_candidate_count = exact_title_candidate_document_ids.len(),
            exact_title_chunk_count = chunks
                .iter()
                .filter(|chunk| exact_title_candidate_document_ids.contains(&chunk.document_id))
                .count(),
            "versioned update procedure evidence anchored into answer context"
        );
    }
    Ok(chunks)
}

async fn load_versioned_update_procedure_instruction_title_anchor_chunks(
    state: &AppState,
    candidates: &[VersionedUpdateProcedureDocumentCandidate],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    focus_terms: &[String],
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let fetch_inputs = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            versioned_update_procedure_candidate_prefers_head_window(candidate)
        })
        .filter_map(|(document_rank, candidate)| {
            let document = document_index.get(&candidate.document_id)?;
            let revision_id = canonical_document_revision_id(document)?;
            Some((document_rank, candidate.document_id, revision_id))
        })
        .collect::<Vec<_>>();
    if fetch_inputs.is_empty() {
        return Ok(Vec::new());
    }
    let fetched = stream::iter(fetch_inputs.into_iter().map(
        |(document_rank, document_id, revision_id)| async move {
            let rows = state
                .document_store
                .list_chunks_by_revision_range(
                    revision_id,
                    0,
                    VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT.saturating_sub(1) as i32,
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to load instruction-title versioned update chunks for document {document_id} revision {revision_id}",
                    )
                })?;
            Ok::<_, anyhow::Error>((document_rank, rows))
        },
    ))
    .buffer_unordered(VERSIONED_UPDATE_PROCEDURE_DOCUMENT_FETCH_CONCURRENCY)
    .collect::<Vec<_>>()
    .await
    .into_iter()
    .collect::<anyhow::Result<Vec<_>>>()?;

    let mut chunks = Vec::new();
    for (document_rank, rows) in fetched {
        for (chunk_rank, row) in rows.into_iter().enumerate() {
            let score = versioned_update_procedure_chunk_score(document_rank, chunk_rank)
                + VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_CHUNK_SCORE_BONUS;
            if let Some(mut chunk) = map_chunk_hit(row, score, document_index, focus_terms) {
                chunk.score = Some(score);
                // This is exact-title evidence, but for merge/truncation it is
                // answer-driving procedure context. Keep the high absolute
                // score while using the procedure-reserved lane kind.
                chunk.score_kind = RuntimeChunkScoreKind::FocusedDocument;
                chunks.push(chunk);
            }
        }
    }
    if !chunks.is_empty() {
        tracing::info!(
            stage = "retrieval.versioned_update_procedure_instruction_title",
            instruction_title_chunk_count = chunks.len(),
            "versioned procedure instruction-title anchors loaded into answer context"
        );
    }
    Ok(chunks)
}

async fn load_versioned_update_procedure_exact_target_runbook_chunks(
    state: &AppState,
    candidates: &[VersionedUpdateProcedureDocumentCandidate],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    focus_terms: &[String],
    question: &str,
    query_ir: Option<&QueryIR>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let Some(query_ir) = query_ir else {
        return Ok(Vec::new());
    };
    if candidates.is_empty()
        || !query_ir_requests_update_procedure_runbook_context(question, query_ir)
    {
        return Ok(Vec::new());
    }
    let fetch_inputs = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            candidate.exact_title_identity
                || !candidate.seed_chunk_indices.is_empty()
                || !candidate.source_local_anchor_indices.is_empty()
        })
        .filter_map(|(document_rank, candidate)| {
            let document = document_index.get(&candidate.document_id)?;
            let revision_id = canonical_document_revision_id(document)?;
            Some((document_rank, candidate.clone(), revision_id))
        })
        .collect::<Vec<_>>();
    if fetch_inputs.is_empty() {
        return Ok(Vec::new());
    }
    let fetched = stream::iter(fetch_inputs.into_iter().map(
        |(document_rank, candidate, revision_id)| async move {
            let rows = if candidate.seed_chunk_indices.is_empty()
                && candidate.source_local_anchor_indices.is_empty()
            {
                state
                    .document_store
                    .list_chunks_by_revision_range(
                        revision_id,
                        0,
                        VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT.saturating_sub(1) as i32,
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "failed to load exact-target versioned update runbook chunks for document {} revision {}",
                            candidate.document_id, revision_id,
                        )
                    })?
            } else {
                let anchor_indices = if candidate.seed_chunk_indices.is_empty() {
                    &candidate.source_local_anchor_indices
                } else {
                    &candidate.seed_chunk_indices
                };
                let seed_rows = load_versioned_update_procedure_seed_context_rows(
                    state,
                    revision_id,
                    anchor_indices,
                    VERSIONED_UPDATE_PROCEDURE_SOURCE_LOCAL_RUNBOOK_CONTEXT_LIMIT,
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to load seeded exact-target versioned update runbook chunks for document {} revision {}",
                        candidate.document_id, revision_id,
                    )
                })?;
                let term_model = versioned_update_procedure_term_model(question, Some(query_ir));
                let probe_rows = state
                    .document_store
                    .list_chunks_by_revision_matching_terms(
                        revision_id,
                        focus_terms,
                        VERSIONED_UPDATE_PROCEDURE_MATCH_PROBE_CHUNKS_PER_DOCUMENT,
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "failed to probe seeded exact-target versioned update runbook chunks for document {} revision {}",
                            candidate.document_id, revision_id,
                        )
                    })?;
                let probe_rows = select_versioned_update_procedure_probe_seed_rows(
                    probe_rows,
                    &candidate,
                    &term_model,
                    document_index,
                    focus_terms,
                    VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT,
                );
                merge_versioned_update_procedure_context_rows(
                    probe_rows,
                    seed_rows,
                    VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT,
                )
            };
            Ok::<_, anyhow::Error>((document_rank, rows))
        },
    ))
    .buffer_unordered(VERSIONED_UPDATE_PROCEDURE_DOCUMENT_FETCH_CONCURRENCY)
    .collect::<Vec<_>>()
    .await
    .into_iter()
    .collect::<anyhow::Result<Vec<_>>>()?;

    let mut chunks = Vec::new();
    for (document_rank, rows) in fetched {
        for (chunk_rank, row) in rows.into_iter().enumerate() {
            let base_score = versioned_update_procedure_exact_target_runbook_chunk_score(
                document_rank,
                chunk_rank,
                0,
            );
            let Some(mut chunk) = map_chunk_hit(row, base_score, document_index, focus_terms)
            else {
                continue;
            };
            let Some(runbook_score) =
                versioned_update_exact_target_runbook_score(question, query_ir, &chunk)
            else {
                continue;
            };
            if !versioned_update_exact_target_runbook_matches_query_specificity(
                question, query_ir, &chunk,
            ) {
                continue;
            }
            let score = versioned_update_procedure_exact_target_runbook_chunk_score(
                document_rank,
                chunk_rank,
                runbook_score,
            );
            chunk.score = Some(score);
            // Exact-target procedure runbooks are both document-identity
            // evidence and answer-driving command context. Keep them in the
            // identity-grade lane so later QueryIR/setup companion merges do
            // not evict the only full procedure body as ordinary focused tail.
            chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
            tracing::debug!(
                stage = "retrieval.versioned_update_procedure_exact_target_runbook_candidate",
                chunk_id = %chunk.chunk_id,
                document_id = %chunk.document_id,
                document_rank,
                chunk_rank,
                runbook_score,
                score,
                "exact-target versioned update runbook candidate accepted"
            );
            chunks.push(chunk);
        }
    }
    if !chunks.is_empty() {
        tracing::info!(
            stage = "retrieval.versioned_update_procedure_exact_target_runbook",
            runbook_candidate_count = candidates.len(),
            runbook_chunk_count = chunks.len(),
            "exact-target versioned update runbook chunks loaded into answer context"
        );
    }
    Ok(chunks)
}

async fn load_versioned_update_procedure_source_local_runbook_chunks(
    state: &AppState,
    source_chunks: &[RuntimeMatchedChunk],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    focus_terms: &[String],
    question: &str,
    query_ir: Option<&QueryIR>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let Some(query_ir) = query_ir else {
        return Ok(Vec::new());
    };
    if source_chunks.is_empty()
        || !query_ir_requests_update_procedure_runbook_context(question, query_ir)
    {
        return Ok(Vec::new());
    }

    let mut fetch_inputs = Vec::<(usize, Uuid, Uuid, Vec<i32>)>::with_capacity(
        VERSIONED_UPDATE_PROCEDURE_SOURCE_LOCAL_RUNBOOK_DOCUMENT_CAP,
    );
    let mut fetch_input_by_document = HashMap::<Uuid, usize>::new();
    for chunk in source_chunks {
        let Some(document) = document_index.get(&chunk.document_id) else {
            continue;
        };
        let Some(revision_id) = canonical_document_revision_id(document) else {
            continue;
        };
        if let Some(index) = fetch_input_by_document.get(&chunk.document_id).copied() {
            let anchor_indices = &mut fetch_inputs[index].3;
            if anchor_indices.len()
                < VERSIONED_UPDATE_PROCEDURE_SOURCE_LOCAL_RUNBOOK_ANCHORS_PER_DOCUMENT
                && !anchor_indices.contains(&chunk.chunk_index)
            {
                anchor_indices.push(chunk.chunk_index);
            }
            continue;
        }
        if fetch_inputs.len() >= VERSIONED_UPDATE_PROCEDURE_SOURCE_LOCAL_RUNBOOK_DOCUMENT_CAP {
            continue;
        }
        let document_rank = fetch_inputs.len();
        fetch_input_by_document.insert(chunk.document_id, document_rank);
        fetch_inputs.push((document_rank, chunk.document_id, revision_id, vec![chunk.chunk_index]));
    }
    if fetch_inputs.is_empty() {
        return Ok(Vec::new());
    }

    let fetched = stream::iter(fetch_inputs.into_iter().map(
        |(document_rank, document_id, revision_id, anchor_indices)| async move {
            let windows = versioned_update_procedure_source_local_context_windows(
                &anchor_indices,
                VERSIONED_UPDATE_PROCEDURE_SOURCE_LOCAL_RUNBOOK_CONTEXT_LIMIT,
            );
            let window_rows = if windows.is_empty() {
                Vec::new()
            } else {
                state
                    .document_store
                    .list_chunks_by_revision_windows(revision_id, &windows)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to scan source-local versioned update runbook windows for document {document_id} revision {revision_id}",
                        )
                    })?
            };
            let probe_rows = state
                .document_store
                .list_chunks_by_revision_matching_terms(
                    revision_id,
                    focus_terms,
                    VERSIONED_UPDATE_PROCEDURE_MATCH_PROBE_CHUNKS_PER_DOCUMENT,
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to probe source-local versioned update runbook chunks for document {document_id} revision {revision_id}",
                    )
                })?;
            let exact_probe_rows = select_versioned_update_procedure_exact_target_runbook_probe_rows(
                &probe_rows,
                question,
                Some(query_ir),
                document_index,
                focus_terms,
                VERSIONED_UPDATE_PROCEDURE_PROBE_SEED_ROWS_PER_DOCUMENT,
            );
            let rows = if exact_probe_rows.is_empty() {
                window_rows
            } else {
                merge_versioned_update_procedure_context_rows(
                    exact_probe_rows,
                    window_rows,
                    VERSIONED_UPDATE_PROCEDURE_SOURCE_LOCAL_RUNBOOK_CONTEXT_LIMIT,
                )
            };
            Ok::<_, anyhow::Error>((document_rank, rows))
        },
    ))
    .buffer_unordered(VERSIONED_UPDATE_PROCEDURE_DOCUMENT_FETCH_CONCURRENCY)
    .collect::<Vec<_>>()
    .await
    .into_iter()
    .collect::<anyhow::Result<Vec<_>>>()?;

    let existing_chunk_ids =
        source_chunks.iter().map(|chunk| chunk.chunk_id).collect::<BTreeSet<_>>();
    let mut chunks = Vec::new();
    for (document_rank, rows) in fetched {
        for (chunk_rank, row) in rows.into_iter().enumerate() {
            if existing_chunk_ids.contains(&row.chunk_id) {
                continue;
            }
            let base_score = versioned_update_procedure_exact_target_runbook_chunk_score(
                document_rank,
                chunk_rank,
                0,
            );
            let Some(mut chunk) = map_chunk_hit(row, base_score, document_index, focus_terms)
            else {
                continue;
            };
            let Some(runbook_score) =
                versioned_update_exact_target_runbook_score(question, query_ir, &chunk)
            else {
                continue;
            };
            if !versioned_update_exact_target_runbook_matches_query_specificity(
                question, query_ir, &chunk,
            ) {
                continue;
            }
            let score = versioned_update_procedure_exact_target_runbook_chunk_score(
                document_rank,
                chunk_rank,
                runbook_score,
            );
            chunk.score = Some(score);
            chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
            chunks.push(chunk);
        }
    }
    if !chunks.is_empty() {
        tracing::info!(
            stage = "retrieval.versioned_update_procedure_source_local_runbook",
            runbook_chunk_count = chunks.len(),
            "source-local versioned update runbook chunks loaded into answer context"
        );
    }
    Ok(chunks)
}

fn versioned_update_exact_target_runbook_matches_query_specificity(
    question: &str,
    query_ir: &QueryIR,
    chunk: &RuntimeMatchedChunk,
) -> bool {
    if !versioned_update_query_requests_version_specific_runbook(question, query_ir) {
        return true;
    }
    let term_model = versioned_update_procedure_term_model(question, Some(query_ir));
    let evidence = versioned_update_procedure_chunk_evidence(chunk, &term_model);
    evidence.version_transition_score > 0
        || evidence.subject_aligned_version_transition_score > 0
        || (evidence.label_subject_overlap > 0
            && evidence.label_procedure_overlap > 0
            && evidence.command_sequence_score > 0
            && !evidence.has_setup_script_signature)
}

fn versioned_update_query_requests_version_specific_runbook(
    question: &str,
    query_ir: &QueryIR,
) -> bool {
    query_ir.target_types.iter().any(|target_type| {
        matches!(
            canonical_target_type_tag(target_type).as_str(),
            "version" | "release" | "changelog"
        )
    }) || query_ir.source_slice.as_ref().is_some_and(|slice| {
        matches!(slice.filter, crate::domains::query_ir::SourceSliceFilter::ReleaseMarker)
    }) || extract_semver_like_version(question).is_some()
        || extract_release_context_version(question).is_some()
        || query_ir.retrieval_query.as_deref().is_some_and(|query| {
            extract_semver_like_version(query).is_some()
                || extract_release_context_version(query).is_some()
        })
}

async fn load_versioned_update_procedure_reference_document_chunks(
    state: &AppState,
    anchor_chunks: &[RuntimeMatchedChunk],
    term_model: &VersionedUpdateProcedureTermModel,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    fallback_focus_terms: &[String],
    question: &str,
    query_ir: Option<&QueryIR>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let phrases = versioned_update_procedure_reference_phrases(
        anchor_chunks,
        term_model,
        VERSIONED_UPDATE_PROCEDURE_REFERENCE_SEARCH_QUERY_CAP.saturating_mul(2),
    );
    if phrases.is_empty() {
        return Ok(Vec::new());
    }
    let candidates = versioned_update_procedure_reference_document_candidates(
        &phrases,
        term_model,
        document_index,
        VERSIONED_UPDATE_PROCEDURE_REFERENCE_DOCUMENT_CANDIDATE_CAP,
    );
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let reference_focus_terms = versioned_update_procedure_reference_focus_terms(
        &phrases,
        term_model,
        fallback_focus_terms,
    );
    if reference_focus_terms.is_empty() {
        return Ok(Vec::new());
    }
    let fetch_inputs = candidates
        .into_iter()
        .enumerate()
        .filter_map(|(document_rank, candidate)| {
            let document = document_index.get(&candidate.document_id)?;
            let revision_id = canonical_document_revision_id(document)?;
            Some((document_rank, candidate, revision_id))
        })
        .collect::<Vec<_>>();
    if fetch_inputs.is_empty() {
        return Ok(Vec::new());
    }

    let fetched_results = stream::iter(fetch_inputs.into_iter().map(
        |(document_rank, candidate, revision_id)| {
            let reference_focus_terms = reference_focus_terms.clone();
            async move {
                let mut rows = state
                    .document_store
                    .list_chunks_by_revision_matching_terms(
                        revision_id,
                        &reference_focus_terms,
                        VERSIONED_UPDATE_PROCEDURE_MATCH_PROBE_CHUNKS_PER_DOCUMENT,
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "failed to load referenced versioned update procedure chunks for document {} revision {}",
                            candidate.document_id, revision_id
                        )
                    })?;
                let exact_runbook_rows =
                    select_versioned_update_procedure_exact_target_runbook_probe_rows(
                        &rows,
                        question,
                        query_ir,
                        document_index,
                        &reference_focus_terms,
                        VERSIONED_UPDATE_PROCEDURE_PROBE_SEED_ROWS_PER_DOCUMENT,
                    );
                rows = select_versioned_update_procedure_probe_seed_rows(
                    rows,
                    &candidate,
                    term_model,
                    document_index,
                    &reference_focus_terms,
                    VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT,
                );
                if !exact_runbook_rows.is_empty() {
                    rows = merge_versioned_update_procedure_context_rows(
                        exact_runbook_rows,
                        rows,
                        VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT,
                    );
                }
                if !rows.is_empty() {
                    rows = expand_versioned_update_procedure_context_rows(
                        state,
                        revision_id,
                        rows,
                        VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT,
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "failed to expand referenced versioned update procedure chunks for document {} revision {}",
                            candidate.document_id, revision_id
                        )
                    })?;
                }
                Ok::<_, anyhow::Error>((document_rank, candidate, rows))
            }
        },
    ))
    .buffer_unordered(VERSIONED_UPDATE_PROCEDURE_DOCUMENT_FETCH_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;
    let mut fetched = fetched_results.into_iter().collect::<anyhow::Result<Vec<_>>>()?;
    fetched.sort_by_key(|(document_rank, _, _)| *document_rank);

    let mut chunks = Vec::new();
    for (document_rank, candidate, rows) in fetched {
        for (chunk_rank, row) in rows.into_iter().enumerate() {
            if let Some(mut chunk) = map_chunk_hit(
                row,
                versioned_update_procedure_chunk_score(document_rank, chunk_rank),
                document_index,
                &reference_focus_terms,
            ) {
                let exact_runbook_score = query_ir.and_then(|query_ir| {
                    versioned_update_exact_target_runbook_score(question, query_ir, &chunk).filter(
                        |_| {
                            versioned_update_exact_target_runbook_matches_query_specificity(
                                question, query_ir, &chunk,
                            )
                        },
                    )
                });
                if exact_runbook_score.is_none()
                    && !versioned_update_procedure_chunk_satisfies_candidate_text_requirements(
                        &chunk, &candidate, term_model,
                    )
                {
                    continue;
                }
                let evidence = versioned_update_procedure_chunk_evidence(&chunk, term_model);
                let score = exact_runbook_score.map_or_else(
                    || {
                        versioned_update_procedure_reference_chunk_score(
                            evidence,
                            document_rank,
                            chunk_rank,
                        )
                    },
                    |runbook_score| {
                        versioned_update_procedure_exact_target_runbook_chunk_score(
                            document_rank,
                            chunk_rank,
                            runbook_score,
                        )
                    },
                );
                chunk.score = Some(score);
                chunk.score_kind = if exact_runbook_score.is_some() {
                    RuntimeChunkScoreKind::DocumentIdentity
                } else {
                    RuntimeChunkScoreKind::FocusedDocument
                };
                chunks.push(chunk);
            }
        }
    }
    if !chunks.is_empty() {
        tracing::info!(
            stage = "retrieval.versioned_update_procedure_reference_document",
            referenced_document_chunk_count = chunks.len(),
            "versioned procedure referenced document title context anchored into answer context"
        );
    }
    Ok(chunks)
}

fn versioned_update_procedure_reference_chunk_score(
    evidence: VersionedUpdateProcedureChunkEvidence,
    document_rank: usize,
    chunk_rank: usize,
) -> f32 {
    VERSIONED_UPDATE_PROCEDURE_SCORE_BASE + 12_000.0 + evidence.score.min(20_000) as f32 / 32.0
        - document_rank as f32 * 8.0
        - chunk_rank as f32
}

fn versioned_update_procedure_reference_document_candidates(
    phrases: &[String],
    term_model: &VersionedUpdateProcedureTermModel,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    limit: usize,
) -> Vec<VersionedUpdateProcedureDocumentCandidate> {
    if phrases.is_empty() || limit == 0 {
        return Vec::new();
    }
    let mut candidates = document_index
        .values()
        .filter(|document| document.document_role == crate::domains::content::DOCUMENT_ROLE_PRIMARY)
        .filter(|document| !setup_focus_document_is_standalone_image(document))
        .filter_map(|document| {
            let title = document.title.as_deref().unwrap_or("").trim();
            if title.is_empty() {
                return None;
            }
            let matched_phrase =
                phrases.iter().find(|phrase| token_sequence_contains(title, phrase, 3))?;
            let title_terms =
                normalized_alnum_tokens(title, 2).into_iter().collect::<BTreeSet<_>>();
            let subject_overlap =
                strict_token_overlap_count(&term_model.subject_terms, &title_terms);
            let subject_acronym_overlap =
                strict_token_overlap_count(&term_model.subject_acronym_terms, &title_terms);
            let procedure_overlap =
                soft_token_overlap_count(&term_model.procedure_terms, &title_terms);
            let phrase_terms =
                normalized_alnum_tokens(matched_phrase, 3).into_iter().collect::<BTreeSet<_>>();
            let phrase_procedure_overlap =
                soft_token_overlap_count(&term_model.procedure_terms, &phrase_terms);
            let phrase_subject_overlap =
                soft_token_overlap_count(&term_model.subject_terms, &phrase_terms).saturating_add(
                    soft_token_overlap_count(&term_model.subject_acronym_terms, &phrase_terms),
                );
            let inherited_subject_identity = VERSIONED_UPDATE_PROCEDURE_STRONG_SUBJECT_IDENTITY_MIN;
            let exact_title_bonus = usize::from(
                token_sequence_contains(matched_phrase, title, 3)
                    && token_sequence_contains(title, matched_phrase, 3),
            )
            .saturating_mul(VERSIONED_UPDATE_PROCEDURE_TITLE_ALIGNMENT_PRIORITY_BONUS);
            let priority = VERSIONED_UPDATE_PROCEDURE_TITLE_ALIGNMENT_PRIORITY_BONUS
                .saturating_add(exact_title_bonus)
                .saturating_add(phrase_procedure_overlap.saturating_mul(1024))
                .saturating_add(phrase_subject_overlap.saturating_mul(768))
                .saturating_add(procedure_overlap.saturating_mul(512))
                .saturating_add(subject_overlap.saturating_mul(128))
                .saturating_add(subject_acronym_overlap.saturating_mul(96))
                .saturating_add(phrase_terms.len().saturating_mul(16));
            Some((
                priority,
                title.to_string(),
                VersionedUpdateProcedureDocumentCandidate {
                    document_id: document.document_id,
                    exact_title_identity: false,
                    target_title_anchor: false,
                    allow_head_fallback: false,
                    requires_action_text_match: true,
                    requires_subject_text_match: false,
                    subject_identity_score: subject_overlap
                        .saturating_add(subject_acronym_overlap)
                        .max(inherited_subject_identity),
                    focus_aligned_command_score: 0,
                    priority,
                    seed_chunk_indices: Vec::new(),
                    source_local_anchor_indices: Vec::new(),
                },
            ))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.document_id.cmp(&right.2.document_id))
    });
    candidates.into_iter().take(limit).map(|(_, _, candidate)| candidate).collect()
}

fn versioned_update_procedure_reference_focus_terms(
    phrases: &[String],
    term_model: &VersionedUpdateProcedureTermModel,
    fallback_focus_terms: &[String],
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut terms = Vec::new();
    for phrase in phrases {
        for token in normalized_alnum_token_sequence(phrase, 3) {
            if seen.insert(token.clone()) {
                terms.push(token);
            }
        }
    }
    for token in term_model
        .procedure_terms
        .iter()
        .chain(term_model.subject_terms.iter())
        .chain(term_model.subject_acronym_terms.iter())
    {
        if seen.insert(token.clone()) {
            terms.push(token.clone());
        }
    }
    for token in fallback_focus_terms {
        let token = token.trim();
        if !token.is_empty() && seen.insert(token.to_lowercase()) {
            terms.push(token.to_string());
        }
    }
    terms
}

async fn load_versioned_update_procedure_reference_chunks(
    state: &AppState,
    library_id: Uuid,
    anchor_chunks: &[RuntimeMatchedChunk],
    term_model: &VersionedUpdateProcedureTermModel,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    temporal_start: Option<DateTime<Utc>>,
    temporal_end: Option<DateTime<Utc>>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let search_queries =
        versioned_update_procedure_reference_search_queries(anchor_chunks, term_model);
    if search_queries.is_empty() {
        return Ok(Vec::new());
    }
    let per_query_futures = search_queries.iter().cloned().map(|search_query| async move {
        state
            .search_store
            .search_chunks(
                library_id,
                &search_query,
                VERSIONED_UPDATE_PROCEDURE_EVIDENCE_SEARCH_HIT_LIMIT,
                temporal_start,
                temporal_end,
            )
            .await
            .map(|rows| {
                rows.into_iter().map(|row| (row.chunk_id, row.score as f32)).collect::<Vec<_>>()
            })
            .with_context(|| {
                format!(
                    "failed to search versioned procedure referenced evidence chunks: {search_query}"
                )
            })
    });
    let per_query_results: Vec<Result<Vec<_>, anyhow::Error>> = join_all(per_query_futures).await;
    let hits =
        combine_versioned_update_procedure_search_results(per_query_results, search_queries.len())?;
    if hits.is_empty() {
        return Ok(Vec::new());
    }
    let chunks = batch_hydrate_hits(state, hits, document_index, plan_keywords, &BTreeSet::new())
        .await
        .context("failed to hydrate versioned procedure referenced chunks")?;
    let chunks = expand_versioned_update_procedure_evidence_chunks(
        state,
        chunks,
        document_index,
        plan_keywords,
    )
    .await
    .context("failed to expand versioned procedure referenced chunks")?;
    let selected = select_versioned_update_procedure_reference_chunks(
        chunks,
        anchor_chunks,
        term_model,
        VERSIONED_UPDATE_PROCEDURE_REFERENCE_CHUNK_CAP,
    );
    if !selected.is_empty() {
        tracing::info!(
            stage = "retrieval.versioned_update_procedure_reference",
            search_query_count = search_queries.len(),
            referenced_chunk_count = selected.len(),
            "versioned procedure referenced command context anchored into answer context"
        );
    }
    Ok(selected)
}

fn versioned_update_procedure_structural_source_candidates(
    candidates: &[VersionedUpdateProcedureDocumentCandidate],
) -> Vec<VersionedUpdateProcedureDocumentCandidate> {
    let mut selected = Vec::new();
    let mut seen_document_ids = HashSet::new();
    for candidate in candidates {
        if selected.len() >= VERSIONED_UPDATE_PROCEDURE_STRUCTURAL_SOURCE_DOCUMENT_CAP {
            break;
        }
        if !versioned_update_procedure_candidate_allows_structural_source_scan(candidate) {
            continue;
        }
        if seen_document_ids.insert(candidate.document_id) {
            selected.push(candidate.clone());
        }
    }
    selected
}

fn versioned_update_procedure_structural_source_candidates_from_chunks(
    chunks: &[RuntimeMatchedChunk],
    term_model: &VersionedUpdateProcedureTermModel,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    limit: usize,
) -> Vec<VersionedUpdateProcedureDocumentCandidate> {
    if chunks.is_empty() || limit == 0 {
        return Vec::new();
    }
    let mut by_document = HashMap::<Uuid, (usize, usize, usize, usize, usize, f32, String)>::new();
    for chunk in chunks {
        if !document_index.get(&chunk.document_id).is_some_and(|document| {
            document.document_role == crate::domains::content::DOCUMENT_ROLE_PRIMARY
        }) {
            continue;
        }
        let evidence = versioned_update_procedure_chunk_evidence(chunk, term_model);
        let identity_score = evidence
            .subject_overlap
            .saturating_add(evidence.label_subject_overlap)
            .saturating_add(evidence.subject_aligned_version_transition_score);
        let command_score = evidence
            .command_or_script_score
            .max(evidence.command_sequence_score)
            .max(evidence.focus_aligned_command_score)
            .max(evidence.ordered_procedure_score);
        let structural_score = evidence
            .structural_score
            .max(versioned_update_procedure_source_context_structural_score(&chunk.source_text));
        if identity_score == 0 && command_score == 0 && structural_score == 0 {
            continue;
        }
        let entry = by_document.entry(chunk.document_id).or_insert_with(|| {
            (0, 0, 0, 0, 0, score_value(chunk.score), chunk.document_label.clone())
        });
        entry.0 = entry.0.max(identity_score);
        entry.1 = entry.1.max(command_score);
        entry.2 = entry.2.max(structural_score);
        entry.3 = entry.3.max(evidence.version_transition_score);
        entry.4 = entry.4.max(evidence.score);
        entry.5 = entry.5.max(score_value(chunk.score));
    }
    let mut rows = by_document
        .into_iter()
        .filter_map(
            |(
                document_id,
                (
                    identity_score,
                    command_score,
                    structural_score,
                    transition_score,
                    evidence_score,
                    chunk_score,
                    label,
                ),
            )| {
                let priority = evidence_score
                    .saturating_add(identity_score.saturating_mul(4096))
                    .saturating_add(command_score.saturating_mul(2048))
                    .saturating_add(structural_score.saturating_mul(1024))
                    .saturating_add(transition_score.saturating_mul(4096))
                    .saturating_add(chunk_score.max(0.0) as usize);
                let candidate = VersionedUpdateProcedureDocumentCandidate {
                    document_id,
                    exact_title_identity: false,
                    target_title_anchor: false,
                    allow_head_fallback: false,
                    requires_action_text_match: false,
                    requires_subject_text_match: false,
                    subject_identity_score: identity_score,
                    focus_aligned_command_score: command_score,
                    priority,
                    seed_chunk_indices: Vec::new(),
                    source_local_anchor_indices: Vec::new(),
                };
                versioned_update_procedure_candidate_allows_structural_source_scan(&candidate)
                    .then_some((priority, label, candidate))
            },
        )
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.document_id.cmp(&right.2.document_id))
    });
    rows.into_iter().take(limit).map(|(_, _, candidate)| candidate).collect()
}

fn versioned_update_procedure_candidate_allows_structural_source_scan(
    candidate: &VersionedUpdateProcedureDocumentCandidate,
) -> bool {
    candidate.exact_title_identity
        || candidate.target_title_anchor
        || candidate.subject_identity_score
            >= VERSIONED_UPDATE_PROCEDURE_STRONG_SUBJECT_IDENTITY_MIN
        || candidate.focus_aligned_command_score > 0
        || candidate.priority >= VERSIONED_UPDATE_PROCEDURE_TITLE_ALIGNMENT_PRIORITY_BONUS
}

async fn load_versioned_update_procedure_structural_source_chunks(
    state: &AppState,
    candidates: &[VersionedUpdateProcedureDocumentCandidate],
    term_model: &VersionedUpdateProcedureTermModel,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    focus_terms: &[String],
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let fetch_inputs = candidates
        .iter()
        .enumerate()
        .filter_map(|(document_rank, candidate)| {
            let document = document_index.get(&candidate.document_id)?;
            let revision_id = canonical_document_revision_id(document)?;
            Some((document_rank, candidate.clone(), revision_id))
        })
        .collect::<Vec<_>>();
    if fetch_inputs.is_empty() {
        return Ok(Vec::new());
    }

    let fetched = stream::iter(fetch_inputs.into_iter().map(
        |(document_rank, candidate, revision_id)| async move {
            let rows = state
                .document_store
                .list_chunks_by_revision_range(
                    revision_id,
                    0,
                    VERSIONED_UPDATE_PROCEDURE_STRUCTURAL_SOURCE_SCAN_CHUNKS,
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to load structural procedure source chunks for document {} revision {}",
                        candidate.document_id, revision_id
                    )
                })?;
            Ok::<_, anyhow::Error>((document_rank, candidate, rows))
        },
    ))
    .buffer_unordered(VERSIONED_UPDATE_PROCEDURE_DOCUMENT_FETCH_CONCURRENCY)
    .collect::<Vec<_>>()
    .await
    .into_iter()
    .collect::<anyhow::Result<Vec<_>>>()?;

    let mut chunks = Vec::new();
    for (document_rank, candidate, rows) in fetched {
        let mut scored_rows = rows
            .into_iter()
            .filter(|row| !is_source_profile_chunk_row(row))
            .filter_map(|row| {
                let base_score = VERSIONED_UPDATE_PROCEDURE_SCORE_BASE + 6_000.0
                    - document_rank as f32 * 8.0
                    - row.chunk_index.max(0) as f32 * 0.01;
                let mut chunk = map_chunk_hit(row, base_score, document_index, focus_terms)?;
                let structural_score = versioned_update_procedure_structural_source_chunk_score(
                    &chunk, &candidate, term_model,
                )?;
                let score = base_score + structural_score.min(20_000) as f32 / 16.0;
                chunk.score = Some(score);
                chunk.score_kind = RuntimeChunkScoreKind::SourceContext;
                Some((structural_score, chunk.chunk_index, chunk))
            })
            .collect::<Vec<_>>();
        scored_rows.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| left.1.cmp(&right.1))
                .then_with(|| left.2.chunk_id.cmp(&right.2.chunk_id))
        });
        chunks.extend(
            scored_rows
                .into_iter()
                .take(VERSIONED_UPDATE_PROCEDURE_STRUCTURAL_SOURCE_CHUNKS_PER_DOCUMENT)
                .map(|(_, _, chunk)| chunk),
        );
    }
    chunks.sort_by(score_desc_chunks);
    chunks.truncate(VERSIONED_UPDATE_PROCEDURE_STRUCTURAL_SOURCE_CHUNK_CAP);
    if !chunks.is_empty() {
        tracing::info!(
            stage = "retrieval.versioned_update_procedure_structural_source",
            structural_source_chunk_count = chunks.len(),
            "structured source-local procedure context anchored into answer context"
        );
    }
    Ok(chunks)
}

fn versioned_update_procedure_structural_source_chunk_score(
    chunk: &RuntimeMatchedChunk,
    candidate: &VersionedUpdateProcedureDocumentCandidate,
    term_model: &VersionedUpdateProcedureTermModel,
) -> Option<usize> {
    let evidence = versioned_update_procedure_chunk_evidence(chunk, term_model);
    if evidence.has_setup_script_signature
        && !versioned_update_procedure_setup_signature_is_action_bound(evidence)
    {
        return None;
    }
    let structural_score =
        versioned_update_procedure_source_context_structural_score(&chunk.source_text);
    if structural_score < 3 {
        return None;
    }
    let candidate_has_identity = candidate.exact_title_identity
        || candidate.target_title_anchor
        || candidate.subject_identity_score
            >= VERSIONED_UPDATE_PROCEDURE_STRONG_SUBJECT_IDENTITY_MIN;
    let chunk_has_identity = evidence.subject_overlap > 0
        || evidence.label_subject_overlap > 0
        || versioned_update_procedure_chunk_has_body_target_identity(chunk, term_model, evidence);
    if !candidate_has_identity && !chunk_has_identity {
        return None;
    }

    Some(
        structural_score
            .saturating_mul(512)
            .saturating_add(evidence.score)
            .saturating_add(evidence.subject_overlap.saturating_mul(256))
            .saturating_add(evidence.label_subject_overlap.saturating_mul(512))
            .saturating_add(evidence.version_transition_score.saturating_mul(1024))
            .saturating_add(evidence.subject_aligned_version_transition_score.saturating_mul(1024))
            .saturating_add(evidence.ordered_procedure_score.saturating_mul(1024))
            .saturating_add(evidence.command_or_script_score.saturating_mul(256)),
    )
}

fn versioned_update_procedure_source_context_structural_score(text: &str) -> usize {
    let list_score = text
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            strip_leading_procedure_order_marker(trimmed) != trimmed
                || trimmed.starts_with(['-', '*', '•'])
        })
        .count()
        .min(8);
    let literal_score = extract_explicit_path_literals(text, 8)
        .len()
        .saturating_add(extract_package_command_literals(text, 4).len())
        .saturating_add(extract_parameter_literals(text, 8).len())
        .saturating_add(extract_config_assignment_literals(text, 8).len())
        .saturating_add(extract_config_section_literals(text, 4).len())
        .min(12);
    let version_score = usize::from(
        extract_release_context_version(text).is_some()
            || extract_semver_like_version(text).is_some(),
    )
    .saturating_mul(3);
    list_score
        .saturating_add(literal_score.saturating_mul(2))
        .saturating_add(version_score)
        .saturating_add(procedure_artifact_token_count(text).min(6))
}

fn select_versioned_update_procedure_reference_chunks(
    chunks: Vec<RuntimeMatchedChunk>,
    existing_chunks: &[RuntimeMatchedChunk],
    term_model: &VersionedUpdateProcedureTermModel,
    limit: usize,
) -> Vec<RuntimeMatchedChunk> {
    if chunks.is_empty() || limit == 0 {
        return Vec::new();
    }
    let candidates = versioned_update_procedure_candidates_from_evidence_chunks(
        &chunks,
        term_model,
        VERSIONED_UPDATE_PROCEDURE_EVIDENCE_DOCUMENT_CANDIDATE_CAP,
    );
    if candidates.is_empty() {
        return Vec::new();
    }
    let candidate_by_document = candidates
        .iter()
        .enumerate()
        .map(|(rank, candidate)| (candidate.document_id, (rank, candidate)))
        .collect::<HashMap<_, _>>();
    let existing_chunk_ids =
        existing_chunks.iter().map(|chunk| chunk.chunk_id).collect::<BTreeSet<_>>();
    let mut scored = Vec::<(usize, RuntimeMatchedChunk)>::new();
    for chunk in chunks {
        if existing_chunk_ids.contains(&chunk.chunk_id) {
            continue;
        }
        let Some((candidate_rank, candidate)) = candidate_by_document.get(&chunk.document_id)
        else {
            continue;
        };
        if !candidate.seed_chunk_indices.iter().any(|seed_index| {
            chunk.chunk_index
                >= seed_index
                    .saturating_sub(VERSIONED_UPDATE_PROCEDURE_EVIDENCE_NEIGHBOR_BACKWARD_CHUNKS)
                && chunk.chunk_index
                    <= seed_index
                        .saturating_add(VERSIONED_UPDATE_PROCEDURE_EVIDENCE_NEIGHBOR_FORWARD_CHUNKS)
        }) {
            continue;
        }
        let evidence = versioned_update_procedure_chunk_evidence(&chunk, term_model);
        if evidence.has_setup_script_signature
            && !versioned_update_procedure_setup_signature_is_action_bound(evidence)
        {
            continue;
        }
        let is_seed = candidate.seed_chunk_indices.contains(&chunk.chunk_index);
        let is_useful_context = evidence.command_or_script_score > 0
            || evidence.procedure_overlap > 0
            || evidence.version_transition_score > 0
            || command_dense_excerpt_for(&chunk.source_text, 280).is_some();
        if !is_seed && !is_useful_context {
            continue;
        }
        let score = candidate
            .priority
            .saturating_sub(candidate_rank.saturating_mul(1024))
            .saturating_add(evidence.score)
            .saturating_add(usize::from(is_seed).saturating_mul(512));
        scored.push((score, chunk));
    }
    scored.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.document_id.cmp(&right.1.document_id))
            .then_with(|| left.1.chunk_index.cmp(&right.1.chunk_index))
            .then_with(|| left.1.chunk_id.cmp(&right.1.chunk_id))
    });
    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    for (_, mut chunk) in scored {
        if selected.len() >= limit {
            break;
        }
        if !seen.insert(chunk.chunk_id) {
            continue;
        }
        let rank = selected.len();
        chunk.score = Some(versioned_update_procedure_chunk_score(0, rank));
        chunk.score_kind = RuntimeChunkScoreKind::FocusedDocument;
        selected.push(chunk);
    }
    selected
}

fn versioned_update_procedure_reference_search_queries(
    chunks: &[RuntimeMatchedChunk],
    term_model: &VersionedUpdateProcedureTermModel,
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut queries = Vec::new();
    for phrase in versioned_update_procedure_reference_phrases(
        chunks,
        term_model,
        VERSIONED_UPDATE_PROCEDURE_REFERENCE_SEARCH_QUERY_CAP,
    ) {
        if queries.len() >= VERSIONED_UPDATE_PROCEDURE_REFERENCE_SEARCH_QUERY_CAP {
            break;
        }
        let Some(query) =
            versioned_update_procedure_reference_query_for_phrase(&phrase, term_model)
        else {
            continue;
        };
        let key = query.to_lowercase();
        if seen.insert(key) {
            queries.push(query);
        }
    }
    queries
}

fn versioned_update_procedure_reference_phrases(
    chunks: &[RuntimeMatchedChunk],
    term_model: &VersionedUpdateProcedureTermModel,
    limit: usize,
) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }
    let mut seen = BTreeSet::new();
    let mut phrases = Vec::new();
    for chunk in chunks {
        if phrases.len() >= limit {
            break;
        }
        if !versioned_update_procedure_chunk_can_seed_reference_queries(chunk, term_model) {
            continue;
        }
        for phrase in quoted_reference_phrases(&chunk.source_text) {
            if phrases.len() >= limit {
                break;
            }
            if versioned_update_procedure_reference_query_for_phrase(&phrase, term_model).is_none()
            {
                continue;
            }
            let key = phrase.to_lowercase();
            if seen.insert(key) {
                phrases.push(phrase);
            }
        }
    }
    phrases
}

fn versioned_update_procedure_chunk_can_seed_reference_queries(
    chunk: &RuntimeMatchedChunk,
    term_model: &VersionedUpdateProcedureTermModel,
) -> bool {
    let evidence = versioned_update_procedure_chunk_evidence(chunk, term_model);
    let has_subject_identity = evidence.subject_overlap > 0 || evidence.label_subject_overlap > 0;
    let has_action = evidence.procedure_overlap > 0 || evidence.label_procedure_overlap > 0;
    has_subject_identity
        && has_action
        && versioned_update_procedure_setup_signature_is_action_bound(evidence)
}

fn versioned_update_procedure_reference_query_for_phrase(
    phrase: &str,
    term_model: &VersionedUpdateProcedureTermModel,
) -> Option<String> {
    let phrase_tokens = normalized_alnum_token_sequence(phrase, 3);
    if !(2..=10).contains(&phrase_tokens.len()) {
        return None;
    }
    let phrase_term_set = phrase_tokens.iter().cloned().collect::<BTreeSet<_>>();
    let phrase_overlap = soft_token_overlap_count(&term_model.procedure_terms, &phrase_term_set)
        .saturating_add(soft_token_overlap_count(&term_model.subject_terms, &phrase_term_set))
        .saturating_add(soft_token_overlap_count(
            &term_model.subject_acronym_terms,
            &phrase_term_set,
        ));
    if phrase_overlap == 0 {
        return None;
    }
    let phrase_token_chars = phrase_tokens.iter().map(|token| token.chars().count()).sum::<usize>();
    if phrase_token_chars < 8 {
        return None;
    }

    let mut seen = BTreeSet::new();
    let mut terms = Vec::new();
    for token in phrase_tokens {
        push_reference_query_token(&mut terms, &mut seen, token);
    }
    for token in
        term_model.subject_terms.iter().chain(term_model.subject_acronym_terms.iter()).take(8)
    {
        push_reference_query_token(&mut terms, &mut seen, token.clone());
    }
    for token in term_model.procedure_terms.iter().take(4) {
        push_reference_query_token(&mut terms, &mut seen, token.clone());
    }
    (!terms.is_empty()).then(|| terms.join(" "))
}

fn push_reference_query_token(terms: &mut Vec<String>, seen: &mut BTreeSet<String>, token: String) {
    let normalized = token.trim().to_string();
    if normalized.is_empty() || !seen.insert(normalized.clone()) {
        return;
    }
    terms.push(normalized);
}

fn quoted_reference_phrases(text: &str) -> Vec<String> {
    const QUOTE_PAIRS: [(char, char); 5] =
        [('«', '»'), ('“', '”'), ('„', '“'), ('"', '"'), ('`', '`')];
    let mut phrases = Vec::new();
    let mut seen = BTreeSet::new();
    for (open, close) in QUOTE_PAIRS {
        for phrase in quoted_phrases_for_pair(text, open, close) {
            let normalized = phrase.split_whitespace().collect::<Vec<_>>().join(" ");
            if normalized.is_empty() {
                continue;
            }
            let key = normalized.to_lowercase();
            if seen.insert(key) {
                phrases.push(normalized);
            }
        }
    }
    phrases
}

fn quoted_phrases_for_pair(text: &str, open: char, close: char) -> Vec<String> {
    let mut phrases = Vec::new();
    let mut current = String::new();
    let mut inside = false;
    for ch in text.chars() {
        if inside {
            if ch == close {
                let phrase = current.trim();
                if !phrase.is_empty() {
                    phrases.push(phrase.to_string());
                }
                current.clear();
                inside = false;
                continue;
            }
            current.push(ch);
        } else if ch == open {
            current.clear();
            inside = true;
        }
    }
    phrases
}

fn versioned_update_procedure_chunk_satisfies_candidate_text_requirements(
    chunk: &RuntimeMatchedChunk,
    candidate: &VersionedUpdateProcedureDocumentCandidate,
    term_model: &VersionedUpdateProcedureTermModel,
) -> bool {
    if !candidate.requires_action_text_match && !candidate.requires_subject_text_match {
        return true;
    }
    let evidence = versioned_update_procedure_chunk_evidence(chunk, term_model);
    (!candidate.requires_action_text_match
        || evidence.procedure_overlap > 0
        || evidence.command_or_script_score > 0)
        && (!candidate.requires_subject_text_match || evidence.subject_overlap > 0)
}

fn select_versioned_update_procedure_probe_seed_rows(
    rows: Vec<KnowledgeChunkRow>,
    candidate: &VersionedUpdateProcedureDocumentCandidate,
    term_model: &VersionedUpdateProcedureTermModel,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    focus_terms: &[String],
    limit: usize,
) -> Vec<KnowledgeChunkRow> {
    if rows.is_empty() || limit == 0 {
        return Vec::new();
    }
    if rows.len() <= limit {
        return rows;
    }

    let mut scored = rows
        .iter()
        .enumerate()
        .filter_map(|(original_rank, row)| {
            let chunk = map_chunk_hit(row.clone(), 0.0, document_index, focus_terms)?;
            if !versioned_update_procedure_chunk_satisfies_candidate_text_requirements(
                &chunk, candidate, term_model,
            ) {
                return None;
            }
            let evidence = versioned_update_procedure_chunk_evidence(&chunk, term_model);
            let has_command_line =
                chunk.source_text.lines().any(versioned_update_procedure_line_has_command_start);
            let is_structural_body_seed = has_command_line;
            let is_version_transition_seed = evidence.version_transition_score > 0
                && (evidence.subject_overlap > 0 || evidence.label_subject_overlap > 0);
            let is_focus_aligned_command_seed = evidence.focus_aligned_command_score > 0
                && (evidence.subject_overlap > 0 || evidence.label_subject_overlap > 0);
            let strong_seed = is_structural_body_seed
                || is_version_transition_seed
                || is_focus_aligned_command_seed;
            let seed_bonus = usize::from(strong_seed)
                .saturating_mul(VERSIONED_UPDATE_PROCEDURE_EVIDENCE_PRIORITY_BONUS);
            let body_subject_action_bonus = usize::from(
                evidence.subject_overlap > 0
                    && (evidence.procedure_overlap > 0 || evidence.command_or_script_score > 0),
            )
            .saturating_mul(VERSIONED_UPDATE_PROCEDURE_TITLE_ALIGNMENT_PRIORITY_BONUS / 2);
            let command_bonus = evidence.command_or_script_score.saturating_mul(4096);
            let focus_aligned_command_bonus =
                evidence.focus_aligned_command_score.saturating_mul(8192);
            let command_sequence_bonus = evidence.command_sequence_score.saturating_mul(32_768);
            let structural_bonus = evidence.structural_score.saturating_mul(1024);
            let has_artifact_materialization =
                versioned_update_procedure_text_has_artifact_materialization(&chunk.source_text);
            let direct_command_runbook_bonus = usize::from(
                evidence.focus_aligned_command_score > 0
                    && evidence.command_sequence_score > 0
                    && !has_artifact_materialization,
            )
            .saturating_mul(VERSIONED_UPDATE_PROCEDURE_TITLE_ALIGNMENT_PRIORITY_BONUS);
            let artifact_materialization_penalty = usize::from(has_artifact_materialization)
                .saturating_mul(VERSIONED_UPDATE_PROCEDURE_EVIDENCE_PRIORITY_BONUS);
            let setup_signature_penalty = usize::from(
                evidence.has_setup_script_signature
                    && !versioned_update_procedure_setup_signature_is_action_bound(evidence),
            )
            .saturating_mul(VERSIONED_UPDATE_PROCEDURE_TITLE_ALIGNMENT_PRIORITY_BONUS);
            let score = seed_bonus
                .saturating_add(body_subject_action_bonus)
                .saturating_add(evidence.score)
                .saturating_add(command_bonus)
                .saturating_add(focus_aligned_command_bonus)
                .saturating_add(command_sequence_bonus)
                .saturating_add(structural_bonus)
                .saturating_add(direct_command_runbook_bonus)
                .saturating_sub(artifact_materialization_penalty)
                .saturating_sub(setup_signature_penalty);
            (score > 0).then_some((strong_seed, score, row.chunk_index, original_rank, row.clone()))
        })
        .collect::<Vec<_>>();

    if scored.is_empty() {
        return rows.into_iter().take(limit).collect();
    }
    scored.sort_by(|left, right| {
        right.1.cmp(&left.1).then_with(|| left.2.cmp(&right.2)).then_with(|| left.3.cmp(&right.3))
    });
    scored.truncate(VERSIONED_UPDATE_PROCEDURE_PROBE_SEED_ROWS_PER_DOCUMENT.min(limit));
    scored.into_iter().map(|(_, _, _, _, row)| row).collect()
}

fn select_versioned_update_procedure_exact_target_runbook_probe_rows(
    rows: &[KnowledgeChunkRow],
    question: &str,
    query_ir: Option<&QueryIR>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    focus_terms: &[String],
    limit: usize,
) -> Vec<KnowledgeChunkRow> {
    let Some(query_ir) = query_ir else {
        return Vec::new();
    };
    if rows.is_empty()
        || limit == 0
        || !query_ir_requests_update_procedure_runbook_context(question, query_ir)
    {
        return Vec::new();
    }
    let mut scored = rows
        .iter()
        .enumerate()
        .filter_map(|(original_rank, row)| {
            let chunk = map_chunk_hit(row.clone(), 0.0, document_index, focus_terms)?;
            let score = versioned_update_exact_target_runbook_score(question, query_ir, &chunk)?;
            if !versioned_update_exact_target_runbook_matches_query_specificity(
                question, query_ir, &chunk,
            ) {
                return None;
            }
            Some((score, row.chunk_index, original_rank, row.chunk_id, row.clone()))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.cmp(&right.2).then_with(|| left.3.cmp(&right.3)))
    });
    scored.truncate(limit);
    scored.into_iter().map(|(_, _, _, _, row)| row).collect()
}

async fn expand_versioned_update_procedure_context_rows(
    state: &AppState,
    revision_id: Uuid,
    rows: Vec<KnowledgeChunkRow>,
    limit: usize,
) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
    if rows.is_empty() || limit == 0 {
        return Ok(rows);
    }
    let windows = versioned_update_procedure_context_windows(&rows, limit);
    let expanded_rows =
        state.document_store.list_chunks_by_revision_windows(revision_id, &windows).await?;
    Ok(merge_versioned_update_procedure_context_rows(rows, expanded_rows, limit))
}

async fn load_versioned_update_procedure_seed_context_rows(
    state: &AppState,
    revision_id: Uuid,
    seed_chunk_indices: &[i32],
    limit: usize,
) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
    if seed_chunk_indices.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let forward_span = limit.saturating_sub(1).min(i32::MAX as usize) as i32;
    let windows = seed_chunk_indices
        .iter()
        .copied()
        .map(|chunk_index| {
            let start = chunk_index.saturating_sub(1).max(0);
            (start, chunk_index.saturating_add(forward_span))
        })
        .collect::<Vec<_>>();
    let mut rows =
        state.document_store.list_chunks_by_revision_windows(revision_id, &windows).await?;
    rows.sort_by(|left, right| {
        left.chunk_index.cmp(&right.chunk_index).then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    let mut seen_chunk_ids = BTreeSet::new();
    rows.retain(|row| seen_chunk_ids.insert(row.chunk_id));
    rows.truncate(limit);
    Ok(rows)
}

fn versioned_update_procedure_context_windows(
    rows: &[KnowledgeChunkRow],
    limit: usize,
) -> Vec<(i32, i32)> {
    rows.iter()
        .map(|row| {
            let backward_span =
                VERSIONED_UPDATE_PROCEDURE_CONTEXT_BACKWARD_CHUNKS.min(row.chunk_index.max(0));
            let forward_span = limit
                .saturating_sub(1)
                .saturating_sub(backward_span.max(0) as usize)
                .min(i32::MAX as usize) as i32;
            (
                row.chunk_index.saturating_sub(backward_span).max(0),
                row.chunk_index.saturating_add(forward_span),
            )
        })
        .collect()
}

fn versioned_update_procedure_source_local_context_windows(
    anchor_indices: &[i32],
    limit: usize,
) -> Vec<(i32, i32)> {
    if limit == 0 {
        return Vec::new();
    }
    let mut seen = BTreeSet::new();
    anchor_indices
        .iter()
        .copied()
        .filter_map(|chunk_index| {
            if !seen.insert(chunk_index) {
                return None;
            }
            let backward_span =
                VERSIONED_UPDATE_PROCEDURE_CONTEXT_BACKWARD_CHUNKS.min(chunk_index.max(0));
            let forward_span = limit
                .saturating_sub(1)
                .saturating_sub(backward_span.max(0) as usize)
                .min(i32::MAX as usize) as i32;
            Some((
                chunk_index.saturating_sub(backward_span).max(0),
                chunk_index.saturating_add(forward_span),
            ))
        })
        .collect()
}

fn merge_versioned_update_procedure_context_rows(
    rows: Vec<KnowledgeChunkRow>,
    expanded_rows: Vec<KnowledgeChunkRow>,
    limit: usize,
) -> Vec<KnowledgeChunkRow> {
    if limit == 0 {
        return Vec::new();
    }
    let seed_indices = rows.iter().map(|row| row.chunk_index).collect::<Vec<_>>();
    let mut selected = Vec::new();
    let mut seen_chunk_ids = BTreeSet::new();
    for row in rows {
        if selected.len() >= limit {
            break;
        }
        if seen_chunk_ids.insert(row.chunk_id) {
            selected.push(row);
        }
    }
    if selected.len() < limit {
        let mut neighbors = expanded_rows
            .into_iter()
            .filter(|row| !seen_chunk_ids.contains(&row.chunk_id))
            .collect::<Vec<_>>();
        neighbors.sort_by(|left, right| {
            nearest_seed_chunk_distance(left.chunk_index, &seed_indices)
                .cmp(&nearest_seed_chunk_distance(right.chunk_index, &seed_indices))
                .then_with(|| left.chunk_index.cmp(&right.chunk_index))
                .then_with(|| left.chunk_id.cmp(&right.chunk_id))
        });
        for row in neighbors {
            if selected.len() >= limit {
                break;
            }
            if seen_chunk_ids.insert(row.chunk_id) {
                selected.push(row);
            }
        }
    }
    selected.sort_by(|left, right| {
        left.chunk_index.cmp(&right.chunk_index).then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    selected
}

fn nearest_seed_chunk_distance(chunk_index: i32, seed_indices: &[i32]) -> i32 {
    seed_indices
        .iter()
        .map(|seed_index| chunk_index.saturating_sub(*seed_index).abs())
        .min()
        .unwrap_or(i32::MAX)
}

fn question_requests_versioned_update_procedure_evidence(
    question: &str,
    query_ir: Option<&QueryIR>,
) -> bool {
    let Some(query_ir) = query_ir else {
        return false;
    };
    if !matches!(query_ir.act, QueryAct::ConfigureHow | QueryAct::Describe)
        || query_ir.source_slice.is_some()
    {
        return false;
    }
    let term_model = versioned_update_procedure_term_model(question, Some(query_ir));
    if query_ir_requests_versioned_update_procedure_context_for_model(
        query_ir,
        &term_model,
        question,
    ) {
        return term_model.query_terms.len() >= 2;
    }
    if query_ir_allows_conceptual_procedure_runbook_evidence(query_ir, &term_model) {
        return true;
    }
    if query_ir_allows_recovered_raw_question_procedure_runbook_evidence(query_ir, &term_model) {
        return true;
    }
    query_ir_allows_procedure_runbook_evidence(query_ir, &term_model)
        || query_ir_allows_raw_question_procedure_runbook_evidence(query_ir, &term_model)
}

pub(super) fn query_ir_requests_versioned_update_procedure_context(
    question: &str,
    query_ir: &QueryIR,
) -> bool {
    let question = question.trim();
    let context_question = if question.is_empty() {
        versioned_update_procedure_scoring_question(query_ir)
    } else {
        question.to_string()
    };
    if context_question.trim().is_empty() && !query_ir_is_unambiguous_versioned_procedure(query_ir)
    {
        return false;
    }
    let term_model = versioned_update_procedure_term_model(&context_question, Some(query_ir));
    query_ir_requests_versioned_update_procedure_context_for_model(
        query_ir,
        &term_model,
        &context_question,
    )
}

fn query_ir_requests_update_procedure_runbook_context(question: &str, query_ir: &QueryIR) -> bool {
    let question = question.trim();
    let context_question = if question.is_empty() {
        versioned_update_procedure_scoring_question(query_ir)
    } else {
        question.to_string()
    };
    if context_question.trim().is_empty() && !query_ir_is_unambiguous_versioned_procedure(query_ir)
    {
        return false;
    }
    let term_model = versioned_update_procedure_term_model(&context_question, Some(query_ir));
    query_ir_requests_versioned_update_procedure_context_for_model(
        query_ir,
        &term_model,
        &context_question,
    ) || query_ir_allows_conceptual_procedure_runbook_evidence(query_ir, &term_model)
        || query_ir_allows_procedure_runbook_evidence(query_ir, &term_model)
        || query_ir_allows_raw_question_procedure_runbook_evidence(query_ir, &term_model)
        || query_ir_allows_recovered_raw_question_procedure_runbook_evidence(query_ir, &term_model)
}

fn query_ir_requests_versioned_update_procedure_context_for_model(
    query_ir: &QueryIR,
    term_model: &VersionedUpdateProcedureTermModel,
    question: &str,
) -> bool {
    if query_ir_is_unambiguous_versioned_procedure(query_ir) {
        return true;
    }
    if !query_ir_has_explicit_versioned_procedure_signal(query_ir, question) {
        return false;
    }
    query_ir.source_slice.is_none()
        && matches!(query_ir.act, QueryAct::ConfigureHow | QueryAct::Describe)
        && query_ir_has_lifecycle_procedure_target(query_ir)
        && (!term_model.procedure_terms.is_empty()
            || query_ir_allows_procedure_runbook_target(query_ir))
}

fn query_ir_has_explicit_versioned_procedure_signal(query_ir: &QueryIR, question: &str) -> bool {
    if query_ir.target_types.iter().any(|target_type| {
        matches!(canonical_target_type_tag(target_type).as_str(), "version" | "release")
    }) {
        return true;
    }
    if query_ir.source_slice.as_ref().is_some_and(|slice| {
        matches!(slice.filter, crate::domains::query_ir::SourceSliceFilter::ReleaseMarker)
    }) {
        return true;
    }
    if query_ir_has_lifecycle_procedure_target(query_ir)
        && query_ir_has_setup_focus_identity(query_ir)
    {
        return true;
    }
    extract_semver_like_version(question).is_some()
        || extract_release_context_version(question).is_some()
        || query_ir.retrieval_query.as_deref().is_some_and(|query| {
            extract_semver_like_version(query).is_some()
                || extract_release_context_version(query).is_some()
        })
}

fn query_ir_has_lifecycle_procedure_target(query_ir: &QueryIR) -> bool {
    if query_ir_has_setup_configuration_target(query_ir) {
        return false;
    }
    let mut has_procedure = false;
    let mut has_concept = false;
    let mut has_context_anchor = query_ir.document_focus.is_some();
    let mut has_version_or_release = false;
    for target_type in &query_ir.target_types {
        match canonical_target_type_tag(target_type).as_str() {
            "procedure" => has_procedure = true,
            "concept" => has_concept = true,
            "artifact" | "document" | "primary_heading" | "secondary_heading" => {
                has_context_anchor = true;
            }
            "version" | "release" => {
                has_context_anchor = true;
                has_version_or_release = true;
            }
            _ => {}
        }
    }
    has_procedure
        && (!has_concept || has_version_or_release)
        && (query_ir.document_focus.is_some() || !query_ir.target_entities.is_empty())
        && has_context_anchor
}

fn query_ir_allows_procedure_runbook_evidence(
    query_ir: &QueryIR,
    term_model: &VersionedUpdateProcedureTermModel,
) -> bool {
    if !query_ir_allows_procedure_runbook_target(query_ir)
        || !query_ir_has_setup_focus_identity(query_ir)
        || term_model.query_terms.len() < 2
        || term_model.procedure_terms.is_empty()
        || (term_model.subject_terms.is_empty() && term_model.subject_acronym_terms.is_empty())
    {
        return false;
    }
    true
}

fn query_ir_allows_conceptual_procedure_runbook_evidence(
    query_ir: &QueryIR,
    term_model: &VersionedUpdateProcedureTermModel,
) -> bool {
    if !matches!(query_ir.scope, QueryScope::SingleDocument)
        || !matches!(query_ir.act, QueryAct::ConfigureHow | QueryAct::Describe)
        || query_ir.source_slice.is_some()
        || query_ir.needs_clarification.is_some()
        || query_ir_has_setup_configuration_target(query_ir)
        || term_model.query_terms.len() < 2
        || term_model.procedure_terms.is_empty()
    {
        return false;
    }
    let has_procedure_target = query_ir
        .target_types
        .iter()
        .any(|target_type| canonical_target_type_tag(target_type).as_str() == "procedure");
    let has_runbook_document_target = query_ir.target_types.iter().any(|target_type| {
        matches!(
            canonical_target_type_tag(target_type).as_str(),
            "document" | "primary_heading" | "secondary_heading"
        )
    });
    let has_subject_identity =
        query_ir.target_entities.iter().any(|entity| matches!(entity.role, EntityRole::Subject));
    has_procedure_target
        && !has_runbook_document_target
        && has_subject_identity
        && !term_model.target_identity_sequences.is_empty()
}

fn query_ir_allows_raw_question_procedure_runbook_evidence(
    query_ir: &QueryIR,
    term_model: &VersionedUpdateProcedureTermModel,
) -> bool {
    if !matches!(query_ir.scope, QueryScope::SingleDocument)
        || !query_ir_allows_procedure_runbook_target(query_ir)
        || query_ir_has_setup_focus_identity(query_ir)
        || query_ir.retrieval_query.as_deref().map(str::trim).is_none_or(str::is_empty)
        || term_model.query_terms.len() < 2
        || term_model.procedure_terms.is_empty()
    {
        return false;
    }
    true
}

fn query_ir_allows_recovered_raw_question_procedure_runbook_evidence(
    query_ir: &QueryIR,
    term_model: &VersionedUpdateProcedureTermModel,
) -> bool {
    if !matches!(query_ir.scope, QueryScope::SingleDocument)
        || !matches!(query_ir.act, QueryAct::ConfigureHow | QueryAct::Describe)
        || query_ir.source_slice.is_some()
        || query_ir.needs_clarification.is_some()
        || query_ir_has_setup_configuration_target(query_ir)
        || !query_ir.target_types.is_empty()
        || !query_ir.target_entities.is_empty()
        || query_ir.document_focus.is_some()
        || !query_ir.literal_constraints.is_empty()
        || term_model.query_terms.len() < 2
        || term_model.procedure_terms.is_empty()
        || (term_model.subject_terms.is_empty() && term_model.subject_acronym_terms.is_empty())
        || term_model.target_identity_sequences.is_empty()
    {
        return false;
    }
    true
}

#[cfg(test)]
fn versioned_update_procedure_candidate_document_ids(
    question: &str,
    query_ir: Option<&QueryIR>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    limit: usize,
) -> Vec<Uuid> {
    versioned_update_procedure_candidate_documents(question, query_ir, document_index, limit)
        .into_iter()
        .map(|candidate| candidate.document_id)
        .collect()
}

#[derive(Debug, Clone)]
struct VersionedUpdateProcedureDocumentCandidate {
    document_id: Uuid,
    exact_title_identity: bool,
    target_title_anchor: bool,
    allow_head_fallback: bool,
    requires_action_text_match: bool,
    requires_subject_text_match: bool,
    subject_identity_score: usize,
    focus_aligned_command_score: usize,
    priority: usize,
    seed_chunk_indices: Vec<i32>,
    source_local_anchor_indices: Vec<i32>,
}

fn versioned_update_procedure_seeded_runbook_candidates(
    candidates: &[VersionedUpdateProcedureDocumentCandidate],
) -> Vec<VersionedUpdateProcedureDocumentCandidate> {
    candidates
        .iter()
        .filter(|candidate| {
            !candidate.seed_chunk_indices.is_empty()
                || !candidate.source_local_anchor_indices.is_empty()
        })
        .take(VERSIONED_UPDATE_PROCEDURE_DOCUMENT_CANDIDATE_CAP)
        .cloned()
        .collect()
}

fn versioned_update_procedure_candidate_is_source_local_anchor_only(
    candidate: &VersionedUpdateProcedureDocumentCandidate,
) -> bool {
    candidate.seed_chunk_indices.is_empty()
        && !candidate.source_local_anchor_indices.is_empty()
        && !candidate.exact_title_identity
        && !candidate.allow_head_fallback
}

fn versioned_update_procedure_candidate_documents(
    question: &str,
    query_ir: Option<&QueryIR>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    limit: usize,
) -> Vec<VersionedUpdateProcedureDocumentCandidate> {
    let term_model = versioned_update_procedure_term_model(question, query_ir);
    let mut candidates = document_index
        .values()
        .filter(|document| document.document_role == crate::domains::content::DOCUMENT_ROLE_PRIMARY)
        .filter_map(|document| {
            let title = document.title.as_deref().unwrap_or("").trim();
            let title_identity_sequence = normalized_alnum_token_sequence(title, 1);
            let title_terms = normalized_alnum_tokens(title, 2);
            let subject_overlap =
                strict_token_overlap_count(&term_model.subject_terms, &title_terms);
            let subject_acronym_overlap =
                strict_token_overlap_count(&term_model.subject_acronym_terms, &title_terms);
            let procedure_overlap =
                soft_token_overlap_count(&term_model.procedure_terms, &title_terms);
            let query_overlap = soft_token_overlap_count(&term_model.query_terms, &title_terms);
            let title_has_target_identity =
                versioned_update_procedure_label_has_target_identity_sequence(
                    &title_identity_sequence,
                    &term_model.target_identity_sequences,
                );
            let title_has_target_anchor = title_has_target_identity;
            let has_subject_overlap = subject_overlap > 0 || subject_acronym_overlap > 0;
            let strong_subject_title = subject_overlap >= 3;
            let exact_title_identity = title_has_target_identity
                && (procedure_overlap > 0
                    || (strong_subject_title && query_overlap > subject_overlap)
                    || versioned_update_procedure_target_identity_is_title_dominant(
                        &title_identity_sequence,
                        &term_model.target_identity_sequences,
                    ));
            if setup_focus_document_is_standalone_image(document) && !exact_title_identity {
                return None;
            }
            if !has_subject_overlap && procedure_overlap == 0 {
                return None;
            }
            let has_explicit_subject_requirement = !term_model.subject_terms.is_empty()
                || !term_model.subject_acronym_terms.is_empty()
                || !term_model.target_identity_sequences.is_empty();
            let requires_action_text_match = !exact_title_identity
                && (!has_subject_overlap || procedure_overlap == 0)
                && !term_model.procedure_terms.is_empty();
            let allow_head_fallback =
                exact_title_identity || (has_subject_overlap && !requires_action_text_match);
            let alignment_bonus = if exact_title_identity {
                VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_IDENTITY_PRIORITY_BONUS.saturating_add(
                    usize::from(title_has_target_anchor)
                        .saturating_mul(VERSIONED_UPDATE_PROCEDURE_TITLE_ALIGNMENT_PRIORITY_BONUS),
                )
            } else if has_subject_overlap && procedure_overlap > 0 {
                VERSIONED_UPDATE_PROCEDURE_TITLE_ALIGNMENT_PRIORITY_BONUS
            } else {
                0
            };
            let score = if has_subject_overlap {
                alignment_bonus
                    .saturating_add(procedure_overlap.saturating_mul(512))
                    .saturating_add(subject_overlap.saturating_mul(96))
                    .saturating_add(subject_acronym_overlap.saturating_mul(64))
                    .saturating_add(query_overlap.saturating_mul(16))
                    .saturating_add(title_terms.len().min(24))
            } else {
                procedure_overlap
                    .saturating_mul(8)
                    .saturating_add(query_overlap.saturating_mul(4))
                    .saturating_add(title_terms.len().min(8))
            };
            Some((
                score,
                title.to_string(),
                VersionedUpdateProcedureDocumentCandidate {
                    document_id: document.document_id,
                    exact_title_identity,
                    target_title_anchor: title_has_target_anchor,
                    allow_head_fallback,
                    requires_action_text_match,
                    requires_subject_text_match: !exact_title_identity
                        && !has_subject_overlap
                        && has_explicit_subject_requirement,
                    subject_identity_score: subject_overlap.saturating_add(subject_acronym_overlap),
                    focus_aligned_command_score: 0,
                    priority: score,
                    seed_chunk_indices: Vec::new(),
                    source_local_anchor_indices: Vec::new(),
                },
            ))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.document_id.cmp(&right.2.document_id))
    });
    select_versioned_update_procedure_title_candidates(candidates, limit)
}

fn versioned_update_procedure_instruction_title_candidates(
    question: &str,
    query_ir: Option<&QueryIR>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    limit: usize,
) -> Vec<VersionedUpdateProcedureDocumentCandidate> {
    if limit == 0 {
        return Vec::new();
    }
    let term_model = versioned_update_procedure_term_model(question, query_ir);
    if term_model.target_identity_sequences.is_empty() {
        return Vec::new();
    }
    let mut candidates = document_index
        .values()
        .filter(|document| document.document_role == crate::domains::content::DOCUMENT_ROLE_PRIMARY)
        .filter_map(|document| {
            let title = document.title.as_deref().unwrap_or("").trim();
            let title_identity_sequence = normalized_alnum_token_sequence(title, 1);
            if !versioned_update_procedure_label_has_target_identity_sequence(
                &title_identity_sequence,
                &term_model.target_identity_sequences,
            ) {
                return None;
            }
            let title_terms = normalized_alnum_tokens(title, 2);
            let subject_overlap =
                strict_token_overlap_count(&term_model.subject_terms, &title_terms);
            let subject_acronym_overlap =
                strict_token_overlap_count(&term_model.subject_acronym_terms, &title_terms);
            let procedure_overlap =
                soft_token_overlap_count(&term_model.procedure_terms, &title_terms);
            let query_overlap = soft_token_overlap_count(&term_model.query_terms, &title_terms);
            let priority = VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_IDENTITY_PRIORITY_BONUS
                .saturating_add(VERSIONED_UPDATE_PROCEDURE_TITLE_ALIGNMENT_PRIORITY_BONUS)
                .saturating_add(subject_overlap.saturating_mul(512))
                .saturating_add(subject_acronym_overlap.saturating_mul(256))
                .saturating_add(procedure_overlap.saturating_mul(128))
                .saturating_add(query_overlap.saturating_mul(32))
                .saturating_add(title_terms.len().min(32));
            Some((
                priority,
                title.to_string(),
                VersionedUpdateProcedureDocumentCandidate {
                    document_id: document.document_id,
                    exact_title_identity: true,
                    target_title_anchor: true,
                    allow_head_fallback: true,
                    requires_action_text_match: false,
                    requires_subject_text_match: false,
                    subject_identity_score: subject_overlap.saturating_add(subject_acronym_overlap),
                    focus_aligned_command_score: 0,
                    priority,
                    seed_chunk_indices: Vec::new(),
                    source_local_anchor_indices: Vec::new(),
                },
            ))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.document_id.cmp(&right.2.document_id))
    });
    candidates.into_iter().take(limit).map(|(_, _, candidate)| candidate).collect()
}

fn versioned_update_procedure_exact_target_runbook_scan_candidates(
    question: &str,
    query_ir: Option<&QueryIR>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    limit: usize,
) -> Vec<VersionedUpdateProcedureDocumentCandidate> {
    if limit == 0 {
        return Vec::new();
    }
    let term_model = versioned_update_procedure_term_model(question, query_ir);
    if term_model.target_identity_sequences.is_empty() {
        return Vec::new();
    }
    let mut candidates = document_index
        .values()
        .filter(|document| document.document_role == crate::domains::content::DOCUMENT_ROLE_PRIMARY)
        .filter_map(|document| {
            let title = document.title.as_deref().unwrap_or("").trim();
            let title_identity_sequence = normalized_alnum_token_sequence(title, 1);
            if !versioned_update_procedure_label_has_target_identity_sequence(
                &title_identity_sequence,
                &term_model.target_identity_sequences,
            ) {
                return None;
            }
            let title_terms = normalized_alnum_tokens(title, 2);
            let subject_overlap =
                strict_token_overlap_count(&term_model.subject_terms, &title_terms);
            let subject_acronym_overlap =
                strict_token_overlap_count(&term_model.subject_acronym_terms, &title_terms);
            let procedure_overlap =
                soft_token_overlap_count(&term_model.procedure_terms, &title_terms);
            let query_overlap = soft_token_overlap_count(&term_model.query_terms, &title_terms);
            let title_version_transition_score =
                versioned_update_procedure_ordered_version_transition_score(title);
            let priority = VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_IDENTITY_PRIORITY_BONUS
                .saturating_add(VERSIONED_UPDATE_PROCEDURE_TITLE_ALIGNMENT_PRIORITY_BONUS)
                .saturating_add(title_version_transition_score.saturating_mul(8_192))
                .saturating_add(procedure_overlap.saturating_mul(4_096))
                .saturating_add(query_overlap.saturating_mul(256))
                .saturating_add(subject_overlap.saturating_mul(128))
                .saturating_add(subject_acronym_overlap.saturating_mul(96))
                .saturating_add(title_terms.len().min(64));
            Some((
                priority,
                title.to_string(),
                VersionedUpdateProcedureDocumentCandidate {
                    document_id: document.document_id,
                    exact_title_identity: true,
                    target_title_anchor: true,
                    allow_head_fallback: true,
                    requires_action_text_match: false,
                    requires_subject_text_match: false,
                    subject_identity_score: subject_overlap.saturating_add(subject_acronym_overlap),
                    focus_aligned_command_score: 0,
                    priority,
                    seed_chunk_indices: Vec::new(),
                    source_local_anchor_indices: Vec::new(),
                },
            ))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.document_id.cmp(&right.2.document_id))
    });
    candidates.into_iter().take(limit).map(|(_, _, candidate)| candidate).collect()
}

fn versioned_update_procedure_exact_action_title_candidates(
    question: &str,
    query_ir: Option<&QueryIR>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    limit: usize,
) -> Vec<VersionedUpdateProcedureDocumentCandidate> {
    let Some(query_ir) = query_ir else {
        return Vec::new();
    };
    if limit == 0 || !query_ir_requests_versioned_update_procedure_context(question, query_ir) {
        return Vec::new();
    }
    let term_model = versioned_update_procedure_term_model(question, Some(query_ir));
    if term_model.target_identity_sequences.is_empty() || term_model.procedure_terms.is_empty() {
        return Vec::new();
    }
    let mut candidates = document_index
        .values()
        .filter(|document| document.document_role == crate::domains::content::DOCUMENT_ROLE_PRIMARY)
        .filter_map(|document| {
            let title = document.title.as_deref().unwrap_or("").trim();
            let title_identity_sequence = normalized_alnum_token_sequence(title, 1);
            if !versioned_update_procedure_label_has_target_identity_sequence(
                &title_identity_sequence,
                &term_model.target_identity_sequences,
            ) {
                return None;
            }
            let title_terms = normalized_alnum_tokens(title, 2);
            let procedure_overlap =
                soft_token_overlap_count(&term_model.procedure_terms, &title_terms);
            let title_version_transition_score =
                versioned_update_procedure_ordered_version_transition_score(title);
            if procedure_overlap == 0 && title_version_transition_score == 0 {
                return None;
            }
            let subject_overlap =
                strict_token_overlap_count(&term_model.subject_terms, &title_terms);
            let subject_acronym_overlap =
                strict_token_overlap_count(&term_model.subject_acronym_terms, &title_terms);
            let query_overlap = soft_token_overlap_count(&term_model.query_terms, &title_terms);
            let priority = VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_IDENTITY_PRIORITY_BONUS
                .saturating_mul(2)
                .saturating_add(
                    VERSIONED_UPDATE_PROCEDURE_TITLE_ALIGNMENT_PRIORITY_BONUS.saturating_mul(4),
                )
                .saturating_add(procedure_overlap.saturating_mul(8_192))
                .saturating_add(title_version_transition_score.saturating_mul(4_096))
                .saturating_add(subject_overlap.saturating_mul(512))
                .saturating_add(subject_acronym_overlap.saturating_mul(256))
                .saturating_add(query_overlap.saturating_mul(64))
                .saturating_add(title_terms.len().min(64));
            Some((
                priority,
                title.to_string(),
                VersionedUpdateProcedureDocumentCandidate {
                    document_id: document.document_id,
                    exact_title_identity: true,
                    target_title_anchor: true,
                    allow_head_fallback: true,
                    requires_action_text_match: false,
                    requires_subject_text_match: false,
                    subject_identity_score: subject_overlap.saturating_add(subject_acronym_overlap),
                    focus_aligned_command_score: 0,
                    priority,
                    seed_chunk_indices: Vec::new(),
                    source_local_anchor_indices: Vec::new(),
                },
            ))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.document_id.cmp(&right.2.document_id))
    });
    candidates.into_iter().take(limit).map(|(_, _, candidate)| candidate).collect()
}

fn versioned_update_procedure_target_identity_token_sequences(
    question: &str,
    query_ir: Option<&QueryIR>,
) -> Vec<Vec<String>> {
    let mut seen = BTreeSet::<Vec<String>>::new();
    let mut sequences = Vec::new();
    if let Some(query_ir) = query_ir {
        for label in query_ir
            .target_entities
            .iter()
            .map(|entity| entity.label.as_str())
            .chain(query_ir.document_focus.iter().map(|focus| focus.hint.as_str()))
        {
            push_versioned_update_procedure_target_identity_sequence(
                &mut sequences,
                &mut seen,
                normalized_alnum_token_sequence(label, 1),
            );
        }
        for literal in &query_ir.literal_constraints {
            if matches!(literal.kind, LiteralKind::Identifier | LiteralKind::Other) {
                push_versioned_update_procedure_target_identity_sequence(
                    &mut sequences,
                    &mut seen,
                    normalized_alnum_token_sequence(&literal.text, 1),
                );
            }
        }
        if sequences.is_empty() {
            if let Some(retrieval_query) = query_ir.retrieval_query.as_deref() {
                for sequence in
                    versioned_update_procedure_raw_target_identity_token_sequences(retrieval_query)
                {
                    push_versioned_update_procedure_target_identity_sequence(
                        &mut sequences,
                        &mut seen,
                        sequence,
                    );
                }
            }
        }
    }
    if sequences.is_empty() {
        for sequence in versioned_update_procedure_raw_target_identity_token_sequences(question) {
            push_versioned_update_procedure_target_identity_sequence(
                &mut sequences,
                &mut seen,
                sequence,
            );
        }
    }
    sequences
}

fn push_versioned_update_procedure_target_identity_sequence(
    sequences: &mut Vec<Vec<String>>,
    seen: &mut BTreeSet<Vec<String>>,
    sequence: Vec<String>,
) {
    if !versioned_update_procedure_target_identity_sequence_is_usable(&sequence)
        || !seen.insert(sequence.clone())
    {
        return;
    }
    sequences.push(sequence);
}

fn versioned_update_procedure_raw_target_identity_token_sequences(
    question: &str,
) -> Vec<Vec<String>> {
    let current = strip_leading_question_marker(current_question_segment(question));
    let tokens = normalized_alnum_token_sequence(current, 1);
    if tokens.is_empty() {
        return Vec::new();
    }
    let mut seen = BTreeSet::<Vec<String>>::new();
    let mut sequences = Vec::<Vec<String>>::new();
    let max_window = tokens.len().min(6);
    for window_len in (2..=max_window).rev() {
        for window in tokens.windows(window_len) {
            let sequence = window.to_vec();
            if versioned_update_procedure_target_identity_sequence_is_usable(&sequence)
                && seen.insert(sequence.clone())
            {
                sequences.push(sequence);
            }
        }
    }
    sequences
}

fn versioned_update_procedure_target_identity_sequence_is_usable(sequence: &[String]) -> bool {
    sequence.len() >= 2
        && sequence.iter().map(|token| token.chars().count()).sum::<usize>() >= 7
        && sequence.iter().any(|token| token.chars().count() >= 3)
}

fn versioned_update_procedure_fuzzy_token_sequence_contains_tokens(
    haystack_tokens: &[String],
    needle_tokens: &[String],
) -> bool {
    if needle_tokens.is_empty() || haystack_tokens.len() < needle_tokens.len() {
        return false;
    }
    haystack_tokens.windows(needle_tokens.len()).any(|window| {
        window.iter().zip(needle_tokens).all(|(haystack_token, needle_token)| {
            versioned_update_procedure_identity_tokens_match(haystack_token, needle_token)
        })
    })
}

fn versioned_update_procedure_identity_tokens_match(left: &str, right: &str) -> bool {
    left == right
        || (left.chars().count() >= VERSIONED_UPDATE_PROCEDURE_TARGET_IDENTITY_PREFIX_MIN_CHARS
            && right.chars().count() >= VERSIONED_UPDATE_PROCEDURE_TARGET_IDENTITY_PREFIX_MIN_CHARS
            && common_prefix_char_count(left, right)
                >= VERSIONED_UPDATE_PROCEDURE_TARGET_IDENTITY_PREFIX_MIN_CHARS)
}

fn versioned_update_procedure_label_has_target_identity_sequence(
    label_sequence: &[String],
    target_identity_sequences: &[Vec<String>],
) -> bool {
    !target_identity_sequences.is_empty()
        && target_identity_sequences.iter().any(|target_sequence| {
            token_sequence_contains_tokens(label_sequence, target_sequence)
                || versioned_update_procedure_fuzzy_token_sequence_contains_tokens(
                    label_sequence,
                    target_sequence,
                )
        })
}

fn versioned_update_procedure_chunk_has_body_target_identity(
    chunk: &RuntimeMatchedChunk,
    term_model: &VersionedUpdateProcedureTermModel,
    evidence: VersionedUpdateProcedureChunkEvidence,
) -> bool {
    if term_model.target_identity_sequences.is_empty() {
        return false;
    }
    let body_sequence = normalized_alnum_token_sequence(&chunk.source_text, 1);
    if term_model.target_identity_sequences.iter().any(|target_sequence| {
        token_sequence_contains_tokens(&body_sequence, target_sequence)
            || versioned_update_procedure_fuzzy_token_sequence_contains_tokens(
                &body_sequence,
                target_sequence,
            )
    }) {
        return true;
    }
    evidence.subject_overlap >= VERSIONED_UPDATE_PROCEDURE_STRONG_SUBJECT_IDENTITY_MIN
}

fn versioned_update_procedure_text_has_bound_target_identity_runbook(
    text: &str,
    term_model: &VersionedUpdateProcedureTermModel,
) -> bool {
    if term_model.target_identity_sequences.is_empty() {
        return false;
    }
    for window in versioned_update_procedure_bound_target_identity_windows(text) {
        if !versioned_update_procedure_text_has_target_identity_sequence(&window, term_model) {
            continue;
        }
        let tokens = normalized_alnum_tokens(&window, 2).into_iter().collect::<BTreeSet<_>>();
        if soft_token_overlap_count(&term_model.procedure_terms, &tokens) == 0 {
            continue;
        }
        if !versioned_update_procedure_window_has_bound_target_action_line(&window, term_model) {
            continue;
        }
        if versioned_update_procedure_text_command_or_script_score(
            &window,
            &term_model.procedure_terms,
        ) > 0
            || procedure_artifact_token_count(&window) > 0
        {
            return true;
        }
    }
    false
}

fn versioned_update_procedure_window_has_bound_target_action_line(
    window: &str,
    term_model: &VersionedUpdateProcedureTermModel,
) -> bool {
    let lines = window.lines().collect::<Vec<_>>();
    if lines.iter().any(|line| {
        if !versioned_update_procedure_text_has_target_identity_sequence(line, term_model) {
            return false;
        }
        let tokens = normalized_alnum_tokens(line, 2).into_iter().collect::<BTreeSet<_>>();
        soft_token_overlap_count(&term_model.procedure_terms, &tokens) > 0
            || versioned_update_procedure_text_has_action_script(line, &term_model.procedure_terms)
    }) {
        return true;
    }
    lines.windows(2).any(|pair| {
        let [left, right] = pair else {
            return false;
        };
        if versioned_update_procedure_line_has_sentence_boundary(left) {
            return false;
        }
        let merged = format!("{left} {right}");
        if !versioned_update_procedure_text_has_target_identity_sequence(&merged, term_model) {
            return false;
        }
        let tokens = normalized_alnum_tokens(&merged, 2).into_iter().collect::<BTreeSet<_>>();
        soft_token_overlap_count(&term_model.procedure_terms, &tokens) > 0
            || versioned_update_procedure_text_has_action_script(
                &merged,
                &term_model.procedure_terms,
            )
    })
}

fn versioned_update_procedure_bound_target_identity_windows(text: &str) -> Vec<String> {
    let lines = text.lines().map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>();
    let mut seen = BTreeSet::<String>::new();
    let mut windows = Vec::new();

    if lines.len() > 1 {
        for start in 0..lines.len() {
            let window = lines
                .iter()
                .skip(start)
                .take(VERSIONED_UPDATE_PROCEDURE_BOUND_TARGET_LINE_WINDOW_MAX_LINES)
                .copied()
                .collect::<Vec<_>>()
                .join("\n");
            if window.chars().count()
                <= VERSIONED_UPDATE_PROCEDURE_BOUND_TARGET_LINE_WINDOW_MAX_CHARS
            {
                push_versioned_update_procedure_bound_target_identity_window(
                    &mut windows,
                    &mut seen,
                    window,
                );
            }
        }
    } else if let Some(line) = lines.first() {
        if !versioned_update_procedure_line_has_sentence_boundary(line) {
            push_versioned_update_procedure_bound_target_identity_window(
                &mut windows,
                &mut seen,
                (*line).to_string(),
            );
        }
    }

    for line in &lines {
        for clause in versioned_update_procedure_sentence_clauses(line) {
            push_versioned_update_procedure_bound_target_identity_window(
                &mut windows,
                &mut seen,
                clause,
            );
        }
    }
    windows
}

fn push_versioned_update_procedure_bound_target_identity_window(
    windows: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    window: String,
) {
    let normalized = window.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() || !seen.insert(normalized.clone()) {
        return;
    }
    windows.push(window.trim().to_string());
}

fn versioned_update_procedure_sentence_clauses(text: &str) -> Vec<String> {
    let mut clauses = Vec::new();
    let mut start = 0usize;
    for (index, ch) in text.char_indices() {
        if !versioned_update_procedure_is_sentence_boundary(text, index, ch) {
            continue;
        }
        let clause = text[start..index].trim();
        if !clause.is_empty() {
            clauses.push(clause.to_string());
        }
        start = index.saturating_add(ch.len_utf8());
    }
    let clause = text[start..].trim();
    if !clause.is_empty() {
        clauses.push(clause.to_string());
    }
    clauses
}

fn versioned_update_procedure_line_has_sentence_boundary(text: &str) -> bool {
    text.char_indices()
        .any(|(index, ch)| versioned_update_procedure_is_sentence_boundary(text, index, ch))
}

fn versioned_update_procedure_is_sentence_boundary(text: &str, index: usize, ch: char) -> bool {
    match ch {
        ';' | '!' | '?' => true,
        '.' => {
            let prev = text[..index].chars().next_back();
            let next_index = index.saturating_add(ch.len_utf8());
            let next = text[next_index..].chars().next();
            if prev.is_some_and(|prev| prev.is_ascii_digit())
                && next.is_some_and(|next| next.is_whitespace())
            {
                return false;
            }
            if next.is_some_and(|next| !next.is_whitespace()) {
                return false;
            }
            true
        }
        _ => false,
    }
}

fn versioned_update_procedure_text_has_target_identity_sequence(
    text: &str,
    term_model: &VersionedUpdateProcedureTermModel,
) -> bool {
    let text_sequence = normalized_alnum_token_sequence(text, 1);
    term_model.target_identity_sequences.iter().any(|target_sequence| {
        token_sequence_contains_tokens(&text_sequence, target_sequence)
            || versioned_update_procedure_fuzzy_token_sequence_contains_tokens(
                &text_sequence,
                target_sequence,
            )
    })
}

fn versioned_update_procedure_setup_signature_is_action_bound(
    evidence: VersionedUpdateProcedureChunkEvidence,
) -> bool {
    if !evidence.has_setup_script_signature {
        return true;
    }
    let has_subject_identity = evidence.label_subject_overlap > 0 || evidence.subject_overlap > 0;
    let has_label_action_binding =
        evidence.label_subject_overlap > 0 && evidence.label_procedure_overlap > 0;
    let has_action_binding = has_label_action_binding || evidence.has_action_script;
    let has_structural_runbook = evidence.structural_score
        >= VERSIONED_UPDATE_PROCEDURE_EVIDENCE_STRUCTURAL_SCORE_FLOOR
        && (evidence.command_or_script_score > 0
            || evidence.ordered_procedure_score > 0
            || evidence.focus_aligned_command_score > 0);
    has_subject_identity && has_action_binding && has_structural_runbook
}

fn versioned_update_procedure_target_identity_is_title_dominant(
    label_sequence: &[String],
    target_identity_sequences: &[Vec<String>],
) -> bool {
    target_identity_sequences.iter().any(|target_sequence| {
        (token_sequence_contains_tokens(label_sequence, target_sequence)
            || versioned_update_procedure_fuzzy_token_sequence_contains_tokens(
                label_sequence,
                target_sequence,
            ))
            && label_sequence.len() <= target_sequence.len().saturating_add(1)
    })
}

pub(crate) fn chunk_is_versioned_update_instruction_title_anchor(
    question: &str,
    query_ir: &QueryIR,
    chunk: &RuntimeMatchedChunk,
) -> bool {
    if !query_ir_requests_versioned_update_procedure_context(question, query_ir) {
        return false;
    }
    let term_model = versioned_update_procedure_term_model(question, Some(query_ir));
    chunk_is_versioned_update_instruction_title_anchor_for_model(chunk, &term_model)
}

pub(crate) fn versioned_update_exact_target_runbook_score(
    question: &str,
    query_ir: &QueryIR,
    chunk: &RuntimeMatchedChunk,
) -> Option<usize> {
    if !query_ir_requests_update_procedure_runbook_context(question, query_ir) {
        return None;
    }
    let term_model = versioned_update_procedure_term_model(question, Some(query_ir));
    if term_model.target_identity_sequences.is_empty() {
        return None;
    }
    let label_sequence = normalized_alnum_token_sequence(&chunk.document_label, 1);
    let label_has_target_identity = versioned_update_procedure_label_has_target_identity_sequence(
        &label_sequence,
        &term_model.target_identity_sequences,
    );
    let evidence = versioned_update_procedure_chunk_evidence(chunk, &term_model);
    let body_has_target_identity =
        versioned_update_procedure_chunk_has_body_target_identity(chunk, &term_model, evidence);
    if !label_has_target_identity && !body_has_target_identity {
        return None;
    }
    if !label_has_target_identity
        && !versioned_update_procedure_text_has_bound_target_identity_runbook(
            &chunk.source_text,
            &term_model,
        )
    {
        return None;
    }
    let has_unfocused_transition = evidence.unfocused_transition_score > 0;
    let has_command_sequence_runbook = (evidence.command_sequence_score > 0
        || evidence.version_transition_score > 0)
        && (!has_unfocused_transition || evidence.focus_aligned_command_score > 0);
    let has_ordered_procedure_runbook = evidence.ordered_procedure_score > 0
        && evidence.structural_score >= VERSIONED_UPDATE_PROCEDURE_EVIDENCE_STRUCTURAL_SCORE_FLOOR;
    let has_structural_focused_runbook = has_command_sequence_runbook
        && evidence.focus_aligned_command_score > 0
        && evidence.structural_score >= VERSIONED_UPDATE_PROCEDURE_EVIDENCE_STRUCTURAL_SCORE_FLOOR
        && evidence.command_or_script_score > 0;
    let has_structural_action_script_runbook = evidence.has_action_script
        && evidence.command_or_script_score > 0
        && evidence.procedure_overlap > 0
        && evidence.structural_score >= VERSIONED_UPDATE_PROCEDURE_EVIDENCE_STRUCTURAL_SCORE_FLOOR;
    let has_action_binding = evidence.label_procedure_overlap > 0
        || evidence.procedure_overlap > 0
        || evidence.version_transition_score > 0
        || has_ordered_procedure_runbook
        || has_structural_focused_runbook
        || has_structural_action_script_runbook;
    let has_runbook_evidence = has_ordered_procedure_runbook
        || has_structural_focused_runbook
        || has_structural_action_script_runbook;
    if (evidence.has_setup_script_signature
        && !versioned_update_procedure_setup_signature_is_action_bound(evidence))
        || !has_action_binding
        || !has_runbook_evidence
        || evidence.structural_score < VERSIONED_UPDATE_PROCEDURE_EVIDENCE_STRUCTURAL_SCORE_FLOOR
    {
        return None;
    }
    let target_identity_bonus = if label_has_target_identity { 65_536 } else { 16_384 };
    Some(
        evidence
            .score
            .saturating_add(target_identity_bonus)
            .saturating_add(evidence.ordered_procedure_score.saturating_mul(12_288))
            .saturating_add(evidence.focus_aligned_command_score.saturating_mul(16_384))
            .saturating_add(evidence.command_or_script_score.saturating_mul(4_096))
            .saturating_add(evidence.label_procedure_overlap.saturating_mul(65_536))
            .saturating_add(evidence.procedure_overlap.saturating_mul(16_384))
            .saturating_add(evidence.version_transition_score.saturating_mul(2_048))
            .saturating_add(evidence.label_subject_overlap.saturating_mul(1_024))
            .saturating_add(evidence.label_procedure_overlap.saturating_mul(1_024))
            .saturating_sub(
                evidence
                    .unfocused_transition_score
                    .saturating_sub(evidence.focus_aligned_command_score)
                    .saturating_mul(2_048),
            ),
    )
}

pub(crate) fn versioned_update_procedure_runbook_anchor_score(
    question: &str,
    query_ir: &QueryIR,
    chunk: &RuntimeMatchedChunk,
) -> Option<usize> {
    if !question_requests_versioned_update_procedure_evidence(question, Some(query_ir)) {
        return None;
    }
    let term_model = versioned_update_procedure_term_model(question, Some(query_ir));
    if term_model.target_identity_sequences.is_empty()
        || (term_model.subject_terms.is_empty() && term_model.subject_acronym_terms.is_empty())
        || term_model.procedure_terms.is_empty()
    {
        return None;
    }

    let evidence = versioned_update_procedure_chunk_evidence(chunk, &term_model);
    if evidence.has_setup_script_signature
        && !versioned_update_procedure_setup_signature_is_action_bound(evidence)
    {
        return None;
    }

    let label_sequence = normalized_alnum_token_sequence(&chunk.document_label, 1);
    let label_has_target_identity = versioned_update_procedure_label_has_target_identity_sequence(
        &label_sequence,
        &term_model.target_identity_sequences,
    );
    let body_has_target_identity =
        versioned_update_procedure_chunk_has_body_target_identity(chunk, &term_model, evidence);
    let has_subject_identity = evidence.subject_overlap > 0
        || evidence.label_subject_overlap > 0
        || body_has_target_identity;
    if !has_subject_identity {
        return None;
    }

    let has_action_evidence = evidence.procedure_overlap > 0
        || evidence.label_procedure_overlap > 0
        || evidence.version_transition_score > 0
        || evidence.subject_aligned_version_transition_score > 0;
    let has_command_runbook = evidence.ordered_procedure_score > 0
        || evidence.command_sequence_score > 0
        || evidence.focus_aligned_command_score > 0
        || (evidence.has_action_script && evidence.command_or_script_score > 0);
    if !has_action_evidence
        || !has_command_runbook
        || evidence.structural_score < VERSIONED_UPDATE_PROCEDURE_EVIDENCE_STRUCTURAL_SCORE_FLOOR
    {
        return None;
    }

    let exact_score =
        versioned_update_exact_target_runbook_score(question, query_ir, chunk).unwrap_or_default();
    let target_identity_bonus = if label_has_target_identity { 65_536usize } else { 16_384usize };
    Some(
        exact_score
            .saturating_add(evidence.score)
            .saturating_add(target_identity_bonus)
            .saturating_add(evidence.ordered_procedure_score.saturating_mul(12_288))
            .saturating_add(evidence.command_sequence_score.saturating_mul(16_384))
            .saturating_add(evidence.focus_aligned_command_score.saturating_mul(16_384))
            .saturating_add(evidence.command_or_script_score.saturating_mul(4_096))
            .saturating_add(evidence.label_subject_overlap.saturating_mul(2_048))
            .saturating_add(evidence.label_procedure_overlap.saturating_mul(4_096))
            .saturating_add(evidence.subject_overlap.saturating_mul(512))
            .saturating_add(evidence.procedure_overlap.saturating_mul(2_048))
            .saturating_add(evidence.version_transition_score.saturating_mul(2_048))
            .saturating_sub(
                evidence
                    .unfocused_transition_score
                    .saturating_sub(evidence.focus_aligned_command_score)
                    .saturating_mul(2_048),
            ),
    )
}

fn chunk_is_versioned_update_instruction_title_anchor_for_model(
    chunk: &RuntimeMatchedChunk,
    term_model: &VersionedUpdateProcedureTermModel,
) -> bool {
    let evidence = versioned_update_procedure_chunk_evidence(chunk, term_model);
    if evidence.has_setup_script_signature
        && !versioned_update_procedure_setup_signature_is_action_bound(evidence)
    {
        return false;
    }
    let label_sequence = normalized_alnum_token_sequence(&chunk.document_label, 1);
    if !versioned_update_procedure_label_has_target_identity_sequence(
        &label_sequence,
        &term_model.target_identity_sequences,
    ) {
        return false;
    }
    let has_body_action = evidence.procedure_overlap > 0 || evidence.version_transition_score > 0;
    let has_procedure_runbook = evidence.ordered_procedure_score > 0
        || evidence.focus_aligned_command_score > 0
        || (evidence.command_or_script_score > 0
            && evidence.structural_score
                >= VERSIONED_UPDATE_PROCEDURE_EVIDENCE_STRUCTURAL_SCORE_FLOOR);
    has_body_action && has_procedure_runbook
}

fn select_versioned_update_procedure_title_candidates(
    candidates: Vec<(usize, String, VersionedUpdateProcedureDocumentCandidate)>,
    limit: usize,
) -> Vec<VersionedUpdateProcedureDocumentCandidate> {
    if limit == 0 {
        return Vec::new();
    }
    let action_title_reserve =
        VERSIONED_UPDATE_PROCEDURE_ACTION_TITLE_RESERVE_CAP.min(limit.saturating_sub(1)).min(
            candidates
                .iter()
                .filter(|(_, _, candidate)| candidate.requires_subject_text_match)
                .count(),
        );
    let mut selected = candidates
        .iter()
        .filter(|(_, _, candidate)| candidate.exact_title_identity && candidate.target_title_anchor)
        .take(VERSIONED_UPDATE_PROCEDURE_SUBJECT_TITLE_RESERVE_CAP.min(limit))
        .map(|(_, _, candidate)| candidate.clone())
        .collect::<Vec<_>>();
    for candidate in candidates
        .iter()
        .filter(|(_, _, candidate)| candidate.exact_title_identity)
        .take(VERSIONED_UPDATE_PROCEDURE_SUBJECT_TITLE_RESERVE_CAP.min(limit))
        .map(|(_, _, candidate)| candidate)
    {
        if !selected.iter().any(|existing| existing.document_id == candidate.document_id) {
            selected.push(candidate.clone());
        }
        if selected.len() >= VERSIONED_UPDATE_PROCEDURE_SUBJECT_TITLE_RESERVE_CAP.min(limit) {
            break;
        }
    }
    let primary_limit = limit.saturating_sub(action_title_reserve).max(selected.len()).min(limit);
    for (_, _, candidate) in
        candidates.iter().filter(|(_, _, candidate)| !candidate.requires_subject_text_match)
    {
        if selected.len() >= primary_limit {
            break;
        }
        let low_confidence_subject_only = candidate.requires_action_text_match
            && !candidate.requires_subject_text_match
            && !candidate.exact_title_identity
            && !candidate.allow_head_fallback;
        if low_confidence_subject_only
            && selected.len() >= VERSIONED_UPDATE_PROCEDURE_SUBJECT_TITLE_RESERVE_CAP.min(limit)
        {
            continue;
        }
        if !selected.iter().any(|existing| existing.document_id == candidate.document_id) {
            selected.push(candidate.clone());
        }
    }
    for (_, _, candidate) in candidates
        .iter()
        .filter(|(_, _, candidate)| candidate.requires_subject_text_match)
        .take(action_title_reserve)
    {
        if selected.len() >= limit {
            break;
        }
        if !selected.iter().any(|existing| existing.document_id == candidate.document_id) {
            selected.push(candidate.clone());
        }
    }
    for (_, _, candidate) in candidates {
        if selected.len() >= limit {
            break;
        }
        let low_confidence_subject_only = candidate.requires_action_text_match
            && !candidate.requires_subject_text_match
            && !candidate.exact_title_identity
            && !candidate.allow_head_fallback;
        if low_confidence_subject_only
            && selected.len() >= VERSIONED_UPDATE_PROCEDURE_SUBJECT_TITLE_RESERVE_CAP.min(limit)
        {
            continue;
        }
        if !selected.iter().any(|existing| existing.document_id == candidate.document_id) {
            selected.push(candidate);
        }
    }
    selected
}

fn merge_versioned_update_procedure_document_candidates(
    title_candidates: Vec<VersionedUpdateProcedureDocumentCandidate>,
    evidence_candidates: Vec<VersionedUpdateProcedureDocumentCandidate>,
    limit: usize,
) -> Vec<VersionedUpdateProcedureDocumentCandidate> {
    if limit == 0 {
        return Vec::new();
    }
    let mut by_document = HashMap::<Uuid, VersionedUpdateProcedureDocumentCandidate>::new();
    for candidate in title_candidates.into_iter().chain(evidence_candidates) {
        by_document
            .entry(candidate.document_id)
            .and_modify(|existing| {
                let exact_title_identity =
                    existing.exact_title_identity || candidate.exact_title_identity;
                let target_title_anchor =
                    existing.target_title_anchor || candidate.target_title_anchor;
                let allow_head_fallback =
                    existing.allow_head_fallback || candidate.allow_head_fallback;
                let requires_action_text_match =
                    existing.requires_action_text_match && candidate.requires_action_text_match;
                let requires_subject_text_match =
                    existing.requires_subject_text_match && candidate.requires_subject_text_match;
                let subject_identity_score =
                    existing.subject_identity_score.max(candidate.subject_identity_score);
                let focus_aligned_command_score =
                    existing.focus_aligned_command_score.max(candidate.focus_aligned_command_score);
                let mut seed_chunk_indices = existing.seed_chunk_indices.clone();
                seed_chunk_indices.extend(candidate.seed_chunk_indices.iter().copied());
                seed_chunk_indices.sort_unstable();
                seed_chunk_indices.dedup();
                let mut source_local_anchor_indices = existing.source_local_anchor_indices.clone();
                source_local_anchor_indices
                    .extend(candidate.source_local_anchor_indices.iter().copied());
                source_local_anchor_indices.sort_unstable();
                source_local_anchor_indices.dedup();
                if candidate.priority > existing.priority {
                    *existing = candidate.clone();
                }
                existing.exact_title_identity = exact_title_identity;
                existing.target_title_anchor = target_title_anchor;
                existing.allow_head_fallback = allow_head_fallback;
                existing.requires_action_text_match = requires_action_text_match;
                existing.requires_subject_text_match = requires_subject_text_match;
                existing.subject_identity_score = subject_identity_score;
                existing.focus_aligned_command_score = focus_aligned_command_score;
                existing.seed_chunk_indices = seed_chunk_indices;
                existing.source_local_anchor_indices = source_local_anchor_indices;
            })
            .or_insert(candidate);
    }
    let mut candidates = by_document.into_values().collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        versioned_update_procedure_candidate_sort_priority(right)
            .cmp(&versioned_update_procedure_candidate_sort_priority(left))
            .then_with(|| right.priority.cmp(&left.priority))
            .then_with(|| left.document_id.cmp(&right.document_id))
    });
    select_versioned_update_procedure_candidates_with_action_title_reserve(candidates, limit)
}

fn versioned_update_procedure_candidate_sort_priority(
    candidate: &VersionedUpdateProcedureDocumentCandidate,
) -> usize {
    let mut priority = candidate.priority;
    if versioned_update_procedure_candidate_has_seeded_exact_evidence(candidate) {
        priority = priority.saturating_add(VERSIONED_UPDATE_PROCEDURE_SEEDED_EXACT_PRIORITY_BONUS);
    }
    if candidate.exact_title_identity && candidate.target_title_anchor {
        priority =
            priority.saturating_add(VERSIONED_UPDATE_PROCEDURE_TITLE_ALIGNMENT_PRIORITY_BONUS);
    }
    priority = priority.saturating_add(candidate.focus_aligned_command_score.saturating_mul(8192));
    priority
}

fn versioned_update_procedure_candidate_has_seeded_exact_evidence(
    candidate: &VersionedUpdateProcedureDocumentCandidate,
) -> bool {
    candidate.exact_title_identity && !candidate.seed_chunk_indices.is_empty()
}

fn select_versioned_update_procedure_candidates_with_action_title_reserve(
    candidates: Vec<VersionedUpdateProcedureDocumentCandidate>,
    limit: usize,
) -> Vec<VersionedUpdateProcedureDocumentCandidate> {
    if limit == 0 {
        return Vec::new();
    }
    let mut selected = Vec::with_capacity(limit);
    for candidate in candidates
        .iter()
        .filter(|candidate| candidate.exact_title_identity && candidate.target_title_anchor)
        .take(VERSIONED_UPDATE_PROCEDURE_SUBJECT_TITLE_RESERVE_CAP.min(limit))
    {
        if !selected.iter().any(|existing: &VersionedUpdateProcedureDocumentCandidate| {
            existing.document_id == candidate.document_id
        }) {
            selected.push(candidate.clone());
        }
    }
    for candidate in candidates
        .iter()
        .filter(|candidate| candidate.exact_title_identity)
        .take(VERSIONED_UPDATE_PROCEDURE_SUBJECT_TITLE_RESERVE_CAP.min(limit))
    {
        if selected.len() >= VERSIONED_UPDATE_PROCEDURE_SUBJECT_TITLE_RESERVE_CAP.min(limit) {
            break;
        }
        if !selected.iter().any(|existing: &VersionedUpdateProcedureDocumentCandidate| {
            existing.document_id == candidate.document_id
        }) {
            selected.push(candidate.clone());
        }
    }

    let subject_title_reserve = VERSIONED_UPDATE_PROCEDURE_SUBJECT_TITLE_RESERVE_CAP
        .min(limit.saturating_sub(selected.len()))
        .min(
            candidates
                .iter()
                .filter(|candidate| {
                    !candidate.exact_title_identity
                        && versioned_update_procedure_candidate_has_strong_subject_title(candidate)
                })
                .count(),
        );
    for candidate in candidates
        .iter()
        .filter(|candidate| {
            !candidate.exact_title_identity
                && versioned_update_procedure_candidate_has_strong_subject_title(candidate)
        })
        .take(subject_title_reserve)
    {
        if !selected.iter().any(|existing: &VersionedUpdateProcedureDocumentCandidate| {
            existing.document_id == candidate.document_id
        }) {
            selected.push(candidate.clone());
        }
    }

    let action_title_reserve = VERSIONED_UPDATE_PROCEDURE_ACTION_TITLE_RESERVE_CAP
        .min(limit.saturating_sub(selected.len()).saturating_sub(1))
        .min(candidates.iter().filter(|candidate| candidate.requires_subject_text_match).count());
    let primary_limit = limit.saturating_sub(action_title_reserve);
    for candidate in candidates.iter().filter(|candidate| !candidate.requires_subject_text_match) {
        if selected.len() >= primary_limit {
            break;
        }
        if !selected.iter().any(|existing| existing.document_id == candidate.document_id) {
            selected.push(candidate.clone());
        }
    }
    for candidate in candidates
        .iter()
        .filter(|candidate| candidate.requires_subject_text_match)
        .take(action_title_reserve)
    {
        if selected.len() >= limit {
            break;
        }
        if !selected.iter().any(|existing| existing.document_id == candidate.document_id) {
            selected.push(candidate.clone());
        }
    }
    for candidate in candidates {
        if selected.len() >= limit {
            break;
        }
        if !selected.iter().any(|existing| existing.document_id == candidate.document_id) {
            selected.push(candidate);
        }
    }
    selected
}

fn ensure_reserved_versioned_update_procedure_title_candidates(
    candidates: &mut Vec<VersionedUpdateProcedureDocumentCandidate>,
    reserved_candidates: Vec<VersionedUpdateProcedureDocumentCandidate>,
    limit: usize,
) {
    if limit == 0 {
        candidates.clear();
        return;
    }
    if reserved_candidates.is_empty() {
        candidates.truncate(limit);
        return;
    }
    let mut selected = Vec::with_capacity(limit);
    let mut seen_document_ids = BTreeSet::new();
    for reserved_candidate in reserved_candidates {
        if selected.len() >= limit {
            break;
        }
        if !seen_document_ids.insert(reserved_candidate.document_id) {
            continue;
        }
        if let Some(position) = candidates
            .iter()
            .position(|candidate| candidate.document_id == reserved_candidate.document_id)
        {
            selected.push(candidates.remove(position));
        } else {
            selected.push(reserved_candidate);
        }
    }
    for candidate in candidates.drain(..) {
        if selected.len() >= limit {
            break;
        }
        if seen_document_ids.insert(candidate.document_id) {
            selected.push(candidate);
        }
    }
    *candidates = selected;
}

fn versioned_update_procedure_candidate_has_strong_subject_title(
    candidate: &VersionedUpdateProcedureDocumentCandidate,
) -> bool {
    candidate.exact_title_identity
        || (!candidate.requires_action_text_match
            && !candidate.requires_subject_text_match
            && candidate.subject_identity_score
                >= VERSIONED_UPDATE_PROCEDURE_STRONG_SUBJECT_IDENTITY_MIN)
}

fn versioned_update_procedure_candidate_prefers_head_window(
    candidate: &VersionedUpdateProcedureDocumentCandidate,
) -> bool {
    candidate.exact_title_identity && candidate.target_title_anchor
}

async fn load_versioned_update_procedure_evidence_candidates(
    state: &AppState,
    library_id: Uuid,
    question: &str,
    term_model: &VersionedUpdateProcedureTermModel,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    temporal_start: Option<DateTime<Utc>>,
    temporal_end: Option<DateTime<Utc>>,
) -> anyhow::Result<Vec<VersionedUpdateProcedureDocumentCandidate>> {
    let search_queries = versioned_update_procedure_evidence_search_queries(question, term_model);
    if search_queries.is_empty() {
        return Ok(Vec::new());
    }
    let per_query_futures = search_queries.iter().cloned().map(|search_query| async move {
        state
            .search_store
            .search_chunks(
                library_id,
                &search_query,
                VERSIONED_UPDATE_PROCEDURE_EVIDENCE_SEARCH_HIT_LIMIT,
                temporal_start,
                temporal_end,
            )
            .await
            .map(|rows| {
                rows.into_iter().map(|row| (row.chunk_id, row.score as f32)).collect::<Vec<_>>()
            })
            .with_context(|| {
                format!("failed to search versioned procedure evidence chunks: {search_query}")
            })
    });
    let per_query_results: Vec<Result<Vec<_>, anyhow::Error>> = join_all(per_query_futures).await;
    let hits =
        combine_versioned_update_procedure_search_results(per_query_results, search_queries.len())?;
    if hits.is_empty() {
        return Ok(Vec::new());
    }
    let chunks = batch_hydrate_hits(state, hits, document_index, plan_keywords, &BTreeSet::new())
        .await
        .context("failed to hydrate versioned procedure evidence chunks")?;
    let chunks = expand_versioned_update_procedure_evidence_chunks(
        state,
        chunks,
        document_index,
        plan_keywords,
    )
    .await
    .context("failed to expand versioned procedure evidence chunks")?;
    let candidates = versioned_update_procedure_candidates_from_evidence_chunks(
        &chunks,
        term_model,
        VERSIONED_UPDATE_PROCEDURE_EVIDENCE_DOCUMENT_CANDIDATE_CAP,
    );
    if !candidates.is_empty() {
        tracing::info!(
            stage = "retrieval.versioned_update_procedure_evidence",
            search_query_count = search_queries.len(),
            evidence_candidate_count = candidates.len(),
            "versioned procedure documents discovered from command-bearing content evidence"
        );
    }
    Ok(candidates)
}

async fn expand_versioned_update_procedure_evidence_chunks(
    state: &AppState,
    seed_chunks: Vec<RuntimeMatchedChunk>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if seed_chunks.is_empty() {
        return Ok(Vec::new());
    }

    #[derive(Debug)]
    struct EvidenceSeedGroup {
        document_id: Uuid,
        revision_id: Uuid,
        best_score: f32,
        seeds: Vec<(i32, f32)>,
    }

    let mut groups = HashMap::<(Uuid, Uuid), EvidenceSeedGroup>::new();
    for chunk in &seed_chunks {
        if !document_index.contains_key(&chunk.document_id) {
            continue;
        }
        let score = chunk.score.unwrap_or(0.0);
        let key = (chunk.document_id, chunk.revision_id);
        groups
            .entry(key)
            .and_modify(|group| {
                group.best_score = group.best_score.max(score);
                group.seeds.push((chunk.chunk_index, score));
            })
            .or_insert_with(|| EvidenceSeedGroup {
                document_id: chunk.document_id,
                revision_id: chunk.revision_id,
                best_score: score,
                seeds: vec![(chunk.chunk_index, score)],
            });
    }
    if groups.is_empty() {
        return Ok(seed_chunks);
    }

    let mut groups = groups.into_values().collect::<Vec<_>>();
    groups.sort_by(|left, right| {
        right
            .best_score
            .partial_cmp(&left.best_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.document_id.cmp(&right.document_id))
            .then_with(|| left.revision_id.cmp(&right.revision_id))
    });
    groups.truncate(VERSIONED_UPDATE_PROCEDURE_EVIDENCE_EXPANSION_DOCUMENT_CAP);

    let fetched = stream::iter(groups.into_iter().map(|mut group| async move {
        group.seeds.sort_by(|left, right| {
            right
                .1
                .partial_cmp(&left.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.0.cmp(&right.0))
        });
        group.seeds.truncate(VERSIONED_UPDATE_PROCEDURE_EVIDENCE_SEEDS_PER_DOCUMENT);
        let windows = group
            .seeds
            .iter()
            .map(|(chunk_index, _)| {
                let start = chunk_index
                    .saturating_sub(VERSIONED_UPDATE_PROCEDURE_EVIDENCE_NEIGHBOR_BACKWARD_CHUNKS)
                    .max(0);
                let end = chunk_index
                    .saturating_add(VERSIONED_UPDATE_PROCEDURE_EVIDENCE_NEIGHBOR_FORWARD_CHUNKS);
                (start, end)
            })
            .collect::<Vec<_>>();
        let rows = state
            .document_store
            .list_chunks_by_revision_windows(group.revision_id, &windows)
            .await?;
        Ok::<_, anyhow::Error>((group.best_score, rows))
    }))
    .buffer_unordered(VERSIONED_UPDATE_PROCEDURE_DOCUMENT_FETCH_CONCURRENCY)
    .collect::<Vec<_>>()
    .await
    .into_iter()
    .collect::<anyhow::Result<Vec<_>>>()?;

    let mut chunks = seed_chunks;
    for (score, rows) in fetched {
        for row in rows {
            if let Some(mut chunk) = map_chunk_hit(row, score, document_index, plan_keywords) {
                chunk.score = Some(score);
                chunks.push(chunk);
            }
        }
    }
    chunks.sort_by(|left, right| {
        left.document_id
            .cmp(&right.document_id)
            .then_with(|| left.revision_id.cmp(&right.revision_id))
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    let mut seen = BTreeSet::new();
    chunks.retain(|chunk| seen.insert(chunk.chunk_id));
    Ok(chunks)
}

fn combine_versioned_update_procedure_search_results(
    per_query_results: Vec<anyhow::Result<Vec<(Uuid, f32)>>>,
    search_query_count: usize,
) -> anyhow::Result<Vec<(Uuid, f32)>> {
    let mut hits = Vec::new();
    let mut seen = HashSet::new();
    let mut failed_query_count = 0usize;
    let mut failures = Vec::new();
    let cap = VERSIONED_UPDATE_PROCEDURE_EVIDENCE_SEARCH_HIT_LIMIT
        .saturating_mul(VERSIONED_UPDATE_PROCEDURE_EVIDENCE_SEARCH_QUERY_CAP);

    for result in per_query_results {
        match result {
            Ok(query_hits) => {
                for (chunk_id, score) in query_hits {
                    if hits.len() >= cap {
                        break;
                    }
                    if seen.insert(chunk_id) {
                        hits.push((chunk_id, score));
                    }
                }
            }
            Err(error) => {
                failed_query_count = failed_query_count.saturating_add(1);
                let summary = format!("{error:#}");
                tracing::warn!(
                    stage = "retrieval.versioned_update_procedure_evidence_failed",
                    error = %summary,
                    retrieval_degraded = true,
                    failed_source = "versioned_update_procedure_evidence",
                    failed_query_count,
                    search_query_count,
                    "versioned procedure evidence search failed; continuing with other searches"
                );
                failures.push(summary);
            }
        }
        if hits.len() >= cap {
            break;
        }
    }
    if search_query_count > 0 && failed_query_count == search_query_count {
        anyhow::bail!("all versioned procedure evidence searches failed: {}", failures.join("; "));
    }
    Ok(hits)
}

fn versioned_update_procedure_evidence_search_queries(
    question: &str,
    term_model: &VersionedUpdateProcedureTermModel,
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut queries = Vec::new();
    let mut push_query = |value: String, queries: &mut Vec<String>| {
        if queries.len() >= VERSIONED_UPDATE_PROCEDURE_EVIDENCE_SEARCH_QUERY_CAP {
            return;
        }
        let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized.is_empty() || !seen.insert(normalized.to_lowercase()) {
            return;
        }
        queries.push(normalized);
    };

    push_query(strip_leading_question_marker(question).to_string(), &mut queries);

    let mut subject_terms = term_model.subject_terms.iter().cloned().collect::<BTreeSet<_>>();
    subject_terms.extend(term_model.subject_acronym_terms.iter().cloned());
    let combined_terms = term_model
        .procedure_terms
        .iter()
        .chain(subject_terms.iter())
        .cloned()
        .collect::<BTreeSet<_>>();
    if !combined_terms.is_empty() {
        push_query(combined_terms.into_iter().collect::<Vec<_>>().join(" "), &mut queries);
    }
    if !term_model.procedure_terms.is_empty() {
        push_query(
            term_model.procedure_terms.iter().cloned().collect::<Vec<_>>().join(" "),
            &mut queries,
        );
    }
    if !subject_terms.is_empty() {
        push_query(subject_terms.iter().cloned().collect::<Vec<_>>().join(" "), &mut queries);
    }
    if !term_model.procedure_terms.is_empty() && !subject_terms.is_empty() {
        let focused = term_model
            .procedure_terms
            .iter()
            .chain(subject_terms.iter())
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        push_query(focused, &mut queries);
    }
    queries
}

#[derive(Default)]
struct VersionedUpdateProcedureEvidenceAccumulator {
    document_id: Uuid,
    exact_title_identity: bool,
    subject_overlap: usize,
    procedure_overlap: usize,
    label_subject_overlap: usize,
    label_procedure_overlap: usize,
    structural_score: usize,
    version_transition_score: usize,
    subject_aligned_version_transition_score: usize,
    ordered_procedure_score: usize,
    focus_aligned_command_score: usize,
    source_local_anchor_score: usize,
    priority: usize,
    seed_chunk_indices: BTreeSet<i32>,
    source_local_anchor_indices: BTreeSet<i32>,
}

#[derive(Debug, Clone, Copy)]
struct VersionedUpdateProcedureChunkEvidence {
    subject_overlap: usize,
    procedure_overlap: usize,
    label_subject_overlap: usize,
    label_procedure_overlap: usize,
    structural_score: usize,
    version_transition_score: usize,
    subject_aligned_version_transition_score: usize,
    ordered_procedure_score: usize,
    command_or_script_score: usize,
    focus_aligned_command_score: usize,
    command_sequence_score: usize,
    unfocused_transition_score: usize,
    has_action_script: bool,
    bound_target_runbook_score: usize,
    score: usize,
    has_setup_script_signature: bool,
}

fn versioned_update_procedure_candidates_from_evidence_chunks(
    chunks: &[RuntimeMatchedChunk],
    term_model: &VersionedUpdateProcedureTermModel,
    limit: usize,
) -> Vec<VersionedUpdateProcedureDocumentCandidate> {
    if limit == 0 {
        return Vec::new();
    }
    let mut by_document = HashMap::<Uuid, VersionedUpdateProcedureEvidenceAccumulator>::new();
    for chunk in chunks {
        let evidence = versioned_update_procedure_chunk_evidence(chunk, term_model);
        let exact_title_identity = versioned_update_procedure_chunk_has_exact_target_title_identity(
            chunk, term_model, evidence,
        );
        let entry = by_document.entry(chunk.document_id).or_insert_with(|| {
            VersionedUpdateProcedureEvidenceAccumulator {
                document_id: chunk.document_id,
                exact_title_identity: false,
                subject_overlap: 0,
                procedure_overlap: 0,
                label_subject_overlap: 0,
                label_procedure_overlap: 0,
                structural_score: 0,
                version_transition_score: 0,
                subject_aligned_version_transition_score: 0,
                ordered_procedure_score: 0,
                focus_aligned_command_score: 0,
                source_local_anchor_score: 0,
                priority: VERSIONED_UPDATE_PROCEDURE_EVIDENCE_PRIORITY_BONUS,
                seed_chunk_indices: BTreeSet::new(),
                source_local_anchor_indices: BTreeSet::new(),
            }
        });
        entry.exact_title_identity |= exact_title_identity;
        entry.subject_overlap = entry.subject_overlap.max(evidence.subject_overlap);
        entry.procedure_overlap = entry.procedure_overlap.max(evidence.procedure_overlap);
        entry.label_subject_overlap =
            entry.label_subject_overlap.max(evidence.label_subject_overlap);
        entry.label_procedure_overlap =
            entry.label_procedure_overlap.max(evidence.label_procedure_overlap);
        entry.structural_score = entry.structural_score.max(evidence.structural_score);
        entry.version_transition_score =
            entry.version_transition_score.max(evidence.version_transition_score);
        entry.subject_aligned_version_transition_score = entry
            .subject_aligned_version_transition_score
            .max(evidence.subject_aligned_version_transition_score);
        entry.ordered_procedure_score =
            entry.ordered_procedure_score.max(evidence.ordered_procedure_score);
        entry.focus_aligned_command_score =
            entry.focus_aligned_command_score.max(evidence.focus_aligned_command_score);
        let is_seed_evidence = versioned_update_procedure_chunk_is_seed_evidence(evidence);
        let source_local_anchor_score =
            versioned_update_procedure_source_local_anchor_score(chunk, term_model, evidence);
        entry.source_local_anchor_score =
            entry.source_local_anchor_score.max(source_local_anchor_score);
        if is_seed_evidence || source_local_anchor_score > 0 {
            if source_local_anchor_score > 0 {
                entry.priority = entry
                    .priority
                    .saturating_add(VERSIONED_UPDATE_PROCEDURE_TITLE_ALIGNMENT_PRIORITY_BONUS / 2)
                    .saturating_add(source_local_anchor_score);
            }
            if !is_seed_evidence {
                entry.source_local_anchor_indices.insert(chunk.chunk_index);
                continue;
            }
            entry.priority = entry.priority.saturating_add(evidence.score);
            if evidence.bound_target_runbook_score > 0 {
                entry.priority = entry
                    .priority
                    .saturating_add(VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_IDENTITY_PRIORITY_BONUS)
                    .saturating_add(VERSIONED_UPDATE_PROCEDURE_TITLE_ALIGNMENT_PRIORITY_BONUS)
                    .saturating_add(evidence.bound_target_runbook_score.saturating_mul(65_536));
            }
            if evidence.ordered_procedure_score > 0 {
                entry.priority = entry
                    .priority
                    .saturating_add(VERSIONED_UPDATE_PROCEDURE_TITLE_ALIGNMENT_PRIORITY_BONUS / 2)
                    .saturating_add(evidence.ordered_procedure_score.saturating_mul(16_384));
            }
            if evidence.command_sequence_score > 0 {
                entry.priority = entry
                    .priority
                    .saturating_add(VERSIONED_UPDATE_PROCEDURE_TITLE_ALIGNMENT_PRIORITY_BONUS / 2)
                    .saturating_add(evidence.command_sequence_score.saturating_mul(32_768));
            }
            if evidence.focus_aligned_command_score > 0 {
                entry.priority = entry
                    .priority
                    .saturating_add(VERSIONED_UPDATE_PROCEDURE_TITLE_ALIGNMENT_PRIORITY_BONUS / 2)
                    .saturating_add(evidence.focus_aligned_command_score.saturating_mul(8192));
            }
            let has_artifact_materialization =
                versioned_update_procedure_text_has_artifact_materialization(&chunk.source_text);
            let has_action_bound_artifact_materialization = has_artifact_materialization
                && evidence.focus_aligned_command_score > 0
                && evidence.command_sequence_score > 0
                && (evidence.procedure_overlap > 0
                    || evidence.label_procedure_overlap > 0
                    || evidence.has_action_script);
            if evidence.focus_aligned_command_score > 0
                && evidence.command_sequence_score > 0
                && !has_artifact_materialization
            {
                entry.priority = entry
                    .priority
                    .saturating_add(VERSIONED_UPDATE_PROCEDURE_TITLE_ALIGNMENT_PRIORITY_BONUS);
            }
            if has_action_bound_artifact_materialization && evidence.ordered_procedure_score > 0 {
                entry.priority = entry
                    .priority
                    .saturating_add(VERSIONED_UPDATE_PROCEDURE_TITLE_ALIGNMENT_PRIORITY_BONUS / 2);
            } else if has_artifact_materialization {
                entry.priority = entry
                    .priority
                    .saturating_sub(VERSIONED_UPDATE_PROCEDURE_EVIDENCE_PRIORITY_BONUS);
            }
            if evidence.label_subject_overlap > 0 {
                entry.priority = entry.priority.saturating_add(
                    VERSIONED_UPDATE_PROCEDURE_TITLE_ALIGNMENT_PRIORITY_BONUS
                        .saturating_add(evidence.label_subject_overlap.saturating_mul(512)),
                );
            }
            if evidence.label_subject_overlap > 0 && evidence.label_procedure_overlap > 0 {
                entry.priority = entry
                    .priority
                    .saturating_add(VERSIONED_UPDATE_PROCEDURE_TITLE_ALIGNMENT_PRIORITY_BONUS);
            }
            if evidence.label_subject_overlap > 0 && evidence.version_transition_score > 0 {
                entry.priority = entry
                    .priority
                    .saturating_add(VERSIONED_UPDATE_PROCEDURE_TITLE_ALIGNMENT_PRIORITY_BONUS);
            }
            if exact_title_identity {
                entry.priority = entry
                    .priority
                    .saturating_add(VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_IDENTITY_PRIORITY_BONUS);
            }
            if evidence.has_setup_script_signature
                && !versioned_update_procedure_setup_signature_is_action_bound(evidence)
            {
                entry.priority = entry
                    .priority
                    .saturating_sub(VERSIONED_UPDATE_PROCEDURE_TITLE_ALIGNMENT_PRIORITY_BONUS);
            }
            entry.seed_chunk_indices.insert(chunk.chunk_index);
        }
    }
    let mut candidates = by_document
        .into_values()
        .filter(|candidate| {
            let has_subject_identity =
                candidate.subject_overlap > 0 || candidate.label_subject_overlap > 0;
            let has_action = candidate.procedure_overlap > 0;
            let has_label_aligned_version_transition =
                candidate.version_transition_score > 0 && candidate.label_subject_overlap > 0;
            let has_body_aligned_version_transition =
                candidate.subject_aligned_version_transition_score > 0;
            let has_ordered_procedure_runbook = candidate.ordered_procedure_score > 0;
            let has_focus_aligned_command_runbook = candidate.focus_aligned_command_score > 0
                && (candidate.procedure_overlap > 0
                    || candidate.label_procedure_overlap > 0
                    || candidate.version_transition_score > 0);
            let has_source_local_anchor = candidate.source_local_anchor_score > 0;
            has_subject_identity
                && (has_action
                    || has_label_aligned_version_transition
                    || has_body_aligned_version_transition
                    || has_ordered_procedure_runbook
                    || has_focus_aligned_command_runbook
                    || has_source_local_anchor)
                && (candidate.structural_score
                    >= VERSIONED_UPDATE_PROCEDURE_EVIDENCE_STRUCTURAL_SCORE_FLOOR
                    || has_source_local_anchor)
                && (!candidate.seed_chunk_indices.is_empty()
                    || !candidate.source_local_anchor_indices.is_empty())
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right.priority.cmp(&left.priority).then_with(|| left.document_id.cmp(&right.document_id))
    });
    candidates
        .into_iter()
        .take(limit)
        .map(|candidate| VersionedUpdateProcedureDocumentCandidate {
            document_id: candidate.document_id,
            exact_title_identity: candidate.exact_title_identity,
            target_title_anchor: false,
            allow_head_fallback: candidate.exact_title_identity,
            requires_action_text_match: true,
            requires_subject_text_match: false,
            subject_identity_score: candidate.subject_overlap.max(candidate.label_subject_overlap),
            focus_aligned_command_score: candidate.focus_aligned_command_score,
            priority: candidate.priority,
            seed_chunk_indices: candidate.seed_chunk_indices.into_iter().collect(),
            source_local_anchor_indices: candidate
                .source_local_anchor_indices
                .into_iter()
                .collect(),
        })
        .collect()
}

fn versioned_update_procedure_chunk_has_exact_target_title_identity(
    chunk: &RuntimeMatchedChunk,
    term_model: &VersionedUpdateProcedureTermModel,
    evidence: VersionedUpdateProcedureChunkEvidence,
) -> bool {
    if term_model.target_identity_sequences.is_empty() {
        return false;
    }
    let label_sequence = normalized_alnum_token_sequence(&chunk.document_label, 1);
    versioned_update_procedure_label_has_target_identity_sequence(
        &label_sequence,
        &term_model.target_identity_sequences,
    ) && (evidence.label_procedure_overlap > 0
        || evidence.procedure_overlap > 0
        || evidence.version_transition_score > 0
        || evidence.command_or_script_score > 0)
}

fn versioned_update_procedure_source_local_anchor_score(
    chunk: &RuntimeMatchedChunk,
    term_model: &VersionedUpdateProcedureTermModel,
    evidence: VersionedUpdateProcedureChunkEvidence,
) -> usize {
    if term_model.target_identity_sequences.is_empty() {
        return 0;
    }
    if !versioned_update_procedure_setup_signature_is_action_bound(evidence) {
        return 0;
    }
    if evidence.command_or_script_score > 0
        || evidence.command_sequence_score > 0
        || evidence.focus_aligned_command_score > 0
        || evidence.ordered_procedure_score > 0
    {
        return 0;
    }

    let has_target_identity = evidence.label_subject_overlap > 0
        || versioned_update_procedure_chunk_has_body_target_identity(chunk, term_model, evidence);
    if !has_target_identity {
        return 0;
    }

    let has_procedure_context = evidence.label_procedure_overlap > 0
        || evidence.subject_aligned_version_transition_score > 0;
    if !has_procedure_context {
        return 0;
    }

    VERSIONED_UPDATE_PROCEDURE_EVIDENCE_PRIORITY_BONUS
        .saturating_add(evidence.label_subject_overlap.saturating_mul(2048))
        .saturating_add(evidence.subject_overlap.saturating_mul(1024))
        .saturating_add(evidence.label_procedure_overlap.saturating_mul(2048))
        .saturating_add(evidence.procedure_overlap.saturating_mul(1024))
        .saturating_add(evidence.version_transition_score.saturating_mul(512))
        .saturating_add(evidence.subject_aligned_version_transition_score.saturating_mul(1024))
        .saturating_add(evidence.structural_score.saturating_mul(256))
}

fn versioned_update_procedure_chunk_is_seed_evidence(
    evidence: VersionedUpdateProcedureChunkEvidence,
) -> bool {
    let has_subject_identity = evidence.subject_overlap > 0 || evidence.label_subject_overlap > 0;
    let has_action = evidence.procedure_overlap > 0;
    let has_label_aligned_version_transition =
        evidence.version_transition_score > 0 && evidence.label_subject_overlap > 0;
    let has_body_aligned_version_transition = evidence.subject_aligned_version_transition_score > 0;
    let has_action_bound_command_evidence = has_action
        && (evidence.ordered_procedure_score > 0
            || evidence.command_sequence_score > 0
            || evidence.focus_aligned_command_score > 0
            || (evidence.has_action_script && evidence.command_or_script_score > 0));
    let has_label_aligned_structural_body = evidence.label_subject_overlap > 0
        && (has_action_bound_command_evidence
            || evidence.command_sequence_score > 0
            || evidence.focus_aligned_command_score > 0
            || evidence.ordered_procedure_score > 0)
        && evidence.structural_score >= VERSIONED_UPDATE_PROCEDURE_EVIDENCE_STRUCTURAL_SCORE_FLOOR;
    let has_ordered_procedure_runbook = evidence.ordered_procedure_score > 0
        && (has_action
            || evidence.label_procedure_overlap > 0
            || evidence.version_transition_score > 0);
    let has_focus_aligned_command_runbook = evidence.focus_aligned_command_score > 0
        && (has_action
            || evidence.label_procedure_overlap > 0
            || evidence.version_transition_score > 0);
    has_subject_identity
        && evidence.structural_score >= VERSIONED_UPDATE_PROCEDURE_EVIDENCE_STRUCTURAL_SCORE_FLOOR
        && (has_action_bound_command_evidence
            || has_label_aligned_version_transition
            || has_body_aligned_version_transition
            || has_label_aligned_structural_body
            || has_ordered_procedure_runbook
            || has_focus_aligned_command_runbook)
        && versioned_update_procedure_setup_signature_is_action_bound(evidence)
}

fn versioned_update_procedure_chunk_evidence(
    chunk: &RuntimeMatchedChunk,
    term_model: &VersionedUpdateProcedureTermModel,
) -> VersionedUpdateProcedureChunkEvidence {
    let combined_text = format!("{} {}", chunk.document_label, chunk.source_text);
    let tokens = normalized_alnum_tokens(&combined_text, 2).into_iter().collect::<BTreeSet<_>>();
    let label_tokens =
        normalized_alnum_tokens(&chunk.document_label, 2).into_iter().collect::<BTreeSet<_>>();
    let subject_overlap = soft_token_overlap_count(&term_model.subject_terms, &tokens)
        .saturating_add(soft_token_overlap_count(&term_model.subject_acronym_terms, &tokens));
    let has_action_script = versioned_update_procedure_text_has_action_script(
        &chunk.source_text,
        &term_model.procedure_terms,
    );
    let procedure_overlap = soft_token_overlap_count(&term_model.procedure_terms, &tokens)
        .saturating_add(usize::from(has_action_script));
    let label_subject_overlap = soft_token_overlap_count(&term_model.subject_terms, &label_tokens)
        .saturating_add(soft_token_overlap_count(&term_model.subject_acronym_terms, &label_tokens));
    let label_procedure_overlap =
        soft_token_overlap_count(&term_model.procedure_terms, &label_tokens);
    let version_transition_score =
        versioned_update_procedure_ordered_version_transition_score(&chunk.source_text);
    let subject_aligned_version_transition_score =
        versioned_update_procedure_subject_aligned_version_transition_score(
            &chunk.source_text,
            term_model,
        );
    let ordered_procedure_score =
        versioned_update_procedure_text_ordered_procedure_score(&chunk.source_text, term_model);
    let command_or_script_score = versioned_update_procedure_text_command_or_script_score(
        &chunk.source_text,
        &term_model.procedure_terms,
    );
    let focus_aligned_command_score =
        versioned_update_procedure_text_focus_aligned_command_score(&chunk.source_text, term_model);
    let command_sequence_score =
        versioned_update_procedure_text_command_sequence_score(&chunk.source_text, term_model);
    let unfocused_transition_score =
        versioned_update_procedure_text_unfocused_transition_score(&chunk.source_text, term_model);
    let structural_score = versioned_update_procedure_text_structural_score(&chunk.source_text)
        .max(focus_aligned_command_score);
    let has_setup_script_signature = versioned_update_procedure_text_has_setup_script_signature(
        &chunk.source_text,
        &term_model.procedure_terms,
    );
    let bound_target_runbook_score =
        usize::from(versioned_update_procedure_text_has_bound_target_identity_runbook(
            &chunk.source_text,
            term_model,
        ));
    let score = procedure_overlap
        .saturating_mul(512)
        .saturating_add(version_transition_score.saturating_mul(384))
        .saturating_add(subject_aligned_version_transition_score.saturating_mul(768))
        .saturating_add(ordered_procedure_score.saturating_mul(1024))
        .saturating_add(command_or_script_score.saturating_mul(256))
        .saturating_add(command_sequence_score.saturating_mul(1536))
        .saturating_add(focus_aligned_command_score.saturating_mul(768))
        .saturating_add(subject_overlap.saturating_mul(256))
        .saturating_add(structural_score.saturating_mul(64));
    VersionedUpdateProcedureChunkEvidence {
        subject_overlap,
        procedure_overlap,
        label_subject_overlap,
        label_procedure_overlap,
        structural_score,
        version_transition_score,
        subject_aligned_version_transition_score,
        ordered_procedure_score,
        command_or_script_score,
        focus_aligned_command_score,
        command_sequence_score,
        unfocused_transition_score,
        has_action_script,
        bound_target_runbook_score,
        score,
        has_setup_script_signature,
    }
}

fn versioned_update_procedure_text_structural_score(text: &str) -> usize {
    ordered_step_marker_count(text).saturating_add(procedure_artifact_token_count(text).min(8))
}

fn versioned_update_procedure_text_ordered_procedure_score(
    text: &str,
    term_model: &VersionedUpdateProcedureTermModel,
) -> usize {
    let context_tokens = normalized_alnum_tokens(text, 2).into_iter().collect::<BTreeSet<_>>();
    let context_subject_score =
        versioned_update_procedure_subject_focus_score(&context_tokens, term_model);
    if soft_token_overlap_count(&term_model.procedure_terms, &context_tokens) == 0
        || context_subject_score
            < versioned_update_procedure_required_subject_focus_score(term_model)
    {
        return 0;
    }
    let ordered_steps = text
        .lines()
        .filter(|line| {
            let stripped = strip_leading_procedure_order_marker(line);
            stripped != line.trim_start() || line.trim_start().starts_with(['-', '*', '•'])
        })
        .filter(|line| {
            if ordered_step_looks_like_sequential_record(line) {
                return false;
            }
            let tokens = normalized_alnum_tokens(line, 2).into_iter().collect::<BTreeSet<_>>();
            soft_token_overlap_count(&term_model.procedure_terms, &tokens) > 0
                || versioned_update_procedure_subject_focus_score(&tokens, term_model) > 0
                || update_like_structural_step_signal(line)
        })
        .count();
    if ordered_steps < 2 {
        return 0;
    }
    ordered_steps.min(VERSIONED_UPDATE_PROCEDURE_COMMAND_SIGNAL_SCORE_CAP)
}

fn ordered_step_looks_like_sequential_record(line: &str) -> bool {
    let body = strip_leading_procedure_order_marker(line).trim_start_matches(['-', '*', '•', ' ']);
    let mut tokens = body.split_whitespace();
    let Some(first) = tokens.next() else {
        return false;
    };
    if extract_semver_like_version(first).is_some() {
        return true;
    }
    let first = first.trim_matches(|ch: char| ch.is_ascii_punctuation()).to_ascii_lowercase();
    first == "version"
        && tokens.next().is_some_and(|token| {
            extract_semver_like_version(token.trim_matches(|ch: char| ch.is_ascii_punctuation()))
                .is_some()
        })
}

fn update_like_structural_step_signal(line: &str) -> bool {
    update_like_step_has_version_or_literal(line) || procedure_artifact_token_count(line) > 0
}

fn update_like_step_has_version_or_literal(line: &str) -> bool {
    line.split_whitespace().any(|token| extract_semver_like_version(token).is_some())
}

fn versioned_update_procedure_text_command_or_script_score(
    text: &str,
    procedure_terms: &BTreeSet<String>,
) -> usize {
    let command_line_count = text
        .lines()
        .filter(|line| versioned_update_procedure_line_has_command_start(line))
        .count()
        .min(VERSIONED_UPDATE_PROCEDURE_COMMAND_SIGNAL_SCORE_CAP);
    let inline_command_count = versioned_update_procedure_inline_command_signal_count(text);
    command_line_count
        .max(inline_command_count)
        .saturating_add(usize::from(versioned_update_procedure_text_has_action_script(
            text,
            procedure_terms,
        )))
        .min(VERSIONED_UPDATE_PROCEDURE_COMMAND_SIGNAL_SCORE_CAP)
}

fn versioned_update_procedure_text_focus_aligned_command_score(
    text: &str,
    term_model: &VersionedUpdateProcedureTermModel,
) -> usize {
    let context_tokens = normalized_alnum_tokens(text, 2).into_iter().collect::<BTreeSet<_>>();
    if soft_token_overlap_count(&term_model.procedure_terms, &context_tokens) == 0 {
        return 0;
    }
    let context_subject_score =
        versioned_update_procedure_subject_focus_score(&context_tokens, term_model);
    versioned_update_procedure_command_segments_from_text(text)
        .iter()
        .map(|segment| {
            versioned_update_procedure_command_segment_focus_score(
                segment,
                term_model,
                context_subject_score,
            )
        })
        .filter(|score| *score > 0)
        .sum::<usize>()
        .min(VERSIONED_UPDATE_PROCEDURE_COMMAND_SIGNAL_SCORE_CAP)
}

fn versioned_update_procedure_text_command_sequence_score(
    text: &str,
    term_model: &VersionedUpdateProcedureTermModel,
) -> usize {
    let context_tokens = normalized_alnum_tokens(text, 2).into_iter().collect::<BTreeSet<_>>();
    if soft_token_overlap_count(&term_model.procedure_terms, &context_tokens) == 0 {
        return 0;
    }
    let context_subject_score =
        versioned_update_procedure_subject_focus_score(&context_tokens, term_model);
    let segments = versioned_update_procedure_command_segments_from_text(text);
    let direct_focus_count = segments
        .iter()
        .filter(|segment| {
            !versioned_update_procedure_command_segment_has_artifact_transfer(segment)
        })
        .filter(|segment| {
            versioned_update_procedure_command_segment_focus_score(
                segment,
                term_model,
                context_subject_score,
            ) > 0
        })
        .count();
    if direct_focus_count < 2 {
        return 0;
    }
    if ordered_step_marker_count(text) == 0
        && direct_focus_count < 3
        && !versioned_update_procedure_text_has_introductory_command_context(text, term_model)
    {
        return 0;
    }
    direct_focus_count.min(VERSIONED_UPDATE_PROCEDURE_COMMAND_SIGNAL_SCORE_CAP)
}

fn versioned_update_procedure_text_has_introductory_command_context(
    text: &str,
    term_model: &VersionedUpdateProcedureTermModel,
) -> bool {
    text.lines().any(|line| {
        let tokens = shellish_tokens_from_text(line);
        (0..tokens.len()).any(|index| {
            if !shellish_inline_token_starts_command(&tokens, index) || index == 0 {
                return false;
            }
            let prefix_tokens =
                normalized_alnum_tokens(&tokens[..index].join(" "), 2).into_iter().collect();
            versioned_update_procedure_subject_focus_score(&prefix_tokens, term_model) > 0
                && soft_token_overlap_count(&term_model.procedure_terms, &prefix_tokens) > 0
        })
    })
}

fn versioned_update_procedure_text_unfocused_transition_score(
    text: &str,
    term_model: &VersionedUpdateProcedureTermModel,
) -> usize {
    let context_tokens = normalized_alnum_tokens(text, 2).into_iter().collect::<BTreeSet<_>>();
    let context_subject_score =
        versioned_update_procedure_subject_focus_score(&context_tokens, term_model);
    versioned_update_procedure_command_segments_from_text(text)
        .iter()
        .filter(|segment| {
            versioned_update_procedure_command_segment_focus_score(
                segment,
                term_model,
                context_subject_score,
            ) == 0
        })
        .count()
        .min(VERSIONED_UPDATE_PROCEDURE_COMMAND_SIGNAL_SCORE_CAP)
}

fn versioned_update_procedure_command_segment_focus_score(
    segment: &str,
    term_model: &VersionedUpdateProcedureTermModel,
    context_subject_score: usize,
) -> usize {
    let tokens = normalized_alnum_tokens(segment, 2).into_iter().collect::<BTreeSet<_>>();
    let subject_score = versioned_update_procedure_subject_focus_score(&tokens, term_model);
    let procedure_score = soft_token_overlap_count(&term_model.procedure_terms, &tokens);
    let required_subject_score =
        versioned_update_procedure_required_subject_focus_score(term_model);
    if subject_score == 0 {
        if context_subject_score < required_subject_score || procedure_score == 0 {
            return 0;
        }
        return 1usize.saturating_add(procedure_score);
    }
    if subject_score < required_subject_score && context_subject_score < required_subject_score {
        return 0;
    }
    let effective_subject_score = subject_score.max(context_subject_score);
    1usize.saturating_add(effective_subject_score).saturating_add(procedure_score)
}

fn versioned_update_procedure_command_segment_has_artifact_transfer(segment: &str) -> bool {
    shellish_tokens_from_text(segment).into_iter().any(|token| {
        shellish_token_has_external_artifact(&token) || shellish_token_is_local_artifact(&token)
    })
}

fn versioned_update_procedure_subject_focus_score(
    tokens: &BTreeSet<String>,
    term_model: &VersionedUpdateProcedureTermModel,
) -> usize {
    soft_token_overlap_count(&term_model.subject_terms, tokens)
        .saturating_add(soft_token_overlap_count(&term_model.subject_acronym_terms, tokens))
}

fn versioned_update_procedure_required_subject_focus_score(
    term_model: &VersionedUpdateProcedureTermModel,
) -> usize {
    let subject_signal_count =
        term_model.subject_terms.len().saturating_add(term_model.subject_acronym_terms.len());
    if subject_signal_count <= 1 { 1 } else { 2 }
}

fn versioned_update_procedure_command_segments_from_text(text: &str) -> Vec<String> {
    let mut segments = Vec::new();
    for line in text.lines() {
        let stripped = strip_leading_procedure_order_marker(line).trim();
        if stripped.is_empty() {
            continue;
        }
        let tokens = shellish_tokens_from_text(stripped);
        let mut index = 0usize;
        while index < tokens.len() {
            if !(shellish_inline_token_starts_command(&tokens, index)
                || index == 0 && shellish_tokens_start_command(&tokens))
            {
                index = index.saturating_add(1);
                continue;
            }
            let mut end = index.saturating_add(1);
            while end < tokens.len() && !shellish_inline_token_starts_command(&tokens, end) {
                end = end.saturating_add(1);
            }
            let segment = tokens[index..end].join(" ");
            if !segment.is_empty() {
                segments.push(segment);
            }
            index = end;
        }
    }
    segments
}

fn versioned_update_procedure_inline_command_signal_count(text: &str) -> usize {
    text.lines()
        .map(|line| {
            let tokens = shellish_tokens_from_text(line);
            let mut count = 0usize;
            for index in 0..tokens.len() {
                if shellish_inline_token_starts_command(&tokens, index) {
                    count = count.saturating_add(1);
                }
            }
            count
        })
        .sum::<usize>()
        .min(VERSIONED_UPDATE_PROCEDURE_COMMAND_SIGNAL_SCORE_CAP)
}

fn shellish_inline_token_starts_command(tokens: &[String], index: usize) -> bool {
    let Some(token) = tokens.get(index).map(String::as_str) else {
        return false;
    };
    if matches!(token, "sudo" | "doas") {
        return shellish_privileged_tokens_start_command(&tokens[index + 1..]);
    }
    shellish_tokens_have_structural_command_shape(&tokens[index..])
        || (shellish_token_is_path_command_start(token) && shellish_token_is_local_artifact(token))
}

fn versioned_update_procedure_line_has_command_start(line: &str) -> bool {
    let trimmed = strip_leading_procedure_order_marker(line).trim();
    let tokens = shellish_tokens_from_text(trimmed);
    shellish_tokens_start_command(&tokens)
}

fn shellish_tokens_start_command(tokens: &[String]) -> bool {
    let Some(first) = tokens.first() else {
        return false;
    };
    if shellish_token_is_path_command_start(first)
        || shellish_tokens_have_structural_command_shape(tokens)
    {
        return true;
    }
    if first == "sudo" || first == "doas" {
        let rest = &tokens[1..];
        return shellish_privileged_tokens_start_command(rest)
            || (rest.len() == 1 && rest.first().is_some_and(|token| token == "su"));
    }
    false
}

fn shellish_privileged_tokens_start_command(tokens: &[String]) -> bool {
    let Some(first) = tokens.first() else {
        return false;
    };
    if first == "su" {
        let rest = &tokens[1..];
        return rest.is_empty() || shellish_privileged_tokens_start_command(rest);
    }
    if shellish_token_is_path_command_start(first) {
        return true;
    }
    shellish_privileged_invocation_has_command_shape(tokens)
}

fn shellish_privileged_invocation_has_command_shape(tokens: &[String]) -> bool {
    let Some(head) = tokens.first().map(String::as_str) else {
        return false;
    };
    if !shellish_token_is_invocable_head(head) {
        return false;
    }
    if shellish_token_has_executable_name_shape(head) {
        return true;
    }
    let args = &tokens[1..];
    args.iter().take(4).any(|arg| {
        shellish_token_is_command_argument_signal(arg)
            || shellish_token_is_path_command_start(arg)
            || shellish_token_is_local_artifact(arg)
            || shellish_token_has_executable_name_shape(arg)
    })
}

fn strip_leading_procedure_order_marker(line: &str) -> &str {
    let trimmed = line.trim_start();
    let trimmed = trimmed.strip_prefix(['-', '*', '•']).unwrap_or(trimmed).trim_start();
    let mut chars = trimmed.char_indices().peekable();
    let mut digit_end = None;
    while let Some((index, ch)) = chars.peek().copied() {
        if ch.is_ascii_digit() {
            digit_end = Some(index + ch.len_utf8());
            chars.next();
        } else {
            break;
        }
    }
    let Some(_) = digit_end else {
        return trimmed;
    };
    let Some((marker_index, marker)) = chars.next() else {
        return trimmed;
    };
    if matches!(marker, '.' | ')') {
        return trimmed[marker_index + marker.len_utf8()..].trim_start();
    }
    trimmed
}

fn versioned_update_procedure_ordered_version_transition_score(text: &str) -> usize {
    let count = text
        .lines()
        .filter(|line| {
            !ordered_step_looks_like_sequential_record(line)
                && ordered_step_marker_count(line) > 0
                && ordered_version_transition_line_has_instruction_version(line)
        })
        .count();
    if count >= 2 { count } else { 0 }
}

fn versioned_update_procedure_subject_aligned_version_transition_score(
    text: &str,
    term_model: &VersionedUpdateProcedureTermModel,
) -> usize {
    let count = text
        .lines()
        .filter(|line| {
            !ordered_step_looks_like_sequential_record(line)
                && ordered_step_marker_count(line) > 0
                && ordered_version_transition_line_has_instruction_version(line)
        })
        .filter(|line| {
            let tokens = normalized_alnum_tokens(line, 2).into_iter().collect::<BTreeSet<_>>();
            versioned_update_procedure_subject_focus_score(&tokens, term_model) > 0
        })
        .count();
    if count >= 2 { count } else { 0 }
}

fn ordered_version_transition_line_has_instruction_version(line: &str) -> bool {
    let body = strip_leading_procedure_order_marker(line);
    body.split_whitespace()
        .position(|token| extract_semver_like_version(token).is_some())
        .is_some_and(|position| position >= 2)
}

fn ordered_step_marker_count(text: &str) -> usize {
    let chars = text.chars().collect::<Vec<_>>();
    let mut index = 0usize;
    let mut count = 0usize;
    while index < chars.len() {
        if !chars[index].is_ascii_digit() || (index > 0 && !chars[index - 1].is_whitespace()) {
            index += 1;
            continue;
        }
        let mut end = index + 1;
        while end < chars.len() && chars[end].is_ascii_digit() {
            end += 1;
        }
        if end < chars.len()
            && matches!(chars[end], '.' | ')')
            && chars.get(end + 1).is_some_and(|ch| ch.is_whitespace())
        {
            count = count.saturating_add(1);
            index = end + 1;
            continue;
        }
        index = end;
    }
    count
}

fn procedure_artifact_token_count(text: &str) -> usize {
    text.split_whitespace().filter(|token| procedure_artifact_token(token)).count()
}

fn procedure_artifact_token(token: &str) -> bool {
    let token = token.trim_matches(|ch: char| ch.is_ascii_punctuation() && ch != '/' && ch != '\\');
    if !token.chars().any(|ch| ch.is_alphanumeric()) {
        return false;
    }
    token.contains('/')
        || token.contains('\\')
        || token.contains("--")
        || token.contains('=')
        || token.starts_with("./")
        || token.chars().filter(|ch| *ch == '.').count() >= 2
}

fn versioned_update_procedure_text_has_setup_script_signature(
    text: &str,
    procedure_terms: &BTreeSet<String>,
) -> bool {
    let tokens = shellish_tokens_from_text(text);
    if tokens.is_empty() {
        return false;
    }
    let has_external_artifact =
        tokens.iter().any(|token| shellish_token_has_external_artifact(token));
    let has_preparation_signal =
        tokens.iter().any(|token| shellish_token_has_artifact_preparation_signal(token));
    let local_artifact_tokens =
        tokens.iter().filter(|token| shellish_token_is_local_artifact(token)).collect::<Vec<_>>();
    if !has_external_artifact || !has_preparation_signal || local_artifact_tokens.is_empty() {
        return false;
    }
    let has_action_specific_artifact = local_artifact_tokens.iter().any(|token| {
        let artifact_tokens =
            normalized_alnum_tokens(token, 3).into_iter().collect::<BTreeSet<_>>();
        soft_token_overlap_count(procedure_terms, &artifact_tokens) > 0
    })
        || versioned_update_procedure_text_has_action_phrase_near_artifact(text, procedure_terms);
    !has_action_specific_artifact
}

fn versioned_update_procedure_text_has_artifact_materialization(text: &str) -> bool {
    let tokens = shellish_tokens_from_text(text);
    if tokens.is_empty() {
        return false;
    }
    let has_external_artifact =
        tokens.iter().any(|token| shellish_token_has_external_artifact(token));
    let has_local_artifact = tokens.iter().any(|token| shellish_token_is_local_artifact(token));
    let has_preparation_signal =
        tokens.iter().any(|token| shellish_token_has_artifact_preparation_signal(token));
    has_external_artifact && has_local_artifact && has_preparation_signal
}

fn versioned_update_procedure_text_has_action_phrase_near_artifact(
    text: &str,
    procedure_terms: &BTreeSet<String>,
) -> bool {
    if procedure_terms.is_empty() {
        return false;
    }
    text.lines().any(|line| {
        let line_tokens = normalized_alnum_tokens(line, 3).into_iter().collect::<BTreeSet<_>>();
        soft_token_overlap_count(procedure_terms, &line_tokens) > 0
            && shellish_tokens_from_text(line).into_iter().any(|token| {
                shellish_token_has_external_artifact(&token)
                    || shellish_token_is_local_artifact(&token)
            })
    })
}

fn versioned_update_procedure_text_has_action_script(
    text: &str,
    procedure_terms: &BTreeSet<String>,
) -> bool {
    if procedure_terms.is_empty() {
        return false;
    }
    shellish_tokens_from_text(text)
        .into_iter()
        .filter(|token| {
            !token.is_empty()
                && (shellish_token_has_external_artifact(token)
                    || shellish_token_is_local_artifact(token))
        })
        .any(|token| {
            let artifact_tokens =
                normalized_alnum_tokens(&token, 3).into_iter().collect::<BTreeSet<_>>();
            soft_token_overlap_count(procedure_terms, &artifact_tokens) > 0
        })
}

fn shellish_tokens_from_text(text: &str) -> Vec<String> {
    text.split_whitespace()
        .flat_map(clean_shellish_token_expansions)
        .filter(|token| !token.is_empty())
        .collect()
}

fn clean_shellish_token_expansions(token: &str) -> Vec<String> {
    let cleaned = clean_shellish_token(token);
    if cleaned.is_empty() {
        return Vec::new();
    }
    expand_shellish_token(&cleaned)
}

fn expand_shellish_token(token: &str) -> Vec<String> {
    if let Some(suffix) = token.strip_prefix("su")
        && !suffix.is_empty()
        && shellish_token_has_executable_name_shape(suffix)
    {
        return vec!["su".to_string(), suffix.to_string()];
    }
    if let Some((left, right)) = split_concatenated_local_artifact_token(token) {
        return vec![left, right];
    }
    vec![token.to_string()]
}

fn split_concatenated_local_artifact_token(token: &str) -> Option<(String, String)> {
    for (index, _) in token.char_indices().skip(1) {
        let rest = &token[index..];
        if (rest.starts_with('/') || rest.starts_with("./"))
            && shellish_token_is_local_artifact(&token[..index])
            && shellish_token_is_local_artifact(rest)
        {
            return Some((token[..index].to_string(), rest.to_string()));
        }
    }
    None
}

fn clean_shellish_token(token: &str) -> String {
    let cleaned = token
        .trim_matches(|ch: char| {
            ch.is_ascii_punctuation() && !matches!(ch, '/' | '.' | '-' | '_' | '+' | '=' | ':')
        })
        .trim_matches('\u{200e}')
        .trim_matches('\u{200f}')
        .to_ascii_lowercase();
    if cleaned.chars().any(|ch| ch.is_alphanumeric()) { cleaned } else { String::new() }
}

fn shellish_token_has_external_artifact(token: &str) -> bool {
    token.contains("://")
}

fn shellish_token_is_local_artifact(token: &str) -> bool {
    shellish_token_is_path_command_start(token)
        || shellish_token_file_artifact_name(token).is_some()
}

fn shellish_token_file_artifact_name(token: &str) -> Option<&str> {
    let file_name = token
        .rsplit('/')
        .next()?
        .split(['?', '#'])
        .next()?
        .trim_end_matches(|ch: char| ch.is_ascii_punctuation());
    let has_extension = file_name
        .rsplit_once('.')
        .is_some_and(|(_, extension)| (2..=12).contains(&extension.len()));
    let has_structural_name = file_name.contains('-')
        || file_name.contains('_')
        || file_name.chars().any(|ch| ch.is_ascii_digit());
    (has_extension || has_structural_name).then_some(file_name)
}

fn shellish_token_has_artifact_preparation_signal(token: &str) -> bool {
    token.starts_with('+')
        || token.contains("+x")
        || token.chars().all(|ch| ch.is_ascii_digit())
        || token.starts_with('-')
}

fn shellish_token_is_path_command_start(token: &str) -> bool {
    (token.starts_with("./")
        || token.starts_with("../")
        || token.starts_with("/tmp/")
        || token.starts_with('/'))
        && token.chars().any(|ch| ch.is_alphanumeric())
        && !token.contains("://")
}

fn shellish_tokens_have_structural_command_shape(tokens: &[String]) -> bool {
    let Some(head) = tokens.first() else {
        return false;
    };
    if !shellish_token_is_invocable_head(head) {
        return false;
    }
    let args = &tokens[1..];
    let has_structural_arg = args.iter().take(8).any(|token| {
        shellish_token_is_command_argument_signal(token) || shellish_token_is_local_artifact(token)
    });
    if shellish_token_has_executable_name_shape(head) && has_structural_arg {
        return true;
    }
    if !shellish_token_has_executable_name_shape(head) && has_structural_arg {
        return true;
    }
    shellish_token_has_executable_name_shape(head)
        && args.len() == 1
        && args.first().is_some_and(|arg| shellish_token_is_subcommand_word(arg))
}

fn shellish_token_is_invocable_head(token: &str) -> bool {
    !token.is_empty()
        && !token.starts_with('-')
        && !token.contains("://")
        && !token.contains('=')
        && token.chars().any(|ch| ch.is_alphabetic())
        && token
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '.' | '_' | '-' | '+' | '/' | '\\'))
}

fn shellish_token_has_executable_name_shape(token: &str) -> bool {
    token.contains('-')
        || token.contains('_')
        || token.contains('.')
        || token.contains('/')
        || token.contains('\\')
        || token.chars().any(|ch| ch.is_ascii_digit())
}

fn shellish_token_is_subcommand_word(token: &str) -> bool {
    let len = token.chars().count();
    (2..=32).contains(&len)
        && !token.starts_with('-')
        && token.chars().all(|ch| ch.is_alphanumeric() || matches!(ch, '-' | '_'))
}

fn shellish_token_is_command_argument_signal(token: &str) -> bool {
    token.starts_with('-')
        || token.contains("--")
        || token.contains('=')
        || token.contains('/')
        || token.contains('\\')
        || token.contains('|')
        || token.contains("://")
}

#[derive(Clone)]
struct VersionedUpdateProcedureTermModel {
    query_terms: BTreeSet<String>,
    subject_terms: BTreeSet<String>,
    subject_acronym_terms: BTreeSet<String>,
    procedure_terms: BTreeSet<String>,
    target_identity_sequences: Vec<Vec<String>>,
}

fn versioned_update_procedure_term_model(
    question: &str,
    query_ir: Option<&QueryIR>,
) -> VersionedUpdateProcedureTermModel {
    let query_terms = versioned_update_procedure_query_terms(question, query_ir);
    let mut subject_terms = BTreeSet::<String>::new();
    let mut subject_acronym_terms = BTreeSet::<String>::new();
    if let Some(query_ir) = query_ir {
        for entity in &query_ir.target_entities {
            add_versioned_update_procedure_subject_terms(
                &mut subject_terms,
                &mut subject_acronym_terms,
                &entity.label,
            );
        }
        if let Some(document_focus) = query_ir.document_focus.as_ref() {
            add_versioned_update_procedure_subject_terms(
                &mut subject_terms,
                &mut subject_acronym_terms,
                &document_focus.hint,
            );
        }
    }
    let target_identity_sequences =
        versioned_update_procedure_target_identity_token_sequences(question, query_ir);
    let mut identity_exclusion_terms = BTreeSet::<String>::new();
    let mut identity_exclusion_acronym_terms = BTreeSet::<String>::new();
    for sequence in &target_identity_sequences {
        let identity = sequence.join(" ");
        let has_structured_identity = query_ir_has_structured_target_identity(query_ir);
        let has_distinctive_raw_surface =
            versioned_update_raw_identity_sequence_has_distinctive_surface(question, sequence)
                || query_ir.and_then(|query_ir| query_ir.retrieval_query.as_deref()).is_some_and(
                    |retrieval_query| {
                        versioned_update_raw_identity_sequence_has_distinctive_surface(
                            retrieval_query,
                            sequence,
                        )
                    },
                );
        if has_structured_identity || has_distinctive_raw_surface {
            add_versioned_update_procedure_subject_terms(
                &mut identity_exclusion_terms,
                &mut identity_exclusion_acronym_terms,
                &identity,
            );
        }
        if has_structured_identity {
            add_versioned_update_procedure_subject_terms(
                &mut subject_terms,
                &mut subject_acronym_terms,
                &identity,
            );
        }
    }
    if subject_terms.is_empty()
        && subject_acronym_terms.is_empty()
        && !query_ir_has_structured_target_identity(query_ir)
    {
        add_versioned_update_procedure_raw_subject_terms(
            &mut subject_terms,
            &mut subject_acronym_terms,
            question,
            query_ir,
        );
    }
    let mut all_subject_terms = subject_terms.clone();
    all_subject_terms.extend(subject_acronym_terms.iter().cloned());
    all_subject_terms.extend(identity_exclusion_terms);
    all_subject_terms.extend(identity_exclusion_acronym_terms);
    let procedure_terms = query_terms
        .iter()
        .filter(|term| !all_subject_terms.iter().any(|subject| soft_token_match(term, subject)))
        .cloned()
        .collect::<BTreeSet<_>>();
    VersionedUpdateProcedureTermModel {
        query_terms,
        subject_terms,
        subject_acronym_terms,
        procedure_terms,
        target_identity_sequences,
    }
}

fn query_ir_has_structured_target_identity(query_ir: Option<&QueryIR>) -> bool {
    let Some(query_ir) = query_ir else {
        return false;
    };
    !query_ir.target_entities.is_empty()
        || query_ir.document_focus.is_some()
        || query_ir
            .literal_constraints
            .iter()
            .any(|literal| matches!(literal.kind, LiteralKind::Identifier | LiteralKind::Other))
}

fn versioned_update_raw_identity_sequence_has_distinctive_surface(
    text: &str,
    sequence: &[String],
) -> bool {
    if sequence.is_empty() {
        return false;
    }
    let surface_tokens = text
        .split_whitespace()
        .filter_map(|token| {
            let normalized = normalized_alnum_token_sequence(token, 1);
            (!normalized.is_empty()).then(|| {
                let distinctive = token.chars().any(|ch| {
                    ch.is_uppercase()
                        || ch.is_ascii_digit()
                        || matches!(ch, '-' | '_' | '.' | '/' | '\\')
                });
                (normalized, distinctive)
            })
        })
        .collect::<Vec<_>>();
    for start in 0..surface_tokens.len() {
        let mut tokens = Vec::<String>::new();
        let mut surface_count = 0usize;
        let mut distinctive_surface_count = 0usize;
        for (normalized, token_is_distinctive) in surface_tokens.iter().skip(start) {
            tokens.extend(normalized.iter().cloned());
            surface_count = surface_count.saturating_add(1);
            if *token_is_distinctive {
                distinctive_surface_count = distinctive_surface_count.saturating_add(1);
            }
            if tokens.len() > sequence.len() {
                break;
            }
            if tokens == sequence {
                return distinctive_surface_count > 0
                    && ((surface_count == 1 && sequence.len() >= 2)
                        || distinctive_surface_count == surface_count);
            }
        }
    }
    false
}

fn versioned_update_procedure_query_terms(
    question: &str,
    query_ir: Option<&QueryIR>,
) -> BTreeSet<String> {
    let mut terms = normalized_alnum_tokens(question, 3).into_iter().collect::<BTreeSet<_>>();
    if let Some(query_ir) = query_ir {
        for entity in &query_ir.target_entities {
            let mut acronym_terms = BTreeSet::new();
            add_versioned_update_procedure_subject_terms(
                &mut terms,
                &mut acronym_terms,
                &entity.label,
            );
            terms.extend(acronym_terms);
        }
        if let Some(document_focus) = query_ir.document_focus.as_ref() {
            let mut acronym_terms = BTreeSet::new();
            add_versioned_update_procedure_subject_terms(
                &mut terms,
                &mut acronym_terms,
                &document_focus.hint,
            );
            terms.extend(acronym_terms);
        }
        if let Some(retrieval_query) = query_ir.retrieval_query.as_deref() {
            terms.extend(normalized_alnum_tokens(retrieval_query, 3));
        }
    }
    terms
}

fn add_versioned_update_procedure_subject_terms(
    terms: &mut BTreeSet<String>,
    acronym_terms: &mut BTreeSet<String>,
    phrase: &str,
) {
    add_label_terms_with_acronyms(terms, acronym_terms, phrase, 2);
}

fn add_versioned_update_procedure_raw_subject_terms(
    terms: &mut BTreeSet<String>,
    acronym_terms: &mut BTreeSet<String>,
    question: &str,
    query_ir: Option<&QueryIR>,
) {
    let mut sources = vec![question];
    if let Some(query_ir) = query_ir
        && let Some(retrieval_query) = query_ir.retrieval_query.as_deref()
    {
        sources.push(retrieval_query);
    }
    for source in sources {
        for sequence in versioned_update_procedure_raw_subject_token_sequences(source) {
            add_versioned_update_procedure_subject_terms(terms, acronym_terms, &sequence.join(" "));
            if !terms.is_empty() || !acronym_terms.is_empty() {
                return;
            }
        }
    }
}

fn versioned_update_procedure_raw_subject_token_sequences(question: &str) -> Vec<Vec<String>> {
    let current = strip_leading_question_marker(current_question_segment(question));
    let tokens = normalized_alnum_token_sequence(current, 1);
    if tokens.len() < 3 {
        return Vec::new();
    }
    let mut sequences = Vec::new();
    let mut seen = BTreeSet::<Vec<String>>::new();
    let sequence = tokens[tokens.len().saturating_sub(2)..].to_vec();
    if versioned_update_procedure_target_identity_sequence_is_usable(&sequence)
        && seen.insert(sequence.clone())
    {
        sequences.push(sequence);
    }
    sequences
}

fn versioned_update_procedure_focus_terms(
    question: &str,
    query_ir: Option<&QueryIR>,
    plan_keywords: &[String],
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut terms = Vec::new();
    for term in versioned_update_procedure_query_terms(question, query_ir) {
        if seen.insert(term.clone()) {
            terms.push(term);
        }
    }
    for keyword in plan_keywords {
        let keyword = keyword.trim();
        if keyword.chars().count() >= 3 && seen.insert(keyword.to_lowercase()) {
            terms.push(keyword.to_string());
        }
    }
    if terms.is_empty() {
        terms.push(question.trim().to_string());
    }
    terms
}

fn versioned_update_procedure_terms_as_focus_terms(terms: &BTreeSet<String>) -> Vec<String> {
    terms.iter().cloned().collect()
}

fn setup_focus_uses_raw_question_fallback(query_ir: &QueryIR) -> bool {
    !query_ir_requests_setup_focus_document_candidates(query_ir)
        && query_ir_allows_raw_question_setup_focus_fallback(query_ir)
}

fn query_ir_warrants_structural_setup_focus_fallback(query_ir: &QueryIR) -> bool {
    query_ir.confidence < 0.4
        && matches!(query_ir.act, QueryAct::Describe | QueryAct::RetrieveValue)
        && query_ir.source_slice.is_none()
        && query_ir.target_types.is_empty()
        && query_ir.document_focus.is_none()
        && query_ir.comparison.is_none()
        && query_ir.temporal_constraints.is_empty()
        && query_ir.conversation_refs.is_empty()
        && (!query_ir.target_entities.is_empty() || !query_ir.literal_constraints.is_empty())
}

fn select_setup_focus_document_chunks(
    document_candidates: Vec<(usize, Uuid, Vec<RuntimeMatchedChunk>)>,
) -> Vec<RuntimeMatchedChunk> {
    let mut document_candidates = document_candidates;
    document_candidates.sort_by(
        |(left_score, left_id, left_chunks), (right_score, right_id, right_chunks)| {
            right_score
                .cmp(left_score)
                .then_with(|| {
                    setup_focus_candidate_best_chunk_score(right_chunks)
                        .total_cmp(&setup_focus_candidate_best_chunk_score(left_chunks))
                })
                .then_with(|| left_id.cmp(right_id))
        },
    );
    let mut selected = Vec::new();
    for (_, _, mut chunks) in document_candidates {
        chunks.sort_by(|left, right| {
            left.chunk_index
                .cmp(&right.chunk_index)
                .then_with(|| score_desc_chunks(left, right))
                .then_with(|| left.chunk_id.cmp(&right.chunk_id))
        });
        for chunk in chunks {
            selected.push(chunk);
            if selected.len() >= SETUP_FOCUS_DOCUMENT_CHUNK_CAP {
                return selected;
            }
        }
    }
    selected
}

fn setup_focus_candidate_best_chunk_score(chunks: &[RuntimeMatchedChunk]) -> f32 {
    chunks.iter().filter_map(|chunk| chunk.score).max_by(f32::total_cmp).unwrap_or(0.0)
}

fn query_ir_requests_setup_focus_document_candidates(query_ir: &QueryIR) -> bool {
    if query_ir.source_slice.is_some()
        || query_ir_requests_versioned_update_procedure_context("", query_ir)
        || !query_ir_has_setup_focus_identity(query_ir)
    {
        return false;
    }
    // A configure/how-to intent qualifies for the focused-document lane when it
    // names a concrete subject or declares a command-object/configuration/parameter target.
    // Candidate selection still requires document-identity overlap, so generic
    // configure questions without a usable identity stay out of this lane.
    if matches!(query_ir.act, QueryAct::ConfigureHow)
        && (query_ir_has_setup_focus_target(query_ir)
            || query_ir.document_focus.is_some()
            || setup_focus_single_subject_identity(query_ir).is_some()
            || query_ir_has_setup_focus_subject_modifier_identity(query_ir)
            || query_ir_has_setup_focus_followup_object_identity(query_ir)
            || query_ir_has_specific_adjacent_entity_identity(query_ir))
    {
        return true;
    }
    let mut has_command_object_target = false;
    let mut has_configuration_target = false;
    for target_type in &query_ir.target_types {
        match canonical_target_type_tag(target_type).as_str() {
            "package" => has_command_object_target = true,
            "configuration_file" | "config_key" => has_configuration_target = true,
            _ => {}
        }
    }
    has_command_object_target && has_configuration_target
}

fn query_ir_allows_raw_question_setup_focus_fallback(query_ir: &QueryIR) -> bool {
    query_ir.confidence < 0.4
        && matches!(query_ir.scope, QueryScope::SingleDocument | QueryScope::MultiDocument)
        && matches!(query_ir.act, QueryAct::Describe | QueryAct::RetrieveValue)
        && query_ir.source_slice.is_none()
        && query_ir.target_types.is_empty()
        && query_ir.target_entities.is_empty()
        && query_ir.document_focus.is_none()
        && query_ir.literal_constraints.is_empty()
        && query_ir.conversation_refs.is_empty()
}

fn query_ir_has_setup_focus_identity(query_ir: &QueryIR) -> bool {
    query_ir.document_focus.is_some()
        || query_ir_has_setup_focus_subject_identity(query_ir)
        || query_ir_has_setup_focus_subject_modifier_identity(query_ir)
        || query_ir_has_setup_focus_followup_object_identity(query_ir)
        || query_ir_has_specific_adjacent_entity_identity(query_ir)
}

fn query_ir_has_setup_focus_subject_identity(query_ir: &QueryIR) -> bool {
    query_ir.target_entities.iter().any(|entity| {
        matches!(entity.role, EntityRole::Subject)
            && setup_focus_entity_has_usable_identity(entity.label.trim())
    })
}

fn query_ir_has_setup_focus_followup_object_identity(query_ir: &QueryIR) -> bool {
    !query_ir.conversation_refs.is_empty()
        && matches!(query_ir.act, QueryAct::ConfigureHow)
        && query_ir.target_entities.iter().any(|entity| {
            matches!(entity.role, EntityRole::Object)
                && !setup_focus_identity_tokens(entity.label.trim()).is_empty()
        })
}

fn setup_focus_single_subject_identity(query_ir: &QueryIR) -> Option<&str> {
    if query_ir.comparison.is_some()
        || !query_ir.temporal_constraints.is_empty()
        || !query_ir.conversation_refs.is_empty()
        || !query_ir.literal_constraints.is_empty()
    {
        return None;
    }
    let mut subjects = query_ir
        .target_entities
        .iter()
        .filter(|entity| matches!(entity.role, EntityRole::Subject))
        .filter_map(|entity| {
            let label = entity.label.trim();
            setup_focus_entity_has_usable_identity(label).then_some(label)
        });
    let subject = subjects.next()?;
    subjects.next().is_none().then_some(subject)
}

fn setup_focus_entity_has_usable_identity(label: &str) -> bool {
    normalized_alnum_tokens(label, 3).len() >= 2 || !short_acronym_identity_tokens(label).is_empty()
}

fn query_ir_has_setup_focus_subject_modifier_identity(query_ir: &QueryIR) -> bool {
    let Some(subject) = setup_focus_single_subject_label(query_ir) else {
        return false;
    };
    !setup_focus_subject_modifier_identity_terms(query_ir, subject).is_empty()
}

fn setup_focus_single_subject_label(query_ir: &QueryIR) -> Option<&str> {
    if query_ir.comparison.is_some()
        || !query_ir.temporal_constraints.is_empty()
        || !query_ir.conversation_refs.is_empty()
        || !query_ir.literal_constraints.is_empty()
    {
        return None;
    }
    let mut subjects = query_ir
        .target_entities
        .iter()
        .filter(|entity| matches!(entity.role, EntityRole::Subject))
        .filter_map(|entity| {
            let label = entity.label.trim();
            (!setup_focus_identity_tokens(label).is_empty()).then_some(label)
        });
    let subject = subjects.next()?;
    subjects.next().is_none().then_some(subject)
}

fn query_ir_has_setup_focus_target(query_ir: &QueryIR) -> bool {
    query_ir.target_types.iter().any(|target_type| {
        matches!(
            canonical_target_type_tag(target_type).as_str(),
            "package" | "configuration_file" | "config_key" | "parameter"
        )
    })
}

fn setup_focus_candidate_document_ids(
    query_ir: &QueryIR,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    limit: usize,
) -> Vec<Uuid> {
    let query_identity_terms = if query_ir_has_specific_adjacent_entity_identity(query_ir) {
        setup_focus_adjacent_entity_identity_terms(query_ir)
    } else {
        setup_focus_query_identity_terms(query_ir)
    };
    let focus_terms = query_identity_terms
        .into_iter()
        .filter_map(|term| {
            let tokens = setup_focus_identity_tokens(&term);
            (!tokens.is_empty()).then_some((term, tokens))
        })
        .collect::<Vec<_>>();
    if focus_terms.is_empty() || limit == 0 {
        return Vec::new();
    }
    let mut candidates = document_index
        .values()
        .filter(|document| !setup_focus_document_is_standalone_image(document))
        .filter_map(|document| {
            let best = focus_terms
                .iter()
                .enumerate()
                .flat_map(|(focus_index, (focus, focus_tokens))| {
                    let required_overlap = focus_tokens.len().clamp(1, 2);
                    let focus_token_set = focus_tokens.iter().cloned().collect::<BTreeSet<_>>();
                    setup_focus_document_identity_values(document).into_iter().filter_map(
                        move |value| {
                            let value_tokens = setup_focus_identity_tokens(&value);
                            let overlap = near_token_overlap_count(&focus_token_set, &value_tokens);
                            (overlap >= required_overlap).then(|| {
                                let exact = normalize_document_identity_value(&value)
                                    == normalize_document_identity_value(focus);
                                let specificity = focus_tokens.len();
                                let length = value.chars().count();
                                (
                                    std::cmp::Reverse(focus_index),
                                    overlap,
                                    exact,
                                    specificity,
                                    std::cmp::Reverse(length),
                                    document.document_id,
                                )
                            })
                        },
                    )
                })
                .max();
            best.map(|score| (score, document.document_id))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|(left, _), (right, _)| right.cmp(left));
    candidates.into_iter().map(|(_, document_id)| document_id).take(limit).collect()
}

#[derive(Debug, Clone, Default)]
struct RawQuestionSetupFocusTokens {
    tokens: BTreeSet<String>,
    standalone_tokens: BTreeSet<String>,
}

fn raw_question_setup_focus_tokens(question: &str) -> RawQuestionSetupFocusTokens {
    let question = strip_leading_question_marker(question);
    RawQuestionSetupFocusTokens {
        tokens: setup_focus_identity_tokens(question),
        standalone_tokens: short_acronym_identity_tokens(question),
    }
}

fn raw_question_setup_focus_candidate_document_ids(
    question_tokens: Option<&RawQuestionSetupFocusTokens>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    limit: usize,
) -> Vec<Uuid> {
    let Some(question_tokens) = question_tokens else {
        return Vec::new();
    };
    if question_tokens.tokens.is_empty() || limit == 0 {
        return Vec::new();
    }
    let distinctive_tokens =
        raw_setup_focus_distinctive_question_tokens(&question_tokens.tokens, document_index);
    if distinctive_tokens.is_empty() {
        return Vec::new();
    }
    let scoring_tokens = question_tokens.scoped_to(&distinctive_tokens);
    let mut candidates = document_index
        .values()
        .filter(|document| !setup_focus_document_is_standalone_image(document))
        .filter_map(|document| {
            let score = raw_question_setup_focus_document_score(&scoring_tokens, document);
            (score > 0).then_some((score, document.document_id))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|(left_score, left_id), (right_score, right_id)| {
        right_score.cmp(left_score).then_with(|| left_id.cmp(right_id))
    });
    candidates.into_iter().map(|(_, document_id)| document_id).take(limit).collect()
}

fn raw_question_setup_focus_document_score(
    question_tokens: &RawQuestionSetupFocusTokens,
    document: &KnowledgeDocumentRow,
) -> usize {
    setup_focus_document_identity_values(document)
        .into_iter()
        .map(|value| {
            let value_tokens = setup_focus_identity_tokens(&value);
            let overlap_tokens =
                question_tokens.tokens.intersection(&value_tokens).cloned().collect::<Vec<_>>();
            let overlap = overlap_tokens.len();
            if overlap >= 2 {
                return overlap.saturating_mul(32).saturating_add(value_tokens.len().min(16));
            }
            if overlap == 1
                && overlap_tokens.first().is_some_and(|token| question_tokens.can_standalone(token))
            {
                return 24usize.saturating_add(value_tokens.len().min(16));
            }
            0
        })
        .max()
        .unwrap_or(0)
}

impl RawQuestionSetupFocusTokens {
    fn scoped_to(&self, distinctive_tokens: &BTreeSet<String>) -> Self {
        let standalone_tokens = self
            .standalone_tokens
            .intersection(distinctive_tokens)
            .cloned()
            .collect::<BTreeSet<_>>();
        Self { tokens: distinctive_tokens.clone(), standalone_tokens }
    }

    fn can_standalone(&self, token: &str) -> bool {
        self.standalone_tokens.contains(token)
    }
}

fn document_evidence_anchor_candidate_document_ids(
    question: &str,
    query_ir: Option<&QueryIR>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    limit: usize,
) -> Vec<Uuid> {
    if limit == 0 {
        return Vec::new();
    }
    if let Some(query_ir) = query_ir {
        if query_ir.source_slice.is_some()
            || query_ir.comparison.is_some()
            || matches!(query_ir.scope, QueryScope::MultiDocument | QueryScope::LibraryMeta)
        {
            return Vec::new();
        }
    }
    let focus_terms = document_evidence_anchor_focus_terms(question, query_ir);
    let focus_tokens = focus_terms.into_iter().collect::<BTreeSet<_>>();
    if focus_tokens.is_empty() {
        return Vec::new();
    }
    let distinctive_tokens =
        raw_setup_focus_distinctive_question_tokens(&focus_tokens, document_index);
    let scoring_tokens =
        if distinctive_tokens.is_empty() { focus_tokens } else { distinctive_tokens };
    let mut candidates = document_index
        .values()
        .filter(|document| !setup_focus_document_is_standalone_image(document))
        .filter_map(|document| {
            let score = setup_focus_document_identity_values(document)
                .into_iter()
                .map(|value| document_evidence_anchor_identity_score(&scoring_tokens, &value))
                .max()
                .unwrap_or(0);
            (score > 0).then_some((score, document.document_id))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|(left_score, left_id), (right_score, right_id)| {
        right_score.cmp(left_score).then_with(|| left_id.cmp(right_id))
    });
    candidates.into_iter().map(|(_, document_id)| document_id).take(limit).collect()
}

fn document_evidence_anchor_focus_terms(question: &str, query_ir: Option<&QueryIR>) -> Vec<String> {
    let mut terms = normalized_alnum_tokens(strip_leading_question_marker(question), 3);
    if let Some(query_ir) = query_ir {
        if let Some(retrieval_query) = query_ir.retrieval_query.as_deref() {
            terms.extend(normalized_alnum_tokens(retrieval_query, 3));
        }
        if let Some(document_focus) = query_ir.document_focus.as_ref() {
            terms.extend(normalized_alnum_tokens(&document_focus.hint, 3));
        }
        for entity in &query_ir.target_entities {
            terms.extend(normalized_alnum_tokens(&entity.label, 3));
        }
        for literal in &query_ir.literal_constraints {
            terms.extend(normalized_alnum_tokens(&literal.text, 3));
        }
    }
    terms.into_iter().collect()
}

fn document_evidence_anchor_identity_score(
    focus_tokens: &BTreeSet<String>,
    identity_value: &str,
) -> usize {
    let identity_tokens = normalized_alnum_tokens(identity_value, 3);
    let overlap = focus_tokens.intersection(&identity_tokens).count();
    if overlap < 2 {
        return 0;
    }
    overlap.saturating_mul(32).saturating_add(identity_tokens.len().min(16))
}

fn raw_setup_focus_distinctive_question_tokens(
    question_tokens: &BTreeSet<String>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> BTreeSet<String> {
    if question_tokens.is_empty() || document_index.is_empty() {
        return BTreeSet::new();
    }
    let max_document_frequency =
        raw_setup_focus_candidate_token_max_document_frequency(document_index.len());
    let mut document_frequency = HashMap::<String, usize>::new();
    for document in document_index.values() {
        let mut seen_for_document = BTreeSet::<String>::new();
        for value in setup_focus_document_identity_values(document) {
            for token in setup_focus_identity_tokens(&value) {
                if question_tokens.contains(&token) {
                    seen_for_document.insert(token);
                }
            }
        }
        for token in seen_for_document {
            *document_frequency.entry(token).or_default() += 1;
        }
    }
    question_tokens
        .iter()
        .filter(|token| {
            document_frequency
                .get(*token)
                .is_some_and(|frequency| *frequency <= max_document_frequency)
        })
        .cloned()
        .collect()
}

fn raw_setup_focus_candidate_token_max_document_frequency(document_count: usize) -> usize {
    document_count.saturating_div(RAW_SETUP_FOCUS_CANDIDATE_TOKEN_DOCUMENT_DIVISOR).clamp(
        RAW_SETUP_FOCUS_CANDIDATE_TOKEN_MAX_DOC_FREQUENCY_FLOOR,
        RAW_SETUP_FOCUS_CANDIDATE_TOKEN_MAX_DOC_FREQUENCY_CAP,
    )
}

fn filter_raw_setup_focus_chunks_by_primary_context(
    setup_focus_chunks: Vec<RuntimeMatchedChunk>,
    primary_chunks: &[RuntimeMatchedChunk],
) -> Vec<RuntimeMatchedChunk> {
    if setup_focus_chunks.is_empty() || primary_chunks.is_empty() {
        return Vec::new();
    }
    let primary_document_labels = primary_chunks
        .iter()
        .take(RAW_SETUP_FOCUS_PRIMARY_SUPPORT_CHUNK_LIMIT)
        .map(|chunk| (chunk.document_id, normalized_alnum_tokens(&chunk.document_label, 3)))
        .collect::<Vec<_>>();
    let (supported, unsupported): (Vec<_>, Vec<_>) =
        setup_focus_chunks.into_iter().partition(|chunk| {
            raw_setup_focus_chunk_has_primary_context_support(chunk, &primary_document_labels)
        });
    if !supported.is_empty() {
        return supported;
    }

    retain_raw_setup_focus_structural_document(unsupported)
}

fn raw_setup_focus_chunk_has_primary_context_support(
    chunk: &RuntimeMatchedChunk,
    primary_document_labels: &[(Uuid, BTreeSet<String>)],
) -> bool {
    let chunk_label_tokens = normalized_alnum_tokens(&chunk.document_label, 3);
    if chunk_label_tokens.is_empty() {
        return false;
    }
    primary_document_labels.iter().any(|(document_id, label_tokens)| {
        *document_id == chunk.document_id
            || chunk_label_tokens.intersection(label_tokens).count()
                >= RAW_SETUP_FOCUS_PRIMARY_LABEL_MIN_OVERLAP
    })
}

fn retain_raw_setup_focus_structural_document(
    setup_focus_chunks: Vec<RuntimeMatchedChunk>,
) -> Vec<RuntimeMatchedChunk> {
    if setup_focus_chunks.is_empty() {
        return Vec::new();
    }
    let mut document_stats = HashMap::<Uuid, (usize, usize)>::new();
    for chunk in &setup_focus_chunks {
        let score = raw_setup_focus_chunk_structural_score(chunk);
        if score == 0 {
            continue;
        }
        let stats = document_stats.entry(chunk.document_id).or_default();
        stats.0 = stats.0.saturating_add(score);
        stats.1 = stats.1.saturating_add(1);
    }
    let Some((document_id, (score, chunk_count))) =
        document_stats.into_iter().max_by(|(left_id, left), (right_id, right)| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.cmp(&right.1))
                .then_with(|| right_id.cmp(left_id))
        })
    else {
        return Vec::new();
    };
    if score < RAW_SETUP_FOCUS_STRUCTURAL_DOCUMENT_SCORE_FLOOR
        || chunk_count < RAW_SETUP_FOCUS_STRUCTURAL_DOCUMENT_CHUNK_FLOOR
    {
        return Vec::new();
    }

    setup_focus_chunks.into_iter().filter(|chunk| chunk.document_id == document_id).collect()
}

fn raw_setup_focus_chunk_structural_score(chunk: &RuntimeMatchedChunk) -> usize {
    let text = format!("{}\n{}", chunk.source_text, chunk.excerpt);
    raw_setup_focus_text_structural_score(&text)
}

fn raw_setup_focus_text_structural_score(text: &str) -> usize {
    let command_literal_count = extract_package_command_literals(&text, 2).len();
    let configuration_path_count = setup_focus_configuration_path_count(&text);
    let assignment_count = setup_focus_parameter_assignment_count(&text);
    let parameter_literal_count = setup_focus_parameter_literal_count(&text);
    let section_count = setup_focus_section_header_count(&text);
    let url_count = setup_focus_url_count(&text);

    command_literal_count
        .saturating_mul(8)
        .saturating_add(configuration_path_count.saturating_mul(12))
        .saturating_add(assignment_count.saturating_mul(4))
        .saturating_add(parameter_literal_count.saturating_mul(6))
        .saturating_add(section_count.saturating_mul(3))
        .saturating_add(url_count.saturating_mul(4))
}

fn setup_focus_query_identity_terms(query_ir: &QueryIR) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut terms = Vec::new();
    if !query_ir_has_specific_adjacent_entity_identity(query_ir)
        && let Some(subject) = setup_focus_subject_only_identity(query_ir)
    {
        push_setup_focus_identity_term(subject, &mut seen, &mut terms);
        for compound in setup_focus_subject_modifier_identity_terms(query_ir, subject) {
            push_setup_focus_identity_term(&compound, &mut seen, &mut terms);
        }
        return terms;
    }
    if !query_ir_has_specific_adjacent_entity_identity(query_ir)
        && let Some(subject) = setup_focus_single_subject_label(query_ir)
    {
        let compounds = setup_focus_subject_modifier_identity_terms(query_ir, subject);
        if !compounds.is_empty() {
            push_setup_focus_identity_term(subject, &mut seen, &mut terms);
            for compound in compounds {
                push_setup_focus_identity_term(&compound, &mut seen, &mut terms);
            }
            return terms;
        }
    }
    for compound in setup_focus_adjacent_entity_identity_terms(query_ir) {
        push_setup_focus_identity_term(&compound, &mut seen, &mut terms);
    }
    for entity in &query_ir.target_entities {
        let label = entity.label.trim();
        push_setup_focus_identity_term(label, &mut seen, &mut terms);
    }
    if let Some(document_focus) = &query_ir.document_focus {
        let focus = document_focus.hint.trim();
        push_setup_focus_identity_term(focus, &mut seen, &mut terms);
    }
    for literal in &query_ir.literal_constraints {
        let literal = literal.text.trim();
        push_setup_focus_identity_term(literal, &mut seen, &mut terms);
    }
    terms
}

fn push_setup_focus_identity_term(
    term: &str,
    seen: &mut BTreeSet<String>,
    terms: &mut Vec<String>,
) {
    let trimmed = term.trim();
    if !trimmed.is_empty() && seen.insert(trimmed.to_lowercase()) {
        terms.push(trimmed.to_string());
    }
}

fn setup_focus_adjacent_entity_identity_terms(query_ir: &QueryIR) -> Vec<String> {
    let entity_values = query_ir
        .target_entities
        .iter()
        .filter_map(|entity| {
            let normalized = entity.label.split_whitespace().collect::<Vec<_>>().join(" ");
            is_usable_query_ir_focus(&normalized).then_some(normalized)
        })
        .collect::<Vec<_>>();
    adjacent_query_ir_focus_compounds(&entity_values)
}

fn setup_focus_subject_modifier_identity_terms(query_ir: &QueryIR, subject: &str) -> Vec<String> {
    let subject_tokens = setup_focus_identity_tokens(subject);
    if subject_tokens.len() > 1 {
        return Vec::new();
    }
    query_ir
        .target_entities
        .iter()
        .filter(|entity| !matches!(entity.role, EntityRole::Subject))
        .filter_map(|entity| {
            let modifier = entity.label.trim();
            if !setup_focus_identity_tokens(modifier).is_empty() {
                Some(format!("{subject} {modifier}"))
            } else {
                None
            }
        })
        .collect()
}

fn setup_focus_subject_only_identity(query_ir: &QueryIR) -> Option<&str> {
    if !matches!(query_ir.act, QueryAct::ConfigureHow)
        || query_ir.document_focus.is_some()
        || query_ir_has_setup_focus_target(query_ir)
    {
        return None;
    }
    setup_focus_single_subject_identity(query_ir)
}

fn setup_focus_identity_tokens(value: &str) -> BTreeSet<String> {
    let mut tokens = normalized_alnum_tokens(value, 3);
    tokens.extend(short_acronym_identity_tokens(value));
    tokens
}

fn short_acronym_identity_tokens(value: &str) -> BTreeSet<String> {
    let mut tokens = BTreeSet::new();
    let mut current = String::new();
    for ch in value.nfkc() {
        if ch.is_alphanumeric() {
            current.push(ch);
        } else {
            push_short_acronym_identity_token(&mut tokens, &mut current);
        }
    }
    push_short_acronym_identity_token(&mut tokens, &mut current);
    tokens
}

fn push_short_acronym_identity_token(tokens: &mut BTreeSet<String>, current: &mut String) {
    let token = current.trim();
    if token_is_short_acronym_identity(token) {
        tokens.extend(normalized_alnum_tokens(token, 1));
    }
    current.clear();
}

fn token_is_short_acronym_identity(token: &str) -> bool {
    let len = token.chars().count();
    if !(2..=4).contains(&len) {
        return false;
    }
    let mut has_uppercase_letter = false;
    for ch in token.chars() {
        if !ch.is_alphabetic() {
            continue;
        }
        if ch.is_lowercase() {
            return false;
        }
        if ch.is_uppercase() {
            has_uppercase_letter = true;
        }
    }
    has_uppercase_letter
}

fn setup_focus_document_is_standalone_image(document: &KnowledgeDocumentRow) -> bool {
    setup_focus_document_identity_values(document).into_iter().any(|value| {
        let lowered = value.trim().to_ascii_lowercase();
        SETUP_FOCUS_IMAGE_EXTENSIONS.iter().any(|extension| lowered.ends_with(extension))
    })
}

const SETUP_FOCUS_IMAGE_EXTENSIONS: [&str; 8] =
    [".png", ".jpg", ".jpeg", ".webp", ".gif", ".bmp", ".tif", ".tiff"];

fn setup_focus_path_has_configuration_extension(path: &str) -> bool {
    let lowered = path.to_ascii_lowercase();
    SETUP_FOCUS_CONFIG_PATH_EXTENSIONS.iter().any(|extension| lowered.ends_with(extension))
}

fn setup_focus_configuration_path_count(text: &str) -> usize {
    extract_explicit_path_literals(text, 8)
        .into_iter()
        .filter(|path| setup_focus_path_has_configuration_extension(path))
        .count()
}

fn setup_focus_parameter_assignment_count(text: &str) -> usize {
    text.lines()
        .filter(|line| {
            let trimmed = line.trim();
            let Some((key, value)) = trimmed.split_once('=') else {
                return false;
            };
            let key = key.trim();
            let value = value.trim();
            !key.is_empty()
                && !value.is_empty()
                && key.chars().any(|ch| ch.is_alphanumeric())
                && key.chars().all(|ch| {
                    ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':' | '[' | ']')
                })
        })
        .take(8)
        .count()
}

fn setup_focus_parameter_literal_count(text: &str) -> usize {
    extract_parameter_literals(text, 16).len()
}

fn setup_focus_section_header_count(text: &str) -> usize {
    text.lines()
        .filter(|line| {
            let trimmed = line.trim();
            trimmed.len() > 2 && trimmed.starts_with('[') && trimmed.ends_with(']')
        })
        .take(8)
        .count()
}

fn setup_focus_url_count(text: &str) -> usize {
    text.match_indices("http://").count() + text.match_indices("https://").count()
}

fn setup_focus_document_identity_values(document: &KnowledgeDocumentRow) -> Vec<String> {
    let mut values = Vec::new();
    if let Some(title) = document.title.as_deref() {
        values.push(title.to_string());
    }
    if let Some(file_name) = document.file_name.as_deref() {
        values.push(file_name.to_string());
    }
    values.push(document.external_key.clone());
    values
}

fn normalize_document_identity_value(value: &str) -> String {
    normalized_alnum_tokens(value, 1).into_iter().collect::<Vec<_>>().join(" ")
}

fn select_setup_focus_document_rows(rows: &[KnowledgeChunkRow]) -> Vec<KnowledgeChunkRow> {
    let mut candidates = Vec::<(usize, &KnowledgeChunkRow)>::new();
    let mut selected_chunk_ids = BTreeSet::new();
    for row in rows {
        let score = setup_focus_row_score(row);
        if score > 0 && selected_chunk_ids.insert(row.chunk_id) {
            candidates.push((setup_focus_selection_row_score(row, score), row));
        }
    }
    for anchor in rows.iter().filter(|row| setup_focus_row_has_command_literal_and_path(row)) {
        let forward_limit = anchor.chunk_index.saturating_add(SETUP_FOCUS_DOCUMENT_FORWARD_CHUNKS);
        for row in rows
            .iter()
            .filter(|row| row.chunk_index > anchor.chunk_index && row.chunk_index <= forward_limit)
        {
            if selected_chunk_ids.insert(row.chunk_id) {
                candidates.push((1, row));
            }
        }
    }
    candidates.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    candidates
        .into_iter()
        .take(SETUP_FOCUS_DOCUMENT_CHUNK_CAP)
        .map(|(_, row)| row.clone())
        .collect()
}

fn setup_focus_selection_row_score(row: &KnowledgeChunkRow, base_score: usize) -> usize {
    if setup_focus_row_has_command_literal_and_path(row) {
        base_score.saturating_add(RAW_SETUP_FOCUS_STRUCTURAL_DOCUMENT_SCORE_FLOOR.saturating_mul(4))
    } else {
        base_score
    }
}

fn setup_focus_document_candidate_score(rows: &[KnowledgeChunkRow], query_ir: &QueryIR) -> usize {
    let literal_terms = query_ir
        .literal_constraints
        .iter()
        .map(|literal| literal.text.trim().to_ascii_lowercase())
        .filter(|literal| !literal.is_empty())
        .collect::<Vec<_>>();
    let mut score = 0usize;
    for row in rows {
        score = score.saturating_add(setup_focus_row_score(row));
        let text = setup_focus_row_text(row).to_ascii_lowercase();
        score = score.saturating_add(
            literal_terms
                .iter()
                .filter(|literal| text.contains(literal.as_str()))
                .count()
                .saturating_mul(64),
        );
    }
    score.saturating_add(rows.len())
}

fn setup_focus_document_label_identity_score(
    document: &KnowledgeDocumentRow,
    query_ir: &QueryIR,
) -> usize {
    let target_sequences = setup_focus_query_label_identity_sequences(query_ir);
    if target_sequences.is_empty() {
        return 0;
    }
    let mut score = 0usize;
    let mut seen_sequences = BTreeSet::<Vec<String>>::new();
    for value in setup_focus_document_identity_values(document) {
        let value_sequence = normalized_alnum_token_sequence(&value, 1);
        if value_sequence.is_empty() {
            continue;
        }
        for target_sequence in &target_sequences {
            if token_sequence_contains_tokens(&value_sequence, target_sequence)
                && seen_sequences.insert(target_sequence.clone())
            {
                score = score.saturating_add(target_sequence.len().max(1));
            }
        }
    }
    score
}

fn setup_focus_query_label_identity_sequences(query_ir: &QueryIR) -> Vec<Vec<String>> {
    let mut sequences = Vec::new();
    let mut seen = BTreeSet::<Vec<String>>::new();
    if let Some(document_focus) = query_ir.document_focus.as_ref() {
        push_setup_focus_label_identity_sequence(&mut sequences, &mut seen, &document_focus.hint);
    }
    for entity in &query_ir.target_entities {
        push_setup_focus_label_identity_sequence(&mut sequences, &mut seen, &entity.label);
    }
    sequences
}

fn push_setup_focus_label_identity_sequence(
    sequences: &mut Vec<Vec<String>>,
    seen: &mut BTreeSet<Vec<String>>,
    value: &str,
) {
    let sequence = normalized_alnum_token_sequence(value, 1);
    if !sequence.is_empty() && seen.insert(sequence.clone()) {
        sequences.push(sequence);
    }
}

fn setup_focus_row_has_command_literal_and_path(row: &KnowledgeChunkRow) -> bool {
    let text = setup_focus_row_text(row);
    let command_literal_count = extract_package_command_literals(&text, 2).len();
    let configuration_path_count = setup_focus_configuration_path_count(&text);
    let setup_signal_count = command_literal_count
        .saturating_add(setup_focus_parameter_assignment_count(&text))
        .saturating_add(setup_focus_parameter_literal_count(&text))
        .saturating_add(setup_focus_section_header_count(&text))
        .saturating_add(setup_focus_url_count(&text));
    configuration_path_count > 0 && setup_signal_count > 0
}

fn setup_focus_row_score(row: &KnowledgeChunkRow) -> usize {
    let text = setup_focus_row_text(row);
    let command_literal_count = extract_package_command_literals(&text, 2).len();
    let configuration_path_count = setup_focus_configuration_path_count(&text);
    let assignment_count = setup_focus_parameter_assignment_count(&text);
    let parameter_literal_count = setup_focus_parameter_literal_count(&text);
    let section_count = setup_focus_section_header_count(&text);
    let url_count = setup_focus_url_count(&text);
    command_literal_count
        .saturating_mul(8)
        .saturating_add(configuration_path_count.saturating_mul(12))
        .saturating_add(assignment_count.saturating_mul(4))
        .saturating_add(parameter_literal_count.saturating_mul(6))
        .saturating_add(section_count.saturating_mul(3))
        .saturating_add(url_count.saturating_mul(4))
}

fn setup_focus_row_text(row: &KnowledgeChunkRow) -> String {
    format!("{}\n{}", row.content_text, row.window_text.as_deref().unwrap_or_default())
}

fn setup_focus_document_chunk_score(chunk: &RuntimeMatchedChunk) -> f32 {
    let command_literal_count =
        extract_package_command_literals(&chunk.source_text, 2).len() as f32;
    let path_count = setup_focus_configuration_path_count(&chunk.source_text) as f32;
    let assignment_count = setup_focus_parameter_assignment_count(&chunk.source_text) as f32;
    let parameter_literal_count = setup_focus_parameter_literal_count(&chunk.source_text) as f32;
    let section_count = setup_focus_section_header_count(&chunk.source_text) as f32;
    let url_count = setup_focus_url_count(&chunk.source_text) as f32;
    SETUP_FOCUS_DOCUMENT_SCORE_BASE
        + command_literal_count * 1_000.0
        + path_count * 500.0
        + assignment_count * 100.0
        + parameter_literal_count * 250.0
        + section_count * 50.0
        + url_count * 100.0
        - chunk.chunk_index.max(0) as f32
}

async fn load_linked_anchor_context_chunks(
    state: &AppState,
    library_id: Uuid,
    question: &str,
    query_ir: Option<&QueryIR>,
    chunks: &[RuntimeMatchedChunk],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    temporal_start: Option<DateTime<Utc>>,
    temporal_end: Option<DateTime<Utc>>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let anchor_queries = linked_anchor_focus_queries(question, query_ir, plan_keywords, chunks);
    if anchor_queries.is_empty() {
        return Ok(Vec::new());
    }

    let per_query_futures = anchor_queries.iter().cloned().map(|anchor_query| async move {
        state
            .search_store
            .search_chunks(
                library_id,
                &anchor_query,
                LINKED_ANCHOR_CONTEXT_CHUNKS_PER_QUERY,
                temporal_start,
                temporal_end,
            )
            .await
            .map(|rows| {
                rows.into_iter().map(|row| (row.chunk_id, row.score as f32)).collect::<Vec<_>>()
            })
            .with_context(|| format!("failed to run linked-anchor chunk search: {anchor_query}"))
    });
    let per_query_results: Vec<Result<Vec<_>, anyhow::Error>> = join_all(per_query_futures).await;
    let hits = combine_query_ir_focus_search_results(per_query_results, anchor_queries.len())?;
    if hits.is_empty() {
        return Ok(Vec::new());
    }

    // Linked anchors are explicit cross-document affordances in the retrieved source text.
    // Hydrate them library-wide instead of applying the current scoped-document filter;
    // otherwise a focused source document can link to the exact answer document and we
    // would pay the search cost only to discard that linked evidence.
    let linked_anchor_target_filter = linked_anchor_hydration_target_filter();
    let mut chunks = batch_hydrate_hits(
        state,
        hits,
        document_index,
        plan_keywords,
        &linked_anchor_target_filter,
    )
    .await
    .context("failed to hydrate linked-anchor chunks")?;
    for chunk in &mut chunks {
        chunk.score_kind = RuntimeChunkScoreKind::SourceContext;
    }
    tracing::info!(
        stage = "retrieval.linked_anchor_context",
        anchor_query_count = anchor_queries.len(),
        anchor_chunk_count = chunks.len(),
        "linked anchor context chunks loaded from retrieved source links",
    );
    Ok(chunks)
}

pub(crate) fn linked_anchor_hydration_target_filter() -> BTreeSet<Uuid> {
    BTreeSet::new()
}

async fn load_artifact_sibling_source_chunks(
    state: &AppState,
    chunks: &[RuntimeMatchedChunk],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let candidate_document_ids = artifact_sibling_source_document_ids(
        chunks,
        document_index,
        ARTIFACT_SIBLING_SOURCE_DOCUMENT_CAP,
    );
    if candidate_document_ids.is_empty() {
        return Ok(Vec::new());
    }

    let anchor_document_ids = chunks.iter().map(|chunk| chunk.document_id).collect::<BTreeSet<_>>();
    let structural_parent_document_ids = artifact_sibling_source_parent_page_document_ids(
        chunks,
        document_index,
        &anchor_document_ids,
        ARTIFACT_SIBLING_SOURCE_DOCUMENT_CAP,
    )
    .into_iter()
    .collect::<BTreeSet<_>>();
    let focus_terms = artifact_sibling_source_focus_terms(plan_keywords, chunks);
    let mut selected = Vec::new();
    for (document_rank, document_id) in candidate_document_ids.iter().enumerate() {
        let Some(document) = document_index.get(document_id) else {
            continue;
        };
        let Some(revision_id) = canonical_document_revision_id(document) else {
            continue;
        };
        let include_revision_history = structural_parent_document_ids.contains(document_id);
        let rows = load_artifact_sibling_source_rows_for_document(
            state,
            document,
            revision_id,
            include_revision_history,
            &focus_terms,
        )
        .await?;
        for (chunk_rank, row) in rows.into_iter().enumerate() {
            let score = artifact_sibling_source_chunk_score(document_rank, chunk_rank);
            if let Some(mut chunk) = map_chunk_hit(row, score, document_index, plan_keywords) {
                chunk.score = Some(score);
                chunk.score_kind = RuntimeChunkScoreKind::SourceContext;
                selected.push(chunk);
            }
        }
    }
    if !selected.is_empty() {
        tracing::info!(
            stage = "retrieval.artifact_sibling_source",
            sibling_document_count = candidate_document_ids.len(),
            structural_parent_document_count = structural_parent_document_ids.len(),
            sibling_chunk_count = selected.len(),
            "artifact sibling source chunks loaded from structurally related documents",
        );
    }
    Ok(selected)
}

async fn load_artifact_sibling_source_rows_for_document(
    state: &AppState,
    document: &KnowledgeDocumentRow,
    canonical_revision_id: Uuid,
    include_revision_history: bool,
    focus_terms: &[String],
) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
    let revision_ids = if include_revision_history {
        let revisions = state
            .document_store
            .list_revisions_by_document(document.document_id)
            .await
            .with_context(|| {
                format!(
                    "failed to list artifact sibling source revisions for document {}",
                    document.document_id
                )
            })?;
        artifact_sibling_source_revision_ids(
            document,
            canonical_revision_id,
            &revisions,
            ARTIFACT_SIBLING_SOURCE_REVISION_CAP,
        )
    } else {
        vec![canonical_revision_id]
    };
    if revision_ids.is_empty() {
        return Ok(Vec::new());
    }

    let chunk_cap = if include_revision_history {
        ARTIFACT_SIBLING_SOURCE_STRUCTURAL_CHUNKS_PER_DOCUMENT
    } else {
        ARTIFACT_SIBLING_SOURCE_CHUNKS_PER_DOCUMENT.max(0) as usize
    };
    let mut rows = Vec::new();
    let mut seen = BTreeSet::new();

    if include_revision_history {
        let windows =
            revision_ids.iter().map(|revision_id| (*revision_id, 0, 0)).collect::<Vec<_>>();
        for row in
            state.document_store.list_chunks_by_revisions_windows(&windows).await.with_context(
                || {
                    format!(
                        "failed to load artifact sibling source head chunks for document {}",
                        document.document_id
                    )
                },
            )?
        {
            if seen.insert(row.chunk_id) {
                rows.push(row);
            }
            if rows.len() >= chunk_cap {
                return Ok(rows);
            }
        }
    }

    if !focus_terms.is_empty() && rows.len() < chunk_cap {
        for row in state
            .document_store
            .list_chunks_by_revisions_matching_terms(&revision_ids, focus_terms, chunk_cap)
            .await
            .with_context(|| {
                format!(
                    "failed to load artifact sibling source focused chunks for document {}",
                    document.document_id
                )
            })?
        {
            if seen.insert(row.chunk_id) {
                rows.push(row);
            }
            if rows.len() >= chunk_cap {
                return Ok(rows);
            }
        }
    }

    if rows.len() < chunk_cap {
        let fallback_max_index = ARTIFACT_SIBLING_SOURCE_CHUNKS_PER_DOCUMENT.saturating_sub(1);
        for row in state
            .document_store
            .list_chunks_by_revision_range(canonical_revision_id, 0, fallback_max_index)
            .await
            .with_context(|| {
                format!(
                    "failed to load artifact sibling source fallback chunks for document {} revision {}",
                    document.document_id, canonical_revision_id
                )
            })?
        {
            if seen.insert(row.chunk_id) {
                rows.push(row);
            }
            if rows.len() >= chunk_cap {
                break;
            }
        }
    }

    Ok(rows)
}

fn artifact_sibling_source_revision_ids(
    document: &KnowledgeDocumentRow,
    canonical_revision_id: Uuid,
    revisions: &[KnowledgeRevisionRow],
    limit: usize,
) -> Vec<Uuid> {
    if limit == 0 {
        return Vec::new();
    }
    let mut revision_ids = Vec::new();
    let mut seen = BTreeSet::new();
    if seen.insert(canonical_revision_id) {
        revision_ids.push(canonical_revision_id);
    }
    for revision in revisions {
        if revision_ids.len() >= limit {
            break;
        }
        if revision.document_id != document.document_id
            || !revision_text_state_is_readable(&revision.text_state)
            || !seen.insert(revision.revision_id)
        {
            continue;
        }
        revision_ids.push(revision.revision_id);
    }
    revision_ids
}

fn artifact_sibling_source_focus_terms(
    plan_keywords: &[String],
    chunks: &[RuntimeMatchedChunk],
) -> Vec<String> {
    let mut terms = Vec::new();
    let mut seen = BTreeSet::new();
    for keyword in plan_keywords {
        push_artifact_sibling_source_focus_term(keyword, &mut seen, &mut terms);
        for token in normalized_alnum_token_sequence(keyword, 3) {
            push_artifact_sibling_source_focus_term(&token, &mut seen, &mut terms);
        }
        if terms.len() >= ARTIFACT_SIBLING_SOURCE_FOCUS_TERM_CAP {
            return terms;
        }
    }
    for chunk in chunks {
        push_artifact_sibling_source_focus_term(&chunk.document_label, &mut seen, &mut terms);
        for token in normalized_alnum_token_sequence(&chunk.document_label, 3) {
            push_artifact_sibling_source_focus_term(&token, &mut seen, &mut terms);
        }
        if terms.len() >= ARTIFACT_SIBLING_SOURCE_FOCUS_TERM_CAP {
            return terms;
        }
    }
    terms
}

fn push_artifact_sibling_source_focus_term(
    value: &str,
    seen: &mut BTreeSet<String>,
    terms: &mut Vec<String>,
) {
    if terms.len() >= ARTIFACT_SIBLING_SOURCE_FOCUS_TERM_CAP {
        return;
    }
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().filter(|ch| ch.is_alphanumeric()).count() < 3 {
        return;
    }
    let key = normalized.to_lowercase();
    if seen.insert(key) {
        terms.push(normalized);
    }
}

fn artifact_sibling_source_document_ids(
    chunks: &[RuntimeMatchedChunk],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    limit: usize,
) -> Vec<Uuid> {
    if chunks.is_empty() || limit == 0 {
        return Vec::new();
    }
    let anchor_document_ids = chunks.iter().map(|chunk| chunk.document_id).collect::<BTreeSet<_>>();
    let mut selected = Vec::<Uuid>::new();
    let mut seen = BTreeSet::<Uuid>::new();
    for document_id in artifact_sibling_source_parent_page_document_ids(
        chunks,
        document_index,
        &anchor_document_ids,
        limit,
    ) {
        if seen.insert(document_id) {
            selected.push(document_id);
        }
        if selected.len() >= limit {
            return selected;
        }
    }

    let anchors = artifact_sibling_source_anchor_tokens(chunks, document_index);
    if anchors.is_empty() {
        return selected;
    }
    let mut candidates = document_index
        .values()
        .filter(|document| !anchor_document_ids.contains(&document.document_id))
        .filter(|document| !seen.contains(&document.document_id))
        .filter(|document| !setup_focus_document_is_standalone_image(document))
        .filter_map(|document| {
            let score = artifact_sibling_source_document_score(&anchors, document);
            (score > 0).then_some((score, document.document_id))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|(left_score, left_id), (right_score, right_id)| {
        right_score.cmp(left_score).then_with(|| left_id.cmp(right_id))
    });
    for (_, document_id) in candidates {
        if seen.insert(document_id) {
            selected.push(document_id);
        }
        if selected.len() >= limit {
            break;
        }
    }
    selected
}

fn artifact_sibling_source_parent_page_document_ids(
    chunks: &[RuntimeMatchedChunk],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    anchor_document_ids: &BTreeSet<Uuid>,
    limit: usize,
) -> Vec<Uuid> {
    let page_ids = artifact_sibling_source_attachment_page_ids(chunks, document_index);
    if page_ids.is_empty() {
        return Vec::new();
    }
    let mut candidates = document_index
        .values()
        .filter(|document| !anchor_document_ids.contains(&document.document_id))
        .filter(|document| !setup_focus_document_is_standalone_image(document))
        .filter_map(|document| {
            let score = artifact_sibling_source_parent_page_score(&page_ids, document);
            (score > 0).then_some((score, document.document_id))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|(left_score, left_id), (right_score, right_id)| {
        right_score.cmp(left_score).then_with(|| left_id.cmp(right_id))
    });
    candidates.into_iter().map(|(_, document_id)| document_id).take(limit).collect()
}

fn artifact_sibling_source_attachment_page_ids(
    chunks: &[RuntimeMatchedChunk],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> BTreeSet<String> {
    chunks
        .iter()
        .filter_map(|chunk| document_index.get(&chunk.document_id))
        .flat_map(document_structural_source_values)
        .filter_map(attachment_parent_page_id)
        .collect()
}

fn artifact_sibling_source_parent_page_score(
    page_ids: &BTreeSet<String>,
    document: &KnowledgeDocumentRow,
) -> usize {
    document_structural_source_values(document)
        .into_iter()
        .filter_map(source_page_id)
        .filter(|page_id| page_ids.contains(page_id))
        .map(|page_id| 10_000usize.saturating_add(page_id.len()))
        .max()
        .unwrap_or(0)
}

fn artifact_sibling_source_anchor_tokens(
    chunks: &[RuntimeMatchedChunk],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> Vec<BTreeSet<String>> {
    let mut anchors = Vec::new();
    let mut seen = BTreeSet::<Vec<String>>::new();
    for chunk in chunks {
        let Some(document) = document_index.get(&chunk.document_id) else {
            continue;
        };
        if !setup_focus_document_is_standalone_image(document) {
            continue;
        }
        for value in setup_focus_document_identity_values(document) {
            for prefix in artifact_sibling_identity_prefixes(&value) {
                let tokens = normalized_alnum_tokens(&prefix, 3);
                if tokens.len() < 2 {
                    continue;
                }
                let token_vec = tokens.iter().cloned().collect::<Vec<_>>();
                if seen.insert(token_vec) {
                    anchors.push(tokens);
                }
            }
        }
    }
    anchors
}

fn document_structural_source_values(document: &KnowledgeDocumentRow) -> Vec<&str> {
    let mut values = Vec::new();
    values.push(document.external_key.as_str());
    if let Some(file_name) = document.file_name.as_deref() {
        values.push(file_name);
    }
    if let Some(source_uri) = document.source_uri.as_deref() {
        values.push(source_uri);
    }
    if let Some(document_hint) = document.document_hint.as_deref() {
        values.push(document_hint);
    }
    values
}

fn artifact_sibling_identity_prefixes(value: &str) -> Vec<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let mut prefixes = Vec::new();
    prefixes.push(trimmed.to_string());
    for separator in [":", " - ", " – ", " — "] {
        if let Some((prefix, _)) = trimmed.split_once(separator) {
            let prefix = prefix.trim();
            if !prefix.is_empty() {
                prefixes.push(prefix.to_string());
            }
        }
    }
    prefixes
}

fn artifact_sibling_source_document_score(
    anchors: &[BTreeSet<String>],
    document: &KnowledgeDocumentRow,
) -> usize {
    setup_focus_document_identity_values(document)
        .into_iter()
        .map(|value| {
            let normalized_value = normalize_document_identity_value(&value);
            let value_tokens = normalized_alnum_tokens(&value, 3);
            anchors
                .iter()
                .map(|anchor_tokens| {
                    let overlap = near_token_overlap_count(anchor_tokens, &value_tokens);
                    if overlap < 2 {
                        return 0;
                    }
                    let normalized_anchor =
                        anchor_tokens.iter().cloned().collect::<Vec<_>>().join(" ");
                    let prefix_score =
                        common_prefix_char_count(&normalized_value, &normalized_anchor);
                    overlap
                        .saturating_mul(64)
                        .saturating_add(prefix_score.min(48))
                        .saturating_add(value_tokens.len().min(16))
                })
                .max()
                .unwrap_or(0)
        })
        .max()
        .unwrap_or(0)
}

fn artifact_sibling_source_chunk_score(document_rank: usize, chunk_rank: usize) -> f32 {
    ARTIFACT_SIBLING_SOURCE_SCORE_BASE
        - (document_rank as f32 * ARTIFACT_SIBLING_SOURCE_SCORE_STEP * 16.0)
        - (chunk_rank as f32 * ARTIFACT_SIBLING_SOURCE_SCORE_STEP)
}

/// Wrap a companion chunk-loader future so it emits a `lane`-kind turn span
/// (elapsed + returned chunk count) into the active inspector sink. The spans
/// surface which retrieval companion lane was heavy on a given turn without
/// changing the loader's own return contract.
async fn timed_lane<Fut>(name: &'static str, fut: Fut) -> anyhow::Result<Vec<RuntimeMatchedChunk>>
where
    Fut: std::future::Future<Output = anyhow::Result<Vec<RuntimeMatchedChunk>>>,
{
    let started = std::time::Instant::now();
    let result = fut.await;
    crate::services::query::turn_spans::record_span(
        name,
        "lane",
        started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        None,
        result.as_ref().ok().map(|chunks| chunks.len() as u64),
    );
    result
}

pub(crate) async fn retrieve_document_chunks(
    state: &AppState,
    library_id: Uuid,
    provider_profile: &EffectiveProviderProfile,
    question: &str,
    forced_target_document_ids: Option<&BTreeSet<Uuid>>,
    plan: &RuntimeQueryPlan,
    limit: usize,
    question_embedding: &[f32],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    query_ir: Option<&QueryIR>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    // Load the library's retrieval config to derive the lexical text-search
    // config for the Postgres FTS lane. Falls back to the default ("simple")
    // when the row is not found or the JSON cannot be deserialized, so that
    // the caller is never blocked by a missing library.
    let text_search_config: String = {
        let row = repositories::catalog_repository::get_library_by_id(
            &state.persistence.postgres,
            library_id,
        )
        .await
        .context("failed to load library retrieval config for chunk search")?;
        row.and_then(|r| {
            crate::domains::retrieval::RetrievalConfig::from_json(r.retrieval_config)
                .ok()
                .map(|c| c.lexical.text_search_config)
        })
        .unwrap_or_else(|| crate::domains::retrieval::DEFAULT_TEXT_SEARCH_CONFIG.to_string())
    };
    let forced_pin = forced_target_document_ids.is_some_and(|ids| !ids.is_empty());
    let targeted_document_ids = forced_target_document_ids
        .filter(|ids| !ids.is_empty())
        .cloned()
        .unwrap_or_else(|| resolve_scoped_target_document_ids(question, query_ir, document_index));
    // P3 coverage fallback is armed only for a *compiler-inferred*
    // single-document focus pin. An explicit document reference (forced
    // target, or a title/filename the user literally named) is an intent
    // signal we must honour even when the doc is thin — broadening it would
    // answer "tell me about doc X" from the whole library. An inferred pin,
    // by contrast, can hard-lock onto a thin same-titled stub and exclude
    // the real procedure docs; if it comes back near-empty we re-broaden.
    let pin_is_inferred = !forced_pin
        && !targeted_document_ids.is_empty()
        && query_ir.is_some_and(query_ir_focus_pin_is_inferred);
    retrieve_document_chunks_with_targets(
        state,
        library_id,
        provider_profile,
        question,
        targeted_document_ids,
        plan,
        limit,
        question_embedding,
        document_index,
        query_ir,
        pin_is_inferred,
        &text_search_config,
    )
    .await
}

/// `true` when the single-document focus pin was derived from a
/// compiler-inferred `document_focus` hint rather than from an explicit
/// document reference the user named verbatim. Only inferred pins are
/// eligible for the P3 coverage-broaden fallback.
fn query_ir_focus_pin_is_inferred(ir: &QueryIR) -> bool {
    matches!(ir.scope, QueryScope::SingleDocument)
        && ir.document_focus.is_some()
        && !ir.is_follow_up()
}

/// Decide whether a focus-locked retrieval came back too thin to trust and
/// the pin should be dropped for a single broad re-retrieval.
///
/// Fires only when the pin was compiler-inferred AND the narrowed retrieval
/// is at or below a small coverage floor — a genuinely thin stub, not merely
/// a small-but-sufficient document. Bounding it to near-empty results keeps
/// the fallback to at most one extra retrieval inside the 30s tool-call SLO.
pub(crate) fn should_broaden_focus(pin_is_inferred: bool, post_filter_chunk_count: usize) -> bool {
    pin_is_inferred && post_filter_chunk_count <= FOCUS_BROADEN_MIN_CHUNKS
}

#[cfg(test)]
mod focus_broaden_decision_tests {
    use super::{FOCUS_BROADEN_MIN_CHUNKS, should_broaden_focus};

    #[test]
    fn inferred_pin_broadens_only_when_coverage_is_at_or_below_floor() {
        // A thin inferred-focus pin (e.g. a 2-chunk same-titled stub) broadens.
        assert!(should_broaden_focus(true, 0));
        assert!(should_broaden_focus(true, FOCUS_BROADEN_MIN_CHUNKS));
        // A focused document with sufficient coverage keeps its pin.
        assert!(!should_broaden_focus(true, FOCUS_BROADEN_MIN_CHUNKS + 1));
    }

    #[test]
    fn non_inferred_pin_never_broadens() {
        // A user-referenced (non-inferred) pin is honored even when thin.
        assert!(!should_broaden_focus(false, 0));
        assert!(!should_broaden_focus(false, FOCUS_BROADEN_MIN_CHUNKS));
    }
}

#[allow(clippy::too_many_arguments)]
async fn retrieve_document_chunks_with_targets(
    state: &AppState,
    library_id: Uuid,
    provider_profile: &EffectiveProviderProfile,
    question: &str,
    targeted_document_ids: BTreeSet<Uuid>,
    plan: &RuntimeQueryPlan,
    limit: usize,
    question_embedding: &[f32],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    query_ir: Option<&QueryIR>,
    allow_broaden: bool,
    text_search_config: &str,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let initial_table_row_count = requested_initial_table_row_count(query_ir);
    let targeted_table_aggregation =
        question_asks_table_aggregation(question, query_ir) && !targeted_document_ids.is_empty();
    let query_ir_focus_queries = query_ir.map(query_ir_lexical_focus_queries).unwrap_or_default();
    let lexical_queries = build_lexical_queries(question, plan, &query_ir_focus_queries, query_ir);
    let lexical_limit = limit.saturating_mul(2).max(24);
    let plan_keywords = &plan.keywords;
    let targeted_document_ids_ref = &targeted_document_ids;
    // Resolved temporal bounds — applied as a hard filter on every
    // chunk-touching search lane. None when QueryIR has no temporal
    // constraints or none parsed as RFC3339.
    let (temporal_start, temporal_end) =
        query_ir.map_or((None, None), |ir| ir.resolved_temporal_bounds());

    let vector_future = async {
        let started = std::time::Instant::now();
        if question_embedding.is_empty() {
            tracing::info!(
                stage = "retrieval.vector_skip",
                reason = "question_embedding_empty",
                "vector retrieve skipped: no question embedding"
            );
            return Ok::<(Vec<RuntimeMatchedChunk>, u128), anyhow::Error>((Vec::new(), 0));
        }
        let context =
            resolve_runtime_vector_search_context(state, library_id, provider_profile).await?;
        let Some(context) = context else {
            tracing::info!(
                stage = "retrieval.vector_skip",
                reason = "no_vector_search_context",
                "vector retrieve skipped: resolve_runtime_vector_search_context returned None (missing EmbedChunk binding or no active vector generation)"
            );
            return Ok::<(Vec<RuntimeMatchedChunk>, u128), anyhow::Error>((Vec::new(), 0));
        };
        let _vector_guard = state.canonical_services.search.vector_plane_read_guard(state).await?;
        let library_dim = library_vector_index_dimensions(state, library_id).await?;
        validate_embedding_vector_dimensions(
            library_dim,
            question_embedding,
            "runtime chunk search",
        )?;
        let raw_hits = state
            .search_store
            .search_chunk_vectors_by_similarity(
                library_dim,
                library_id,
                &context.model_catalog_id.to_string(),
                question_embedding,
                limit.max(1),
                None,
                temporal_start,
                temporal_end,
            )
            .await
            .context("failed to search canonical chunk vectors for runtime query")?;
        tracing::info!(
            stage = "retrieval.vector_search",
            raw_hit_count = raw_hits.len(),
            embedding_dims = question_embedding.len(),
            limit = limit.max(1),
            "vector search returned raw hits"
        );
        // Batch-hydrate all hits in one `list_chunks_by_ids` call to avoid an
        // N+1 knowledge-store round-trip per vector match.
        let hits = batch_hydrate_hits(
            state,
            raw_hits.iter().map(|hit| (hit.chunk_id, hit.score as f32)).collect(),
            document_index,
            plan_keywords,
            targeted_document_ids_ref,
        )
        .await?;
        let elapsed = started.elapsed();
        crate::services::query::turn_spans::record_span(
            "retrieve.vector",
            "lane",
            elapsed.as_millis().try_into().unwrap_or(u64::MAX),
            None,
            Some(hits.len() as u64),
        );
        Ok((hits, elapsed.as_millis()))
    };

    // Run lexical queries concurrently; the RRF merge below preserves output
    // order.
    let lexical_future = async {
        let started = std::time::Instant::now();
        let lexical_query_count = lexical_queries.len();
        // Fan lexical searches out in parallel — same as before — but
        // hydrate each query's hits through `batch_hydrate_hits` to
        // replace the per-hit `get_chunk` N+1 with a single
        // `list_chunks_by_ids` round-trip. With 4 lexical queries × ~20
        // hits each the old path fired ~80 serial chunk loads per
        // request; now it's at most 4 batched reads.
        let text_search_config_owned = text_search_config.to_owned();
        let per_query_futures = lexical_queries.into_iter().map(|lexical_query| {
            let text_search_config_owned = text_search_config_owned.clone();
            async move {
                let hits = state
                    .search_store
                    .search_chunks_with_config(
                        library_id,
                        &lexical_query,
                        lexical_limit,
                        temporal_start,
                        temporal_end,
                        &text_search_config_owned,
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "failed to run lexical chunk search for runtime query: {lexical_query}"
                        )
                    })?;
                batch_hydrate_hits(
                    state,
                    hits.into_iter().map(|hit| (hit.chunk_id, hit.score as f32)).collect(),
                    document_index,
                    plan_keywords,
                    targeted_document_ids_ref,
                )
                .await
            } // end async move
        });
        let per_query_results: Vec<Result<Vec<RuntimeMatchedChunk>, anyhow::Error>> =
            join_all(per_query_futures).await;
        let lexical_hits =
            combine_lexical_query_results(per_query_results, lexical_query_count, lexical_limit)?;
        let elapsed = started.elapsed();
        crate::services::query::turn_spans::record_span(
            "retrieve.lexical",
            "lane",
            elapsed.as_millis().try_into().unwrap_or(u64::MAX),
            None,
            Some(lexical_hits.len() as u64),
        );
        Ok::<(Vec<RuntimeMatchedChunk>, usize, u128), anyhow::Error>((
            lexical_hits,
            lexical_query_count,
            elapsed.as_millis(),
        ))
    };

    let (vector_result, lexical_result) = tokio::join!(vector_future, lexical_future);
    let lane_outcome = combine_chunk_retrieval_lanes(vector_result, lexical_result)?;
    tracing::info!(
        stage = "retrieval.chunks_fanout",
        vector_elapsed_ms = lane_outcome.vector_elapsed_ms,
        vector_hits = lane_outcome.vector_hits.len(),
        lexical_elapsed_ms = lane_outcome.lexical_elapsed_ms,
        lexical_query_count = lane_outcome.lexical_query_count,
        lexical_hits = lane_outcome.lexical_hits.len(),
        degraded_lane_count = lane_outcome.degraded_lane_count,
        "vector + lexical chunk fan-out"
    );
    let mut chunks = merge_chunks(
        lane_outcome.vector_hits,
        lane_outcome.lexical_hits,
        limit.max(initial_table_row_count.unwrap_or(0)),
    );
    let latest_version_selection =
        select_latest_version_documents(query_ir, question, document_index, &chunks);
    let latest_version_requested_count = latest_version_selection.requested_count;
    let latest_version_documents = latest_version_selection.documents;
    if latest_version_selection.inferred_from_retrieved_evidence {
        tracing::info!(
            stage = "retrieval.latest_version_evidence_fallback",
            latest_version_requested_count,
            latest_version_document_count = latest_version_documents.len(),
            "enabled latest-version lane from retrieved document evidence"
        );
    }
    let mut latest_version_document_ids =
        latest_version_scoped_document_ids(&latest_version_documents, &[]);
    // Fan out the four independent companion-chunk loaders. None of them
    // reads the running `chunks` accumulator; they all consume the same
    // inputs (state, query_ir, document_index, plan_keywords, targeted
    // document ids). Merges below stay sequential in their original order
    // so the per-source budget caps still apply correctly.
    let document_identity_future = timed_lane(
        "retrieve.document_identity",
        load_document_identity_chunks_for_targets(
            state,
            document_index,
            &targeted_document_ids,
            plan_keywords,
            query_ir,
        ),
    );
    let latest_version_future = timed_lane(
        "retrieve.latest_version",
        load_latest_version_document_chunks(
            state,
            document_index,
            plan_keywords,
            latest_version_requested_count,
            &latest_version_documents,
        ),
    );
    let latest_version_scope_terms = query_ir.map(latest_version_scope_terms).unwrap_or_default();
    let latest_version_has_explicit_source_tail =
        query_ir.and_then(|query_ir| query_ir.source_slice.as_ref()).is_some_and(|source_slice| {
            matches!(source_slice.direction, crate::domains::query_ir::SourceSliceDirection::Tail)
        });
    let latest_version_prefers_source_tail = latest_version_has_explicit_source_tail
        || query_ir.is_some_and(query_requests_latest_versions);
    let latest_version_allows_unscoped_density_fallback =
        latest_version_prefers_source_tail && !latest_version_has_explicit_source_tail;
    let latest_version_semantic_future = timed_lane(
        "retrieve.latest_version_semantic",
        load_latest_version_semantic_document_chunks(
            state,
            document_index,
            plan_keywords,
            latest_version_requested_count,
            &latest_version_scope_terms,
            query_ir.is_some_and(query_requests_latest_versions),
            latest_version_prefers_source_tail,
            latest_version_allows_unscoped_density_fallback,
        ),
    );
    let entity_bio_future = timed_lane(
        "retrieve.entity_bio",
        load_entity_bio_chunks(
            state,
            library_id,
            query_ir,
            document_index,
            plan_keywords,
            &targeted_document_ids,
        ),
    );
    let query_ir_focus_future = timed_lane(
        "retrieve.query_ir_focus",
        load_query_ir_focus_chunks(
            state,
            library_id,
            question,
            &query_ir_focus_queries,
            &targeted_document_ids,
            document_index,
            plan_keywords,
            temporal_start,
            temporal_end,
        ),
    );
    let content_anchor_future = timed_lane(
        "retrieve.content_anchor",
        load_content_anchor_chunks(
            state,
            question,
            query_ir,
            &targeted_document_ids,
            document_index,
            plan_keywords,
            temporal_start,
            temporal_end,
        ),
    );
    let document_evidence_anchor_future = timed_lane(
        "retrieve.document_evidence_anchor",
        load_document_evidence_anchor_chunks(
            state,
            question,
            query_ir,
            document_index,
            plan_keywords,
            temporal_start,
            temporal_end,
        ),
    );
    let versioned_update_procedure_future = timed_lane(
        "retrieve.versioned_update_procedure",
        load_versioned_update_procedure_chunks(
            state,
            library_id,
            question,
            query_ir,
            document_index,
            plan_keywords,
            temporal_start,
            temporal_end,
        ),
    );
    let setup_focus_document_future = timed_lane(
        "retrieve.setup_focus",
        load_setup_focus_document_chunks(
            state,
            question,
            query_ir,
            document_index,
            plan_keywords,
            temporal_start,
            temporal_end,
        ),
    );
    let setup_variant_document_future = timed_lane(
        "retrieve.setup_variant",
        load_setup_variant_document_chunks(
            state,
            question,
            query_ir,
            document_index,
            plan_keywords,
            temporal_start,
            temporal_end,
        ),
    );
    let (
        document_identity_result,
        latest_version_result,
        latest_version_semantic_result,
        entity_bio_result,
        query_ir_focus_chunks_result,
        content_anchor_chunks_result,
        document_evidence_anchor_chunks_result,
        versioned_update_procedure_chunks_result,
        setup_focus_document_chunks_result,
        setup_variant_document_chunks_result,
    ) = tokio::join!(
        document_identity_future,
        latest_version_future,
        latest_version_semantic_future,
        entity_bio_future,
        query_ir_focus_future,
        content_anchor_future,
        document_evidence_anchor_future,
        versioned_update_procedure_future,
        setup_focus_document_future,
        setup_variant_document_future,
    );
    let document_identity_chunks = document_identity_result?;
    if !document_identity_chunks.is_empty() {
        let identity_budget_per_document =
            DOCUMENT_IDENTITY_CHUNKS_PER_DOCUMENT + DOCUMENT_IDENTITY_FOCUSED_CHUNKS_PER_DOCUMENT;
        chunks = merge_chunks(
            chunks,
            document_identity_chunks,
            limit
                .max(initial_table_row_count.unwrap_or(0))
                .saturating_add(targeted_document_ids.len() * identity_budget_per_document),
        );
    }
    let mut latest_version_chunks = latest_version_result?;
    let latest_version_semantic_chunks = latest_version_semantic_result?;
    latest_version_document_ids
        .extend(latest_version_scoped_document_ids(&[], &latest_version_semantic_chunks));
    if !latest_version_semantic_chunks.is_empty() {
        let merged_limit = query_ir.map_or(limit, |ir| latest_version_context_top_k(ir, limit));
        latest_version_chunks =
            merge_chunks(latest_version_chunks, latest_version_semantic_chunks, merged_limit);
    }
    if !latest_version_chunks.is_empty() {
        let latest_version_context_budget =
            if latest_version_selection.inferred_from_retrieved_evidence {
                latest_version_requested_count.saturating_mul(LATEST_VERSION_CHUNKS_PER_DOCUMENT)
            } else {
                // query_ir is always Some when explicit latest-version chunks are
                // non-empty (guarded by select_latest_version_documents).
                #[allow(clippy::expect_used)]
                latest_version_context_top_k(query_ir.expect("latest chunks require QueryIR"), 0)
            };
        let latest_version_context_limit = limit
            .max(initial_table_row_count.unwrap_or(0))
            .saturating_add(latest_version_context_budget);
        chunks = merge_chunks(chunks, latest_version_chunks, latest_version_context_limit);
    }
    let entity_bio_chunks = entity_bio_result?;
    if !entity_bio_chunks.is_empty() {
        // Cap at limit + the bio budget so entity-bio hits are additive
        // rather than pushing other high-score chunks off the top-K.
        let merged_limit = limit.saturating_add(ENTITY_BIO_CHUNK_CAP);
        chunks = merge_entity_bio_chunks(chunks, entity_bio_chunks, merged_limit);
    }
    match query_ir_focus_chunks_result {
        Ok(query_ir_focus_chunks) if !query_ir_focus_chunks.is_empty() => {
            chunks = merge_query_ir_focus_chunks_for_query(
                chunks,
                query_ir_focus_chunks,
                query_ir_focus_context_top_k(limit),
                query_ir,
            );
        }
        Ok(_) => {}
        Err(error) if !chunks.is_empty() => {
            let summary = format!("{error:#}");
            tracing::warn!(
                stage = "retrieval.query_ir_focus_failed",
                error = %summary,
                retrieval_degraded = true,
                failed_source = "query_ir_focus",
                retained_chunk_count = chunks.len(),
                "query-IR focus retrieval failed; continuing with primary retrieved chunks"
            );
        }
        Err(error) => return Err(error),
    }
    let mut protected_document_ids = BTreeSet::new();
    match content_anchor_chunks_result {
        Ok(content_anchor_chunks) if !content_anchor_chunks.is_empty() => {
            protected_document_ids
                .extend(content_anchor_chunks.iter().map(|chunk| chunk.document_id));
            chunks = merge_query_ir_focus_chunks_for_query(
                chunks,
                content_anchor_chunks,
                query_ir_focus_context_top_k(limit).saturating_add(CONTENT_ANCHOR_CHUNK_CAP),
                query_ir,
            );
        }
        Ok(_) => {}
        Err(error) if !chunks.is_empty() => {
            let summary = format!("{error:#}");
            tracing::warn!(
                stage = "retrieval.content_anchor_failed",
                error = %summary,
                retrieval_degraded = true,
                failed_source = "content_anchor",
                retained_chunk_count = chunks.len(),
                "content-anchor retrieval failed; continuing with retrieved chunks"
            );
        }
        Err(error) => return Err(error),
    }
    match document_evidence_anchor_chunks_result {
        Ok(document_evidence_anchor_chunks) if !document_evidence_anchor_chunks.is_empty() => {
            protected_document_ids
                .extend(document_evidence_anchor_chunks.iter().map(|chunk| chunk.document_id));
            chunks = merge_query_ir_focus_chunks_for_query(
                chunks,
                document_evidence_anchor_chunks,
                query_ir_focus_context_top_k(limit),
                query_ir,
            );
        }
        Ok(_) => {}
        Err(error) if !chunks.is_empty() => {
            let summary = format!("{error:#}");
            tracing::warn!(
                stage = "retrieval.document_evidence_anchor_failed",
                error = %summary,
                retrieval_degraded = true,
                failed_source = "document_evidence_anchor",
                retained_chunk_count = chunks.len(),
                "document evidence anchor retrieval failed; continuing with retrieved chunks"
            );
        }
        Err(error) => return Err(error),
    }
    match versioned_update_procedure_chunks_result {
        Ok(versioned_update_procedure_chunks) if !versioned_update_procedure_chunks.is_empty() => {
            protected_document_ids
                .extend(versioned_update_procedure_chunks.iter().map(|chunk| chunk.document_id));
            chunks = merge_versioned_update_procedure_chunks_for_query(
                chunks,
                versioned_update_procedure_chunks,
                query_ir_focus_context_top_k(limit),
                query_ir,
            );
        }
        Ok(_) => {}
        Err(error) if !chunks.is_empty() => {
            let summary = format!("{error:#}");
            tracing::warn!(
                stage = "retrieval.versioned_update_procedure_failed",
                error = %summary,
                retrieval_degraded = true,
                failed_source = "versioned_update_procedure",
                retained_chunk_count = chunks.len(),
                "versioned update procedure evidence retrieval failed; continuing with retrieved chunks"
            );
        }
        Err(error) => return Err(error),
    }
    let post_merge_versioned_update_focus_terms =
        versioned_update_procedure_focus_terms(question, query_ir, plan_keywords);
    let post_merge_source_local_runbook_chunks_result =
        load_versioned_update_procedure_source_local_runbook_chunks(
            state,
            &chunks,
            document_index,
            &post_merge_versioned_update_focus_terms,
            question,
            query_ir,
        )
        .await;
    match post_merge_source_local_runbook_chunks_result {
        Ok(source_local_runbook_chunks) if !source_local_runbook_chunks.is_empty() => {
            protected_document_ids
                .extend(source_local_runbook_chunks.iter().map(|chunk| chunk.document_id));
            chunks = merge_versioned_update_procedure_chunks_for_query(
                chunks,
                source_local_runbook_chunks,
                query_ir_focus_context_top_k(limit),
                query_ir,
            );
        }
        Ok(_) => {}
        Err(error) if !chunks.is_empty() => {
            let summary = format!("{error:#}");
            tracing::warn!(
                stage = "retrieval.versioned_update_procedure_source_local_failed",
                error = %summary,
                retrieval_degraded = true,
                failed_source = "versioned_update_procedure_source_local",
                retained_chunk_count = chunks.len(),
                "source-local versioned update procedure evidence failed; continuing with retrieved chunks"
            );
        }
        Err(error) => return Err(error),
    }
    match setup_focus_document_chunks_result {
        Ok(mut setup_focus_document_chunks) if !setup_focus_document_chunks.is_empty() => {
            if query_ir.is_some_and(setup_focus_uses_raw_question_fallback) {
                let loaded_chunk_count = setup_focus_document_chunks.len();
                setup_focus_document_chunks = filter_raw_setup_focus_chunks_by_primary_context(
                    setup_focus_document_chunks,
                    &chunks,
                );
                if setup_focus_document_chunks.is_empty() {
                    tracing::info!(
                        stage = "retrieval.setup_focus_document_filtered",
                        loaded_chunk_count,
                        "raw setup-focused document chunks lacked primary retrieval support"
                    );
                }
            }
            protected_document_ids
                .extend(setup_focus_document_chunks.iter().map(|chunk| chunk.document_id));
            if !setup_focus_document_chunks.is_empty() {
                chunks = merge_query_ir_focus_chunks_for_query(
                    chunks,
                    setup_focus_document_chunks,
                    query_ir_focus_context_top_k(limit),
                    query_ir,
                );
            }
        }
        Ok(_) => {}
        Err(error) if !chunks.is_empty() => {
            let summary = format!("{error:#}");
            tracing::warn!(
                stage = "retrieval.setup_focus_document_failed",
                error = %summary,
                retrieval_degraded = true,
                failed_source = "setup_focus_document",
                retained_chunk_count = chunks.len(),
                "setup-focused document evidence failed; continuing with retrieved chunks"
            );
        }
        Err(error) => return Err(error),
    }
    match setup_variant_document_chunks_result {
        Ok(setup_variant_document_chunks) if !setup_variant_document_chunks.is_empty() => {
            protected_document_ids
                .extend(setup_variant_document_chunks.iter().map(|chunk| chunk.document_id));
            chunks = merge_query_ir_focus_chunks_for_query(
                chunks,
                setup_variant_document_chunks,
                query_ir_focus_context_top_k(limit).max(SETUP_VARIANT_CHUNK_CAP),
                query_ir,
            );
        }
        Ok(_) => {}
        Err(error) if !chunks.is_empty() => {
            let summary = format!("{error:#}");
            tracing::warn!(
                stage = "retrieval.setup_variant_failed",
                error = %summary,
                retrieval_degraded = true,
                failed_source = "setup_variant",
                retained_chunk_count = chunks.len(),
                "setup variant evidence failed; continuing with retrieved chunks"
            );
        }
        Err(error) => return Err(error),
    }
    let linked_anchor_context_chunks_result = load_linked_anchor_context_chunks(
        state,
        library_id,
        question,
        query_ir,
        &chunks,
        document_index,
        plan_keywords,
        temporal_start,
        temporal_end,
    )
    .await;
    match linked_anchor_context_chunks_result {
        Ok(linked_anchor_context_chunks) if !linked_anchor_context_chunks.is_empty() => {
            chunks = merge_query_ir_focus_chunks_for_query(
                chunks,
                linked_anchor_context_chunks,
                query_ir_focus_context_top_k(limit),
                query_ir,
            );
        }
        Ok(_) => {}
        Err(error) if !chunks.is_empty() => {
            let summary = format!("{error:#}");
            tracing::warn!(
                stage = "retrieval.linked_anchor_context_failed",
                error = %summary,
                retrieval_degraded = true,
                failed_source = "linked_anchor_context",
                retained_chunk_count = chunks.len(),
                "linked anchor context retrieval failed; continuing with primary retrieved chunks"
            );
        }
        Err(error) => return Err(error),
    }
    let artifact_sibling_source_chunks_result =
        load_artifact_sibling_source_chunks(state, &chunks, document_index, plan_keywords).await;
    match artifact_sibling_source_chunks_result {
        Ok(artifact_sibling_source_chunks) if !artifact_sibling_source_chunks.is_empty() => {
            chunks = merge_query_ir_focus_chunks_for_query(
                chunks,
                artifact_sibling_source_chunks,
                query_ir_focus_context_top_k(limit),
                query_ir,
            );
        }
        Ok(_) => {}
        Err(error) if !chunks.is_empty() => {
            let summary = format!("{error:#}");
            tracing::warn!(
                stage = "retrieval.artifact_sibling_source_failed",
                error = %summary,
                retrieval_degraded = true,
                failed_source = "artifact_sibling_source",
                retained_chunk_count = chunks.len(),
                "artifact sibling source retrieval failed; continuing with primary retrieved chunks"
            );
        }
        Err(error) => return Err(error),
    }
    // Diversify by document: cap at `MAX_CHUNKS_PER_DOCUMENT` chunks per
    // document_id in the final hit list. Without this, analyzer collisions
    // can let one document dominate the top results and squeeze out other
    // documents that carry the actual answer.
    let max_chunks_per_document = if targeted_document_ids.is_empty() {
        MAX_CHUNKS_PER_DOCUMENT
    } else {
        MAX_CHUNKS_PER_DOCUMENT.max(DOCUMENT_IDENTITY_CHUNKS_PER_DOCUMENT)
    };
    // Protected evidence is already globally bounded by its source-specific
    // loaders; keep that inventory intact so answer context does not lose the
    // document that the query identity resolved to before final truncation.
    chunks = diversify_chunks_by_document(chunks, max_chunks_per_document, &protected_document_ids);
    retain_scoped_documents(
        &mut chunks,
        &targeted_document_ids,
        &latest_version_document_ids,
        &protected_document_ids,
    );
    // Post-retrieval temporal hard-filter. The lexical and vector lanes
    // already FILTER on `occurred_at` at query time, but companion paths
    // (source_context focused/neighbor expansion, graph entity hydration,
    // RAPTOR / table summary loaders, query-IR focus chunks) bypass that
    // filter and pull chunks regardless of date. When the user explicitly
    // scopes a question to a window we drop any chunk whose underlying
    // `KnowledgeChunkRow.occurred_at` is null OR falls outside the bounds.
    // RuntimeMatchedChunk does not carry temporal data, so we re-query
    // `list_chunks_by_ids` once over the surviving set — single knowledge-store
    // read, no per-chunk lookup. Verified necessary on stage
    // 2026-05-03: image-OCR chunks (no occurred_at) were leaking into
    // "messages in March 2026" answers via source_context companions.
    if temporal_start.is_some() && temporal_end.is_some() && !chunks.is_empty() {
        let chunk_ids: Vec<Uuid> = chunks.iter().map(|c| c.chunk_id).collect();
        let rows = state
            .document_store
            .list_chunks_by_ids(&chunk_ids)
            .await
            .context("failed to look up chunks for temporal post-filter")?;
        let allowed: std::collections::HashSet<Uuid> = rows
            .into_iter()
            .filter(|row| {
                let Some(at) = row.occurred_at else {
                    return false;
                };
                if let Some(start) = temporal_start
                    && row.occurred_until.unwrap_or(at) < start
                {
                    return false;
                }
                if let Some(end) = temporal_end
                    && at >= end
                {
                    return false;
                }
                true
            })
            .map(|row| row.chunk_id)
            .collect();
        let before = chunks.len();
        chunks.retain(|chunk| allowed.contains(&chunk.chunk_id));
        tracing::info!(
            stage = "retrieval.temporal_post_filter",
            before,
            after = chunks.len(),
            "applied temporal hard-filter to companion-path chunks"
        );
    }
    if let Some(row_count) = initial_table_row_count {
        let initial_rows = load_initial_table_rows_for_documents(
            state,
            document_index,
            &targeted_document_ids,
            row_count,
            plan_keywords,
        )
        .await?;
        chunks = merge_chunks(chunks, initial_rows, limit.max(row_count));
    }
    if targeted_table_aggregation {
        let direct_summary_chunks = load_table_summary_chunks_for_documents(
            state,
            document_index,
            &targeted_document_ids,
            DIRECT_TABLE_AGGREGATION_SUMMARY_LIMIT,
            plan_keywords,
        )
        .await?;
        let direct_row_chunks = load_table_rows_for_documents(
            state,
            document_index,
            &targeted_document_ids,
            DIRECT_TABLE_AGGREGATION_ROW_LIMIT,
            plan_keywords,
        )
        .await?;
        chunks = merge_canonical_table_aggregation_chunks(
            chunks,
            direct_summary_chunks,
            direct_row_chunks,
            limit.max(DIRECT_TABLE_AGGREGATION_CHUNK_LIMIT),
        );
    }
    if query_ir.is_some_and(query_ir_requests_table_section_siblings) {
        let table_section_chunks = load_table_section_sibling_chunks(
            state,
            document_index,
            &chunks,
            TABLE_SECTION_SIBLING_LIMIT_PER_SECTION,
            plan_keywords,
        )
        .await?;
        if !table_section_chunks.is_empty() {
            chunks = merge_chunks(
                chunks,
                table_section_chunks,
                limit.max(TABLE_SECTION_SIBLING_CHUNK_LIMIT),
            );
        }
    }

    // P3 coverage fallback. A compiler-inferred single-document focus pin
    // can hard-lock onto a thin same-titled stub and exclude the real
    // procedure docs. When the narrowed retrieval comes back at/below the
    // coverage floor, drop the pin once and re-broaden to the whole library
    // so the answer step has material to work with. `allow_broaden` is false
    // on the recursive call, so this costs at most one extra retrieval and
    // respects the 30s tool-call SLO.
    if should_broaden_focus(allow_broaden, chunks.len()) {
        tracing::info!(
            stage = "retrieval.focus_broaden_fallback",
            library_id = %library_id,
            pinned_document_count = targeted_document_ids.len(),
            narrowed_chunk_count = chunks.len(),
            coverage_floor = FOCUS_BROADEN_MIN_CHUNKS,
            "inferred focus pin returned too few chunks; dropping pin and re-broadening retrieval"
        );
        return Box::pin(retrieve_document_chunks_with_targets(
            state,
            library_id,
            provider_profile,
            question,
            BTreeSet::new(),
            plan,
            limit,
            question_embedding,
            document_index,
            query_ir,
            false,
            text_search_config,
        ))
        .await;
    }

    Ok(chunks)
}

pub(crate) fn retain_scoped_documents(
    chunks: &mut Vec<RuntimeMatchedChunk>,
    targeted_document_ids: &BTreeSet<Uuid>,
    latest_version_document_ids: &BTreeSet<Uuid>,
    protected_document_ids: &BTreeSet<Uuid>,
) {
    let mut scoped_document_ids = targeted_document_ids.clone();
    scoped_document_ids.extend(latest_version_document_ids.iter().copied());
    scoped_document_ids.extend(protected_document_ids.iter().copied());
    if scoped_document_ids.is_empty() {
        return;
    }
    chunks.retain(|chunk| scoped_document_ids.contains(&chunk.document_id));
}

fn latest_version_scoped_document_ids(
    documents: &[LatestVersionDocument],
    semantic_chunks: &[RuntimeMatchedChunk],
) -> BTreeSet<Uuid> {
    let mut document_ids =
        documents.iter().map(|document| document.document_id).collect::<BTreeSet<_>>();
    document_ids.extend(
        semantic_chunks
            .iter()
            .filter(|chunk| chunk.score_kind == RuntimeChunkScoreKind::LatestVersion)
            .map(|chunk| chunk.document_id),
    );
    document_ids
}

fn combine_lexical_query_results(
    per_query_results: Vec<anyhow::Result<Vec<RuntimeMatchedChunk>>>,
    lexical_query_count: usize,
    lexical_limit: usize,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let mut lexical_hits: Vec<RuntimeMatchedChunk> = Vec::new();
    let mut failed_query_count = 0usize;
    let mut failures = Vec::new();
    for result in per_query_results {
        match result {
            Ok(query_hits) => {
                lexical_hits = merge_chunks(lexical_hits, query_hits, lexical_limit);
            }
            Err(error) => {
                failed_query_count += 1;
                let summary = format!("{error:#}");
                tracing::warn!(
                    stage = "retrieval.lexical_query_failed",
                    error = %summary,
                    retrieval_degraded = true,
                    failed_source = "lexical_query",
                    failed_query_count,
                    lexical_query_count,
                    "lexical chunk search query failed; continuing with other lexical queries"
                );
                failures.push(summary);
            }
        }
    }
    if lexical_query_count > 0 && failed_query_count == lexical_query_count {
        anyhow::bail!("all lexical chunk search queries failed: {}", failures.join("; "));
    }
    Ok(lexical_hits)
}

fn combine_query_ir_focus_search_results(
    per_query_results: Vec<anyhow::Result<Vec<(Uuid, f32)>>>,
    focus_query_count: usize,
) -> anyhow::Result<Vec<(Uuid, f32)>> {
    let mut hits = Vec::new();
    let mut seen = HashSet::new();
    let mut failed_query_count = 0usize;
    let mut failures = Vec::new();

    for result in per_query_results {
        match result {
            Ok(query_hits) => {
                for (chunk_id, raw_score) in query_hits {
                    if hits.len() >= QUERY_IR_FOCUS_CHUNK_CAP {
                        break;
                    }
                    if seen.insert(chunk_id) {
                        let fallback_score = query_ir_focus_chunk_score(hits.len());
                        let score = if raw_score.is_finite() && raw_score > 0.0 {
                            raw_score
                        } else {
                            fallback_score
                        };
                        hits.push((chunk_id, score));
                    }
                }
            }
            Err(error) => {
                failed_query_count += 1;
                let summary = format!("{error:#}");
                tracing::warn!(
                    stage = "retrieval.query_ir_focus_query_failed",
                    error = %summary,
                    retrieval_degraded = true,
                    failed_source = "query_ir_focus",
                    failed_query_count,
                    focus_query_count,
                    "query-IR focus chunk search query failed; continuing with other focus queries"
                );
                failures.push(summary);
            }
        }
        if hits.len() >= QUERY_IR_FOCUS_CHUNK_CAP {
            break;
        }
    }

    if focus_query_count > 0 && failed_query_count == focus_query_count {
        anyhow::bail!("all query-IR focus chunk searches failed: {}", failures.join("; "));
    }

    Ok(hits)
}

struct ChunkRetrievalLaneOutcome {
    vector_hits: Vec<RuntimeMatchedChunk>,
    vector_elapsed_ms: u128,
    lexical_hits: Vec<RuntimeMatchedChunk>,
    lexical_query_count: usize,
    lexical_elapsed_ms: u128,
    degraded_lane_count: usize,
}

fn combine_chunk_retrieval_lanes(
    vector_result: anyhow::Result<(Vec<RuntimeMatchedChunk>, u128)>,
    lexical_result: anyhow::Result<(Vec<RuntimeMatchedChunk>, usize, u128)>,
) -> anyhow::Result<ChunkRetrievalLaneOutcome> {
    let mut degraded_lane_count = 0usize;
    let mut failures = Vec::new();

    let (vector_hits, vector_elapsed_ms) = match vector_result {
        Ok(result) => result,
        Err(error) => {
            degraded_lane_count += 1;
            let summary = format!("{error:#}");
            tracing::warn!(
                stage = "retrieval.vector_failed",
                error = %summary,
                retrieval_degraded = true,
                failed_lane = "vector",
                "vector chunk retrieval failed; continuing with lexical lane if available"
            );
            failures.push(format!("vector: {summary}"));
            (Vec::new(), 0)
        }
    };

    let (lexical_hits, lexical_query_count, lexical_elapsed_ms) = match lexical_result {
        Ok(result) => result,
        Err(error) => {
            degraded_lane_count += 1;
            let summary = format!("{error:#}");
            tracing::warn!(
                stage = "retrieval.lexical_failed",
                error = %summary,
                retrieval_degraded = true,
                failed_lane = "lexical",
                "lexical chunk retrieval failed; continuing with vector lane if available"
            );
            failures.push(format!("lexical: {summary}"));
            (Vec::new(), 0, 0)
        }
    };

    if degraded_lane_count == 2 {
        anyhow::bail!("all chunk retrieval lanes failed: {}", failures.join("; "));
    }

    Ok(ChunkRetrievalLaneOutcome {
        vector_hits,
        vector_elapsed_ms,
        lexical_hits,
        lexical_query_count,
        lexical_elapsed_ms,
        degraded_lane_count,
    })
}

fn retain_entity_bio_candidates(
    candidates: Vec<RuntimeMatchedChunk>,
    evidence_chunk_ids: &HashSet<Uuid>,
    label_tokens: &[String],
) -> Vec<RuntimeMatchedChunk> {
    candidates
        .into_iter()
        .filter(|chunk| {
            if evidence_chunk_ids.contains(&chunk.chunk_id) {
                return true;
            }
            let haystack = chunk.source_text.to_lowercase();
            label_tokens.iter().any(|token| haystack.contains(token))
        })
        .collect()
}

async fn load_document_identity_chunks_for_targets(
    state: &AppState,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    targeted_document_ids: &BTreeSet<Uuid>,
    plan_keywords: &[String],
    query_ir: Option<&QueryIR>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if targeted_document_ids.is_empty() {
        return Ok(Vec::new());
    }

    let focus_terms = document_identity_focus_terms(plan_keywords, query_ir);
    let mut chunks = Vec::new();
    for (document_rank, document_id) in targeted_document_ids.iter().enumerate() {
        let Some(document) = document_index.get(document_id) else {
            continue;
        };
        let Some(revision_id) = canonical_document_revision_id(document) else {
            continue;
        };
        let rows = state
            .document_store
            .list_chunks_by_revision_range(
                revision_id,
                0,
                DOCUMENT_IDENTITY_CHUNKS_PER_DOCUMENT.saturating_sub(1) as i32,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to load document-identity chunks for document {} revision {}",
                    document_id, revision_id
                )
            })?;
        for (chunk_rank, row) in rows.into_iter().enumerate() {
            let score = document_identity_chunk_score(document_rank, chunk_rank);
            if let Some(mut chunk) = map_chunk_hit(row, score, document_index, plan_keywords) {
                chunk.score = Some(score);
                chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
                chunks.push(chunk);
            }
        }
        let focused_rows = state
            .document_store
            .list_chunks_by_revision_matching_terms(
                revision_id,
                &focus_terms,
                DOCUMENT_IDENTITY_FOCUSED_CHUNKS_PER_DOCUMENT,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to load document-identity focus chunks for document {} revision {}",
                    document_id, revision_id
                )
            })?;
        for (chunk_rank, row) in focused_rows.into_iter().enumerate() {
            let score = document_identity_chunk_score(
                document_rank,
                DOCUMENT_IDENTITY_CHUNKS_PER_DOCUMENT.saturating_add(chunk_rank),
            );
            if let Some(mut chunk) = map_chunk_hit(row, score, document_index, &focus_terms) {
                chunk.score = Some(score);
                chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
                chunks.push(chunk);
            }
        }
    }
    Ok(chunks)
}

fn document_identity_chunk_score(document_rank: usize, chunk_rank: usize) -> f32 {
    DOCUMENT_IDENTITY_SCORE_FLOOR + 10_000.0 - document_rank as f32 * 100.0 - chunk_rank as f32
}

fn versioned_update_procedure_chunk_score(document_rank: usize, chunk_rank: usize) -> f32 {
    VERSIONED_UPDATE_PROCEDURE_SCORE_BASE + 10_000.0
        - document_rank as f32 * 100.0
        - chunk_rank as f32
}

fn versioned_update_procedure_exact_target_runbook_chunk_score(
    document_rank: usize,
    chunk_rank: usize,
    runbook_score: usize,
) -> f32 {
    versioned_update_procedure_chunk_score(document_rank, chunk_rank)
        + VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_RUNBOOK_CHUNK_SCORE_BONUS
        + runbook_score.min(524_288) as f32 / 8.0
}

fn versioned_update_procedure_candidate_chunk_score(
    candidate: &VersionedUpdateProcedureDocumentCandidate,
    document_rank: usize,
    chunk_rank: usize,
) -> f32 {
    let mut score = versioned_update_procedure_chunk_score(document_rank, chunk_rank);
    if candidate.exact_title_identity {
        score += VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_CHUNK_SCORE_BONUS;
    }
    if candidate.target_title_anchor {
        score += VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_CHUNK_SCORE_BONUS / 2.0;
    }
    score += candidate.focus_aligned_command_score as f32 * 128.0;
    score
}

fn document_identity_focus_terms(
    plan_keywords: &[String],
    query_ir: Option<&QueryIR>,
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut terms = Vec::new();
    let mut push_term = |value: &str, terms: &mut Vec<String>| {
        let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized.is_empty() {
            return;
        }
        if seen.insert(normalized.to_lowercase()) {
            terms.push(normalized.clone());
        }
        for token in normalized_alnum_tokens(&normalized, 3) {
            if seen.insert(token.clone()) {
                terms.push(token.clone());
            }
            let token_len = token.chars().count();
            if token_len > DOCUMENT_IDENTITY_FOCUS_PREFIX_CHARS {
                let prefix =
                    token.chars().take(DOCUMENT_IDENTITY_FOCUS_PREFIX_CHARS).collect::<String>();
                if seen.insert(prefix.clone()) {
                    terms.push(prefix);
                }
            }
        }
    };

    for keyword in plan_keywords {
        push_term(keyword, &mut terms);
    }
    if let Some(query_ir) = query_ir {
        if let Some(document_focus) = query_ir.document_focus.as_ref() {
            push_term(&document_focus.hint, &mut terms);
        }
        for entity in &query_ir.target_entities {
            push_term(&entity.label, &mut terms);
        }
        for literal in &query_ir.literal_constraints {
            push_term(&literal.text, &mut terms);
        }
    }
    terms
}

struct LatestVersionSelection {
    requested_count: usize,
    documents: Vec<LatestVersionDocument>,
    inferred_from_retrieved_evidence: bool,
}

fn select_latest_version_documents(
    query_ir: Option<&QueryIR>,
    question: &str,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    chunks: &[RuntimeMatchedChunk],
) -> LatestVersionSelection {
    let Some(query_ir) = query_ir else {
        return LatestVersionSelection {
            requested_count: 0,
            documents: Vec::new(),
            inferred_from_retrieved_evidence: false,
        };
    };
    if query_requests_latest_versions(query_ir) {
        let requested_count = requested_latest_version_count(query_ir);
        let scope_terms = latest_version_scope_terms(query_ir);
        return LatestVersionSelection {
            requested_count,
            documents: latest_version_documents(document_index, requested_count, &scope_terms),
            inferred_from_retrieved_evidence: false,
        };
    }
    let requested_count = fallback_latest_version_requested_count(question);
    let documents = fallback_latest_version_documents_from_retrieved_evidence(
        query_ir,
        question,
        chunks,
        document_index,
        requested_count,
    );
    let inferred_from_retrieved_evidence = !documents.is_empty();
    LatestVersionSelection {
        requested_count: inferred_from_retrieved_evidence
            .then_some(requested_count)
            .unwrap_or_default(),
        documents,
        inferred_from_retrieved_evidence,
    }
}

pub(crate) fn infer_latest_version_query_ir_from_retrieved_evidence(
    query_ir: &QueryIR,
    question: &str,
    chunks: &[RuntimeMatchedChunk],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> Option<QueryIR> {
    let latest_version_chunks = chunks
        .iter()
        .filter(|chunk| chunk.score_kind == RuntimeChunkScoreKind::LatestVersion)
        .cloned()
        .collect::<Vec<_>>();
    if latest_version_chunks.is_empty() {
        return None;
    }
    let requested_count = fallback_latest_version_requested_count(question);
    let documents = fallback_latest_version_documents_from_retrieved_evidence(
        query_ir,
        question,
        &latest_version_chunks,
        document_index,
        requested_count,
    );
    if documents.is_empty() {
        return None;
    }

    let mut inferred = query_ir.clone();
    inferred.act = QueryAct::Enumerate;
    inferred.scope = QueryScope::MultiDocument;
    inferred.target_types = vec!["release".to_string(), "version".to_string()];
    inferred.source_slice = Some(crate::domains::query_ir::SourceSliceSpec {
        direction: crate::domains::query_ir::SourceSliceDirection::Tail,
        count: Some(requested_count as u16),
        filter: crate::domains::query_ir::SourceSliceFilter::ReleaseMarker,
    });
    inferred.confidence = inferred.confidence.max(0.5);
    Some(inferred)
}

fn fallback_latest_version_documents_from_retrieved_evidence(
    query_ir: &QueryIR,
    question: &str,
    chunks: &[RuntimeMatchedChunk],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    requested_count: usize,
) -> Vec<LatestVersionDocument> {
    if !query_ir_allows_retrieved_latest_version_fallback(query_ir, question) {
        return Vec::new();
    }
    let Some(family_key) = retrieved_latest_version_family_key(chunks, document_index) else {
        return Vec::new();
    };
    latest_version_documents_for_family(document_index, requested_count, &family_key)
}

fn query_ir_allows_retrieved_latest_version_fallback(query_ir: &QueryIR, question: &str) -> bool {
    if structured_current_question_segment(question).is_some()
        || !query_ir.conversation_refs.is_empty()
    {
        return false;
    }
    query_ir.confidence <= 0.6
        && matches!(query_ir.act, QueryAct::Describe | QueryAct::Enumerate | QueryAct::Meta)
        && query_ir.source_slice.is_none()
        && query_ir.target_types.is_empty()
        && !latest_version_fallback_has_blocking_literal_constraints(query_ir)
        && extract_semver_like_version(question).is_none()
}

fn latest_version_fallback_has_blocking_literal_constraints(query_ir: &QueryIR) -> bool {
    query_ir.literal_constraints.iter().any(|literal| match literal.kind {
        LiteralKind::NumericCode => false,
        kind => literal_kind_has_exact_technical_shape(
            kind,
            literal.text.trim_matches(|ch: char| ch.is_ascii_punctuation()),
        ),
    })
}

fn fallback_latest_version_requested_count(question: &str) -> usize {
    question
        .split(|ch: char| !ch.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .find_map(|part| {
            let value = part.parse::<usize>().ok()?;
            if value == 0 || (1900..=2100).contains(&value) {
                return None;
            }
            Some(value.clamp(1, FALLBACK_LATEST_VERSION_MAX_COUNT))
        })
        .unwrap_or(FALLBACK_LATEST_VERSION_DEFAULT_COUNT)
}

fn retrieved_latest_version_family_key(
    chunks: &[RuntimeMatchedChunk],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> Option<String> {
    let mut family_documents = HashMap::<String, BTreeSet<Uuid>>::new();
    let mut family_chunk_counts = HashMap::<String, usize>::new();
    for chunk in chunks {
        let Some(document) = document_index.get(&chunk.document_id) else {
            continue;
        };
        let Some(latest_document) = latest_version_document_from_index_row(document) else {
            continue;
        };
        family_documents
            .entry(latest_document.family_key.clone())
            .or_default()
            .insert(latest_document.document_id);
        *family_chunk_counts.entry(latest_document.family_key).or_default() += 1;
    }
    let mut candidates = family_documents
        .iter()
        .map(|(family_key, document_ids)| {
            (
                family_key.clone(),
                document_ids.len(),
                family_chunk_counts.get(family_key).copied().unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right.1.cmp(&left.1).then_with(|| right.2.cmp(&left.2)).then_with(|| left.0.cmp(&right.0))
    });
    let (family_key, document_count, chunk_count) = candidates.first()?.clone();
    let runner_up_document_count = candidates.get(1).map(|candidate| candidate.1).unwrap_or(0);
    if document_count >= 2 && document_count > runner_up_document_count && chunk_count >= 2 {
        Some(family_key)
    } else {
        None
    }
}

async fn load_latest_version_document_chunks(
    state: &AppState,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    requested_count: usize,
    documents: &[LatestVersionDocument],
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if requested_count == 0 || documents.is_empty() {
        return Ok(Vec::new());
    }

    let mut chunks = Vec::new();
    for (rank, document) in documents.iter().enumerate() {
        let rows = state
            .document_store
            .list_chunks_by_revision_range(
                document.revision_id,
                0,
                LATEST_VERSION_CHUNKS_PER_DOCUMENT as i32,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to load latest-version chunks for document {} revision {}",
                    document.document_id, document.revision_id
                )
            })?;
        for (chunk_rank, row) in rows
            .into_iter()
            .filter(|row| !is_source_profile_chunk_row(row))
            .take(LATEST_VERSION_CHUNKS_PER_DOCUMENT)
            .enumerate()
        {
            let score = latest_version_chunk_score(
                DOCUMENT_IDENTITY_SCORE_FLOOR,
                requested_count,
                rank,
                chunk_rank,
            );
            if let Some(mut chunk) = map_chunk_hit(row, score, document_index, plan_keywords) {
                chunk.score = Some(score);
                chunk.score_kind = RuntimeChunkScoreKind::LatestVersion;
                chunks.push(chunk);
            }
        }
    }
    Ok(chunks)
}

async fn load_latest_version_semantic_document_chunks(
    state: &AppState,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    requested_count: usize,
    scope_terms: &[String],
    explicit_latest_requested: bool,
    prefer_source_tail: bool,
    allow_unscoped_density_fallback: bool,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if !explicit_latest_requested || requested_count == 0 {
        return Ok(Vec::new());
    }
    let candidates = latest_version_semantic_candidate_documents(
        document_index,
        scope_terms,
        LATEST_VERSION_SEMANTIC_DOCUMENT_CANDIDATE_CAP,
    );
    let mut candidates = candidates;
    let semantic_document_ids =
        candidates.iter().map(|document| document.document_id).collect::<BTreeSet<_>>();
    let mut candidate_document_ids = semantic_document_ids.clone();
    if prefer_source_tail {
        let mut density_candidates = latest_version_structural_density_candidate_documents(
            state,
            document_index,
            scope_terms,
            &candidate_document_ids,
            LATEST_VERSION_STRUCTURAL_DENSITY_DOCUMENT_CAP,
        )
        .await?;
        candidate_document_ids.extend(
            density_candidates.iter().map(|document| document.document_id).collect::<BTreeSet<_>>(),
        );
        if allow_unscoped_density_fallback
            && !scope_terms.is_empty()
            && density_candidates.len() < requested_count.min(4)
        {
            let remaining_density_cap = LATEST_VERSION_STRUCTURAL_DENSITY_DOCUMENT_CAP
                .saturating_sub(density_candidates.len());
            let unscoped_density_candidates =
                latest_version_structural_density_candidate_documents(
                    state,
                    document_index,
                    &[],
                    &candidate_document_ids,
                    remaining_density_cap,
                )
                .await?;
            candidate_document_ids.extend(
                unscoped_density_candidates
                    .iter()
                    .map(|document| document.document_id)
                    .collect::<BTreeSet<_>>(),
            );
            density_candidates.extend(unscoped_density_candidates);
        }
        candidate_document_ids.extend(
            density_candidates.iter().map(|document| document.document_id).collect::<BTreeSet<_>>(),
        );
        candidates.extend(density_candidates);
    }
    let structural_probe_limit = if scope_terms.is_empty() {
        LATEST_VERSION_STRUCTURAL_PROBE_DOCUMENT_CAP
    } else {
        LATEST_VERSION_STRUCTURAL_PROBE_SCOPED_DOCUMENT_CAP
    };
    candidates.extend(latest_version_structural_probe_candidate_documents(
        document_index,
        scope_terms,
        &candidate_document_ids,
        structural_probe_limit,
    ));
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let mut rows = Vec::<LatestVersionSemanticRow>::new();
    let mut dense_rows = Vec::<LatestVersionSemanticRow>::new();
    let mut deep_scan_document_count = 0usize;
    for (document_rank, document) in candidates.iter().enumerate() {
        let chunk_rows = state
            .document_store
            .list_chunks_by_revision_range(
                document.revision_id,
                0,
                LATEST_VERSION_SEMANTIC_CHUNK_SCAN_LIMIT as i32,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to load semantic latest-version chunks for document {} revision {}",
                    document.document_id, document.revision_id
                )
            })?;
        let (mut candidate_rows, mut distinct_versions, mut structural_score) =
            latest_version_semantic_rows_from_chunk_rows(
                chunk_rows,
                document_rank,
                document.requires_structural_release_density,
            );
        if document.requires_structural_release_density
            && deep_scan_document_count < LATEST_VERSION_SEMANTIC_DEEP_DOCUMENT_CAP
        {
            let deep_rows = state
                .document_store
                .list_chunks_by_revision_range(
                    document.revision_id,
                    LATEST_VERSION_SEMANTIC_CHUNK_SCAN_LIMIT as i32 + 1,
                    LATEST_VERSION_SEMANTIC_DEEP_CHUNK_SCAN_LIMIT,
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to deep-load semantic latest-version chunks for document {} revision {}",
                        document.document_id, document.revision_id
                    )
                })?;
            let (additional_rows, additional_versions, additional_score) =
                latest_version_semantic_rows_from_chunk_rows(
                    deep_rows,
                    document_rank,
                    document.requires_structural_release_density,
                );
            candidate_rows.extend(additional_rows);
            distinct_versions.extend(additional_versions);
            structural_score = structural_score.saturating_add(additional_score);
            deep_scan_document_count = deep_scan_document_count.saturating_add(1);
        }
        if document.requires_structural_release_density
            && !latest_version_structural_inventory_candidate_has_density(
                distinct_versions.len(),
                candidate_rows.len(),
                structural_score,
            )
        {
            continue;
        }
        if document.requires_structural_release_density {
            for mut row in candidate_rows {
                row.structural_density_score = structural_score;
                dense_rows.push(row);
            }
        } else {
            rows.extend(candidate_rows);
        }
    }
    let dense_distinct_version_count =
        dense_rows.iter().map(|row| row.version.clone()).collect::<BTreeSet<_>>().len();
    if dense_distinct_version_count
        >= requested_count.min(LATEST_VERSION_STRUCTURAL_MIN_DISTINCT_VERSIONS)
    {
        if scope_terms.is_empty() && prefer_source_tail {
            rows.extend(dense_rows);
        } else {
            rows = dense_rows;
        }
    } else {
        rows.extend(dense_rows);
    }
    tracing::info!(
        stage = "retrieval.latest_version_semantic",
        candidate_document_count = candidates.len(),
        semver_row_count = rows.len(),
        dense_semver_version_count = dense_distinct_version_count,
        deep_scan_document_count,
        requested_count,
        "semantic latest-version document scan completed"
    );
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let source_tail_inventory = prefer_source_tail;
    rows = order_latest_version_semantic_rows(rows, source_tail_inventory, requested_count);
    let selection_preview = rows
        .iter()
        .take(16)
        .map(|row| {
            format!(
                "{}:{}:{}:{}",
                row.document_rank,
                row.structural_density_score,
                row.row.chunk_index,
                usize::from(row.from_structural_inventory)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    tracing::debug!(
        stage = "retrieval.latest_version_semantic_selection",
        source_tail_inventory,
        selection_preview = %selection_preview,
        "semantic latest-version row selection ordered"
    );

    let mut chunks = Vec::new();
    let mut seen_versions = BTreeSet::<Vec<u32>>::new();
    let mut seen_chunk_ids = HashSet::<Uuid>::new();
    let mut selection_rank = 0usize;
    for semantic_row in rows {
        let version_seen = if source_tail_inventory {
            false
        } else {
            !seen_versions.insert(semantic_row.version.clone())
        };
        if version_seen {
            continue;
        }
        if !seen_chunk_ids.insert(semantic_row.row.chunk_id) {
            continue;
        }
        let score = latest_version_chunk_score(
            DOCUMENT_IDENTITY_SCORE_FLOOR + 256.0,
            requested_count,
            selection_rank,
            0,
        );
        selection_rank = selection_rank.saturating_add(1);
        if let Some(mut chunk) =
            map_chunk_hit(semantic_row.row, score, document_index, plan_keywords)
        {
            chunk.score = Some(score);
            chunk.score_kind = RuntimeChunkScoreKind::LatestVersion;
            chunks.push(chunk);
        }
        if chunks.len() >= requested_count.max(LATEST_VERSION_SEMANTIC_CHUNK_CAP) {
            break;
        }
    }
    chunks.truncate(LATEST_VERSION_SEMANTIC_CHUNK_CAP);
    Ok(chunks)
}

#[derive(Clone)]
struct LatestVersionSemanticRow {
    version: Vec<u32>,
    document_rank: usize,
    structural_density_score: usize,
    from_structural_inventory: bool,
    row: KnowledgeChunkRow,
}

fn order_latest_version_semantic_rows(
    mut rows: Vec<LatestVersionSemanticRow>,
    source_tail_inventory: bool,
    requested_count: usize,
) -> Vec<LatestVersionSemanticRow> {
    if !source_tail_inventory {
        rows.sort_by(|left, right| {
            compare_version_desc(&left.version, &right.version)
                .then_with(|| right.from_structural_inventory.cmp(&left.from_structural_inventory))
                .then_with(|| right.structural_density_score.cmp(&left.structural_density_score))
                .then_with(|| left.document_rank.cmp(&right.document_rank))
                .then_with(|| left.row.chunk_index.cmp(&right.row.chunk_index))
                .then_with(|| left.row.chunk_id.cmp(&right.row.chunk_id))
        });
        return rows;
    }

    let identity_prefix_cap = requested_count
        .saturating_mul(LATEST_VERSION_SEMANTIC_UNSCOPED_IDENTITY_DOCUMENT_CAP_MULTIPLIER)
        .clamp(requested_count.max(1), LATEST_VERSION_SEMANTIC_CHUNK_CAP);
    let mut identity_candidates =
        rows.iter().filter(|row| !row.from_structural_inventory).cloned().collect::<Vec<_>>();
    identity_candidates.sort_by(|left, right| {
        left.document_rank
            .cmp(&right.document_rank)
            .then_with(|| right.row.chunk_index.cmp(&left.row.chunk_index))
            .then_with(|| compare_version_desc(&left.version, &right.version))
            .then_with(|| left.row.chunk_id.cmp(&right.row.chunk_id))
    });
    let mut identity_prefix = Vec::new();
    let mut identity_documents = HashSet::<Uuid>::new();
    for row in identity_candidates {
        if identity_prefix.len() >= identity_prefix_cap {
            break;
        }
        if identity_documents.insert(row.row.document_id) {
            identity_prefix.push(row);
        }
    }

    let mut per_document = BTreeMap::<Uuid, Vec<LatestVersionSemanticRow>>::new();
    for row in rows {
        per_document.entry(row.row.document_id).or_default().push(row);
    }
    let mut documents = per_document
        .iter()
        .map(|(document_id, rows)| {
            let from_structural_inventory = rows.iter().any(|row| row.from_structural_inventory);
            let structural_density_score =
                rows.iter().map(|row| row.structural_density_score).max().unwrap_or_default();
            let document_rank = rows.iter().map(|row| row.document_rank).min().unwrap_or_default();
            (*document_id, from_structural_inventory, structural_density_score, document_rank)
        })
        .collect::<Vec<_>>();
    documents.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| right.2.cmp(&left.2))
            .then_with(|| left.3.cmp(&right.3))
            .then_with(|| left.0.cmp(&right.0))
    });

    for rows in per_document.values_mut() {
        rows.sort_by(|left, right| {
            right
                .row
                .chunk_index
                .cmp(&left.row.chunk_index)
                .then_with(|| compare_version_desc(&left.version, &right.version))
                .then_with(|| left.row.chunk_id.cmp(&right.row.chunk_id))
        });
    }

    let rows_per_document = LATEST_VERSION_SEMANTIC_UNSCOPED_ROWS_PER_DOCUMENT
        .clamp(1, LATEST_VERSION_SEMANTIC_CHUNK_CAP);
    let document_pool_limit =
        requested_count.clamp(4, LATEST_VERSION_SEMANTIC_CHUNK_CAP).min(documents.len());
    let (primary_documents, secondary_documents) = documents.split_at(document_pool_limit);
    let mut ordered = Vec::new();
    for row_offset in 0..rows_per_document {
        for (document_id, _, _, _) in primary_documents {
            let Some(rows) = per_document.get(document_id) else {
                continue;
            };
            let Some(row) = rows.get(row_offset) else {
                continue;
            };
            ordered.push(row.clone());
        }
    }
    ordered.extend(identity_prefix);
    for (document_id, _, _, _) in primary_documents {
        let Some(rows) = per_document.get(document_id) else {
            continue;
        };
        ordered.extend(rows.iter().skip(rows_per_document).cloned());
    }
    for (document_id, _, _, _) in secondary_documents {
        let Some(rows) = per_document.get(document_id) else {
            continue;
        };
        ordered.extend(rows.iter().cloned());
    }
    ordered
}

fn latest_version_semantic_rows_from_chunk_rows(
    chunk_rows: Vec<KnowledgeChunkRow>,
    document_rank: usize,
    from_structural_inventory: bool,
) -> (Vec<LatestVersionSemanticRow>, BTreeSet<Vec<u32>>, usize) {
    let mut rows = Vec::new();
    let mut distinct_versions = BTreeSet::<Vec<u32>>::new();
    let mut structural_score = 0usize;
    for row in chunk_rows.into_iter().filter(|row| !is_source_profile_chunk_row(row)) {
        let source_text = chunk_answer_source_text(&row);
        let mut versions = latest_version_context_versions(&source_text);
        if versions.is_empty() {
            continue;
        }
        versions.sort_by(|left, right| compare_version_desc(left, right));
        distinct_versions.extend(versions.iter().cloned());
        structural_score = structural_score
            .saturating_add(latest_version_structural_inventory_text_score(&source_text));
        rows.push(LatestVersionSemanticRow {
            version: versions[0].clone(),
            document_rank,
            structural_density_score: 0,
            from_structural_inventory,
            row,
        });
    }
    (rows, distinct_versions, structural_score)
}

#[derive(Clone)]
struct LatestVersionSemanticDocument {
    document_id: Uuid,
    revision_id: Uuid,
    semantic_score: usize,
    identity_text: String,
    requires_structural_release_density: bool,
}

fn latest_version_semantic_candidate_documents(
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    scope_terms: &[String],
    limit: usize,
) -> Vec<LatestVersionSemanticDocument> {
    if limit == 0 {
        return Vec::new();
    }
    let rows = document_index
        .values()
        .filter(|document| latest_version_semantic_document_is_content_candidate(document))
        .filter_map(|document| {
            let identity_text = latest_version_document_identity_text(document);
            let semantic_score =
                latest_version_identity_structural_score(&identity_text, scope_terms);
            if semantic_score == 0 {
                return None;
            }
            let revision_id = canonical_document_revision_id(document)?;
            Some(LatestVersionSemanticDocument {
                document_id: document.document_id,
                revision_id,
                semantic_score,
                identity_text,
                requires_structural_release_density: false,
            })
        })
        .collect::<Vec<_>>();
    let scoped_rows = if scope_terms.is_empty() {
        rows
    } else {
        let scoped = rows
            .iter()
            .filter(|candidate| {
                scope_terms.iter().any(|term| candidate.identity_text.contains(term))
            })
            .cloned()
            .collect::<Vec<_>>();
        if scoped.is_empty() { rows } else { scoped }
    };
    let mut rows = scoped_rows;
    rows.sort_by(|left, right| {
        right
            .semantic_score
            .cmp(&left.semantic_score)
            .then_with(|| left.identity_text.cmp(&right.identity_text))
            .then_with(|| left.document_id.cmp(&right.document_id))
    });
    rows.truncate(limit);
    rows
}

fn latest_version_identity_structural_score(identity_text: &str, scope_terms: &[String]) -> usize {
    let mut score = 0usize;
    if extract_semver_like_version(identity_text).is_some() {
        score = score.saturating_add(512);
    }
    let scope_overlap =
        scope_terms.iter().filter(|term| identity_text.contains(term.as_str())).count();
    score
        .saturating_add(scope_overlap.saturating_mul(1024))
        .saturating_add(usize::from(scope_overlap > 0).saturating_mul(128))
}

fn latest_version_structural_probe_candidate_documents(
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    scope_terms: &[String],
    excluded_document_ids: &BTreeSet<Uuid>,
    limit: usize,
) -> Vec<LatestVersionSemanticDocument> {
    if limit == 0 {
        return Vec::new();
    }
    let mut rows = document_index
        .values()
        .filter(|document| latest_version_semantic_document_is_content_candidate(document))
        .filter(|document| !excluded_document_ids.contains(&document.document_id))
        .filter_map(|document| {
            let revision_id = canonical_document_revision_id(document)?;
            let identity_text = latest_version_document_identity_text(document);
            let scope_overlap =
                scope_terms.iter().filter(|term| identity_text.contains(term.as_str())).count();
            if !scope_terms.is_empty() && scope_overlap == 0 {
                return None;
            }
            let semantic_score = scope_overlap
                .saturating_mul(1024)
                .saturating_add(document.latest_revision_no.unwrap_or_default() as usize);
            Some((
                document.updated_at,
                document.latest_revision_no.unwrap_or_default(),
                LatestVersionSemanticDocument {
                    document_id: document.document_id,
                    revision_id,
                    semantic_score,
                    identity_text,
                    requires_structural_release_density: true,
                },
            ))
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.1.cmp(&left.1))
            .then_with(|| right.2.semantic_score.cmp(&left.2.semantic_score))
            .then_with(|| left.2.identity_text.cmp(&right.2.identity_text))
            .then_with(|| left.2.document_id.cmp(&right.2.document_id))
    });
    rows.truncate(limit);
    rows.into_iter().map(|(_, _, document)| document).collect()
}

#[derive(sqlx::FromRow)]
struct LatestVersionStructuralDensityCandidateRow {
    document_id: Uuid,
    version_row_count: i64,
    max_version_chunk_index: Option<i32>,
}

async fn latest_version_structural_density_candidate_documents(
    state: &AppState,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    scope_terms: &[String],
    excluded_document_ids: &BTreeSet<Uuid>,
    limit: usize,
) -> anyhow::Result<Vec<LatestVersionSemanticDocument>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let Some(library_id) = document_index.values().next().map(|document| document.library_id)
    else {
        return Ok(Vec::new());
    };
    let candidate_document_ids = document_index
        .values()
        .filter(|document| latest_version_semantic_document_is_content_candidate(document))
        .filter(|document| !excluded_document_ids.contains(&document.document_id))
        .filter(|document| {
            if scope_terms.is_empty() {
                return true;
            }
            let identity_text = latest_version_document_identity_text(document);
            scope_terms.iter().any(|term| identity_text.contains(term))
        })
        .filter(|document| canonical_document_revision_id(document).is_some())
        .map(|document| document.document_id)
        .collect::<Vec<_>>();
    if candidate_document_ids.is_empty() {
        return Ok(Vec::new());
    }

    let rows = sqlx::query_as::<_, LatestVersionStructuralDensityCandidateRow>(
        "with candidate_documents as (
           select distinct document_id
           from unnest($2::uuid[]) as candidate(document_id)
         ),
         chunk_signals as (
           select
             c.document_id,
             c.chunk_index,
             ((coalesce(c.content_text, '') || E'\n' || coalesce(c.window_text, '')) ~ $3) as has_version_marker
           from knowledge_chunk c
           join candidate_documents candidate on candidate.document_id = c.document_id
           where c.library_id = $1
             and c.chunk_state = 'ready'
             and c.raptor_level is null
         ),
         scored as (
           select
             document_id,
             count(*) filter (where has_version_marker)::bigint as version_row_count,
             max(chunk_index) filter (where has_version_marker) as max_version_chunk_index
           from chunk_signals
           group by document_id
         )
         select document_id, version_row_count, max_version_chunk_index
         from scored
         where version_row_count >= $4
         order by version_row_count desc, max_version_chunk_index desc nulls last, document_id asc
         limit $5",
    )
    .bind(library_id)
    .bind(&candidate_document_ids)
    .bind(LATEST_VERSION_STRUCTURAL_DENSITY_REGEX)
    .bind(LATEST_VERSION_STRUCTURAL_DENSITY_MIN_ROWS)
    .bind(i64::try_from(limit).unwrap_or(i64::MAX))
    .fetch_all(&state.persistence.postgres)
    .await
    .context("failed to load latest-version structural-density candidates")?;

    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let document = document_index.get(&row.document_id)?;
            let revision_id = canonical_document_revision_id(document)?;
            let identity_text = latest_version_document_identity_text(document);
            let version_row_score = usize::try_from(row.version_row_count).unwrap_or(usize::MAX);
            let max_index_score = row
                .max_version_chunk_index
                .and_then(|index| usize::try_from(index).ok())
                .unwrap_or_default();
            Some(LatestVersionSemanticDocument {
                document_id: document.document_id,
                revision_id,
                semantic_score: version_row_score
                    .saturating_mul(1024)
                    .saturating_add(max_index_score.min(1023)),
                identity_text,
                requires_structural_release_density: true,
            })
        })
        .collect())
}

fn latest_version_context_versions(text: &str) -> Vec<Vec<u32>> {
    let mut versions = BTreeSet::<Vec<u32>>::new();
    for line in text.lines() {
        if let Some(version) = extract_release_context_version(line) {
            versions.insert(version);
        }
    }
    if versions.is_empty()
        && let Some(version) = extract_release_context_version(text)
    {
        versions.insert(version);
    }
    versions.into_iter().collect()
}

fn latest_version_structural_inventory_text_score(text: &str) -> usize {
    let version_score = latest_version_context_versions(text).len().min(8).saturating_mul(4);
    let row_marker_score = text
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            strip_leading_procedure_order_marker(trimmed) != trimmed
                || trimmed.starts_with(['-', '*', '•'])
                || trimmed.matches('|').count() >= 2
                || trimmed.matches('\t').count() >= 2
        })
        .count()
        .min(8);
    let literal_score = extract_explicit_path_literals(text, 4)
        .len()
        .saturating_add(extract_parameter_literals(text, 4).len())
        .saturating_add(extract_config_assignment_literals(text, 4).len())
        .min(4);
    version_score.saturating_add(row_marker_score).saturating_add(literal_score)
}

fn latest_version_structural_inventory_candidate_has_density(
    distinct_version_count: usize,
    version_row_count: usize,
    structural_score: usize,
) -> bool {
    distinct_version_count >= LATEST_VERSION_STRUCTURAL_MIN_DISTINCT_VERSIONS
        && (version_row_count >= LATEST_VERSION_STRUCTURAL_MIN_DISTINCT_VERSIONS
            || structural_score >= LATEST_VERSION_STRUCTURAL_MIN_DISTINCT_VERSIONS * 4)
}

fn latest_version_semantic_document_is_content_candidate(document: &KnowledgeDocumentRow) -> bool {
    if document.document_state != "active"
        || document.document_role != crate::domains::content::DOCUMENT_ROLE_PRIMARY
    {
        return false;
    }
    ![
        document.title.as_deref(),
        document.file_name.as_deref(),
        Some(document.external_key.as_str()),
    ]
    .into_iter()
    .flatten()
    .any(latest_version_identity_value_is_image_like)
}

fn latest_version_identity_value_is_image_like(value: &str) -> bool {
    matches!(
        std::path::Path::new(value.trim())
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .map(str::to_ascii_lowercase)
            .as_deref()
            .unwrap_or_default(),
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg"
    )
}

fn latest_version_document_identity_text(document: &KnowledgeDocumentRow) -> String {
    [
        document.title.as_deref(),
        document.file_name.as_deref(),
        document.source_uri.as_deref(),
        document.document_hint.as_deref(),
        Some(document.external_key.as_str()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ")
    .to_lowercase()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LatestVersionDocument {
    pub(crate) document_id: Uuid,
    pub(crate) revision_id: Uuid,
    pub(crate) version: Vec<u32>,
    pub(crate) title: String,
    pub(crate) family_key: String,
}

pub(crate) fn latest_version_documents(
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    count: usize,
    scope_terms: &[String],
) -> Vec<LatestVersionDocument> {
    let rows = document_index
        .values()
        .filter_map(|document| {
            let latest_document = latest_version_document_from_index_row(document)?;
            let identity_text = format!(
                "{} {}",
                latest_document.title.to_lowercase(),
                document.external_key.to_lowercase()
            );
            Some((latest_document, identity_text))
        })
        .collect::<Vec<_>>();
    let scoped_rows = if scope_terms.is_empty() {
        rows
    } else {
        let scoped = rows
            .iter()
            .filter(|(_, identity_text)| {
                scope_terms.iter().any(|term| identity_text.contains(term))
            })
            .cloned()
            .collect::<Vec<_>>();
        if scoped.is_empty() { rows } else { scoped }
    };
    let mut rows = scoped_rows.into_iter().map(|(document, _)| document).collect::<Vec<_>>();
    rows.sort_by(compare_latest_version_documents);
    rows = dedupe_latest_version_documents(rows);
    if count > 1 {
        let family_sizes =
            rows.iter().fold(HashMap::<String, usize>::new(), |mut acc, document| {
                *acc.entry(document.family_key.clone()).or_default() += 1;
                acc
            });
        let top_two_counts = {
            let mut counts = family_sizes.values().copied().collect::<Vec<_>>();
            counts.sort_unstable_by(|left, right| right.cmp(left));
            counts
        };
        if let Some((family_key, family_count)) = family_sizes
            .iter()
            .max_by(|left, right| left.1.cmp(right.1).then_with(|| left.0.cmp(right.0)))
            .map(|(family_key, count)| (family_key.clone(), *count))
        {
            let runner_up = top_two_counts.get(1).copied().unwrap_or(0);
            if family_count >= count && family_count > runner_up {
                rows.retain(|document| document.family_key == family_key);
            }
        }
    }
    rows.sort_by(compare_latest_version_documents);
    rows.truncate(count);
    rows
}

fn latest_version_documents_for_family(
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    count: usize,
    family_key: &str,
) -> Vec<LatestVersionDocument> {
    let mut rows = document_index
        .values()
        .filter_map(latest_version_document_from_index_row)
        .filter(|document| document.family_key == family_key)
        .collect::<Vec<_>>();
    rows.sort_by(compare_latest_version_documents);
    rows = dedupe_latest_version_documents(rows);
    rows.truncate(count);
    rows
}

fn latest_version_document_from_index_row(
    document: &KnowledgeDocumentRow,
) -> Option<LatestVersionDocument> {
    if document.document_state != "active" {
        return None;
    }
    let primary_title = document
        .title
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .or(document.file_name.as_deref())?;
    if !text_has_release_version_marker(primary_title) {
        return None;
    }
    let primary_title_lower = primary_title.to_lowercase();
    let version = extract_semver_like_version(&primary_title_lower)?;
    let revision_id = canonical_document_revision_id(document)?;
    Some(LatestVersionDocument {
        document_id: document.document_id,
        revision_id,
        version,
        title: primary_title.to_string(),
        family_key: latest_version_family_key(primary_title),
    })
}

fn compare_latest_version_documents(
    left: &LatestVersionDocument,
    right: &LatestVersionDocument,
) -> std::cmp::Ordering {
    compare_version_desc(&left.version, &right.version)
        .then_with(|| left.title.cmp(&right.title))
        .then_with(|| left.document_id.cmp(&right.document_id))
}

fn dedupe_latest_version_documents(rows: Vec<LatestVersionDocument>) -> Vec<LatestVersionDocument> {
    let mut seen = HashSet::<(Vec<u32>, String)>::with_capacity(rows.len());
    rows.into_iter()
        .filter(|document| {
            let key = (document.version.clone(), document.title.to_lowercase());
            seen.insert(key)
        })
        .collect()
}

/// Hydrate a bag of `(chunk_id, score)` hits into ranked
/// `RuntimeMatchedChunk` rows with exactly one knowledge-store read.
/// The previous `join_all(get_chunk)` pattern turned every hit into a
/// separate coordinator call — on a typical 16-hit vector + 4×20-hit
/// lexical fan-out that was ~100 sequential chunk fetches per
/// grounded_answer turn. Batch hydration collapses them into ≤5.
///
/// Score/order is preserved via an id→score map: `list_chunks_by_ids`
/// returns rows unordered, so we re-zip the scores in a hash lookup
/// instead of relying on the database's ordering.
async fn batch_hydrate_hits(
    state: &AppState,
    hits: Vec<(Uuid, f32)>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    targeted_document_ids: &BTreeSet<Uuid>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if hits.is_empty() {
        return Ok(Vec::new());
    }
    // Build the score lookup and the id list in one pass. Dedupe ids
    // — a hit list can legitimately contain the same chunk across
    // vector and lexical queries before the RRF merge, and we don't
    // want to waste payload bytes on duplicate filter args.
    let mut score_by_chunk: HashMap<Uuid, f32> = HashMap::with_capacity(hits.len());
    for (chunk_id, score) in &hits {
        // Keep the best (highest) score if the same chunk appears
        // twice. Ranking downstream expects a single row per chunk.
        score_by_chunk
            .entry(*chunk_id)
            .and_modify(|existing| {
                if *score > *existing {
                    *existing = *score;
                }
            })
            .or_insert(*score);
    }
    let chunk_ids: Vec<Uuid> = score_by_chunk.keys().copied().collect();
    let chunk_rows = state
        .document_store
        .list_chunks_by_ids(&chunk_ids)
        .await
        .context("failed to batch-load runtime query chunks")?;
    let mut mapped: Vec<RuntimeMatchedChunk> = Vec::with_capacity(chunk_rows.len());
    for chunk in chunk_rows {
        let Some(score) = score_by_chunk.get(&chunk.chunk_id).copied() else {
            continue;
        };
        if !is_answer_driving_search_chunk_row(&chunk) {
            continue;
        }
        if !targeted_document_ids.is_empty() && !targeted_document_ids.contains(&chunk.document_id)
        {
            continue;
        }
        let Some(matched) = map_chunk_hit(chunk, score, document_index, plan_keywords) else {
            continue;
        };
        mapped.push(matched);
    }
    // Preserve score order — the merge/rerank pipeline relies on
    // the hit list coming in "best-first".
    mapped.sort_by(score_desc_chunks);
    Ok(mapped)
}

fn is_answer_driving_search_chunk_row(chunk: &KnowledgeChunkRow) -> bool {
    !is_source_profile_chunk_row(chunk)
}

pub(crate) async fn resolve_runtime_vector_search_context(
    state: &AppState,
    library_id: Uuid,
    _provider_profile: &EffectiveProviderProfile,
) -> anyhow::Result<Option<RuntimeVectorSearchContext>> {
    let Some(binding) = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::QueryRetrieve)
        .await
        .context("failed to resolve query retrieval binding for runtime vector search")?
    else {
        return Ok(None);
    };

    let Some(generation) = load_latest_library_generation(state, library_id).await? else {
        return Ok(None);
    };
    if generation.active_vector_generation <= 0 {
        return Ok(None);
    }

    Ok(Some(RuntimeVectorSearchContext { model_catalog_id: binding.model_catalog_id }))
}

pub(crate) fn expanded_candidate_limit(
    planned_mode: RuntimeQueryMode,
    top_k: usize,
    rerank_enabled: bool,
    rerank_candidate_limit: usize,
) -> usize {
    if matches!(planned_mode, RuntimeQueryMode::Hybrid | RuntimeQueryMode::Mix) {
        let intrinsic_limit = top_k.saturating_mul(3).clamp(top_k, 96);
        if rerank_enabled {
            return intrinsic_limit.max(rerank_candidate_limit);
        }
        return intrinsic_limit;
    }
    top_k
}

/// Always returns `false` — both vector and lexical lanes run on every
/// query. The `exact_literal_technical` planner bit still affects boosts
/// and context packing; it does not disable the semantic lane.
pub(crate) const fn should_skip_vector_search(_plan: &RuntimeQueryPlan) -> bool {
    false
}

/// Hard cap on the number of lexical searches dispatched per query. Every
/// additional query is a full `search_chunks` round-trip; with a ~500 ms p50 per query and a
/// 1000+ document corpus, a 10-query fan-out added 5–8 s of
/// retrieval latency even when every query returned zero hits.
/// Eight is the empirical sweet spot: enough to carry multiple focus
/// segments through the lexical path when vector search might miss, while
/// the concurrent `join_all` fan-out keeps wall-clock inside the
/// coordinator's fan-out budget. Anything above 8 returned diminishing
/// recall for order-of-magnitude more latency.
const MAX_LEXICAL_QUERIES: usize = 8;
const MAX_GRAPH_EVIDENCE_TEXT_QUERIES: usize = 8;

/// Maximum number of chunks from a single document the retriever is
/// allowed to surface in its final hit list. Two chunks (typically one
/// for context + one for the actual answer) gives the answer model
/// enough signal while preserving top-k diversity. Higher caps let a
/// single over-tokenised document drown out every other candidate.
const MAX_CHUNKS_PER_DOCUMENT: usize = 2;

/// Caps the number of chunks from any single `document_id` in a
/// retrieval result. Preserves the input order (which reflects the
/// caller's merged score ranking): walks the list, admits each chunk
/// only if its document has fewer than `max_per_doc` chunks already
/// admitted. Keeps all single-document results if one only has < N
/// chunks (no silent drop of legitimate results).
fn diversify_chunks_by_document(
    chunks: Vec<RuntimeMatchedChunk>,
    max_per_doc: usize,
    setup_focus_document_ids: &BTreeSet<Uuid>,
) -> Vec<RuntimeMatchedChunk> {
    if max_per_doc == 0 {
        return chunks;
    }
    let mut counts: std::collections::HashMap<Uuid, usize> =
        std::collections::HashMap::with_capacity(chunks.len());
    let mut out = Vec::with_capacity(chunks.len());
    let mut setup_focus_count = 0usize;
    for chunk in chunks {
        let setup_focus_document = setup_focus_document_ids.contains(&chunk.document_id);
        let document_cap = if setup_focus_document {
            SETUP_FOCUS_DOCUMENT_CHUNK_CAP.max(max_per_doc)
        } else {
            max_per_doc
        };
        if setup_focus_document && setup_focus_count >= SETUP_FOCUS_DOCUMENT_CHUNK_CAP {
            continue;
        }
        let count = counts.entry(chunk.document_id).or_insert(0);
        if *count >= document_cap {
            continue;
        }
        *count += 1;
        if setup_focus_document {
            setup_focus_count = setup_focus_count.saturating_add(1);
        }
        out.push(chunk);
    }
    out
}

pub(crate) fn build_lexical_queries(
    question: &str,
    plan: &RuntimeQueryPlan,
    query_ir_focus_queries: &[String],
    query_ir: Option<&QueryIR>,
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut queries = Vec::new();

    let mut push_query = |value: String, queries: &mut Vec<String>| {
        if queries.len() >= MAX_LEXICAL_QUERIES {
            return;
        }
        let normalized = value.trim().to_string();
        if normalized.is_empty() || !seen.insert(normalized.clone()) {
            return;
        }
        queries.push(normalized);
    };

    // Priority 1 — the raw user question. The full-text analyzer already
    // splits it into relevant tokens; this is the
    // highest-signal query and must always go first.
    let retrieval_question = strip_leading_question_marker(question);
    push_query(retrieval_question.to_string(), &mut queries);

    // Priority 2 — IR focus spans. The compiler has already isolated
    // the entities and literals the user cares about, so these short
    // queries rescue rare exact tokens that a broad sentence-level BM25
    // query can drown in common words.
    for focus in query_ir_focus_queries {
        push_query(focus.clone(), &mut queries);
    }

    // Priority 3 — the plan's combined hi + lo keyword phrase.
    push_query(request_safe_query(plan), &mut queries);

    // Priority 4 — for exact-literal technical queries (port numbers,
    // error codes, config keys), push focus segments for narrow recall.
    // The focus-keyword helper is still useful when the compiler did
    // not emit typed literal constraints: it degrades to structural
    // token segments without hard-coded vocabulary.
    if plan.intent_profile.exact_literal_technical {
        for segment in technical_literal_focus_keyword_segments(retrieval_question, query_ir) {
            push_query(segment.join(" "), &mut queries);
        }
    }

    // Priority 5 — plan-derived keyword variants. Hi/lo splits first
    // (they collapse the user's question to the canonical nouns),
    // then individual keywords for narrow-tail recall on corpora
    // where the vector space is sparse.
    if !plan.high_level_keywords.is_empty() {
        push_query(plan.high_level_keywords.join(" "), &mut queries);
    }
    if !plan.low_level_keywords.is_empty() {
        push_query(plan.low_level_keywords.join(" "), &mut queries);
    }
    if plan.keywords.len() > 1 {
        push_query(plan.keywords.join(" "), &mut queries);
    }
    for keyword in plan.keywords.iter().take(MAX_LEXICAL_QUERIES) {
        push_query(keyword.clone(), &mut queries);
    }

    queries
}

pub(crate) fn build_graph_evidence_text_queries(
    question: &str,
    plan: &RuntimeQueryPlan,
    query_ir_focus_queries: &[String],
    query_ir: Option<&QueryIR>,
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut queries = Vec::new();

    let mut push_query = |value: String, queries: &mut Vec<String>| {
        if queries.len() >= MAX_GRAPH_EVIDENCE_TEXT_QUERIES {
            return;
        }
        let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized.is_empty() || !seen.insert(normalized.to_lowercase()) {
            return;
        }
        queries.push(normalized);
    };

    // Graph evidence text lookup is backed by Postgres full-text/trigram
    // indexes over the activated evidence table. Keep the first compiler
    // focus spans ahead of the broad prose question, but keep the raw
    // question inside the bounded DB-facing budget as the canonical recall
    // fallback when the compiler produced narrow focus spans.
    for focus in query_ir_focus_queries.iter().take(3) {
        push_query(focus.clone(), &mut queries);
    }
    let retrieval_question = strip_leading_question_marker(question);
    push_query(retrieval_question.to_string(), &mut queries);
    for focus in query_ir_focus_queries.iter().skip(3) {
        push_query(focus.clone(), &mut queries);
    }

    if plan.intent_profile.exact_literal_technical {
        for segment in technical_literal_focus_keyword_segments(retrieval_question, query_ir) {
            push_query(segment.join(" "), &mut queries);
        }
    }

    push_query(request_safe_query(plan), &mut queries);
    if !plan.high_level_keywords.is_empty() {
        push_query(plan.high_level_keywords.join(" "), &mut queries);
    }
    if !plan.low_level_keywords.is_empty() {
        push_query(plan.low_level_keywords.join(" "), &mut queries);
    }
    if plan.keywords.len() > 1 {
        push_query(plan.keywords.join(" "), &mut queries);
    }
    queries
}

pub(crate) fn graph_evidence_db_text_queries(text_queries: &[String]) -> Vec<String> {
    text_queries.iter().take(MAX_GRAPH_EVIDENCE_DB_TEXT_QUERIES).cloned().collect()
}

pub(crate) fn query_ir_lexical_focus_queries(query_ir: &QueryIR) -> Vec<String> {
    const MAX_QUERY_IR_FOCUS_QUERIES: usize = 6;
    const MAX_QUERY_IR_FOCUS_CHARS: usize = 160;

    let mut seen = BTreeSet::new();
    let mut queries = Vec::new();
    let mut push_focus = |value: &str, queries: &mut Vec<String>| {
        if queries.len() >= MAX_QUERY_IR_FOCUS_QUERIES {
            return;
        }
        let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if !is_usable_query_ir_focus(&normalized) || !seen.insert(normalized.to_lowercase()) {
            return;
        }
        let bounded = normalized.chars().take(MAX_QUERY_IR_FOCUS_CHARS).collect::<String>();
        queries.push(bounded);
    };

    let (mut primary_focus_values, mut modifier_focus_values) =
        query_ir_focus_value_groups(query_ir);
    sort_query_ir_focus_values_by_specificity(&mut primary_focus_values);
    sort_query_ir_focus_values_by_specificity(&mut modifier_focus_values);
    if query_ir_document_focus_should_anchor_focus_queries(query_ir)
        && let Some(document_focus) = &query_ir.document_focus
    {
        let mut anchored_compounds =
            document_focus_anchored_focus_compounds(&document_focus.hint, &primary_focus_values);
        sort_query_ir_focus_values_by_specificity(&mut anchored_compounds);
        for compound in &anchored_compounds {
            push_focus(compound, &mut queries);
        }
        push_focus(&document_focus.hint, &mut queries);
    }
    let compound_values = query_ir_compound_focus_values(query_ir);
    let mut compounds = adjacent_query_ir_focus_compounds(&compound_values);
    sort_query_ir_focus_values_by_specificity(&mut compounds);
    for compound in &compounds {
        push_focus(&compound, &mut queries);
    }
    for focus in &primary_focus_values {
        push_focus(focus, &mut queries);
    }
    for focus in &modifier_focus_values {
        push_focus(focus, &mut queries);
    }
    if queries.is_empty()
        && let Some(document_focus) = &query_ir.document_focus
    {
        push_focus(&document_focus.hint, &mut queries);
    }
    queries
}

fn query_ir_document_focus_should_anchor_focus_queries(query_ir: &QueryIR) -> bool {
    query_ir.document_focus.is_some()
        && matches!(query_ir.scope, QueryScope::SingleDocument)
        && matches!(query_ir.act, QueryAct::Compare | QueryAct::ConfigureHow)
}

fn document_focus_anchored_focus_compounds(
    document_focus: &str,
    primary_focus_values: &[String],
) -> Vec<String> {
    let normalized_focus = document_focus.split_whitespace().collect::<Vec<_>>().join(" ");
    if !is_usable_query_ir_focus(&normalized_focus) {
        return Vec::new();
    }
    let focus_key = normalized_focus.to_lowercase();
    let mut seen = BTreeSet::new();
    let mut compounds = Vec::new();
    for primary in primary_focus_values {
        let normalized_primary = primary.split_whitespace().collect::<Vec<_>>().join(" ");
        if !is_usable_query_ir_focus(&normalized_primary)
            || normalized_primary.to_lowercase() == focus_key
        {
            continue;
        }
        let compound = format!("{normalized_focus} {normalized_primary}");
        if seen.insert(compound.to_lowercase()) {
            compounds.push(compound);
        }
    }
    compounds
}

fn query_ir_compound_focus_values(query_ir: &QueryIR) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut values = Vec::new();
    let mut push_value = |value: &str, values: &mut Vec<String>| {
        let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if !is_usable_query_ir_focus(&normalized) || !seen.insert(normalized.to_lowercase()) {
            return;
        }
        values.push(normalized);
    };

    let focus_uses_target_entities = query_ir_has_focused_document_answer_intent(query_ir)
        && !query_ir.target_entities.is_empty();
    if focus_uses_target_entities {
        let (primary_entity_values, _) = query_ir_entity_focus_value_groups(query_ir);
        for entity_value in primary_entity_values {
            push_value(&entity_value, &mut values);
        }
    } else {
        for literal in &query_ir.literal_constraints {
            push_value(&literal.text, &mut values);
        }
        let (primary_entity_values, _) = query_ir_entity_focus_value_groups(query_ir);
        for entity_value in primary_entity_values {
            push_value(&entity_value, &mut values);
        }
    }
    values
}

fn query_ir_entity_focus_value_groups(query_ir: &QueryIR) -> (Vec<String>, Vec<String>) {
    let mut primary_values = Vec::new();
    let mut modifier_values = Vec::new();
    for entity in &query_ir.target_entities {
        match entity.role {
            EntityRole::Subject | EntityRole::Object => {
                primary_values.push(entity.label.clone());
            }
            EntityRole::Modifier => {
                modifier_values.push(entity.label.clone());
            }
        }
    }
    (primary_values, modifier_values)
}

fn query_ir_focus_value_groups(query_ir: &QueryIR) -> (Vec<String>, Vec<String>) {
    let mut seen = BTreeSet::new();
    let mut primary_values = Vec::new();
    let mut modifier_values = Vec::new();
    let mut push_value = |value: &str, values: &mut Vec<String>| {
        let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if !is_usable_query_ir_focus(&normalized) || !seen.insert(normalized.to_lowercase()) {
            return;
        }
        values.push(normalized);
    };

    let focus_uses_target_entities = query_ir_has_focused_document_answer_intent(query_ir)
        && !query_ir.target_entities.is_empty();
    for temporal in &query_ir.temporal_constraints {
        for focus in temporal_constraint_focus_values(temporal) {
            push_value(&focus, &mut primary_values);
        }
    }
    if focus_uses_target_entities {
        let (primary_entity_values, modifier_entity_values) =
            query_ir_entity_focus_value_groups(query_ir);
        for entity_value in primary_entity_values {
            push_value(&entity_value, &mut primary_values);
        }
        for entity_value in modifier_entity_values {
            push_value(&entity_value, &mut modifier_values);
        }
    } else {
        for literal in &query_ir.literal_constraints {
            push_value(&literal.text, &mut primary_values);
        }
        let (primary_entity_values, modifier_entity_values) =
            query_ir_entity_focus_value_groups(query_ir);
        for entity_value in primary_entity_values {
            push_value(&entity_value, &mut primary_values);
        }
        for entity_value in modifier_entity_values {
            push_value(&entity_value, &mut modifier_values);
        }
    }
    (primary_values, modifier_values)
}

fn adjacent_query_ir_focus_compounds(focus_values: &[String]) -> Vec<String> {
    if focus_values.len() < 2 {
        return Vec::new();
    }
    focus_values.windows(2).map(|window| window.join(" ")).collect::<Vec<_>>()
}

fn sort_query_ir_focus_values_by_specificity(values: &mut [String]) {
    values.sort_by(|left, right| {
        query_ir_focus_specificity_score(right)
            .cmp(&query_ir_focus_specificity_score(left))
            .then_with(|| left.cmp(right))
    });
}

fn query_ir_focus_specificity_score(value: &str) -> usize {
    let tokens = normalized_alnum_token_sequence(value, 2);
    if tokens.is_empty() {
        return 0;
    }
    let token_score = tokens
        .iter()
        .map(|token| {
            let char_count = token.chars().count();
            let numeric_bonus = token.chars().any(char::is_numeric) as usize * 16;
            char_count.saturating_add(numeric_bonus)
        })
        .sum::<usize>();
    let structural_bonus =
        value.chars().any(|ch| !ch.is_alphanumeric() && !ch.is_whitespace()) as usize * 8;
    token_score.saturating_add(tokens.len().saturating_mul(4)).saturating_add(structural_bonus)
}

fn temporal_constraint_focus_values(temporal: &TemporalConstraint) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut values = Vec::new();
    let mut push = |value: String, values: &mut Vec<String>| {
        let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized.is_empty() || !seen.insert(normalized.to_lowercase()) {
            return;
        }
        values.push(normalized);
    };

    let start = temporal.start.as_deref().and_then(parse_temporal_bound);
    let end = temporal.end.as_deref().and_then(parse_temporal_bound);
    if let Some(start) = start {
        for prefix in temporal_bound_prefixes(start, end) {
            push(prefix, &mut values);
        }
    }
    if values.is_empty() {
        push(temporal.surface.clone(), &mut values);
    }
    values
}

fn parse_temporal_bound(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value).ok().map(|parsed| parsed.with_timezone(&Utc))
}

fn temporal_bound_prefixes(start: DateTime<Utc>, end: Option<DateTime<Utc>>) -> Vec<String> {
    const MAX_TEMPORAL_PREFIXES: usize = 8;

    let mut prefixes = Vec::new();
    let push_unique = |value: String, prefixes: &mut Vec<String>| {
        if prefixes.len() >= MAX_TEMPORAL_PREFIXES
            || prefixes.iter().any(|existing| existing == &value)
        {
            return;
        }
        prefixes.push(value);
    };

    let day_prefix = iso_day_prefix(start);
    let month_prefix = iso_month_prefix(start.year(), start.month());
    let range_seconds = end.map(|end| end.signed_duration_since(start).num_seconds());
    let single_day_range = range_seconds.is_some_and(|seconds| seconds > 0 && seconds <= 86_400);

    if single_day_range {
        push_unique(day_prefix, &mut prefixes);
        push_unique(month_prefix, &mut prefixes);
    } else {
        if let Some(end) = end {
            let month_span = temporal_month_span(start, end);
            if month_span > 6 {
                push_unique(format!("{:04}", start.year()), &mut prefixes);
            }
            for (year, month) in temporal_month_prefix_range(start, end) {
                push_unique(iso_month_prefix(year, month), &mut prefixes);
            }
        }
        push_unique(month_prefix, &mut prefixes);
        push_unique(day_prefix, &mut prefixes);
    }

    prefixes
}

fn temporal_month_prefix_range(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> impl Iterator<Item = (i32, u32)> {
    let end_key = (end.year(), end.month());
    let mut year = start.year();
    let mut month = start.month();
    std::iter::from_fn(move || {
        if (year, month) >= end_key {
            return None;
        }
        let current = (year, month);
        (year, month) = next_month(year, month);
        Some(current)
    })
    .take(12)
}

fn temporal_month_span(start: DateTime<Utc>, end: DateTime<Utc>) -> i32 {
    let years = end.year().saturating_sub(start.year());
    let end_month = i32::try_from(end.month()).unwrap_or(12);
    let start_month = i32::try_from(start.month()).unwrap_or(1);
    years.saturating_mul(12).saturating_add(end_month.saturating_sub(start_month))
}

const fn next_month(year: i32, month: u32) -> (i32, u32) {
    if month >= 12 { (year + 1, 1) } else { (year, month + 1) }
}

fn iso_day_prefix(value: DateTime<Utc>) -> String {
    format!("{:04}-{:02}-{:02}", value.year(), value.month(), value.day())
}

fn iso_month_prefix(year: i32, month: u32) -> String {
    format!("{year:04}-{month:02}")
}

pub(crate) fn query_ir_focus_search_queries(
    question: &str,
    focus_queries: &[String],
) -> Vec<String> {
    const MAX_QUERY_IR_FOCUS_SEARCH_QUERIES: usize = 5;
    const MAX_QUERY_IR_FOCUS_CHARS: usize = 160;

    let mut seen = BTreeSet::new();
    let mut queries = Vec::new();
    let mut push_focus = |value: &str, queries: &mut Vec<String>| {
        if queries.len() >= MAX_QUERY_IR_FOCUS_SEARCH_QUERIES {
            return;
        }
        let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if !is_usable_query_ir_focus(&normalized) || !seen.insert(normalized.to_lowercase()) {
            return;
        }
        queries.push(normalized.chars().take(MAX_QUERY_IR_FOCUS_CHARS).collect());
    };

    for focus_query in focus_queries {
        push_focus(focus_query, &mut queries);
    }
    if queries.is_empty() {
        push_focus(question, &mut queries);
    }

    queries
}

pub(crate) fn linked_anchor_focus_queries(
    question: &str,
    query_ir: Option<&QueryIR>,
    plan_keywords: &[String],
    chunks: &[RuntimeMatchedChunk],
) -> Vec<String> {
    let focus_tokens = linked_anchor_focus_tokens(question, query_ir, plan_keywords);
    if focus_tokens.is_empty() {
        return Vec::new();
    }

    let mut scored_labels = Vec::<(usize, String)>::new();
    let mut seen = BTreeSet::new();
    for chunk in chunks {
        for label in markdown_link_labels(&chunk.source_text)
            .into_iter()
            .chain(markdown_link_labels(&chunk.excerpt))
        {
            let normalized = label.split_whitespace().collect::<Vec<_>>().join(" ");
            if !is_usable_query_ir_focus(&normalized)
                || normalized.chars().count() > 120
                || !seen.insert(normalized.to_lowercase())
            {
                continue;
            }
            let overlap = linked_anchor_token_overlap(&normalized, &focus_tokens);
            if overlap > 0 {
                scored_labels.push((overlap, normalized));
            }
        }
    }
    scored_labels.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    let mut queries = Vec::new();
    let mut seen_queries = BTreeSet::new();
    for (_, label) in scored_labels {
        for query in linked_anchor_query_variants(&label) {
            if seen_queries.insert(query.to_lowercase()) {
                queries.push(query);
            }
            if queries.len() >= LINKED_ANCHOR_CONTEXT_QUERY_CAP {
                return queries;
            }
        }
    }
    queries
}

fn linked_anchor_query_variants(label: &str) -> Vec<String> {
    let normalized = label.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return Vec::new();
    }

    let mut variants = Vec::new();
    let mut seen = BTreeSet::new();
    let mut push_variant = |value: String, variants: &mut Vec<String>| {
        let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if !normalized.is_empty() && seen.insert(normalized.to_lowercase()) {
            variants.push(normalized);
        }
    };

    push_variant(normalized.clone(), &mut variants);

    let lexical_tokens = normalized_alnum_token_sequence(&normalized, 2)
        .into_iter()
        .filter(|token| token.chars().any(|ch| ch.is_alphabetic()))
        .collect::<Vec<_>>();
    if lexical_tokens.is_empty() {
        return variants;
    }

    push_variant(lexical_tokens.join(" "), &mut variants);
    for (index, token) in lexical_tokens.iter().enumerate() {
        if token.chars().count() <= LINKED_ANCHOR_CONTEXT_QUERY_PREFIX_CHARS {
            continue;
        }
        let mut prefix_tokens = lexical_tokens.clone();
        prefix_tokens[index] =
            token.chars().take(LINKED_ANCHOR_CONTEXT_QUERY_PREFIX_CHARS).collect();
        push_variant(prefix_tokens.join(" "), &mut variants);
    }

    variants
}

fn linked_anchor_focus_tokens(
    question: &str,
    query_ir: Option<&QueryIR>,
    plan_keywords: &[String],
) -> BTreeSet<String> {
    let mut tokens = normalized_alnum_tokens(question, 3);
    for keyword in plan_keywords {
        tokens.extend(normalized_alnum_tokens(keyword, 3));
    }
    if let Some(query_ir) = query_ir {
        if let Some(document_focus) = query_ir.document_focus.as_ref() {
            tokens.extend(normalized_alnum_tokens(&document_focus.hint, 3));
        }
        for entity in &query_ir.target_entities {
            tokens.extend(normalized_alnum_tokens(&entity.label, 3));
        }
        for literal in &query_ir.literal_constraints {
            tokens.extend(normalized_alnum_tokens(&literal.text, 3));
        }
    }
    tokens
}

fn markdown_link_labels(value: &str) -> Vec<&str> {
    let mut labels = Vec::new();
    let mut search_from = 0;
    while let Some(open_rel) = value[search_from..].find('[') {
        let open = search_from + open_rel;
        let label_start = open + '['.len_utf8();
        let Some(close_rel) = value[label_start..].find(']') else {
            break;
        };
        let close = label_start + close_rel;
        let after_close = close + ']'.len_utf8();
        if value[after_close..].starts_with('(') {
            let label = value[label_start..close].trim();
            if !label.is_empty() {
                labels.push(label);
            }
        }
        search_from = after_close;
    }
    labels
}

fn linked_anchor_token_overlap(label: &str, focus_tokens: &BTreeSet<String>) -> usize {
    let label_tokens = normalized_alnum_tokens(label, 3);
    label_tokens
        .iter()
        .filter(|label_token| {
            focus_tokens
                .iter()
                .any(|focus_token| linked_anchor_token_match(label_token, focus_token))
        })
        .count()
}

fn linked_anchor_token_match(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    let left_len = left.chars().count();
    let right_len = right.chars().count();
    let min_len = left_len.min(right_len);
    if min_len < 4 {
        return false;
    }
    left.contains(right)
        || right.contains(left)
        || common_prefix_char_count(left, right) >= LINKED_ANCHOR_CONTEXT_PREFIX_CHARS
}

fn is_usable_query_ir_focus(value: &str) -> bool {
    let alphanumeric_count = value.chars().filter(|ch| ch.is_alphanumeric()).count();
    alphanumeric_count >= 2
}

pub(crate) fn request_safe_query(plan: &RuntimeQueryPlan) -> String {
    if !plan.low_level_keywords.is_empty() {
        let combined =
            format!("{} {}", plan.high_level_keywords.join(" "), plan.low_level_keywords.join(" "));
        return combined.trim().to_string();
    }
    plan.keywords.join(" ")
}

pub(crate) fn map_chunk_hit(
    chunk: KnowledgeChunkRow,
    score: f32,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    keywords: &[String],
) -> Option<RuntimeMatchedChunk> {
    let document = document_index.get(&chunk.document_id)?;
    // Require the document to have AT LEAST ONE head pointer (readable
    // or active). This drops orphan chunks whose document was deleted /
    // never promoted. We do NOT compare `chunk.revision_id` to the
    // canonical head: when a document has multiple revisions with
    // persisted chunks (e.g. partial incremental re-ingest where the
    // newer head revision is a subset of the older complete one),
    // strict equality can hide a large fraction of historical chunks.
    // Downstream dedup by `chunk_id` keeps the result set clean against
    // any cross-revision duplicates. A future ingest cleanup should
    // resolve the underlying
    // ingest issue (every re-ingest must produce a head revision that
    // strictly supersedes the prior one); until then this guard
    // surfaces all data the documents actually contain.
    let _has_canonical_head = canonical_document_revision_id(document)?;
    let source_text = chunk_answer_source_text(&chunk);
    let excerpt = if chunk.chunk_kind.as_deref() == Some("source_unit") {
        focused_record_unit_excerpt(&source_text, keywords, 280)
            .unwrap_or_else(|| focused_excerpt_for(&source_text, keywords, 280))
    } else {
        focused_excerpt_for(&source_text, keywords, 280)
    };
    Some(RuntimeMatchedChunk {
        chunk_id: chunk.chunk_id,
        document_id: chunk.document_id,
        revision_id: chunk.revision_id,
        chunk_index: chunk.chunk_index,
        chunk_kind: chunk.chunk_kind.clone(),
        document_label: document
            .title
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| document.external_key.clone()),
        excerpt,
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
        score: Some(score),
        source_text,
    })
}

pub(crate) fn retain_canonical_document_head_chunks(
    chunks: &mut Vec<RuntimeMatchedChunk>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> usize {
    let before = chunks.len();
    // Mirror of `map_chunk_hit` relaxation (2026-05-03 stage incident:
    // 41% of all chunks dropped by strict-equality gate). Require the
    // document to have at least one head pointer (drops orphan chunks
    // whose document was deleted / never promoted) but accept any
    // revision_id — partial incremental re-ingest leaves valid older
    // chunks under non-head revisions and the strict gate would hide
    // them. Downstream chunk_id dedup handles cross-revision duplicates.
    chunks.retain(|chunk| {
        document_index.get(&chunk.document_id).and_then(canonical_document_revision_id).is_some()
    });
    before.saturating_sub(chunks.len())
}

fn chunk_answer_source_text(chunk: &KnowledgeChunkRow) -> String {
    if chunk.chunk_kind.as_deref() == Some("table_row") {
        return repair_technical_layout_noise(&chunk.normalized_text);
    }
    if chunk.chunk_kind.as_deref() == Some("source_unit") {
        if !chunk.content_text.trim().is_empty() {
            return repair_technical_layout_noise(&chunk.content_text);
        }
        if !chunk.normalized_text.trim().is_empty() {
            return repair_technical_layout_noise(&chunk.normalized_text);
        }
    }
    let content_text = (!chunk.content_text.trim().is_empty())
        .then(|| repair_technical_layout_noise(&chunk.content_text));
    let window_text = chunk
        .window_text
        .as_deref()
        .filter(|window| !window.trim().is_empty())
        .map(repair_technical_layout_noise);
    let normalized_text = (!chunk.normalized_text.trim().is_empty())
        .then(|| repair_technical_layout_noise(&chunk.normalized_text));
    let fallback = if content_text.is_some() {
        merge_chunk_source_text_variants([content_text.as_deref(), window_text.as_deref()])
    } else {
        merge_chunk_source_text_variants([window_text.as_deref(), normalized_text.as_deref()])
    };
    chunk_literal_preserving_source_text(chunk, &fallback).unwrap_or(fallback)
}

fn merge_chunk_source_text_variants<const N: usize>(values: [Option<&str>; N]) -> String {
    const MAX_MERGED_CHUNK_SOURCE_CHARS: usize = 16_000;

    let mut parts = Vec::new();
    let mut seen = HashSet::new();
    for value in values.into_iter().flatten() {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        let normalized = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
        if seen.iter().any(|seen_value: &String| {
            seen_value.contains(&normalized) || normalized.contains(seen_value)
        }) {
            continue;
        }
        if seen.insert(normalized) {
            parts.push(trimmed.to_string());
        }
    }
    let merged = parts.join("\n");
    if merged.chars().count() <= MAX_MERGED_CHUNK_SOURCE_CHARS {
        merged
    } else {
        excerpt_for(&merged, MAX_MERGED_CHUNK_SOURCE_CHARS)
    }
}

fn chunk_literal_preserving_source_text(
    chunk: &KnowledgeChunkRow,
    fallback: &str,
) -> Option<String> {
    const MAX_LITERAL_PRESERVING_SOURCE_CHARS: usize = 16_000;

    let mut parts = Vec::new();
    let mut seen = HashSet::new();
    for value in [
        chunk.window_text.as_deref(),
        Some(chunk.content_text.as_str()),
        Some(chunk.normalized_text.as_str()),
    ]
    .into_iter()
    .flatten()
    {
        let repaired = repair_technical_layout_noise(value);
        let trimmed = repaired.trim();
        if trimmed.is_empty() {
            continue;
        }
        let normalized = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
        if seen.insert(normalized) {
            parts.push(trimmed.to_string());
        }
    }
    if parts.len() <= 1 {
        return None;
    }
    let candidate = parts.join("\n");
    let candidate_score = chunk_structured_literal_text_score(&candidate);
    if candidate_score == 0 {
        return None;
    }
    let fallback_score = chunk_structured_literal_text_score(fallback);
    if candidate_score <= fallback_score
        && !chunk_text_preserves_missing_structured_literals(&candidate, fallback)
    {
        return None;
    }
    if candidate.chars().count() <= MAX_LITERAL_PRESERVING_SOURCE_CHARS {
        return Some(candidate);
    }
    Some(excerpt_for(&candidate, MAX_LITERAL_PRESERVING_SOURCE_CHARS))
}

fn chunk_structured_literal_text_score(text: &str) -> usize {
    text.lines().map(str::trim).map(chunk_structured_literal_line_score).sum()
}

fn chunk_structured_literal_line_score(line: &str) -> usize {
    extract_config_assignment_literals(line, 4).len().saturating_mul(8)
        + extract_config_section_literals(line, 4).len().saturating_mul(6)
        + extract_explicit_path_literals(line, 4).len().saturating_mul(5)
        + extract_package_command_literals(line, 2).len().saturating_mul(5)
        + extract_parameter_literals(line, 8).len().saturating_mul(3)
        + usize::from(chunk_line_has_key_value_literal_surface(line)).saturating_mul(3)
        + usize::from(chunk_line_has_table_like_literal_surface(line)).saturating_mul(2)
}

fn chunk_line_has_key_value_literal_surface(line: &str) -> bool {
    let Some((key, value)) = line.split_once('=') else {
        return false;
    };
    let key = key.trim().trim_start_matches(['-', '*']).trim();
    let value = value.trim();
    key.chars().any(|ch| ch.is_alphabetic())
        && key.chars().filter(|ch| ch.is_alphanumeric() || *ch == '_' || *ch == '-').count() >= 2
        && !value.is_empty()
        && !key.contains("==")
}

fn chunk_line_has_table_like_literal_surface(line: &str) -> bool {
    let alphanumeric_count = line.chars().filter(|ch| ch.is_alphanumeric()).count();
    alphanumeric_count >= 3
        && (line.matches('|').count() >= 2
            || line.split('\t').filter(|cell| !cell.trim().is_empty()).count() >= 3)
}

fn chunk_text_preserves_missing_structured_literals(candidate: &str, fallback: &str) -> bool {
    chunk_structured_literals(candidate, 32).iter().any(|literal| !fallback.contains(literal))
}

fn chunk_structured_literals(text: &str, limit: usize) -> Vec<String> {
    let mut values = Vec::new();
    let mut seen = HashSet::new();
    for value in extract_config_assignment_literals(text, limit) {
        push_unique_chunk_literal(&mut values, &mut seen, value, limit);
    }
    for value in extract_config_section_literals(text, limit) {
        push_unique_chunk_literal(&mut values, &mut seen, value, limit);
    }
    for value in extract_explicit_path_literals(text, limit) {
        push_unique_chunk_literal(&mut values, &mut seen, value, limit);
    }
    for value in extract_package_command_literals(text, limit) {
        push_unique_chunk_literal(&mut values, &mut seen, value, limit);
    }
    for value in extract_parameter_literals(text, limit) {
        push_unique_chunk_literal(&mut values, &mut seen, value, limit);
    }
    values
}

fn push_unique_chunk_literal(
    values: &mut Vec<String>,
    seen: &mut HashSet<String>,
    value: String,
    limit: usize,
) {
    if values.len() >= limit {
        return;
    }
    if seen.insert(value.to_lowercase()) {
        values.push(value);
    }
}

pub(crate) fn canonical_document_revision_id(document: &KnowledgeDocumentRow) -> Option<Uuid> {
    document.readable_revision_id.or(document.active_revision_id)
}

#[cfg(test)]
mod document_index_tests {
    use chrono::TimeZone;

    use crate::domains::query_ir::{
        EntityMention, LiteralSpan, QueryLanguage, QueryScope, SourceSliceSpec, UnresolvedRef,
    };

    use super::*;

    #[test]
    fn postgres_document_index_row_maps_canonical_heads() {
        let document_id = Uuid::now_v7();
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let active_revision_id = Uuid::now_v7();
        let readable_revision_id = Uuid::now_v7();
        let created_at = Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap();
        let updated_at = Utc.with_ymd_and_hms(2026, 1, 3, 3, 4, 5).unwrap();

        let row = query_document_index_row_to_knowledge_document_row(QueryDocumentIndexRow {
            document_id,
            workspace_id,
            library_id,
            external_key: "event-stream.jsonl".to_string(),
            title: Some("Event Stream".to_string()),
            source_uri: Some("https://example.invalid/events".to_string()),
            document_hint: None,
            document_state: "active".to_string(),
            active_revision_id: Some(active_revision_id),
            readable_revision_id: Some(readable_revision_id),
            latest_revision_no: Some(4),
            parent_document_id: None,
            document_role: crate::domains::content::DOCUMENT_ROLE_PRIMARY.to_string(),
            created_at,
            updated_at,
            deleted_at: None,
        });

        assert_eq!(row.document_id, document_id);
        assert_eq!(row.workspace_id, workspace_id);
        assert_eq!(row.library_id, library_id);
        assert_eq!(row.external_key, "event-stream.jsonl");
        assert_eq!(row.file_name.as_deref(), Some("event-stream.jsonl"));
        assert_eq!(row.title.as_deref(), Some("Event Stream"));
        assert_eq!(row.source_uri.as_deref(), Some("https://example.invalid/events"));
        assert_eq!(row.document_hint.as_deref(), None);
        assert_eq!(canonical_document_revision_id(&row), Some(readable_revision_id));
        assert_eq!(row.active_revision_id, Some(active_revision_id));
        assert_eq!(row.latest_revision_no, Some(4));
        assert_eq!(row.updated_at, updated_at);
    }

    #[test]
    fn semantic_latest_candidates_use_structural_identity_or_scope_signal() {
        let versioned = document_row("alpha-9.8.7.html", "Alpha 9.8.7");
        let reference = document_row("alpha-reference.html", "Alpha reference");
        let image_versioned = document_row("alpha-9.8.7.svg", "Alpha 9.8.7 diagram");
        let versioned_id = versioned.document_id;
        let document_index = HashMap::from([
            (versioned.document_id, versioned),
            (reference.document_id, reference),
            (image_versioned.document_id, image_versioned),
        ]);

        let selected = latest_version_semantic_candidate_documents(&document_index, &[], 8);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].document_id, versioned_id);

        let scoped =
            latest_version_semantic_candidate_documents(&document_index, &["alpha".to_string()], 8);
        assert!(scoped.iter().any(|document| document.document_id == versioned_id));
    }

    #[test]
    fn semantic_latest_candidates_keep_large_scoped_sets() {
        let documents = (0..40)
            .map(|index| document_row(&format!("alpha-module-{index}.html"), "Alpha module"))
            .collect::<Vec<_>>();
        let document_index =
            documents.iter().cloned().map(|document| (document.document_id, document)).collect();

        let selected = latest_version_semantic_candidate_documents(
            &document_index,
            &["alpha".to_string()],
            LATEST_VERSION_SEMANTIC_DOCUMENT_CANDIDATE_CAP,
        );

        assert_eq!(selected.len(), documents.len());
    }

    #[test]
    fn semantic_latest_structural_probe_uses_version_density_not_identity_words() {
        let dense = document_row("neutral-ledger.html", "Neutral Ledger");
        let dense_id = dense.document_id;
        let plain = document_row("neutral-manual.html", "Neutral Manual");
        let document_index =
            HashMap::from([(dense.document_id, dense.clone()), (plain.document_id, plain)]);

        assert_eq!(
            latest_version_identity_structural_score(
                &latest_version_document_identity_text(&dense),
                &[]
            ),
            0
        );

        let candidates = latest_version_structural_probe_candidate_documents(
            &document_index,
            &[],
            &BTreeSet::new(),
            8,
        );
        assert!(candidates.iter().any(|candidate| candidate.document_id == dense_id));

        let source = concat!(
            "1. 9.8.7 | token-a = enabled\n",
            "2. 9.8.6 | token-b = disabled\n",
            "3. 9.8.5 | /opt/neutral/config"
        );
        let versions = latest_version_context_versions(source);
        assert!(latest_version_structural_inventory_candidate_has_density(
            versions.len(),
            1,
            latest_version_structural_inventory_text_score(source),
        ));
    }

    #[test]
    fn semantic_latest_unscoped_tail_prefers_dense_source_local_inventory() {
        let dense = document_row("neutral-ledger.html", "Neutral Ledger");
        let sparse = document_row("neutral-release-99.html", "Neutral Release 99.0.0");
        let dense_revision_id = canonical_document_revision_id(&dense).unwrap();
        let sparse_revision_id = canonical_document_revision_id(&sparse).unwrap();
        let mut rows = (0..5)
            .map(|offset| LatestVersionSemanticRow {
                version: vec![1, 0, offset],
                document_rank: 20,
                structural_density_score: 80,
                from_structural_inventory: true,
                row: chunk_row(
                    dense.workspace_id,
                    dense.library_id,
                    dense.document_id,
                    dense_revision_id,
                    100 + offset as i32,
                    &format!("{} | neutral row", 1 + offset),
                ),
            })
            .collect::<Vec<_>>();
        rows.push(LatestVersionSemanticRow {
            version: vec![99, 0, 0],
            document_rank: 1,
            structural_density_score: 4,
            from_structural_inventory: true,
            row: chunk_row(
                sparse.workspace_id,
                sparse.library_id,
                sparse.document_id,
                sparse_revision_id,
                1,
                "99.0.0 | sparse row",
            ),
        });
        for index in 0..40 {
            let document = document_row(
                &format!("neutral-sparse-{index}.html"),
                &format!("Neutral sparse {index}"),
            );
            let revision_id = canonical_document_revision_id(&document).unwrap();
            rows.push(LatestVersionSemanticRow {
                version: vec![50, index],
                document_rank: 30 + index as usize,
                structural_density_score: 4,
                from_structural_inventory: true,
                row: chunk_row(
                    document.workspace_id,
                    document.library_id,
                    document.document_id,
                    revision_id,
                    1,
                    &format!("50.{index} | sparse row"),
                ),
            });
        }

        let unscoped_tail = order_latest_version_semantic_rows(rows.clone(), true, 10);

        assert_eq!(unscoped_tail[0].row.document_id, dense.document_id);
        assert_eq!(unscoped_tail[0].row.chunk_index, 104);
        assert!(
            unscoped_tail
                .iter()
                .take(LATEST_VERSION_SEMANTIC_CHUNK_CAP)
                .filter(|row| row.row.document_id == dense.document_id)
                .count()
                >= 3,
            "{:?}",
            unscoped_tail
                .iter()
                .take(LATEST_VERSION_SEMANTIC_CHUNK_CAP)
                .map(|row| (row.row.document_id, row.row.chunk_index))
                .collect::<Vec<_>>()
        );

        let scoped_or_version_sorted = order_latest_version_semantic_rows(rows, false, 10);

        assert_eq!(scoped_or_version_sorted[0].row.document_id, sparse.document_id);
    }

    #[test]
    fn semantic_latest_unscoped_tail_does_not_starve_dense_inventory_with_identity_prefix() {
        let mut rows = Vec::new();
        for index in 0..24 {
            let document = document_row(
                &format!("neutral-identity-{index}.html"),
                &format!("Neutral identity {index}.0.0"),
            );
            let revision_id = canonical_document_revision_id(&document).unwrap();
            rows.push(LatestVersionSemanticRow {
                version: vec![90, index],
                document_rank: index as usize,
                structural_density_score: 0,
                from_structural_inventory: false,
                row: chunk_row(
                    document.workspace_id,
                    document.library_id,
                    document.document_id,
                    revision_id,
                    1,
                    &format!("90.{index} | identity row"),
                ),
            });
        }

        let mut required_dense_document_id = Uuid::nil();
        for index in 0..24 {
            let document = document_row(
                &format!("neutral-dense-{index}.html"),
                &format!("Neutral dense {index}"),
            );
            if index == 15 {
                required_dense_document_id = document.document_id;
            }
            let revision_id = canonical_document_revision_id(&document).unwrap();
            rows.push(LatestVersionSemanticRow {
                version: vec![10, index],
                document_rank: 100 + index as usize,
                structural_density_score: 100usize.saturating_sub(index as usize),
                from_structural_inventory: true,
                row: chunk_row(
                    document.workspace_id,
                    document.library_id,
                    document.document_id,
                    revision_id,
                    50 + index as i32,
                    &format!("10.{index} | dense row"),
                ),
            });
        }

        let ordered = order_latest_version_semantic_rows(rows, true, 10);
        let selected = ordered.iter().take(LATEST_VERSION_SEMANTIC_CHUNK_CAP).collect::<Vec<_>>();

        assert!(selected[0].from_structural_inventory);
        assert!(
            selected.iter().any(|row| row.row.document_id == required_dense_document_id),
            "{:?}",
            selected
                .iter()
                .map(|row| (row.from_structural_inventory, row.structural_density_score))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn setup_focus_candidates_match_focused_title_without_unique_document_target() {
        let target = document_row("target.md", "Subject Alpha setup manual");
        let screenshot = document_row("image.png", "Subject Alpha: screenshot");
        let unrelated = document_row("other.md", "Environment Variant setup manual");
        let document_index = HashMap::from([
            (target.document_id, target.clone()),
            (screenshot.document_id, screenshot.clone()),
            (unrelated.document_id, unrelated),
        ]);
        let mut query_ir = setup_query_ir("Subject Alpha");

        let candidates = setup_focus_candidate_document_ids(&query_ir, &document_index, 8);

        assert_eq!(candidates, [target.document_id]);

        query_ir.source_slice = Some(SourceSliceSpec {
            direction: crate::domains::query_ir::SourceSliceDirection::Tail,
            count: None,
            filter: crate::domains::query_ir::SourceSliceFilter::ReleaseMarker,
        });
        assert!(!query_ir_requests_setup_focus_document_candidates(&query_ir));
    }

    #[test]
    fn setup_focus_candidates_prioritize_entity_focus_before_generic_document_focus() {
        let focused =
            document_row("environment-beta-admin-guide.md", "Environment Variant setup reference");
        let generic = document_row("alpha-suite-admin-guide.md", "Sample Subject administration");
        let screenshot = document_row("environment-beta.png", "Environment Variant subject");
        let document_index = HashMap::from([
            (focused.document_id, focused.clone()),
            (generic.document_id, generic.clone()),
            (screenshot.document_id, screenshot),
        ]);
        let mut query_ir = setup_query_ir("Sample Subject");
        query_ir.target_entities = vec![EntityMention {
            label: "Environment Variant".to_string(),
            role: EntityRole::Object,
        }];

        let candidates = setup_focus_candidate_document_ids(&query_ir, &document_index, 8);

        assert_eq!(candidates.first(), Some(&focused.document_id));
        assert!(candidates.contains(&generic.document_id));
    }

    #[test]
    fn configure_procedure_queries_with_document_focus_request_setup_focus_candidates() {
        // A configure/how-to question that points at one specific document via
        // document_focus must engage the focused-document lane even when the
        // compiler tagged the target only as a generic procedure — otherwise the
        // focused document's own install/config chunks get diluted by a
        // multi-document salad.
        let mut query_ir = setup_query_ir("Sample Subject");
        query_ir.target_types = vec!["procedure".to_string()];

        assert!(query_ir_requests_setup_focus_document_candidates(&query_ir));
    }

    #[test]
    fn configure_procedure_queries_without_focus_or_setup_target_skip_setup_focus() {
        // Without an explicit document_focus and without a command-object/configuration target
        // type, a bare procedure-tagged configure query stays out of the
        // focused-document lane so broad how-to questions are not over-scoped.
        let mut query_ir = setup_query_ir("Sample Subject");
        query_ir.target_types = vec!["procedure".to_string()];
        query_ir.document_focus = None;
        query_ir.target_entities =
            vec![EntityMention { label: "settings".to_string(), role: EntityRole::Object }];

        assert!(!query_ir_requests_setup_focus_document_candidates(&query_ir));
    }

    #[test]
    fn configure_subject_queries_without_setup_target_request_setup_focus_candidates() {
        let mut query_ir = setup_query_ir("Sample Subject");
        query_ir.target_types = vec!["concept".to_string(), "procedure".to_string()];
        query_ir.document_focus = None;
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Subject".to_string(), role: EntityRole::Subject }];

        assert!(query_ir_requests_setup_focus_document_candidates(&query_ir));
        assert_eq!(setup_focus_query_identity_terms(&query_ir), ["Sample Subject".to_string()]);
    }

    #[test]
    fn versioned_update_procedure_queries_skip_setup_focus_document_lane() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.document_focus = None;
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        query_ir.retrieval_query = Some("how to update Sample Target?".to_string());

        assert!(!query_ir_requests_setup_focus_document_candidates(&query_ir));
    }

    #[test]
    fn configure_subject_setup_focus_skips_ambiguous_subject_context() {
        let mut query_ir = setup_query_ir("Sample Subject");
        query_ir.target_types = vec!["concept".to_string(), "procedure".to_string()];
        query_ir.document_focus = None;
        query_ir.target_entities = vec![
            EntityMention { label: "Sample Subject".to_string(), role: EntityRole::Subject },
            EntityMention { label: "Beta Tool".to_string(), role: EntityRole::Subject },
        ];
        assert!(!query_ir_requests_setup_focus_document_candidates(&query_ir));

        query_ir.target_entities =
            vec![EntityMention { label: "Sample Subject".to_string(), role: EntityRole::Subject }];
        query_ir.comparison = Some(crate::domains::query_ir::ComparisonSpec {
            a: None,
            b: None,
            dimension: "compatibility".to_string(),
        });
        assert!(!query_ir_requests_setup_focus_document_candidates(&query_ir));

        query_ir.comparison = None;
        query_ir.literal_constraints =
            vec![LiteralSpan { text: "1.2.3".to_string(), kind: LiteralKind::Version }];
        assert!(!query_ir_requests_setup_focus_document_candidates(&query_ir));
    }

    #[test]
    fn setup_focus_candidates_use_target_entities_without_document_focus() {
        let mut query_ir = setup_query_ir("Sample Subject");
        query_ir.document_focus = None;
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Subject".to_string(), role: EntityRole::Subject }];

        assert!(query_ir_requests_setup_focus_document_candidates(&query_ir));
        assert_eq!(setup_focus_query_identity_terms(&query_ir), vec!["Sample Subject".to_string()]);
    }

    #[test]
    fn setup_focus_candidates_match_short_acronym_subject_exactly() {
        let target = document_row("target.md", "QX setup manual");
        let unrelated = document_row("other.md", "QY setup manual");
        let document_index = HashMap::from([
            (target.document_id, target.clone()),
            (unrelated.document_id, unrelated),
        ]);
        let mut query_ir = setup_query_ir("Sample Subject");
        query_ir.target_types = vec!["concept".to_string(), "procedure".to_string()];
        query_ir.document_focus = None;
        query_ir.target_entities =
            vec![EntityMention { label: "QX".to_string(), role: EntityRole::Subject }];

        assert!(query_ir_requests_setup_focus_document_candidates(&query_ir));
        assert_eq!(
            setup_focus_candidate_document_ids(&query_ir, &document_index, 8),
            [target.document_id]
        );

        query_ir.target_entities =
            vec![EntityMention { label: "qx".to_string(), role: EntityRole::Subject }];
        assert!(!query_ir_requests_setup_focus_document_candidates(&query_ir));
    }

    #[test]
    fn setup_focus_acronym_identity_is_unicode_case_aware() {
        let mut query_ir = setup_query_ir("Sample Subject");
        query_ir.target_types = vec!["concept".to_string(), "procedure".to_string()];
        query_ir.document_focus = None;
        query_ir.target_entities =
            vec![EntityMention { label: "абв".to_string(), role: EntityRole::Subject }];

        assert!(!query_ir_requests_setup_focus_document_candidates(&query_ir));

        query_ir.target_entities =
            vec![EntityMention { label: "АБВ".to_string(), role: EntityRole::Subject }];
        assert!(query_ir_requests_setup_focus_document_candidates(&query_ir));
    }

    #[test]
    fn setup_focus_candidates_ignore_single_token_entity_without_document_focus() {
        let mut query_ir = setup_query_ir("Sample Subject");
        query_ir.document_focus = None;
        query_ir.target_entities =
            vec![EntityMention { label: "settings".to_string(), role: EntityRole::Object }];

        assert!(!query_ir_requests_setup_focus_document_candidates(&query_ir));
    }

    #[test]
    fn setup_focus_candidates_ignore_object_entity_without_document_focus() {
        let mut query_ir = setup_query_ir("Sample Subject");
        query_ir.document_focus = None;
        query_ir.target_entities =
            vec![EntityMention { label: "retry timeout".to_string(), role: EntityRole::Object }];

        assert!(!query_ir_requests_setup_focus_document_candidates(&query_ir));
    }

    #[test]
    fn configure_followup_object_identity_requests_setup_focus_candidates() {
        let target = document_row("delta-pay.html", "DeltaVariant setup reference");
        let unrelated = document_row("alpha-subject.html", "AlphaSubject setup reference");
        let document_index = HashMap::from([
            (target.document_id, target.clone()),
            (unrelated.document_id, unrelated),
        ]);
        let mut query_ir = setup_query_ir("Workflow library");
        query_ir.document_focus = None;
        query_ir.target_types = vec!["concept".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "DeltaVariant".to_string(), role: EntityRole::Object }];
        query_ir.conversation_refs = vec![UnresolvedRef {
            surface: "that".to_string(),
            kind: crate::domains::query_ir::ConversationRefKind::Pronoun,
        }];

        assert!(query_ir_requests_setup_focus_document_candidates(&query_ir));
        assert_eq!(
            setup_focus_candidate_document_ids(&query_ir, &document_index, 8),
            [target.document_id]
        );
    }

    #[test]
    fn setup_focus_candidates_compose_short_subject_with_variant_modifier() {
        let target = document_row("target.html", "Subject BetaVariant setup");
        let changelog = document_row("history.md", "Release history");
        let document_index = HashMap::from([
            (target.document_id, target.clone()),
            (changelog.document_id, changelog),
        ]);
        let mut query_ir = setup_query_ir("Workflow library");
        query_ir.target_types = vec!["concept".to_string()];
        query_ir.document_focus = None;
        query_ir.target_entities = vec![
            EntityMention { label: "Subject".to_string(), role: EntityRole::Subject },
            EntityMention { label: "BetaVariant".to_string(), role: EntityRole::Modifier },
        ];

        assert!(query_ir_requests_setup_focus_document_candidates(&query_ir));
        let terms = setup_focus_query_identity_terms(&query_ir);
        assert_eq!(terms, ["Subject", "Subject BetaVariant"]);
        assert_eq!(
            setup_focus_candidate_document_ids(&query_ir, &document_index, 8),
            [target.document_id]
        );
    }

    #[test]
    fn setup_focus_candidates_prioritize_compound_entity_identity_for_specific_module() {
        let target = document_row("target.html", "Subject DeltaVariant setup guide");
        let neighboring = document_row("neighbor.html", "Subject BetaVariant setup guide");
        let changelog = document_row("history.md", "Subject DeltaVariant release history");
        let document_index = HashMap::from([
            (target.document_id, target.clone()),
            (neighboring.document_id, neighboring.clone()),
            (changelog.document_id, changelog),
        ]);
        let mut query_ir = setup_query_ir("Workflow library");
        query_ir.target_types =
            vec!["concept".to_string(), "artifact".to_string(), "procedure".to_string()];
        query_ir.document_focus = None;
        query_ir.target_entities = vec![
            EntityMention { label: "subject".to_string(), role: EntityRole::Subject },
            EntityMention { label: "deltavariant".to_string(), role: EntityRole::Subject },
        ];

        assert!(query_ir_requests_setup_focus_document_candidates(&query_ir));
        assert_eq!(
            setup_focus_query_identity_terms(&query_ir),
            ["subject deltavariant".to_string(), "subject".to_string(), "deltavariant".to_string()]
        );
        let candidates = setup_focus_candidate_document_ids(&query_ir, &document_index, 8);
        assert_eq!(candidates.first(), Some(&target.document_id));
        assert!(!candidates.contains(&neighboring.document_id));
    }

    #[test]
    fn setup_variant_candidates_keep_multiple_primary_subject_documents() {
        let alpha = document_row("alpha.html", "Subject Alpha setup");
        let beta = document_row("beta.html", "Subject Beta setup");
        let screenshot = document_row("subject.png", "Subject Beta screenshot");
        let unrelated = document_row("other.html", "Other setup");
        let unrelated_document_id = unrelated.document_id;
        let document_index = HashMap::from([
            (alpha.document_id, alpha.clone()),
            (beta.document_id, beta.clone()),
            (screenshot.document_id, screenshot),
            (unrelated.document_id, unrelated),
        ]);

        let query_ir = setup_query_ir("Subject");
        assert!(question_requests_setup_variant_evidence(
            "how to configure subject?",
            Some(&query_ir)
        ));
        let candidates = setup_variant_candidate_document_ids(
            "how to configure subject?",
            Some(&query_ir),
            &document_index,
            8,
        );

        assert!(candidates.contains(&alpha.document_id));
        assert!(candidates.contains(&beta.document_id));
        assert!(!candidates.contains(&unrelated_document_id));
    }

    #[test]
    fn setup_variant_evidence_skips_unambiguous_versioned_procedure() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.document_focus = None;
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Subject".to_string(), role: EntityRole::Subject }];

        assert!(!question_requests_setup_variant_evidence(
            "how to update Sample Target?",
            Some(&query_ir)
        ));
        assert!(!query_ir_requests_setup_variant_anchor_reservation(&query_ir));
    }

    #[test]
    fn setup_variant_evidence_allows_named_focus_literals_but_skips_exact_literals() {
        let mut query_ir = setup_query_ir("Subject Alpha");
        query_ir.document_focus = None;
        query_ir.target_types =
            vec!["package".to_string(), "configuration_file".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Subject Alpha".to_string(), role: EntityRole::Subject }];
        query_ir.literal_constraints =
            vec![LiteralSpan { text: "Subject Alpha".to_string(), kind: LiteralKind::Other }];

        assert!(question_requests_setup_variant_evidence(
            "how to configure subject alpha?",
            Some(&query_ir)
        ));
        assert!(question_requests_setup_variant_evidence(
            "how to configure subject alpha variants with package, command, settings files, sections, and main parameters",
            Some(&query_ir)
        ));

        query_ir.literal_constraints = vec![LiteralSpan {
            text: "/etc/subject-alpha.conf".to_string(),
            kind: LiteralKind::Path,
        }];
        assert!(!question_requests_setup_variant_evidence(
            "how to configure subject alpha?",
            Some(&query_ir)
        ));
    }

    #[test]
    fn setup_variant_evidence_skips_specific_compound_module_focus() {
        let mut query_ir = setup_query_ir("Workflow library");
        query_ir.target_types =
            vec!["concept".to_string(), "artifact".to_string(), "procedure".to_string()];
        query_ir.document_focus = None;
        query_ir.target_entities = vec![
            EntityMention { label: "Subject".to_string(), role: EntityRole::Subject },
            EntityMention { label: "DeltaVariant".to_string(), role: EntityRole::Subject },
        ];

        assert!(!question_requests_setup_variant_evidence(
            "how to configure Subject DeltaVariant?",
            Some(&query_ir)
        ));
        assert!(!question_requests_versioned_update_procedure_evidence(
            "how to configure Subject DeltaVariant?",
            Some(&query_ir)
        ));
    }

    #[test]
    fn setup_variant_document_family_deduplicates_same_environment_variant() {
        let query_ir = setup_query_ir("Subject");
        let query_terms = setup_variant_query_terms("how to configure subject?", Some(&query_ir));
        let alpha = document_row("alpha.html", "Subject Alpha setup guide");
        let alpha_new = document_row("alpha-new.html", "Subject Alpha updated protocol guide");
        let beta = document_row("beta.html", "Subject Beta setup guide");
        let document_index = HashMap::from([
            (alpha.document_id, alpha.clone()),
            (alpha_new.document_id, alpha_new.clone()),
            (beta.document_id, beta.clone()),
        ]);
        let candidate_document_ids =
            vec![alpha.document_id, alpha_new.document_id, beta.document_id];
        let family_model =
            setup_variant_family_model(&candidate_document_ids, &document_index, &query_terms);

        assert_eq!(
            setup_variant_document_family(&alpha, &query_terms, &family_model),
            setup_variant_document_family(&alpha_new, &query_terms, &family_model)
        );
        assert_ne!(
            setup_variant_document_family(&alpha, &query_terms, &family_model),
            setup_variant_document_family(&beta, &query_terms, &family_model)
        );
    }

    #[test]
    fn low_confidence_untyped_ir_allows_raw_question_setup_focus_fallback() {
        let mut query_ir = setup_query_ir("Sample Subject");
        query_ir.act = QueryAct::Describe;
        query_ir.confidence = 0.25;
        query_ir.target_types.clear();
        query_ir.target_entities.clear();
        query_ir.document_focus = None;

        assert!(query_ir_allows_raw_question_setup_focus_fallback(&query_ir));

        query_ir.confidence = 0.4;
        assert!(!query_ir_allows_raw_question_setup_focus_fallback(&query_ir));

        query_ir.confidence = 0.25;
        query_ir.target_types = vec!["configuration_file".to_string()];
        assert!(!query_ir_allows_raw_question_setup_focus_fallback(&query_ir));
    }

    #[test]
    fn raw_question_setup_focus_candidates_match_document_identity_without_typed_ir() {
        let target = document_row("alpha-suite-x9.md", "Sample Subject X9 setup reference");
        let screenshot = document_row("alpha-suite-x9.png", "Sample Subject X9 screenshot");
        let weaker = document_row("alpha-suite-y1.md", "Sample Subject Y1 setup reference");
        let unrelated = document_row("beta-suite.md", "Beta Suite setup reference");
        let unrelated_id = unrelated.document_id;
        let document_index = HashMap::from([
            (target.document_id, target.clone()),
            (screenshot.document_id, screenshot),
            (weaker.document_id, weaker.clone()),
            (unrelated.document_id, unrelated),
        ]);
        let question_tokens = raw_question_setup_focus_tokens("Sample Subject X9");

        let candidates = raw_question_setup_focus_candidate_document_ids(
            Some(&question_tokens),
            &document_index,
            8,
        );

        assert_eq!(candidates.first(), Some(&target.document_id));
        assert!(candidates.contains(&weaker.document_id));
        assert!(!candidates.contains(&unrelated_id));
    }

    #[test]
    fn raw_question_setup_focus_candidates_accept_single_short_identity_token() {
        let alpha = document_row("qps-alpha.md", "QPS Alpha setup reference");
        let beta = document_row("qps-beta.md", "QPS Beta setup reference");
        let unrelated = document_row("gamma-adapter.md", "Gamma Adapter setup reference");
        let unrelated_id = unrelated.document_id;
        let document_index = HashMap::from([
            (alpha.document_id, alpha.clone()),
            (beta.document_id, beta.clone()),
            (unrelated.document_id, unrelated),
        ]);
        let question_tokens = raw_question_setup_focus_tokens("how to configure QPS?");

        let candidates = raw_question_setup_focus_candidate_document_ids(
            Some(&question_tokens),
            &document_index,
            8,
        );

        assert!(candidates.contains(&alpha.document_id), "{candidates:#?}");
        assert!(candidates.contains(&beta.document_id), "{candidates:#?}");
        assert!(!candidates.contains(&unrelated_id), "{candidates:#?}");
    }

    #[test]
    fn raw_question_setup_focus_candidates_reject_single_common_short_token() {
        let alpha = document_row("api-alpha.md", "API Alpha setup reference");
        let beta = document_row("api-beta.md", "API Beta setup reference");
        let unrelated = document_row("gamma-adapter.md", "Gamma Adapter setup reference");
        let document_index = HashMap::from([
            (alpha.document_id, alpha),
            (beta.document_id, beta),
            (unrelated.document_id, unrelated),
        ]);
        let question_tokens = raw_question_setup_focus_tokens("api settings");

        let candidates = raw_question_setup_focus_candidate_document_ids(
            Some(&question_tokens),
            &document_index,
            8,
        );

        assert!(candidates.is_empty(), "{candidates:#?}");
    }

    #[test]
    fn retrieved_latest_version_fallback_selects_dominant_semver_family() {
        let alpha_new = document_row("alpha-2-4.md", "Sample Subject 2.4.0 changelog");
        let alpha_mid = document_row("alpha-2-3.md", "Sample Subject 2.3.0 changelog");
        let alpha_old = document_row("alpha-2-2.md", "Sample Subject 2.2.0 changelog");
        let beta_new = document_row("beta-2-4.md", "Beta Suite 2.4.0 changelog");
        let index = HashMap::from([
            (alpha_new.document_id, alpha_new.clone()),
            (alpha_mid.document_id, alpha_mid.clone()),
            (alpha_old.document_id, alpha_old.clone()),
            (beta_new.document_id, beta_new.clone()),
        ]);
        let chunks = vec![
            setup_focus_runtime_chunk(alpha_mid.document_id, 0, 1.0),
            setup_focus_runtime_chunk(alpha_new.document_id, 0, 1.0),
            setup_focus_runtime_chunk(beta_new.document_id, 0, 1.0),
        ];
        let query_ir = low_confidence_untyped_query_ir();

        let selection = select_latest_version_documents(
            Some(&query_ir),
            "show latest 2 records",
            &index,
            &chunks,
        );

        assert!(selection.inferred_from_retrieved_evidence);
        assert_eq!(selection.requested_count, 2);
        assert_eq!(
            selection.documents.iter().map(|document| document.title.as_str()).collect::<Vec<_>>(),
            vec!["Sample Subject 2.4.0 changelog", "Sample Subject 2.3.0 changelog"]
        );
    }

    #[test]
    fn retrieved_latest_version_fallback_defaults_to_max_inventory_window() {
        let documents = (1..=11)
            .map(|minor| {
                document_row(
                    &format!("alpha-2-{minor}.md"),
                    &format!("Sample Subject 2.{minor}.0 changelog"),
                )
            })
            .collect::<Vec<_>>();
        let index = documents
            .iter()
            .cloned()
            .map(|document| (document.document_id, document))
            .collect::<HashMap<_, _>>();
        let chunks = documents
            .iter()
            .take(3)
            .map(|document| setup_focus_runtime_chunk(document.document_id, 0, 1.0))
            .collect::<Vec<_>>();
        let query_ir = low_confidence_untyped_query_ir();

        let selection = select_latest_version_documents(
            Some(&query_ir),
            "show latest records",
            &index,
            &chunks,
        );

        assert!(selection.inferred_from_retrieved_evidence);
        assert_eq!(selection.requested_count, FALLBACK_LATEST_VERSION_MAX_COUNT);
        assert_eq!(selection.documents.len(), FALLBACK_LATEST_VERSION_MAX_COUNT);
        assert_eq!(
            selection.documents.first().map(|document| document.title.as_str()),
            Some("Sample Subject 2.11.0 changelog")
        );
    }

    #[test]
    fn retrieved_latest_version_fallback_repairs_moderate_confidence_untyped_ir() {
        let alpha_new = document_row("alpha-2-4.md", "Sample Subject 2.4.0 changelog");
        let alpha_mid = document_row("alpha-2-3.md", "Sample Subject 2.3.0 changelog");
        let alpha_old = document_row("alpha-2-2.md", "Sample Subject 2.2.0 changelog");
        let index = HashMap::from([
            (alpha_new.document_id, alpha_new.clone()),
            (alpha_mid.document_id, alpha_mid.clone()),
            (alpha_old.document_id, alpha_old.clone()),
        ]);
        let chunks = vec![
            latest_version_runtime_chunk(alpha_mid.document_id, 0, 1.0),
            latest_version_runtime_chunk(alpha_old.document_id, 0, 1.0),
        ];
        let mut query_ir = low_confidence_untyped_query_ir();
        query_ir.confidence = 0.55;

        let inferred = infer_latest_version_query_ir_from_retrieved_evidence(
            &query_ir,
            "show latest 2 records",
            &chunks,
            &index,
        )
        .expect("moderate confidence untyped version evidence should repair QueryIR");

        assert!(query_requests_latest_versions(&inferred));
        assert_eq!(inferred.source_slice.as_ref().and_then(|slice| slice.count), Some(2));
    }

    #[test]
    fn retrieved_latest_version_fallback_allows_untyped_target_entity() {
        let alpha_new = document_row("alpha-2-4.md", "Sample Subject 2.4.0 changelog");
        let alpha_mid = document_row("alpha-2-3.md", "Sample Subject 2.3.0 changelog");
        let alpha_old = document_row("alpha-2-2.md", "Sample Subject 2.2.0 changelog");
        let index = HashMap::from([
            (alpha_new.document_id, alpha_new.clone()),
            (alpha_mid.document_id, alpha_mid.clone()),
            (alpha_old.document_id, alpha_old.clone()),
        ]);
        let chunks = vec![
            latest_version_runtime_chunk(alpha_mid.document_id, 0, 1.0),
            latest_version_runtime_chunk(alpha_old.document_id, 0, 1.0),
        ];
        let mut query_ir = low_confidence_untyped_query_ir();
        query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
            label: "Sample Subject records".to_string(),
            role: crate::domains::query_ir::EntityRole::Subject,
        }];

        let inferred = infer_latest_version_query_ir_from_retrieved_evidence(
            &query_ir,
            "show latest 2 records",
            &chunks,
            &index,
        )
        .expect("untyped target entities must not suppress latest-version evidence repair");

        assert!(query_requests_latest_versions(&inferred));
        assert_eq!(inferred.source_slice.as_ref().and_then(|slice| slice.count), Some(2));
    }

    #[test]
    fn retrieved_latest_version_fallback_ignores_plain_word_literal_echo() {
        let alpha_new = document_row("alpha-2-4.md", "Sample Subject 2.4.0 changelog");
        let alpha_mid = document_row("alpha-2-3.md", "Sample Subject 2.3.0 changelog");
        let alpha_old = document_row("alpha-2-2.md", "Sample Subject 2.2.0 changelog");
        let index = HashMap::from([
            (alpha_new.document_id, alpha_new.clone()),
            (alpha_mid.document_id, alpha_mid.clone()),
            (alpha_old.document_id, alpha_old.clone()),
        ]);
        let chunks = vec![
            latest_version_runtime_chunk(alpha_mid.document_id, 0, 1.0),
            latest_version_runtime_chunk(alpha_old.document_id, 0, 1.0),
        ];
        let mut query_ir = low_confidence_untyped_query_ir();
        query_ir.literal_constraints = vec![crate::domains::query_ir::LiteralSpan {
            text: "records.".to_string(),
            kind: crate::domains::query_ir::LiteralKind::Identifier,
        }];

        let inferred = infer_latest_version_query_ir_from_retrieved_evidence(
            &query_ir,
            "show latest 2 records",
            &chunks,
            &index,
        )
        .expect("sentence-edge word echoes must not block latest-version evidence repair");

        assert!(query_requests_latest_versions(&inferred));
        assert_eq!(inferred.source_slice.as_ref().and_then(|slice| slice.count), Some(2));
    }

    #[test]
    fn retrieved_latest_version_fallback_keeps_exact_identifier_constraints_blocking() {
        let alpha_new = document_row("alpha-2-4.md", "Sample Subject 2.4.0 changelog");
        let alpha_mid = document_row("alpha-2-3.md", "Sample Subject 2.3.0 changelog");
        let index = HashMap::from([
            (alpha_new.document_id, alpha_new.clone()),
            (alpha_mid.document_id, alpha_mid.clone()),
        ]);
        let chunks = vec![
            latest_version_runtime_chunk(alpha_new.document_id, 0, 1.0),
            latest_version_runtime_chunk(alpha_mid.document_id, 0, 1.0),
        ];
        let mut query_ir = low_confidence_untyped_query_ir();
        query_ir.literal_constraints = vec![crate::domains::query_ir::LiteralSpan {
            text: "callbackUrl".to_string(),
            kind: crate::domains::query_ir::LiteralKind::Identifier,
        }];

        assert!(
            infer_latest_version_query_ir_from_retrieved_evidence(
                &query_ir,
                "show latest records for callbackUrl",
                &chunks,
                &index,
            )
            .is_none()
        );
    }

    #[test]
    fn retrieved_latest_version_fallback_repairs_query_ir_for_ordered_answer() {
        let alpha_new = document_row("alpha-2-4.md", "Sample Subject 2.4.0 changelog");
        let alpha_mid = document_row("alpha-2-3.md", "Sample Subject 2.3.0 changelog");
        let index = HashMap::from([
            (alpha_new.document_id, alpha_new.clone()),
            (alpha_mid.document_id, alpha_mid.clone()),
        ]);
        let chunks = vec![
            latest_version_runtime_chunk(alpha_new.document_id, 0, 1.0),
            latest_version_runtime_chunk(alpha_mid.document_id, 0, 1.0),
        ];
        let query_ir = low_confidence_untyped_query_ir();

        let inferred = infer_latest_version_query_ir_from_retrieved_evidence(
            &query_ir,
            "latest 10 records",
            &chunks,
            &index,
        )
        .expect("retrieved semver family should infer ordered source slice");

        assert!(query_requests_latest_versions(&inferred));
        assert_eq!(inferred.scope, QueryScope::MultiDocument);
        assert_eq!(inferred.source_slice.as_ref().and_then(|slice| slice.count), Some(10));
    }

    #[test]
    fn retrieved_latest_version_query_ir_repair_skips_structured_follow_up_context() {
        let alpha_new = document_row("alpha-2-4.md", "Sample Subject 2.4.0 changelog");
        let alpha_mid = document_row("alpha-2-3.md", "Sample Subject 2.3.0 changelog");
        let index = HashMap::from([
            (alpha_new.document_id, alpha_new.clone()),
            (alpha_mid.document_id, alpha_mid.clone()),
        ]);
        let chunks = vec![
            latest_version_runtime_chunk(alpha_new.document_id, 0, 1.0),
            latest_version_runtime_chunk(alpha_mid.document_id, 0, 1.0),
        ];
        let query_ir = low_confidence_untyped_query_ir();

        assert!(
            infer_latest_version_query_ir_from_retrieved_evidence(
                &query_ir,
                "scope: Subject Alpha configuration literals\nquestion: explain these settings",
                &chunks,
                &index,
            )
            .is_none()
        );
    }

    #[test]
    fn retrieved_latest_version_query_ir_repair_ignores_plain_semver_relevance_chunks() {
        let alpha_new = document_row("alpha-2-4.md", "Sample Subject 2.4.0 changelog");
        let alpha_mid = document_row("alpha-2-3.md", "Sample Subject 2.3.0 changelog");
        let index = HashMap::from([
            (alpha_new.document_id, alpha_new.clone()),
            (alpha_mid.document_id, alpha_mid.clone()),
        ]);
        let chunks = vec![
            setup_focus_runtime_chunk(alpha_new.document_id, 0, 1.0),
            setup_focus_runtime_chunk(alpha_mid.document_id, 0, 1.0),
        ];
        let query_ir = low_confidence_untyped_query_ir();

        assert!(
            infer_latest_version_query_ir_from_retrieved_evidence(
                &query_ir,
                "latest 10 records",
                &chunks,
                &index,
            )
            .is_none()
        );
    }

    #[test]
    fn config_queries_reserve_source_context_chunks_during_truncation() {
        let document_id = Uuid::now_v7();
        let mut query_ir = setup_query_ir("Subject Alpha");
        query_ir.target_types = vec!["configuration_file".to_string(), "parameter".to_string()];

        let mut chunks = (0..6)
            .map(|index| setup_focus_runtime_chunk(Uuid::now_v7(), index, 1.0))
            .collect::<Vec<_>>();
        chunks.extend(
            [
                "[Main] url = https://alpha.example.invalid/api",
                "| fillDetails | boolean | true false | Fill detail fields |",
                "[Display] visible = true",
                "| visible | boolean | true false | Display code |",
                "printSlip = true",
                "credentialToken = value",
            ]
            .into_iter()
            .enumerate()
            .map(|(offset, text)| {
                let mut chunk = setup_focus_runtime_chunk(document_id, 20 + offset as i32, 0.1);
                chunk.score_kind = RuntimeChunkScoreKind::SourceContext;
                chunk.document_label = "Subject Alpha setup manual".to_string();
                chunk.excerpt = text.to_string();
                chunk.source_text = text.to_string();
                chunk
            }),
        );
        let mut bundle =
            RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks };

        truncate_bundle(&mut bundle, 8, Some(&query_ir), &std::collections::HashSet::new());

        let retained_source_text =
            bundle.chunks.iter().map(|chunk| chunk.source_text.as_str()).collect::<Vec<_>>();
        assert!(
            bundle
                .chunks
                .iter()
                .filter(|chunk| chunk.score_kind == RuntimeChunkScoreKind::SourceContext)
                .count()
                >= 6,
            "{retained_source_text:?}"
        );
        assert!(retained_source_text.iter().any(|text| text.contains("fillDetails")));
        assert!(retained_source_text.iter().any(|text| text.contains("visible = true")));
        assert!(retained_source_text.iter().any(|text| text.contains("credentialToken")));
    }

    #[test]
    fn table_inventory_queries_reserve_source_context_chunks_during_truncation() {
        let document_id = Uuid::now_v7();
        let mut query_ir = setup_query_ir("Accounts schema");
        query_ir.act = QueryAct::Enumerate;
        query_ir.document_focus = None;
        query_ir.target_types = vec!["table_row".to_string(), "table_summary".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "accounts".to_string(), role: EntityRole::Subject }];

        let mut chunks = (0..8)
            .map(|index| setup_focus_runtime_chunk(Uuid::now_v7(), index, 100.0))
            .collect::<Vec<_>>();
        chunks.extend(
            [
                "| account_id | UUID | PRIMARY KEY | Unique account identifier |",
                "| email | VARCHAR(255) | UNIQUE, NOT NULL | Login email |",
                "| status | VARCHAR(20) | NOT NULL | Account state |",
                "| created_at | TIMESTAMPTZ | NOT NULL | Creation timestamp |",
                "| updated_at | TIMESTAMPTZ | NOT NULL | Last update timestamp |",
                "| archived_at | TIMESTAMPTZ | nullable | Archive timestamp |",
            ]
            .into_iter()
            .enumerate()
            .map(|(offset, text)| {
                let mut chunk = setup_focus_runtime_chunk(document_id, 20 + offset as i32, 1.0);
                chunk.score_kind = RuntimeChunkScoreKind::SourceContext;
                chunk.document_label = "Accounts schema".to_string();
                chunk.excerpt = text.to_string();
                chunk.source_text = text.to_string();
                chunk
            }),
        );
        let mut bundle =
            RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks };

        truncate_bundle(&mut bundle, 10, Some(&query_ir), &std::collections::HashSet::new());

        let retained_source_text =
            bundle.chunks.iter().map(|chunk| chunk.source_text.as_str()).collect::<Vec<_>>();
        assert!(
            bundle
                .chunks
                .iter()
                .filter(|chunk| chunk.score_kind == RuntimeChunkScoreKind::SourceContext)
                .count()
                >= 6,
            "{retained_source_text:?}"
        );
        assert!(retained_source_text.iter().any(|text| text.contains("account_id")));
        assert!(retained_source_text.iter().any(|text| text.contains("updated_at")));
    }

    #[test]
    fn low_confidence_structured_fallback_reserves_source_context_chunks() {
        let document_id = Uuid::now_v7();
        let query_ir = low_confidence_untyped_query_ir();

        let mut chunks = (0..6)
            .map(|index| setup_focus_runtime_chunk(Uuid::now_v7(), index, 1.0))
            .collect::<Vec<_>>();
        chunks.extend(
            [
                "[Main] endpointUrl = https://alpha.example.invalid/api",
                "| fillDetails | boolean | true false | Fill detail fields |",
                "[Display] visible = true",
                "| visible | boolean | true false | Display code |",
                "printSlip = false",
                "credentialToken = value",
            ]
            .into_iter()
            .enumerate()
            .map(|(offset, text)| {
                let mut chunk = setup_focus_runtime_chunk(document_id, 20 + offset as i32, 0.1);
                chunk.score_kind = RuntimeChunkScoreKind::SourceContext;
                chunk.document_label = "Subject Alpha setup manual".to_string();
                chunk.excerpt = text.to_string();
                chunk.source_text = text.to_string();
                chunk
            }),
        );
        let mut bundle =
            RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks };

        truncate_bundle(&mut bundle, 8, Some(&query_ir), &std::collections::HashSet::new());

        let retained_source_text =
            bundle.chunks.iter().map(|chunk| chunk.source_text.as_str()).collect::<Vec<_>>();
        assert!(
            bundle
                .chunks
                .iter()
                .filter(|chunk| chunk.score_kind == RuntimeChunkScoreKind::SourceContext)
                .count()
                >= 6,
            "{retained_source_text:?}"
        );
        assert!(retained_source_text.iter().any(|text| text.contains("endpointUrl")));
        assert!(retained_source_text.iter().any(|text| text.contains("visible = true")));
        assert!(retained_source_text.iter().any(|text| text.contains("printSlip = false")));
    }

    #[test]
    fn low_confidence_structured_fallback_keeps_late_source_context_examples() {
        let document_id = Uuid::now_v7();
        let query_ir = low_confidence_untyped_query_ir();

        let mut chunks = (0..8)
            .map(|index| setup_focus_runtime_chunk(Uuid::now_v7(), index, 1.0))
            .collect::<Vec<_>>();
        chunks.extend((0..28).map(|offset| {
            let text = match offset {
                24 => "[Display] visible = true".to_string(),
                26 => "[Receipt] printSlip = false".to_string(),
                _ => format!("tableRow{offset} = value"),
            };
            let mut chunk = setup_focus_runtime_chunk(document_id, 20 + offset, 0.1);
            chunk.score_kind = RuntimeChunkScoreKind::SourceContext;
            chunk.document_label = "Subject Alpha setup manual".to_string();
            chunk.excerpt = text.clone();
            chunk.source_text = text;
            chunk
        }));
        let mut bundle =
            RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks };

        truncate_bundle(&mut bundle, 32, Some(&query_ir), &std::collections::HashSet::new());

        let retained_source_text =
            bundle.chunks.iter().map(|chunk| chunk.source_text.as_str()).collect::<Vec<_>>();
        assert!(
            bundle
                .chunks
                .iter()
                .filter(|chunk| chunk.score_kind == RuntimeChunkScoreKind::SourceContext)
                .count()
                >= 28,
            "{retained_source_text:?}"
        );
        assert!(retained_source_text.iter().any(|text| text.contains("visible = true")));
        assert!(retained_source_text.iter().any(|text| text.contains("printSlip = false")));
    }

    #[test]
    fn retrieved_latest_version_fallback_skips_exact_version_questions() {
        let alpha_new = document_row("alpha-2-4.md", "Sample Subject 2.4.0 changelog");
        let alpha_mid = document_row("alpha-2-3.md", "Sample Subject 2.3.0 changelog");
        let index = HashMap::from([
            (alpha_new.document_id, alpha_new.clone()),
            (alpha_mid.document_id, alpha_mid.clone()),
        ]);
        let chunks = vec![
            latest_version_runtime_chunk(alpha_new.document_id, 0, 1.0),
            latest_version_runtime_chunk(alpha_mid.document_id, 0, 1.0),
        ];
        let query_ir = low_confidence_untyped_query_ir();

        assert!(
            infer_latest_version_query_ir_from_retrieved_evidence(
                &query_ir,
                "show 2.4.0",
                &chunks,
                &index,
            )
            .is_none()
        );
    }

    #[test]
    fn retrieved_latest_version_fallback_requires_multiple_family_documents() {
        let alpha_new = document_row("alpha-2-4.md", "Sample Subject 2.4.0 changelog");
        let generic = document_row("overview.md", "Sample Subject overview");
        let index = HashMap::from([
            (alpha_new.document_id, alpha_new.clone()),
            (generic.document_id, generic.clone()),
        ]);
        let chunks = vec![
            latest_version_runtime_chunk(alpha_new.document_id, 0, 1.0),
            latest_version_runtime_chunk(generic.document_id, 0, 1.0),
        ];
        let query_ir = low_confidence_untyped_query_ir();

        assert!(
            infer_latest_version_query_ir_from_retrieved_evidence(
                &query_ir,
                "show latest records",
                &chunks,
                &index,
            )
            .is_none()
        );
    }

    #[test]
    fn setup_focus_rows_select_package_plus_configuration_path() {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let rows = vec![
            chunk_row(
                workspace_id,
                library_id,
                document_id,
                revision_id,
                0,
                "Subject Alpha overview",
            ),
            chunk_row(
                workspace_id,
                library_id,
                document_id,
                revision_id,
                1,
                "Install the module:\nsample-runner --install sample-link\n\nConfigure it:\nsample-configure alpha-connector\n\nSettings are stored in /opt/alpha/connector/connector.conf.",
            ),
            chunk_row(
                workspace_id,
                library_id,
                document_id,
                revision_id,
                2,
                "Example display file /opt/alpha/display.ini.",
            ),
        ];

        let selected = select_setup_focus_document_rows(&rows);

        assert_eq!(selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>(), [1, 2]);
        assert!(setup_focus_row_score(&selected[0]) > setup_focus_row_score(&selected[1]));
    }

    #[test]
    fn setup_focus_rows_select_configuration_parameter_blocks_without_package_command() {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let rows = vec![
            chunk_row(
                workspace_id,
                library_id,
                document_id,
                revision_id,
                0,
                "Subject Alpha overview",
            ),
            chunk_row(
                workspace_id,
                library_id,
                document_id,
                revision_id,
                1,
                "[Connector]\nendpoint = https://127.0.0.1/api\nretryTimeout = 15\nFile: /opt/alpha/connector/settings.conf",
            ),
        ];

        let selected = select_setup_focus_document_rows(&rows);

        assert_eq!(selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>(), [1]);
        assert!(setup_focus_row_score(&selected[0]) > 0);
    }

    #[test]
    fn setup_focus_rows_keep_parameter_table_after_setup_anchor() {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let rows = vec![
            chunk_row(
                workspace_id,
                library_id,
                document_id,
                revision_id,
                0,
                "Install the module:\nsample-runner --install sample-link\nSettings are stored in /opt/alpha/connector/connector.conf.",
            ),
            chunk_row(
                workspace_id,
                library_id,
                document_id,
                revision_id,
                1,
                "Sheet: Connector settings | Row 1 | Name: partnerId | Type: string | Description: Partner identifier",
            ),
            chunk_row(
                workspace_id,
                library_id,
                document_id,
                revision_id,
                2,
                "Sheet: Connector settings | Row 2 | Name: credentialToken | Type: string | Description: Shared credential",
            ),
        ];

        let selected = select_setup_focus_document_rows(&rows);

        assert_eq!(selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>(), [0, 1, 2]);
    }

    #[test]
    fn setup_focus_rows_keep_late_parameter_table_after_setup_anchor() {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mut rows = vec![chunk_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            0,
            "Install the module:\nsample-runner --install sample-link\nSettings are stored in /opt/alpha/connector/connector.conf.",
        )];
        for index in 1..20 {
            rows.push(chunk_row(
                workspace_id,
                library_id,
                document_id,
                revision_id,
                index,
                "Configuration table continuation",
            ));
        }
        rows.push(chunk_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            20,
            "| sendDetails | boolean | Send detailed workflow payload | Default true |",
        ));

        let selected = select_setup_focus_document_rows(&rows);

        assert!(selected.iter().any(|row| row.chunk_index == 20));
    }

    #[test]
    fn setup_focus_chunk_score_prioritizes_parameter_context_above_exact_identity_hits() {
        let chunk = RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 12,
            chunk_kind: Some("code_block".to_string()),
            document_id: Uuid::now_v7(),
            document_label: "Connector Alpha".to_string(),
            excerpt: "mode\ncredentialToken\nprimaryKey".to_string(),
            score: Some(1.0),
            score_kind: RuntimeChunkScoreKind::Relevance,
            source_text: "mode\ncredentialToken\nprimaryKey".to_string(),
        };

        let score = setup_focus_document_chunk_score(&chunk);

        assert!(score > DOCUMENT_IDENTITY_SCORE_FLOOR * 10.0);
    }

    #[test]
    fn setup_focus_row_score_includes_structured_parameter_table_rows() {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let row = chunk_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            21,
            "Sheet: Connector settings | Table: Workflow details | Row 1 | Name: fillWorkflowDetails | Type: boolean | Values: true false | Description: Fill workflowDetails | Default: true",
        );

        assert!(setup_focus_row_score(&row) > 0);
    }

    #[test]
    fn setup_focus_document_chunks_preserve_best_document_context_before_noisy_matches() {
        let noisy_document_id = Uuid::now_v7();
        let target_document_id = Uuid::now_v7();
        let noisy_chunks = (0..SETUP_FOCUS_DOCUMENT_CHUNK_CAP)
            .map(|index| setup_focus_runtime_chunk(noisy_document_id, index as i32, 1.0))
            .collect::<Vec<_>>();
        let target_chunks = vec![
            setup_focus_runtime_chunk(target_document_id, 1, SETUP_FOCUS_DOCUMENT_SCORE_BASE),
            setup_focus_runtime_chunk(target_document_id, 20, SETUP_FOCUS_DOCUMENT_SCORE_BASE),
        ];

        let selected = select_setup_focus_document_chunks(vec![
            (1, noisy_document_id, noisy_chunks),
            (100, target_document_id, target_chunks),
        ]);

        assert_eq!(
            selected
                .iter()
                .take(2)
                .map(|chunk| (chunk.document_id, chunk.chunk_index))
                .collect::<Vec<_>>(),
            vec![(target_document_id, 1), (target_document_id, 20)]
        );
    }

    #[test]
    fn diversification_preserves_setup_focus_document_inventory() {
        let setup_document_id = Uuid::now_v7();
        let ordinary_document_id = Uuid::now_v7();
        let mut chunks = (0..6)
            .map(|index| {
                setup_focus_runtime_chunk(
                    setup_document_id,
                    index,
                    SETUP_FOCUS_DOCUMENT_SCORE_BASE - index as f32,
                )
            })
            .collect::<Vec<_>>();
        chunks.extend((0..6).map(|index| {
            setup_focus_runtime_chunk(ordinary_document_id, index, 1.0 - index as f32 * 0.01)
        }));
        let setup_focus_document_ids = BTreeSet::from([setup_document_id]);

        let selected = diversify_chunks_by_document(
            chunks,
            MAX_CHUNKS_PER_DOCUMENT,
            &setup_focus_document_ids,
        );

        assert_eq!(
            selected.iter().filter(|chunk| chunk.document_id == setup_document_id).count(),
            6
        );
        assert_eq!(
            selected.iter().filter(|chunk| chunk.document_id == ordinary_document_id).count(),
            MAX_CHUNKS_PER_DOCUMENT
        );
    }

    #[test]
    fn diversification_preserves_protected_document_evidence_inventory() {
        let protected_document_id = Uuid::now_v7();
        let ordinary_document_id = Uuid::now_v7();
        let mut chunks = (0..4)
            .map(|index| {
                let mut chunk = setup_focus_runtime_chunk(
                    protected_document_id,
                    index,
                    document_identity_chunk_score(0, index as usize),
                );
                chunk.score_kind = RuntimeChunkScoreKind::FocusedDocument;
                chunk
            })
            .collect::<Vec<_>>();
        chunks.extend((0..4).map(|index| {
            setup_focus_runtime_chunk(ordinary_document_id, index, 1.0 - index as f32 * 0.01)
        }));
        let protected_document_ids = BTreeSet::from([protected_document_id]);

        let selected =
            diversify_chunks_by_document(chunks, MAX_CHUNKS_PER_DOCUMENT, &protected_document_ids);

        assert_eq!(
            selected.iter().filter(|chunk| chunk.document_id == protected_document_id).count(),
            4
        );
        assert_eq!(
            selected.iter().filter(|chunk| chunk.document_id == ordinary_document_id).count(),
            MAX_CHUNKS_PER_DOCUMENT
        );
    }

    #[test]
    fn retain_scoped_documents_keeps_protected_document_evidence() {
        let scoped_document_id = Uuid::now_v7();
        let protected_document_id = Uuid::now_v7();
        let unrelated_document_id = Uuid::now_v7();
        let protected_chunk = setup_focus_runtime_chunk(
            protected_document_id,
            0,
            document_identity_chunk_score(0, 0),
        );
        let protected_chunk_id = protected_chunk.chunk_id;
        let mut chunks = vec![
            setup_focus_runtime_chunk(scoped_document_id, 0, 1.0),
            protected_chunk,
            setup_focus_runtime_chunk(unrelated_document_id, 0, 0.5),
        ];

        retain_scoped_documents(
            &mut chunks,
            &BTreeSet::from([scoped_document_id]),
            &BTreeSet::new(),
            &BTreeSet::from([protected_document_id]),
        );

        assert!(chunks.iter().any(|chunk| chunk.document_id == scoped_document_id));
        assert!(chunks.iter().any(|chunk| chunk.chunk_id == protected_chunk_id));
        assert!(!chunks.iter().any(|chunk| chunk.document_id == unrelated_document_id));
    }

    #[test]
    fn diversification_uses_global_multi_document_setup_focus_budget() {
        let setup_document_ids = (0..3).map(|_| Uuid::now_v7()).collect::<Vec<_>>();
        let chunks = (0..12)
            .flat_map(|index| {
                setup_document_ids.iter().map(move |document_id| {
                    setup_focus_runtime_chunk(
                        *document_id,
                        index,
                        SETUP_FOCUS_DOCUMENT_SCORE_BASE - index as f32,
                    )
                })
            })
            .collect::<Vec<_>>();
        let setup_focus_document_ids = setup_document_ids.iter().copied().collect::<BTreeSet<_>>();

        let selected = diversify_chunks_by_document(
            chunks,
            MAX_CHUNKS_PER_DOCUMENT,
            &setup_focus_document_ids,
        );

        assert_eq!(
            selected
                .iter()
                .filter(|chunk| setup_focus_document_ids.contains(&chunk.document_id))
                .count(),
            SETUP_FOCUS_DOCUMENT_CHUNK_CAP
        );
        for document_id in setup_document_ids {
            let selected_for_document =
                selected.iter().filter(|chunk| chunk.document_id == document_id).count();
            assert!(
                selected_for_document > MAX_CHUNKS_PER_DOCUMENT,
                "setup focus budget should keep more than the ordinary per-document cap"
            );
            assert!(
                selected_for_document <= SETUP_FOCUS_DOCUMENT_CHUNK_CAP,
                "setup focus budget remains per-document bounded"
            );
        }
    }

    #[test]
    fn diversification_caps_ordinary_documents_even_with_setup_focus_budget() {
        let setup_document_id = Uuid::now_v7();
        let ordinary_document_id = Uuid::now_v7();
        let mut chunks = (0..12)
            .map(|index| {
                setup_focus_runtime_chunk(
                    setup_document_id,
                    index,
                    SETUP_FOCUS_DOCUMENT_SCORE_BASE - index as f32,
                )
            })
            .collect::<Vec<_>>();
        chunks.extend((0..12).map(|index| {
            setup_focus_runtime_chunk(ordinary_document_id, index, 1.0 - index as f32 * 0.01)
        }));
        let setup_focus_document_ids = BTreeSet::from([setup_document_id]);

        let selected = diversify_chunks_by_document(
            chunks,
            MAX_CHUNKS_PER_DOCUMENT,
            &setup_focus_document_ids,
        );

        assert_eq!(
            selected.iter().filter(|chunk| chunk.document_id == setup_document_id).count(),
            12
        );
        assert_eq!(
            selected.iter().filter(|chunk| chunk.document_id == ordinary_document_id).count(),
            MAX_CHUNKS_PER_DOCUMENT
        );
    }

    #[test]
    fn setup_focus_document_score_prefers_literal_matching_parameter_rows() {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let first_document = Uuid::now_v7();
        let second_document = Uuid::now_v7();
        let first_revision = Uuid::now_v7();
        let second_revision = Uuid::now_v7();
        let generic_rows = vec![
            chunk_row(
                workspace_id,
                library_id,
                first_document,
                first_revision,
                1,
                "Install with sample-runner --install sample-link. Configure /opt/alpha/alpha.conf.",
            ),
            chunk_row(
                workspace_id,
                library_id,
                first_document,
                first_revision,
                2,
                "Sheet: Settings | Row 1 | Name: url | Type: string",
            ),
        ];
        let matching_rows = vec![
            chunk_row(
                workspace_id,
                library_id,
                second_document,
                second_revision,
                1,
                "Install with sample-runner --install sample-link. Configure /opt/alpha/alpha.conf.",
            ),
            chunk_row(
                workspace_id,
                library_id,
                second_document,
                second_revision,
                2,
                "Sheet: Settings | Row 2 | Name: primaryKey | Type: string",
            ),
            chunk_row(
                workspace_id,
                library_id,
                second_document,
                second_revision,
                3,
                "Sheet: Settings | Row 3 | Name: credentialToken | Type: string",
            ),
        ];
        let mut query_ir = setup_query_ir("Subject Alpha");
        query_ir.literal_constraints = vec![
            LiteralSpan {
                text: "primary".to_string(),
                kind: crate::domains::query_ir::LiteralKind::Identifier,
            },
            LiteralSpan {
                text: "secret".to_string(),
                kind: crate::domains::query_ir::LiteralKind::Identifier,
            },
        ];

        assert!(
            setup_focus_document_candidate_score(&matching_rows, &query_ir)
                > setup_focus_document_candidate_score(&generic_rows, &query_ir)
        );
    }

    #[test]
    fn setup_focus_document_label_identity_score_prefers_full_subject_coverage() {
        let generic = document_row("subject-alpha.md", "Subject Alpha setup reference");
        let focused = document_row("subject-beta.md", "Subject Beta setup reference");
        let mut query_ir = setup_query_ir("Subject");
        query_ir.document_focus = None;
        query_ir.target_entities = vec![
            EntityMention { label: "Subject".to_string(), role: EntityRole::Subject },
            EntityMention { label: "Beta".to_string(), role: EntityRole::Subject },
        ];

        assert!(
            setup_focus_document_label_identity_score(&focused, &query_ir)
                > setup_focus_document_label_identity_score(&generic, &query_ir)
        );
    }

    #[test]
    fn setup_focus_candidates_keep_long_manual_after_many_short_matches() {
        let target = document_row(
            "environment-alpha-admin-guide.md",
            "Subject Alpha - Administrator setup manual",
        );
        let mut documents = vec![target.clone()];
        for index in 0..64 {
            documents.push(document_row(
                &format!("image-{index}.png"),
                &format!("Subject Alpha: subject screenshot {index}"),
            ));
        }
        let document_index = documents
            .into_iter()
            .map(|document| (document.document_id, document))
            .collect::<HashMap<_, _>>();
        let query_ir = setup_query_ir("Subject Alpha");

        let candidates = setup_focus_candidate_document_ids(
            &query_ir,
            &document_index,
            SETUP_FOCUS_DOCUMENT_CANDIDATE_CAP,
        );

        assert!(candidates.contains(&target.document_id));
    }

    #[test]
    fn raw_setup_focus_candidates_ignore_high_frequency_title_tokens() {
        let documents = (0..12)
            .map(|index| {
                document_row(
                    &format!("shared-device-{index}.md"),
                    &format!("Shared Device operator note {index}"),
                )
            })
            .collect::<Vec<_>>();
        let document_index = documents
            .into_iter()
            .map(|document| (document.document_id, document))
            .collect::<HashMap<_, _>>();
        let question_tokens = raw_question_setup_focus_tokens("Shared Device status");

        let candidates = raw_question_setup_focus_candidate_document_ids(
            Some(&question_tokens),
            &document_index,
            SETUP_FOCUS_DOCUMENT_CANDIDATE_CAP,
        );

        assert!(candidates.is_empty());
    }

    #[test]
    fn raw_setup_focus_candidates_keep_rare_subject_token() {
        let mut documents = (0..12)
            .map(|index| {
                document_row(
                    &format!("shared-device-{index}.md"),
                    &format!("Shared Device operator note {index}"),
                )
            })
            .collect::<Vec<_>>();
        let target = document_row("alpha-connector.md", "Alpha Connector setup reference");
        documents.push(target.clone());
        let document_index = documents
            .into_iter()
            .map(|document| (document.document_id, document))
            .collect::<HashMap<_, _>>();
        let question_tokens = raw_question_setup_focus_tokens("Alpha Connector settings");

        let candidates = raw_question_setup_focus_candidate_document_ids(
            Some(&question_tokens),
            &document_index,
            SETUP_FOCUS_DOCUMENT_CANDIDATE_CAP,
        );

        assert_eq!(candidates.first().copied(), Some(target.document_id));
    }

    #[test]
    fn structural_setup_focus_fallback_uses_environment_free_entity_identity() {
        let target = document_row("alpha-connector.md", "Alpha Connector setup reference");
        let unrelated = document_row("beta-connector.md", "Beta Connector setup reference");
        let document_index = [unrelated, target.clone()]
            .into_iter()
            .map(|document| (document.document_id, document))
            .collect::<HashMap<_, _>>();
        let mut query_ir = setup_query_ir("");
        query_ir.act = QueryAct::Describe;
        query_ir.scope = QueryScope::MultiDocument;
        query_ir.target_types.clear();
        query_ir.target_entities =
            vec![EntityMention { label: "Alpha Connector".to_string(), role: EntityRole::Subject }];
        query_ir.document_focus = None;
        query_ir.confidence = 0.25;

        assert!(query_ir_warrants_structural_setup_focus_fallback(&query_ir));
        let candidates = setup_focus_candidate_document_ids(
            &query_ir,
            &document_index,
            SETUP_FOCUS_DOCUMENT_CANDIDATE_CAP,
        );

        assert_eq!(candidates.first().copied(), Some(target.document_id));
    }

    #[test]
    fn raw_setup_focus_fallback_accepts_low_confidence_multi_document_ir() {
        let target = document_row("alpha-connector.md", "Alpha Connector setup reference");
        let unrelated = document_row("beta-connector.md", "Beta Connector setup reference");
        let document_index = [unrelated, target.clone()]
            .into_iter()
            .map(|document| (document.document_id, document))
            .collect::<HashMap<_, _>>();
        let mut query_ir = setup_query_ir("");
        query_ir.act = QueryAct::Describe;
        query_ir.scope = QueryScope::MultiDocument;
        query_ir.target_types.clear();
        query_ir.target_entities.clear();
        query_ir.document_focus = None;
        query_ir.confidence = 0.25;
        let question_tokens = raw_question_setup_focus_tokens("Alpha Connector");

        assert!(setup_focus_uses_raw_question_fallback(&query_ir));
        let candidates = raw_question_setup_focus_candidate_document_ids(
            Some(&question_tokens),
            &document_index,
            SETUP_FOCUS_DOCUMENT_CANDIDATE_CAP,
        );

        assert_eq!(candidates.first().copied(), Some(target.document_id));
    }

    #[test]
    fn raw_setup_focus_score_does_not_expand_distinctive_tokens_by_near_match() {
        let document = document_row("sample-desk.md", "Sample desk operator reference");
        let question_tokens = RawQuestionSetupFocusTokens {
            tokens: BTreeSet::from(["desks".to_string()]),
            standalone_tokens: BTreeSet::new(),
        };

        assert_eq!(raw_question_setup_focus_document_score(&question_tokens, &document), 0);
    }

    #[test]
    fn raw_setup_focus_filter_keeps_primary_supported_sibling_documents() {
        let setup_document_id = Uuid::now_v7();
        let mut setup_chunk = setup_focus_runtime_chunk(setup_document_id, 0, 1.0);
        setup_chunk.document_label = "Alpha Connector setup reference".to_string();
        let mut unrelated_setup_chunk = setup_focus_runtime_chunk(Uuid::now_v7(), 0, 1.0);
        unrelated_setup_chunk.document_label = "Gamma Verifier setup reference".to_string();
        let mut primary_chunk = setup_focus_runtime_chunk(Uuid::now_v7(), 0, 1.0);
        primary_chunk.document_label = "Alpha Connector status screen".to_string();

        let filtered = filter_raw_setup_focus_chunks_by_primary_context(
            vec![setup_chunk.clone(), unrelated_setup_chunk],
            &[primary_chunk],
        );

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].document_id, setup_document_id);
    }

    #[test]
    fn raw_setup_focus_filter_keeps_exact_primary_document() {
        let document_id = Uuid::now_v7();
        let mut setup_chunk = setup_focus_runtime_chunk(document_id, 0, 1.0);
        setup_chunk.document_label = "Standalone setup reference".to_string();
        let mut primary_chunk = setup_focus_runtime_chunk(document_id, 3, 1.0);
        primary_chunk.document_label = "Different title for same document".to_string();

        let filtered =
            filter_raw_setup_focus_chunks_by_primary_context(vec![setup_chunk], &[primary_chunk]);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].document_id, document_id);
    }

    #[test]
    fn raw_setup_focus_filter_keeps_structural_config_document_without_primary_label_support() {
        let document_id = Uuid::now_v7();
        let mut anchor_chunk = setup_focus_runtime_chunk(document_id, 0, 1.0);
        anchor_chunk.document_label = "Subject Alpha setup reference".to_string();
        anchor_chunk.source_text = [
            "Install `alpha-module`.",
            "Edit /opt/alpha/alpha.conf.",
            "[Main]",
            "endpointUrl = https://alpha.example/api",
        ]
        .join("\n");
        let mut parameter_chunk = setup_focus_runtime_chunk(document_id, 1, 1.0);
        parameter_chunk.document_label = anchor_chunk.document_label.clone();
        parameter_chunk.source_text =
            "fillDetails = true\nvisible = true\nprintSlip = false".to_string();
        let mut primary_chunk = setup_focus_runtime_chunk(Uuid::now_v7(), 0, 1.0);
        primary_chunk.document_label = "General workflow operations".to_string();

        let filtered = filter_raw_setup_focus_chunks_by_primary_context(
            vec![anchor_chunk, parameter_chunk],
            &[primary_chunk],
        );

        assert_eq!(filtered.len(), 2);
        assert!(filtered.iter().all(|chunk| chunk.document_id == document_id));
    }

    #[test]
    fn raw_setup_focus_filter_discards_weak_unbacked_candidates() {
        let mut setup_chunk = setup_focus_runtime_chunk(Uuid::now_v7(), 0, 1.0);
        setup_chunk.document_label = "Subject Alpha setup reference".to_string();
        setup_chunk.source_text = "General setup overview.".to_string();
        let mut primary_chunk = setup_focus_runtime_chunk(Uuid::now_v7(), 0, 1.0);
        primary_chunk.document_label = "Unrelated operations".to_string();

        let filtered =
            filter_raw_setup_focus_chunks_by_primary_context(vec![setup_chunk], &[primary_chunk]);

        assert!(filtered.is_empty());
    }

    #[test]
    fn artifact_sibling_source_candidates_use_image_title_prefix() {
        let image =
            document_row("alpha-connector-screen.png", "Alpha Connector: status screen.png");
        let manual = document_row("alpha-connector-manual.md", "Alpha Connector - operator manual");
        let unrelated =
            document_row("beta-connector-manual.md", "Beta Connector - operator manual");
        let document_index = [image.clone(), manual.clone(), unrelated]
            .into_iter()
            .map(|document| (document.document_id, document))
            .collect::<HashMap<_, _>>();
        let mut image_chunk = setup_focus_runtime_chunk(image.document_id, 0, 1.0);
        image_chunk.score_kind = RuntimeChunkScoreKind::Relevance;

        let candidates = artifact_sibling_source_document_ids(&[image_chunk], &document_index, 10);

        assert_eq!(candidates.first().copied(), Some(manual.document_id));
        assert!(!candidates.contains(&image.document_id));
    }

    #[test]
    fn artifact_sibling_source_candidates_follow_attachment_parent_page_id() {
        let image = document_row_with_source(
            "alpha-connector-screen.png",
            "Alpha Connector: status screen.png",
            Some("upload://alpha-connector-screen.png"),
            Some("https://example.invalid/download/attachments/12345/status-screen.png"),
        );
        let parent_page = document_row_with_source(
            "alpha-connector-page.html",
            "Alpha Connector",
            Some("upload://file.html"),
            Some("https://example.invalid/pages/viewpage.action?pageId=12345"),
        );
        let title_neighbor =
            document_row("alpha-connector-notes.md", "Alpha Connector - screenshot notes");
        let document_index = [image.clone(), title_neighbor, parent_page.clone()]
            .into_iter()
            .map(|document| (document.document_id, document))
            .collect::<HashMap<_, _>>();
        let mut image_chunk = setup_focus_runtime_chunk(image.document_id, 0, 1.0);
        image_chunk.score_kind = RuntimeChunkScoreKind::Relevance;

        let candidates = artifact_sibling_source_document_ids(&[image_chunk], &document_index, 10);

        assert_eq!(candidates.first().copied(), Some(parent_page.document_id));
        assert!(!candidates.contains(&image.document_id));
    }

    #[test]
    fn artifact_sibling_source_revision_ids_include_readable_history() {
        let document = document_row("alpha-connector-page.html", "Alpha Connector");
        let canonical_revision_id = document.readable_revision_id.expect("readable head");
        let older_revision_id = Uuid::now_v7();
        let processing_revision_id = Uuid::now_v7();
        let other_document_id = Uuid::now_v7();
        let other_revision_id = Uuid::now_v7();
        let revisions = vec![
            revision_row(document.document_id, canonical_revision_id, 4, "ready"),
            revision_row(document.document_id, processing_revision_id, 3, "processing"),
            revision_row(other_document_id, other_revision_id, 2, "ready"),
            revision_row(document.document_id, older_revision_id, 1, "readable"),
        ];

        let revision_ids =
            artifact_sibling_source_revision_ids(&document, canonical_revision_id, &revisions, 8);

        assert_eq!(revision_ids, vec![canonical_revision_id, older_revision_id]);
    }

    #[test]
    fn artifact_sibling_source_merge_keeps_source_context_kind() {
        let mut primary_chunk = setup_focus_runtime_chunk(Uuid::now_v7(), 0, 0.9);
        primary_chunk.score_kind = RuntimeChunkScoreKind::Relevance;
        let mut sibling_chunk =
            setup_focus_runtime_chunk(Uuid::now_v7(), 0, artifact_sibling_source_chunk_score(0, 0));
        sibling_chunk.score_kind = RuntimeChunkScoreKind::SourceContext;
        let sibling_chunk_id = sibling_chunk.chunk_id;

        let merged = merge_query_ir_focus_chunks(vec![primary_chunk], vec![sibling_chunk], 8);
        let merged_sibling = merged
            .iter()
            .find(|chunk| chunk.chunk_id == sibling_chunk_id)
            .expect("sibling source context chunk retained");

        assert_eq!(merged_sibling.score_kind, RuntimeChunkScoreKind::SourceContext);
        assert!(score_value(merged_sibling.score) < DOCUMENT_IDENTITY_SCORE_FLOOR);
    }

    #[test]
    fn query_ir_focus_merge_keeps_focused_document_kind() {
        let mut primary_chunk = setup_focus_runtime_chunk(Uuid::now_v7(), 0, 0.9);
        primary_chunk.score_kind = RuntimeChunkScoreKind::Relevance;
        let mut focused_document =
            setup_focus_runtime_chunk(Uuid::now_v7(), 0, document_identity_chunk_score(0, 0));
        focused_document.score_kind = RuntimeChunkScoreKind::FocusedDocument;
        let focused_document_chunk_id = focused_document.chunk_id;

        let merged = merge_query_ir_focus_chunks(vec![primary_chunk], vec![focused_document], 8);
        let merged_focused_document = merged
            .iter()
            .find(|chunk| chunk.chunk_id == focused_document_chunk_id)
            .expect("focused document chunk retained");

        assert_eq!(merged_focused_document.score_kind, RuntimeChunkScoreKind::FocusedDocument);
    }

    #[test]
    fn query_ir_focus_merge_reserves_source_context_against_document_identity_hits() {
        let primary_chunks = (0..16)
            .map(|index| {
                let mut chunk = setup_focus_runtime_chunk(
                    Uuid::now_v7(),
                    index,
                    DOCUMENT_IDENTITY_SCORE_FLOOR + 100.0 - index as f32,
                );
                chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
                chunk
            })
            .collect::<Vec<_>>();
        let mut source_context =
            setup_focus_runtime_chunk(Uuid::now_v7(), 20, ARTIFACT_SIBLING_SOURCE_SCORE_BASE);
        source_context.score_kind = RuntimeChunkScoreKind::SourceContext;
        source_context.source_text = "companion source evidence".to_string();
        let source_context_chunk_id = source_context.chunk_id;

        let merged = merge_query_ir_focus_chunks(primary_chunks, vec![source_context], 8);

        assert!(
            merged.iter().any(|chunk| chunk.chunk_id == source_context_chunk_id),
            "{merged:#?}"
        );
    }

    #[test]
    fn query_ir_focus_merge_reserves_focused_document_against_document_identity_hits() {
        let primary_chunks = (0..16)
            .map(|index| {
                let mut chunk = setup_focus_runtime_chunk(
                    Uuid::now_v7(),
                    index,
                    DOCUMENT_IDENTITY_SCORE_FLOOR + 100.0 - index as f32,
                );
                chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
                chunk
            })
            .collect::<Vec<_>>();
        let mut focused_document =
            setup_focus_runtime_chunk(Uuid::now_v7(), 20, document_identity_chunk_score(0, 0));
        focused_document.score_kind = RuntimeChunkScoreKind::FocusedDocument;
        let focused_document_chunk_id = focused_document.chunk_id;

        let merged = merge_query_ir_focus_chunks(primary_chunks, vec![focused_document], 8);

        assert!(
            merged.iter().any(|chunk| chunk.chunk_id == focused_document_chunk_id),
            "{merged:#?}"
        );
    }

    #[test]
    fn truncate_reserves_comparison_literal_context_against_identity_hits() {
        let target_document_id = Uuid::now_v7();
        let mut query_ir = setup_query_ir("Alpha Beta");
        query_ir.act = QueryAct::Compare;
        query_ir.comparison = Some(crate::domains::query_ir::ComparisonSpec {
            a: Some("Alpha".to_string()),
            b: Some("Beta".to_string()),
            dimension: "threshold control".to_string(),
        });
        query_ir.retrieval_query =
            Some("Alpha Beta threshold control SAMPLE_LIMIT configuration".to_string());

        let mut chunks = (0..12)
            .map(|index| {
                let mut chunk = setup_focus_runtime_chunk(
                    Uuid::now_v7(),
                    index,
                    DOCUMENT_IDENTITY_SCORE_FLOOR + 100.0 - index as f32,
                );
                chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
                chunk.document_label = format!("General comparison reference {index}");
                chunk.source_text =
                    "Alpha and Beta both describe threshold control behavior in prose.".to_string();
                chunk
            })
            .collect::<Vec<_>>();
        let mut exact = setup_focus_runtime_chunk(target_document_id, 20, 1.0);
        exact.score_kind = RuntimeChunkScoreKind::Relevance;
        exact.document_label = "Alpha Beta comparison".to_string();
        exact.source_text = concat!(
            "AlphaLimitRequests: read SAMPLE_LIMIT_REQUESTS=100\n",
            "AlphaLimitWindowSeconds: read SAMPLE_LIMIT_WINDOW_SECONDS=60"
        )
        .to_string();
        let exact_id = exact.chunk_id;
        chunks.push(exact);

        truncate_chunks_for_context(&mut chunks, 8, Some(&query_ir), &HashSet::new());

        assert!(chunks.iter().any(|chunk| chunk.chunk_id == exact_id), "{chunks:#?}");
        assert_eq!(chunks.len(), 8);
    }

    #[test]
    fn query_ir_focus_merge_reserves_latest_version_inventory() {
        let release_document_id = Uuid::now_v7();
        let latest_chunks = (0..4)
            .map(|index| {
                latest_version_runtime_chunk(
                    release_document_id,
                    index,
                    DOCUMENT_IDENTITY_SCORE_FLOOR + 10.0 - index as f32,
                )
            })
            .collect::<Vec<_>>();
        let latest_chunk_ids =
            latest_chunks.iter().map(|chunk| chunk.chunk_id).collect::<BTreeSet<_>>();
        let focus_chunks = (0..16)
            .map(|index| {
                let mut chunk = setup_focus_runtime_chunk(
                    Uuid::now_v7(),
                    index,
                    DOCUMENT_IDENTITY_SCORE_FLOOR + 500.0 - index as f32,
                );
                chunk.score_kind = RuntimeChunkScoreKind::QueryIrFocus;
                chunk
            })
            .collect::<Vec<_>>();

        let merged = merge_query_ir_focus_chunks(latest_chunks, focus_chunks, 5);

        assert_eq!(merged.len(), 5);
        for chunk_id in latest_chunk_ids {
            assert!(merged.iter().any(|chunk| chunk.chunk_id == chunk_id), "{merged:#?}");
        }
    }

    #[test]
    fn latest_version_scoped_document_ids_include_semantic_chunks() {
        let structural_document_id = Uuid::now_v7();
        let semantic_document_id = Uuid::now_v7();
        let structural_document = LatestVersionDocument {
            document_id: structural_document_id,
            revision_id: Uuid::now_v7(),
            version: vec![1, 0, 2],
            title: "Sample Subject release 1.0.2".to_string(),
            family_key: "alpha suite release {version}".to_string(),
        };
        let mut semantic_chunk =
            latest_version_runtime_chunk(semantic_document_id, 0, DOCUMENT_IDENTITY_SCORE_FLOOR);
        semantic_chunk.document_label = "Sample Subject changelog".to_string();

        let ids = latest_version_scoped_document_ids(&[structural_document], &[semantic_chunk]);

        assert!(ids.contains(&structural_document_id));
        assert!(ids.contains(&semantic_document_id));
    }

    #[test]
    fn versioned_update_procedure_merge_reserves_command_neighbor_chunks() {
        let primary_chunks = (0..16)
            .map(|index| {
                let mut chunk = setup_focus_runtime_chunk(
                    Uuid::now_v7(),
                    index,
                    DOCUMENT_IDENTITY_SCORE_FLOOR + 100.0 - index as f32,
                );
                chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
                chunk
            })
            .collect::<Vec<_>>();
        let procedure_document_id = Uuid::now_v7();
        let procedure_chunks = (0..4)
            .map(|index| {
                let mut chunk = setup_focus_runtime_chunk(
                    procedure_document_id,
                    index,
                    document_identity_chunk_score(0, index as usize),
                );
                chunk.score_kind = RuntimeChunkScoreKind::FocusedDocument;
                chunk.source_text = if index == 1 {
                    "1. Stop Alpha worker\n2. Run ./upgrade_alpha.sh".to_string()
                } else {
                    "Alpha update context".to_string()
                };
                chunk
            })
            .collect::<Vec<_>>();
        let procedure_chunk_ids =
            procedure_chunks.iter().map(|chunk| chunk.chunk_id).collect::<BTreeSet<_>>();

        let merged = merge_versioned_update_procedure_chunks(primary_chunks, procedure_chunks, 8);

        let retained_procedure_ids = merged
            .iter()
            .filter(|chunk| chunk.document_id == procedure_document_id)
            .map(|chunk| chunk.chunk_id)
            .collect::<BTreeSet<_>>();
        assert_eq!(retained_procedure_ids, procedure_chunk_ids, "{merged:#?}");
        assert!(merged.iter().any(|chunk| chunk.source_text.contains("./upgrade_alpha.sh")));
    }

    #[test]
    fn versioned_update_procedure_merge_keeps_instruction_title_anchor_against_identity_tail() {
        let primary_chunks = (0..16)
            .map(|index| {
                let mut chunk = setup_focus_runtime_chunk(
                    Uuid::now_v7(),
                    index,
                    DOCUMENT_IDENTITY_SCORE_FLOOR + 100.0 - index as f32,
                );
                chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
                chunk.document_label = format!("Beta Module {index} reference");
                chunk.source_text = "Beta Module reference note".to_string();
                chunk
            })
            .collect::<Vec<_>>();
        let generic_document_id = Uuid::now_v7();
        let mut procedure_chunks = (0..16)
            .map(|index| {
                let mut chunk = setup_focus_runtime_chunk(
                    generic_document_id,
                    index,
                    versioned_update_procedure_chunk_score(index as usize, 0),
                );
                chunk.score_kind = RuntimeChunkScoreKind::FocusedDocument;
                chunk.document_label = "Generic Alpha maintenance notes".to_string();
                chunk.source_text =
                    "Generic maintenance note without the target command block.".to_string();
                chunk
            })
            .collect::<Vec<_>>();
        let target_document_id = Uuid::now_v7();
        let mut instruction_anchor = setup_focus_runtime_chunk(
            target_document_id,
            0,
            versioned_update_procedure_chunk_score(0, 0)
                + VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_CHUNK_SCORE_BONUS,
        );
        instruction_anchor.score_kind = RuntimeChunkScoreKind::FocusedDocument;
        instruction_anchor.document_label =
            "Instruction for updating Sample Control Object".to_string();
        instruction_anchor.source_text = concat!(
            "Sample Control Object update procedure\n",
            "1. sample-runner --refresh\n",
            "2. sample-runner --apply\n",
            "3. sudo sample-configure alpha-control\n",
            "4. sudo service alpha-control restart"
        )
        .to_string();
        let instruction_anchor_id = instruction_anchor.chunk_id;
        procedure_chunks.push(instruction_anchor);

        let merged = merge_versioned_update_procedure_chunks(primary_chunks, procedure_chunks, 8);

        assert!(merged.iter().any(|chunk| chunk.chunk_id == instruction_anchor_id), "{merged:#?}");
    }

    #[test]
    fn versioned_update_procedure_merge_keeps_exact_target_runbook_against_dense_focused_tail() {
        let primary_chunks = (0..16)
            .map(|index| {
                let mut chunk = setup_focus_runtime_chunk(
                    Uuid::now_v7(),
                    index,
                    DOCUMENT_IDENTITY_SCORE_FLOOR + 100.0 - index as f32,
                );
                chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
                chunk
            })
            .collect::<Vec<_>>();
        let mut procedure_chunks = (0..64)
            .map(|index| {
                let mut chunk = setup_focus_runtime_chunk(
                    Uuid::now_v7(),
                    index,
                    versioned_update_procedure_chunk_score(index as usize, 0)
                        + VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_CHUNK_SCORE_BONUS,
                );
                chunk.score_kind = RuntimeChunkScoreKind::FocusedDocument;
                chunk.document_label = format!("Sample Target update note {index}");
                chunk.source_text = "1. Run sample-runner --refresh.".to_string();
                chunk
            })
            .collect::<Vec<_>>();
        let runbook_document_id = Uuid::now_v7();
        let mut runbook = setup_focus_runtime_chunk(
            runbook_document_id,
            0,
            versioned_update_procedure_exact_target_runbook_chunk_score(23, 0, 240_000),
        );
        runbook.score_kind = RuntimeChunkScoreKind::FocusedDocument;
        runbook.document_label = "Sample Target update runbook".to_string();
        runbook.source_text = concat!(
            "Sample Target update procedure\n",
            "1. sample-runner --refresh\n",
            "2. sample-runner --apply\n",
            "3. sudo sample-configure alpha-console\n",
            "4. sudo service alpha-console restart"
        )
        .to_string();
        let runbook_id = runbook.chunk_id;
        procedure_chunks.push(runbook);

        let merged = merge_versioned_update_procedure_chunks(
            primary_chunks,
            procedure_chunks,
            query_ir_focus_context_top_k(12),
        );

        assert!(
            merged.iter().any(|chunk| chunk.chunk_id == runbook_id),
            "exact-target command runbook must survive dense focused merge tail: {merged:#?}"
        );
    }

    #[test]
    fn query_ir_focus_merge_keeps_identity_grade_versioned_update_runbook() {
        let dense_focused_tail = (0..8)
            .map(|index| {
                let mut chunk = setup_focus_runtime_chunk(
                    Uuid::now_v7(),
                    index,
                    versioned_update_procedure_exact_target_runbook_chunk_score(
                        index as usize,
                        0,
                        300_000,
                    ),
                );
                chunk.score_kind = RuntimeChunkScoreKind::FocusedDocument;
                chunk.document_label = format!("Neighbor procedure fragment {index}");
                chunk.source_text = "1. sample-runner --refresh.".to_string();
                chunk
            })
            .collect::<Vec<_>>();
        let runbook_document_id = Uuid::now_v7();
        let mut runbook = setup_focus_runtime_chunk(
            runbook_document_id,
            0,
            versioned_update_procedure_exact_target_runbook_chunk_score(9, 0, 240_000),
        );
        runbook.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
        runbook.document_label = "Sample Control Object update runbook".to_string();
        runbook.source_text = concat!(
            "Sample Control Object update procedure\n",
            "1. sample-runner --refresh\n",
            "2. sample-runner --apply\n",
            "3. sudo sample-configure alpha-control\n",
            "4. sudo service alpha-control restart"
        )
        .to_string();
        let runbook_id = runbook.chunk_id;
        let mut existing_chunks = dense_focused_tail;
        existing_chunks.push(runbook);
        let query_focus_chunks = (0..64)
            .map(|index| {
                let mut chunk = setup_focus_runtime_chunk(
                    Uuid::now_v7(),
                    index,
                    query_ir_focus_chunk_score(index as usize),
                );
                chunk.score_kind = RuntimeChunkScoreKind::QueryIrFocus;
                chunk.document_label = format!("Query focus companion {index}");
                chunk.source_text = "Companion context".to_string();
                chunk
            })
            .collect::<Vec<_>>();

        let merged = merge_query_ir_focus_chunks(existing_chunks, query_focus_chunks, 12);

        assert!(
            merged.iter().any(|chunk| chunk.chunk_id == runbook_id),
            "identity-grade exact procedure runbook must survive later focus merges: {merged:#?}"
        );
    }

    #[test]
    fn versioned_update_query_aware_merge_keeps_exact_runbook_against_identity_noise() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types =
            vec!["artifact".to_string(), "version".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        query_ir.literal_constraints =
            vec![LiteralSpan { text: "Sample Target".to_string(), kind: LiteralKind::Identifier }];
        query_ir.retrieval_query = Some("Sample Target version update procedure".to_string());

        let identity_noise = (0..64)
            .map(|index| {
                let mut chunk = setup_focus_runtime_chunk(
                    Uuid::now_v7(),
                    index,
                    DOCUMENT_IDENTITY_SCORE_FLOOR + 10_000.0 - index as f32,
                );
                chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
                chunk.document_label = format!("Sample Target reference {index}");
                chunk.source_text = "Reference text without update commands.".to_string();
                chunk
            })
            .collect::<Vec<_>>();

        let mut procedure_chunks = (0..64)
            .map(|index| {
                let mut chunk = setup_focus_runtime_chunk(
                    Uuid::now_v7(),
                    index,
                    versioned_update_procedure_chunk_score(index as usize, 0)
                        + VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_CHUNK_SCORE_BONUS,
                );
                chunk.score_kind = RuntimeChunkScoreKind::FocusedDocument;
                chunk.document_label = format!("Sample Target update overview {index}");
                chunk.source_text =
                    "Overview of supported releases without the command sequence.".to_string();
                chunk
            })
            .collect::<Vec<_>>();

        let runbook_document_id = Uuid::now_v7();
        let mut runbook = setup_focus_runtime_chunk(runbook_document_id, 0, 1.0);
        runbook.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
        runbook.document_label =
            "Instruction for updating Sample Target from Platform 1.0 to 2.0".to_string();
        runbook.source_text = concat!(
            "Sample Target update from Platform 1.0 to 2.0\n",
            "1. Refresh bundle metadata with sample-runner --refresh.\n",
            "2. Upgrade installed packages with sample-runner --apply.\n",
            "3. Apply distribution upgrade with sample-runner --migrate.\n",
            "4. Run sample-platform-update.\n",
            "5. Reconfigure Alpha REST with sudo sample-configure alpha-rest.\n",
            "6. Restart Alpha REST with sudo service alpha-rest restart."
        )
        .to_string();
        let runbook_id = runbook.chunk_id;
        procedure_chunks.push(runbook);

        let merged = merge_versioned_update_procedure_chunks_for_query(
            identity_noise,
            procedure_chunks,
            query_ir_focus_context_top_k(12),
            Some(&query_ir),
        );

        assert!(
            merged.iter().any(|chunk| chunk.chunk_id == runbook_id),
            "query-aware versioned procedure merge must reserve the exact command runbook: {merged:#?}"
        );
    }

    #[test]
    fn versioned_update_context_allows_concept_when_procedure_is_versioned() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types =
            vec!["concept".to_string(), "procedure".to_string(), "version".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        query_ir.retrieval_query = Some("how to update Sample Target version?".to_string());

        assert!(
            query_ir_requests_versioned_update_procedure_context(
                "how to update Sample Target version?",
                &query_ir,
            ),
            "a compiler-emitted concept tag should not suppress a versioned procedure lane"
        );
    }

    #[test]
    fn query_ir_focus_merge_keeps_exact_versioned_update_runbook_for_query() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types =
            vec!["artifact".to_string(), "version".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        query_ir.retrieval_query = Some("how to update Sample Target version?".to_string());

        let mut existing_chunks = (0..64)
            .map(|index| {
                let mut chunk = setup_focus_runtime_chunk(
                    Uuid::now_v7(),
                    index,
                    versioned_update_procedure_chunk_score(index as usize, 0)
                        + VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_CHUNK_SCORE_BONUS,
                );
                chunk.score_kind = RuntimeChunkScoreKind::FocusedDocument;
                chunk.document_label = format!("Sample Target update note {index}");
                chunk.source_text =
                    "Release overview without the full command sequence.".to_string();
                chunk
            })
            .collect::<Vec<_>>();

        let runbook_document_id = Uuid::now_v7();
        let mut runbook = setup_focus_runtime_chunk(runbook_document_id, 0, 1.0);
        runbook.score_kind = RuntimeChunkScoreKind::FocusedDocument;
        runbook.document_label =
            "Instruction for updating Sample Target from Platform 1.0 to 2.0".to_string();
        runbook.source_text = concat!(
            "Sample Target update from Platform 1.0 to 2.0\n",
            "1. sample-runner --refresh\n",
            "2. sample-runner --apply\n",
            "3. sample-runner --migrate\n",
            "4. sample-platform-update\n",
            "5. sudo sample-configure alpha-rest\n",
            "6. sudo service alpha-rest restart"
        )
        .to_string();
        let runbook_id = runbook.chunk_id;
        existing_chunks.push(runbook);

        let query_focus_chunks = (0..64)
            .map(|index| {
                let mut chunk = setup_focus_runtime_chunk(
                    Uuid::now_v7(),
                    index,
                    query_ir_focus_chunk_score(index as usize),
                );
                chunk.score_kind = RuntimeChunkScoreKind::QueryIrFocus;
                chunk.document_label = format!("Query focus companion {index}");
                chunk.source_text = "Companion context".to_string();
                chunk
            })
            .collect::<Vec<_>>();

        let merged = merge_query_ir_focus_chunks_for_query(
            existing_chunks,
            query_focus_chunks,
            query_ir_focus_context_top_k(12),
            Some(&query_ir),
        );

        assert!(
            merged.iter().any(|chunk| chunk.chunk_id == runbook_id),
            "query-aware focus merge must keep the exact versioned update runbook: {merged:#?}"
        );
    }

    #[test]
    fn versioned_update_procedure_context_truncation_keeps_seed_neighbors() {
        let mut chunks = (0..16)
            .map(|index| {
                let mut chunk = setup_focus_runtime_chunk(
                    Uuid::now_v7(),
                    index,
                    DOCUMENT_IDENTITY_SCORE_FLOOR + 100.0 - index as f32,
                );
                chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
                chunk
            })
            .collect::<Vec<_>>();

        let revision_id = Uuid::now_v7();
        let procedure_document_id = Uuid::now_v7();
        let mut seed = setup_focus_runtime_chunk(
            procedure_document_id,
            0,
            document_identity_chunk_score(0, 0),
        );
        seed.revision_id = revision_id;
        seed.score_kind = RuntimeChunkScoreKind::FocusedDocument;
        seed.document_label = "Alpha product transition guide".to_string();
        seed.source_text = "Alpha product transition:\n\
             1. Update AlphaControlCenter to version 9.8.7.6.\n\
             2. Update Alpha subject artifact to version 10.4.2."
            .to_string();
        let mut package_neighbor = setup_focus_runtime_chunk(
            procedure_document_id,
            1,
            document_identity_chunk_score(0, 1),
        );
        package_neighbor.revision_id = revision_id;
        package_neighbor.score_kind = RuntimeChunkScoreKind::FocusedDocument;
        package_neighbor.document_label = seed.document_label.clone();
        package_neighbor.source_text =
            "Manual artifact update:\n1. Run sample-refresh.\n2. Run sample-install alpha-pos."
                .to_string();
        let package_neighbor_id = package_neighbor.chunk_id;
        chunks.push(seed);
        chunks.push(package_neighbor);

        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        query_ir.retrieval_query = Some("how to update Sample Target version?".to_string());

        truncate_chunks_for_context(&mut chunks, 8, Some(&query_ir), &HashSet::new());

        assert!(chunks.iter().any(|chunk| chunk.chunk_id == package_neighbor_id), "{chunks:#?}");
    }

    #[test]
    fn versioned_update_procedure_context_truncation_prefers_instruction_title_anchor() {
        let mut chunks = (0..16)
            .map(|index| {
                let mut chunk = setup_focus_runtime_chunk(
                    Uuid::now_v7(),
                    index,
                    DOCUMENT_IDENTITY_SCORE_FLOOR + 200.0 - index as f32,
                );
                chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
                chunk.document_label = format!("Beta Module {index} reference");
                chunk.source_text = "Beta Module package compatibility note".to_string();
                chunk
            })
            .collect::<Vec<_>>();

        let generic_procedure_document_id = Uuid::now_v7();
        for index in 0..12 {
            let mut chunk = setup_focus_runtime_chunk(
                generic_procedure_document_id,
                index,
                DOCUMENT_IDENTITY_SCORE_FLOOR + 100.0 - index as f32,
            );
            chunk.score_kind = RuntimeChunkScoreKind::FocusedDocument;
            chunk.document_label = "Generic Alpha update notes".to_string();
            chunk.source_text =
                "Run ./generic-update.sh and verify the service status.".to_string();
            chunks.push(chunk);
        }

        let instruction_document_id = Uuid::now_v7();
        let mut instruction_anchor = setup_focus_runtime_chunk(
            instruction_document_id,
            0,
            versioned_update_procedure_chunk_score(0, 0)
                + VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_CHUNK_SCORE_BONUS,
        );
        instruction_anchor.score_kind = RuntimeChunkScoreKind::FocusedDocument;
        instruction_anchor.document_label =
            "Instruction for updating Sample Control Object".to_string();
        instruction_anchor.source_text = concat!(
            "Sample Control Object update procedure\n",
            "1. sample-runner --refresh\n",
            "2. sample-runner --apply\n",
            "3. sudo sample-configure alpha-control\n",
            "4. sudo service alpha-control restart"
        )
        .to_string();
        let instruction_anchor_id = instruction_anchor.chunk_id;
        chunks.push(instruction_anchor);

        let mut query_ir = setup_query_ir("Sample Control Object");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities = vec![EntityMention {
            label: "Sample Control Object".to_string(),
            role: EntityRole::Subject,
        }];
        query_ir.retrieval_query = Some("how to update Sample Control Object version?".to_string());

        truncate_chunks_for_context(&mut chunks, 8, Some(&query_ir), &HashSet::new());

        assert!(chunks.iter().any(|chunk| chunk.chunk_id == instruction_anchor_id), "{chunks:#?}");
    }

    #[test]
    fn document_evidence_anchor_candidates_match_document_identity_terms() {
        let manifest = document_row("sample_manifest.yaml", "Sample manifest");
        let orchestration =
            document_row("sample_orchestration.md", "Sample orchestration reference");
        let infra = document_row("sample_infra.plan", "Sample infrastructure reference");
        let document_index = HashMap::from([
            (manifest.document_id, manifest.clone()),
            (orchestration.document_id, orchestration),
            (infra.document_id, infra),
        ]);
        let mut query_ir = setup_query_ir("Sample Manifest");
        query_ir.act = QueryAct::Enumerate;
        query_ir.target_types = vec!["service".to_string()];
        query_ir.document_focus = None;
        query_ir.target_entities.clear();
        query_ir.retrieval_query = Some("alpha entries fields values".to_string());

        let candidates = document_evidence_anchor_candidate_document_ids(
            "What services and ports are defined in the Sample Manifest configuration?",
            Some(&query_ir),
            &document_index,
            3,
        );

        assert_eq!(candidates.first(), Some(&manifest.document_id));
    }

    #[test]
    fn versioned_update_procedure_evidence_triggers_for_typed_procedure_questions() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];

        assert!(question_requests_versioned_update_procedure_evidence(
            "how to update Sample Target?",
            Some(&query_ir)
        ));
        assert!(question_requests_versioned_update_procedure_evidence(
            "how do I refresh Sample Target?",
            Some(&query_ir)
        ));

        query_ir.act = QueryAct::RetrieveValue;
        assert!(!question_requests_versioned_update_procedure_evidence(
            "what version is Sample Target?",
            Some(&query_ir)
        ));

        query_ir.act = QueryAct::ConfigureHow;
        query_ir.target_types =
            vec!["concept".to_string(), "artifact".to_string(), "procedure".to_string()];
        assert!(!question_requests_versioned_update_procedure_evidence(
            "how to configure Sample Subject connector?",
            Some(&query_ir)
        ));
    }

    #[test]
    fn versioned_update_procedure_evidence_accepts_procedure_runbook_ir_without_software_tag() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.document_focus = None;
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        query_ir.target_types = vec!["procedure".to_string(), "document".to_string()];

        assert!(question_requests_versioned_update_procedure_evidence(
            "how to update Sample Target?",
            Some(&query_ir)
        ));

        query_ir.target_types =
            vec!["procedure".to_string(), "document".to_string(), "concept".to_string()];
        assert!(!question_requests_versioned_update_procedure_evidence(
            "how to configure Sample Target?",
            Some(&query_ir)
        ));

        query_ir.target_types =
            vec!["procedure".to_string(), "document".to_string(), "package".to_string()];
        assert!(!question_requests_versioned_update_procedure_evidence(
            "how to configure Sample Target?",
            Some(&query_ir)
        ));
    }

    #[test]
    fn versioned_update_procedure_evidence_recovers_raw_subject_when_compiler_misses_target() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types.clear();
        query_ir.target_entities.clear();
        query_ir.document_focus = None;
        query_ir.retrieval_query = None;
        let term_model =
            versioned_update_procedure_term_model("how to update support node?", Some(&query_ir));

        assert!(term_model.procedure_terms.contains("update"));
        assert!(term_model.subject_terms.contains("support"));
        assert!(term_model.subject_terms.contains("node"));
        assert!(question_requests_versioned_update_procedure_evidence(
            "how to update support node?",
            Some(&query_ir)
        ));
    }

    #[test]
    fn versioned_update_procedure_evidence_uses_recovered_raw_subject_for_command_seed() {
        let document_id = Uuid::now_v7();
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types.clear();
        query_ir.target_entities.clear();
        query_ir.document_focus = None;
        query_ir.retrieval_query = None;
        let term_model =
            versioned_update_procedure_term_model("how to update support node?", Some(&query_ir));
        let mut update = setup_focus_runtime_chunk(document_id, 7, 1.0);
        update.document_label = "Installation and maintenance".to_string();
        update.source_text = concat!(
            "Support node update\n",
            "1. sample-transfer https://updates.example.invalid/alpha/update_secondary.sh -o /tmp/sample-runner.sh\n",
            "2. sample-prepare +x /tmp/sample-runner.sh\n",
            "3. /tmp/sample-runner.sh"
        )
        .to_string();

        let candidates =
            versioned_update_procedure_candidates_from_evidence_chunks(&[update], &term_model, 4);

        assert_eq!(candidates.len(), 1, "{candidates:#?}");
        assert_eq!(candidates[0].document_id, document_id);
        assert_eq!(candidates[0].seed_chunk_indices, vec![7]);
    }

    #[test]
    fn setup_variant_candidates_prioritize_subject_identity_over_generic_setup_titles() {
        let alpha = document_row("alpha-subject.html", "Subject Alpha setup guide");
        let beta = document_row("beta-subject.html", "Subject Beta integration settings");
        let generic = document_row("display-settings.html", "Settings appearance setup guide");
        let document_index = HashMap::from([
            (generic.document_id, generic.clone()),
            (alpha.document_id, alpha.clone()),
            (beta.document_id, beta.clone()),
        ]);
        let mut query_ir = setup_query_ir("Subject");
        query_ir.document_focus = None;
        query_ir.target_entities =
            vec![EntityMention { label: "Subject".to_string(), role: EntityRole::Subject }];
        query_ir.retrieval_query = Some("how to configure Subject variants".to_string());

        let candidates = setup_variant_candidate_document_ids(
            "how to configure Subject?",
            Some(&query_ir),
            &document_index,
            3,
        );

        assert!(candidates.contains(&alpha.document_id));
        assert!(candidates.contains(&beta.document_id));
        assert!(!candidates.contains(&generic.document_id));
    }

    #[test]
    fn setup_variant_candidates_weight_rare_identity_terms_over_common_setup_terms() {
        let target = document_row("target.html", "Subject Alpha connector guide");
        let generic_a = document_row("generic-a.html", "Configure display settings");
        let generic_b = document_row("generic-b.html", "Configure server appearance");
        let generic_c = document_row("generic-c.html", "Configure endpoint layout");
        let document_index = HashMap::from([
            (generic_a.document_id, generic_a.clone()),
            (generic_b.document_id, generic_b.clone()),
            (generic_c.document_id, generic_c.clone()),
            (target.document_id, target.clone()),
        ]);
        let mut query_ir = setup_query_ir("Workflow library");
        query_ir.document_focus = None;
        query_ir.target_entities = Vec::new();
        query_ir.retrieval_query = Some("how to configure subject?".to_string());

        let candidates = setup_variant_candidate_document_ids(
            "how to configure subject?",
            Some(&query_ir),
            &document_index,
            1,
        );

        assert_eq!(candidates, [target.document_id]);
    }

    #[test]
    fn versioned_update_procedure_candidates_prioritize_howto_transition_document() {
        let howto =
            document_row("alpha-upgrade-howto.html", "Sample Target upgrade procedure - HOW-TO");
        let transition = document_row("alpha-migration.html", "Sample Target migration guide");
        let screenshot = document_row("alpha-screenshot.png", "Sample Target migration screenshot");
        let history = document_row("alpha-history.html", "Sample Subject release history");
        let unrelated = document_row("license.png", "Beta Gateway license notice");
        let document_index = HashMap::from([
            (howto.document_id, howto.clone()),
            (transition.document_id, transition.clone()),
            (screenshot.document_id, screenshot.clone()),
            (history.document_id, history),
            (unrelated.document_id, unrelated),
        ]);
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];

        let candidates = versioned_update_procedure_candidate_document_ids(
            "how to update Sample Target?",
            Some(&query_ir),
            &document_index,
            4,
        );

        assert_eq!(candidates.first(), Some(&howto.document_id));
        assert!(candidates.contains(&transition.document_id));
        assert!(!candidates.contains(&screenshot.document_id));
    }

    #[test]
    fn conceptual_procedure_query_requests_runbook_evidence_without_version_signal() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.document_focus = None;
        query_ir.target_types = vec!["procedure".to_string(), "concept".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        query_ir.retrieval_query = Some("how to update Sample Target".to_string());

        assert!(question_requests_versioned_update_procedure_evidence(
            "how to update Sample Target?",
            Some(&query_ir)
        ));
    }

    #[test]
    fn versioned_update_procedure_candidates_tier_exact_target_title_identity_first() {
        let exact = document_row("alpha-suite-90.html", "Sample Subject 9.0 update guide");
        let neighboring_transition =
            document_row("alpha-platform.html", "Sample Subject platform update transition guide");
        let generic_update = document_row("generic-update.html", "General update checklist");
        let document_index = HashMap::from([
            (neighboring_transition.document_id, neighboring_transition.clone()),
            (generic_update.document_id, generic_update),
            (exact.document_id, exact.clone()),
        ]);
        let mut query_ir = setup_query_ir("Sample Subject 9.0");
        query_ir.target_types =
            vec!["artifact".to_string(), "procedure".to_string(), "version".to_string()];
        query_ir.target_entities = vec![EntityMention {
            label: "Sample Subject 9.0".to_string(),
            role: EntityRole::Subject,
        }];

        let candidates = versioned_update_procedure_candidate_documents(
            "how to update Sample Subject 9.0?",
            Some(&query_ir),
            &document_index,
            3,
        );

        assert_eq!(
            candidates.first().map(|candidate| candidate.document_id),
            Some(exact.document_id)
        );
        let exact_candidate = candidates
            .iter()
            .find(|candidate| candidate.document_id == exact.document_id)
            .expect("exact title identity candidate");
        assert!(exact_candidate.exact_title_identity);
        assert!(exact_candidate.allow_head_fallback);
        assert!(!exact_candidate.requires_action_text_match);
    }

    #[test]
    fn versioned_update_procedure_candidates_accept_exact_target_title_anchor() {
        let instruction = document_row(
            "alpha-console-platform-update.html",
            "Instruction for updating Sample Target from platform 1 to platform 2",
        );
        let overview = document_row("alpha-console-overview.html", "Sample Target overview");
        let document_index = HashMap::from([
            (instruction.document_id, instruction.clone()),
            (overview.document_id, overview),
        ]);
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types =
            vec!["artifact".to_string(), "procedure".to_string(), "version".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];

        let candidates = versioned_update_procedure_candidate_documents(
            "how to update Sample Target version?",
            Some(&query_ir),
            &document_index,
            4,
        );

        let candidate = candidates
            .iter()
            .find(|candidate| candidate.document_id == instruction.document_id)
            .expect("exact instruction action title candidate");
        assert!(candidate.exact_title_identity);
        assert!(candidate.allow_head_fallback);
        assert!(!candidate.requires_action_text_match);
    }

    #[test]
    fn versioned_update_procedure_exact_runbook_scan_keeps_action_title_among_exact_target_docs() {
        let runbook = document_row(
            "alpha-control-upgrade.html",
            "Instruction for updating Sample Target from platform 1 to platform 2",
        );
        let mut documents = (0..40)
            .map(|index| {
                let file_name = format!("alpha-control-reference-{index}.html");
                let title = format!("Sample Target reference section {index:02}");
                let document = document_row(&file_name, &title);
                (document.document_id, document)
            })
            .collect::<HashMap<_, _>>();
        documents.insert(runbook.document_id, runbook.clone());
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types =
            vec!["artifact".to_string(), "procedure".to_string(), "version".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        query_ir.retrieval_query = Some("Sample Target update version procedure".to_string());

        let candidates = versioned_update_procedure_exact_target_runbook_scan_candidates(
            "how to update Sample Target version?",
            Some(&query_ir),
            &documents,
            16,
        );

        assert_eq!(
            candidates.first().map(|candidate| candidate.document_id),
            Some(runbook.document_id),
            "{candidates:#?}"
        );
    }

    #[test]
    fn versioned_update_procedure_candidates_keep_exact_target_image_runbook_titles() {
        let exact_image = document_row("alpha-control-update.png", "Alpha Control update runbook");
        let neighboring_transition =
            document_row("alpha-platform.html", "Alpha Control platform transition guide");
        let document_index = HashMap::from([
            (neighboring_transition.document_id, neighboring_transition),
            (exact_image.document_id, exact_image.clone()),
        ]);
        let mut query_ir = setup_query_ir("Alpha Control");
        query_ir.target_types =
            vec!["artifact".to_string(), "procedure".to_string(), "version".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Alpha Control".to_string(), role: EntityRole::Subject }];

        let candidates = versioned_update_procedure_candidate_documents(
            "how to update Alpha Control version?",
            Some(&query_ir),
            &document_index,
            2,
        );

        let candidate = candidates
            .iter()
            .find(|candidate| candidate.document_id == exact_image.document_id)
            .expect("exact OCR/image runbook candidate");
        assert!(candidate.exact_title_identity);
        assert!(candidate.allow_head_fallback);
        assert!(!candidate.requires_action_text_match);
    }

    #[test]
    fn versioned_update_procedure_candidates_match_subject_acronym_titles() {
        let acronym_howto = document_row("as-update.html", "AS update with migration checks");
        let broad_subject = document_row(
            "alpha-service-online.html",
            "Alpha Service online receipt upload example",
        );
        let full_subject_howto =
            document_row("alpha-service-update.html", "Alpha Service update checklist");
        let generic_howto = document_row("generic-update.html", "General update checklist");
        let acronym_overview = document_row("as-overview.html", "AS overview");
        let history = document_row("alpha-service-history.html", "Alpha Service release history");
        let document_index = HashMap::from([
            (broad_subject.document_id, broad_subject.clone()),
            (full_subject_howto.document_id, full_subject_howto.clone()),
            (generic_howto.document_id, generic_howto.clone()),
            (acronym_howto.document_id, acronym_howto.clone()),
            (acronym_overview.document_id, acronym_overview.clone()),
            (history.document_id, history),
        ]);
        let mut query_ir = setup_query_ir("Alpha Service");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Alpha Service".to_string(), role: EntityRole::Subject }];

        let candidates = versioned_update_procedure_candidate_documents(
            "how to update Alpha Service?",
            Some(&query_ir),
            &document_index,
            4,
        );
        let candidate_ids =
            candidates.iter().map(|candidate| candidate.document_id).collect::<Vec<_>>();

        assert!(candidate_ids.contains(&acronym_howto.document_id));
        assert!(candidate_ids.contains(&full_subject_howto.document_id));
        assert!(!candidate_ids.contains(&broad_subject.document_id));
        assert!(!candidate_ids.contains(&acronym_overview.document_id));
        if let Some(candidate) =
            candidates.iter().find(|candidate| candidate.document_id == generic_howto.document_id)
        {
            assert!(!candidate.allow_head_fallback);
            assert!(candidate.requires_action_text_match);
            assert!(candidate.requires_subject_text_match);
        }
    }

    #[test]
    fn versioned_update_procedure_candidates_require_action_text_for_subject_only_titles() {
        let action_title =
            document_row("alpha-node-refresh.html", "Sample Target refresh checklist");
        let subject_only_title =
            document_row("alpha-node-operations.html", "Sample Target operations guide");
        let document_index = HashMap::from([
            (subject_only_title.document_id, subject_only_title.clone()),
            (action_title.document_id, action_title.clone()),
        ]);
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];

        let candidates = versioned_update_procedure_candidate_documents(
            "how to refresh Sample Target?",
            Some(&query_ir),
            &document_index,
            4,
        );

        let action_candidate = candidates
            .iter()
            .find(|candidate| candidate.document_id == action_title.document_id)
            .expect("action-title candidate");
        assert!(action_candidate.allow_head_fallback);
        assert!(!action_candidate.requires_action_text_match);
        assert!(!action_candidate.requires_subject_text_match);
        let subject_only_candidate = candidates
            .iter()
            .find(|candidate| candidate.document_id == subject_only_title.document_id)
            .expect("subject-only candidate");
        assert!(!subject_only_candidate.allow_head_fallback);
        assert!(subject_only_candidate.requires_action_text_match);
        assert!(!subject_only_candidate.requires_subject_text_match);
    }

    #[test]
    fn versioned_update_procedure_candidates_include_action_title_with_body_match_required() {
        let action_only_title = document_row("manual-update.html", "Manual update checklist");
        let subject_title = document_row("alpha-node-ops.html", "Sample Target operations guide");
        let unrelated = document_row("other.html", "General troubleshooting guide");
        let document_index = HashMap::from([
            (subject_title.document_id, subject_title),
            (action_only_title.document_id, action_only_title.clone()),
            (unrelated.document_id, unrelated),
        ]);
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];

        let candidates = versioned_update_procedure_candidate_documents(
            "how to update Sample Target?",
            Some(&query_ir),
            &document_index,
            8,
        );

        let action_candidate = candidates
            .iter()
            .find(|candidate| candidate.document_id == action_only_title.document_id)
            .expect("action-only title candidate");
        assert!(!action_candidate.allow_head_fallback);
        assert!(action_candidate.requires_action_text_match);
        assert!(action_candidate.requires_subject_text_match);
    }

    #[test]
    fn versioned_update_procedure_candidates_reserve_action_only_runbook_titles() {
        let action_only_title = document_row("manual-update.html", "Manual update checklist");
        let subject_a = document_row("alpha-a.html", "Sample Target migration guide");
        let subject_b = document_row("alpha-b.html", "Sample Target update policy");
        let subject_c = document_row("alpha-c.html", "Sample Target release history");
        let subject_d = document_row("alpha-d.html", "Sample Target operations guide");
        let document_index = HashMap::from([
            (subject_a.document_id, subject_a),
            (subject_b.document_id, subject_b),
            (subject_c.document_id, subject_c),
            (subject_d.document_id, subject_d),
            (action_only_title.document_id, action_only_title.clone()),
        ]);
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];

        let candidates = versioned_update_procedure_candidate_documents(
            "how to update Sample Target?",
            Some(&query_ir),
            &document_index,
            4,
        );

        let action_candidate = candidates
            .iter()
            .find(|candidate| candidate.document_id == action_only_title.document_id)
            .expect("reserved action-only title candidate");
        assert!(!action_candidate.allow_head_fallback);
        assert!(action_candidate.requires_action_text_match);
        assert!(action_candidate.requires_subject_text_match);
    }

    #[test]
    fn versioned_update_procedure_candidates_accept_raw_question_focus_without_ir_subject() {
        let action_title = document_row("alpha-node-update.html", "Sample Target update checklist");
        let overview = document_row("alpha-node-overview.html", "Sample Target overview");
        let document_index = HashMap::from([
            (action_title.document_id, action_title.clone()),
            (overview.document_id, overview),
        ]);
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["procedure".to_string(), "document".to_string()];
        query_ir.target_entities.clear();
        query_ir.document_focus = None;
        query_ir.retrieval_query = Some("how to update Sample Target?".to_string());

        let candidates = versioned_update_procedure_candidate_documents(
            "how to update Sample Target?",
            Some(&query_ir),
            &document_index,
            4,
        );

        let action_candidate = candidates
            .iter()
            .find(|candidate| candidate.document_id == action_title.document_id)
            .expect("raw-question action-title candidate");
        assert!(action_candidate.exact_title_identity);
        assert!(action_candidate.allow_head_fallback);
        assert!(!action_candidate.requires_action_text_match);
        assert!(!action_candidate.requires_subject_text_match);
    }

    #[test]
    fn versioned_update_procedure_title_scan_uses_raw_question_identity_without_ir_subject() {
        let exact = document_row(
            "sample-process-update.html",
            "Instruction for updating Sample Process from baseline 1 to baseline 2",
        );
        let generic = document_row("manual-update.html", "Manual update checklist");
        let document_index =
            HashMap::from([(exact.document_id, exact.clone()), (generic.document_id, generic)]);
        let mut query_ir = setup_query_ir("unfocused");
        query_ir.target_types =
            vec!["procedure".to_string(), "document".to_string(), "version".to_string()];
        query_ir.target_entities.clear();
        query_ir.document_focus = None;
        query_ir.retrieval_query = Some("how to update Sample Process version?".to_string());

        let term_model = versioned_update_procedure_term_model(
            "how to update Sample Process version?",
            Some(&query_ir),
        );
        assert!(
            term_model
                .target_identity_sequences
                .iter()
                .any(|sequence| sequence == &vec!["sample".to_string(), "process".to_string()])
        );
        assert!(!term_model.procedure_terms.contains("sample"));
        assert!(!term_model.procedure_terms.contains("process"));

        let candidates = versioned_update_procedure_instruction_title_candidates(
            "how to update Sample Process version?",
            Some(&query_ir),
            &document_index,
            4,
        );

        assert_eq!(candidates.len(), 1, "{candidates:#?}");
        assert_eq!(candidates[0].document_id, exact.document_id);
        assert!(candidates[0].exact_title_identity);
    }

    #[test]
    fn versioned_update_procedure_title_scan_matches_inflected_raw_identity() {
        let exact = document_row("sample-processes-update.html", "Sample Processes update guide");
        let unrelated = document_row("manual-update.html", "Manual update checklist");
        let document_index =
            HashMap::from([(exact.document_id, exact.clone()), (unrelated.document_id, unrelated)]);
        let mut query_ir = setup_query_ir("unfocused");
        query_ir.target_types = vec!["procedure".to_string(), "document".to_string()];
        query_ir.target_entities.clear();
        query_ir.document_focus = None;
        query_ir.retrieval_query = Some("how to update Sample Process?".to_string());

        let candidates = versioned_update_procedure_instruction_title_candidates(
            "how to update Sample Process?",
            Some(&query_ir),
            &document_index,
            4,
        );

        assert_eq!(candidates.len(), 1, "{candidates:#?}");
        assert_eq!(candidates[0].document_id, exact.document_id);
    }

    #[test]
    fn raw_question_procedure_runbook_evidence_requires_single_document_procedure_target() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["procedure".to_string(), "document".to_string()];
        query_ir.target_entities.clear();
        query_ir.document_focus = None;
        query_ir.retrieval_query = Some("how to update Sample Target?".to_string());
        let term_model =
            versioned_update_procedure_term_model("how to update Sample Target?", Some(&query_ir));

        assert!(query_ir_allows_raw_question_procedure_runbook_evidence(&query_ir, &term_model));

        query_ir.scope = QueryScope::MultiDocument;
        let term_model =
            versioned_update_procedure_term_model("how to update Sample Target?", Some(&query_ir));

        assert!(!query_ir_allows_raw_question_procedure_runbook_evidence(&query_ir, &term_model));
    }

    #[test]
    fn versioned_update_procedure_evidence_candidates_promote_seeded_command_chunks() {
        let evidence_document_id = Uuid::now_v7();
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        let term_model =
            versioned_update_procedure_term_model("how to update Sample Target?", Some(&query_ir));
        let mut evidence_chunk = setup_focus_runtime_chunk(evidence_document_id, 3, 1.0);
        evidence_chunk.document_label = "Lifecycle maintenance guide".to_string();
        evidence_chunk.source_text = concat!(
            "Sample Target update procedure\n",
            "1. Remove /etc/alpha/sources.list\n",
            "2. sample-runner --install sample-update-unit\n",
            "3. sh ./update_alpha.sh --now"
        )
        .to_string();
        let mut prose_chunk = setup_focus_runtime_chunk(Uuid::now_v7(), 0, 1.0);
        prose_chunk.document_label = "Sample Target history".to_string();
        prose_chunk.source_text =
            "Sample Target update policy was announced in the release notes.".to_string();
        let mut wrong_action_chunk = setup_focus_runtime_chunk(Uuid::now_v7(), 0, 1.0);
        wrong_action_chunk.document_label = "Sample Target API example".to_string();
        wrong_action_chunk.source_text = concat!(
            "Sample Target upload procedure\n",
            "1. POST /v1/documents\n",
            "2. Run ./send_example.sh"
        )
        .to_string();

        let candidates = versioned_update_procedure_candidates_from_evidence_chunks(
            &[prose_chunk, wrong_action_chunk, evidence_chunk],
            &term_model,
            4,
        );

        assert_eq!(candidates.len(), 1, "{candidates:#?}");
        assert_eq!(candidates[0].document_id, evidence_document_id);
        assert_eq!(candidates[0].seed_chunk_indices, vec![3]);
        assert!(!candidates[0].allow_head_fallback);
        assert!(candidates[0].requires_action_text_match);
    }

    #[test]
    fn versioned_update_procedure_evidence_keeps_action_bound_setup_signature() {
        let document_id = Uuid::now_v7();
        let mut query_ir = setup_query_ir("Sample Process");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Process".to_string(), role: EntityRole::Subject }];
        let term_model =
            versioned_update_procedure_term_model("how to update Sample Process?", Some(&query_ir));
        let mut chunk = setup_focus_runtime_chunk(document_id, 2, 1.0);
        chunk.document_label = "Sample Process install and update".to_string();
        chunk.source_text = concat!(
            "Install and update Sample Process agent\n",
            "1. Download the maintenance artifact.\n",
            "sample-transfer https://example.invalid/sample/install_agent.bin -o /tmp/install_agent.bin\n",
            "2. sample-prepare +x /tmp/install_agent.bin\n",
            "3. /tmp/install_agent.bin\n",
            "4. Restart Sample Process workers."
        )
        .to_string();
        let evidence = versioned_update_procedure_chunk_evidence(&chunk, &term_model);
        assert!(evidence.has_setup_script_signature, "{evidence:#?}");
        assert!(versioned_update_procedure_setup_signature_is_action_bound(evidence));

        let candidates =
            versioned_update_procedure_candidates_from_evidence_chunks(&[chunk], &term_model, 4);

        assert_eq!(candidates.len(), 1, "{candidates:#?}");
        assert_eq!(candidates[0].document_id, document_id);
        assert_eq!(candidates[0].seed_chunk_indices, vec![2]);
    }

    #[test]
    fn versioned_update_procedure_evidence_rejects_action_changelog_without_commands() {
        let command_document_id = Uuid::now_v7();
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        let term_model =
            versioned_update_procedure_term_model("how to update Sample Target?", Some(&query_ir));
        let mut changelog = setup_focus_runtime_chunk(Uuid::now_v7(), 0, 1.0);
        changelog.document_label = "Sample Target release history".to_string();
        changelog.source_text = concat!(
            "1. Version 4.2.10 - updated /opt/alpha/module.conf compatibility.\n",
            "2. Version 4.2.11 - refreshed alpha-package=stable metadata.\n",
            "3. Version 4.2.12 - documented --retry flag behavior."
        )
        .to_string();
        let mut command_runbook = setup_focus_runtime_chunk(command_document_id, 1, 1.0);
        command_runbook.document_label = "Maintenance update procedure".to_string();
        command_runbook.source_text = concat!(
            "Sample Target update\n",
            "1. sample-runner --refresh\n",
            "2. sample-runner --install sample-unit\n",
            "3. sudo service alpha-node restart"
        )
        .to_string();

        let candidates = versioned_update_procedure_candidates_from_evidence_chunks(
            &[changelog, command_runbook],
            &term_model,
            4,
        );

        assert_eq!(candidates.len(), 1, "{candidates:#?}");
        assert_eq!(candidates[0].document_id, command_document_id);
        assert_eq!(candidates[0].seed_chunk_indices, vec![1]);
    }

    #[test]
    fn versioned_update_procedure_evidence_marks_exact_target_command_runbook() {
        let command_document_id = Uuid::now_v7();
        let mut query_ir = setup_query_ir("Alpha Control Console");
        query_ir.target_types =
            vec!["artifact".to_string(), "version".to_string(), "procedure".to_string()];
        query_ir.target_entities = vec![EntityMention {
            label: "Alpha Control Console".to_string(),
            role: EntityRole::Subject,
        }];
        let term_model = versioned_update_procedure_term_model(
            "how to update Alpha Control Console?",
            Some(&query_ir),
        );
        let mut command_runbook = setup_focus_runtime_chunk(command_document_id, 0, 1.0);
        command_runbook.document_label =
            "Alpha Control Console environment update guide".to_string();
        command_runbook.source_text = concat!(
            "Alpha Control Console update guide\n",
            "1. sample-runner --refresh\n",
            "2. sample-runner --apply\n",
            "3. sudo sample-configure alpha-console\n",
            "4. sudo service alpha-console restart"
        )
        .to_string();

        let candidates = versioned_update_procedure_candidates_from_evidence_chunks(
            &[command_runbook],
            &term_model,
            4,
        );

        assert_eq!(candidates.len(), 1, "{candidates:#?}");
        assert_eq!(candidates[0].document_id, command_document_id);
        assert!(candidates[0].exact_title_identity);
        assert!(candidates[0].allow_head_fallback);
        assert_eq!(candidates[0].seed_chunk_indices, vec![0]);
    }

    #[test]
    fn versioned_update_procedure_seeded_runbook_candidates_include_body_aligned_evidence() {
        let body_aligned_document_id = Uuid::now_v7();
        let title_only_document_id = Uuid::now_v7();
        let candidates = vec![
            VersionedUpdateProcedureDocumentCandidate {
                document_id: title_only_document_id,
                exact_title_identity: true,
                target_title_anchor: true,
                allow_head_fallback: true,
                requires_action_text_match: false,
                requires_subject_text_match: false,
                subject_identity_score: 4,
                focus_aligned_command_score: 0,
                priority: VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_IDENTITY_PRIORITY_BONUS,
                seed_chunk_indices: Vec::new(),
                source_local_anchor_indices: Vec::new(),
            },
            VersionedUpdateProcedureDocumentCandidate {
                document_id: body_aligned_document_id,
                exact_title_identity: false,
                target_title_anchor: false,
                allow_head_fallback: false,
                requires_action_text_match: true,
                requires_subject_text_match: false,
                subject_identity_score: 0,
                focus_aligned_command_score: 4,
                priority: VERSIONED_UPDATE_PROCEDURE_EVIDENCE_PRIORITY_BONUS,
                seed_chunk_indices: vec![22],
                source_local_anchor_indices: Vec::new(),
            },
        ];

        let seeded = versioned_update_procedure_seeded_runbook_candidates(&candidates);

        assert_eq!(seeded.len(), 1, "{seeded:#?}");
        assert_eq!(seeded[0].document_id, body_aligned_document_id);
        assert_eq!(seeded[0].seed_chunk_indices, vec![22]);
    }

    #[test]
    fn versioned_update_procedure_evidence_candidate_preserves_body_subject_identity() {
        let document_id = Uuid::now_v7();
        let mut query_ir = setup_query_ir("Sample Console");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Console".to_string(), role: EntityRole::Subject }];
        let term_model =
            versioned_update_procedure_term_model("how to update Sample Console?", Some(&query_ir));
        let mut runbook = setup_focus_runtime_chunk(document_id, 12, 1.0);
        runbook.document_label = "Install and update".to_string();
        runbook.source_text = concat!(
            "Sample Console update procedure\n",
            "1. sample-runner --refresh\n",
            "2. sample-runner --apply sample-console-rest\n",
            "3. sudo service sample-console-rest restart"
        )
        .to_string();

        let candidates =
            versioned_update_procedure_candidates_from_evidence_chunks(&[runbook], &term_model, 4);

        assert_eq!(candidates.len(), 1, "{candidates:#?}");
        assert_eq!(candidates[0].document_id, document_id);
        assert!(
            candidates[0].subject_identity_score > 0,
            "body identity must not be discarded for generic titles: {candidates:#?}"
        );
        assert_eq!(candidates[0].seed_chunk_indices, vec![12]);
    }

    #[test]
    fn versioned_update_procedure_merge_retains_body_bound_generic_runbook_with_title_noise() {
        let body_bound_document_id = Uuid::now_v7();
        let mut query_ir = setup_query_ir("Sample Console");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Console".to_string(), role: EntityRole::Subject }];
        let term_model =
            versioned_update_procedure_term_model("how to update Sample Console?", Some(&query_ir));
        let mut runbook = setup_focus_runtime_chunk(body_bound_document_id, 12, 1.0);
        runbook.document_label = "Install and maintain".to_string();
        runbook.source_text = concat!(
            "Sample Console update procedure\n",
            "1. sample-runner --refresh\n",
            "2. sample-runner --apply sample-console-rest\n",
            "3. sudo service sample-console-rest restart"
        )
        .to_string();
        let evidence_candidates =
            versioned_update_procedure_candidates_from_evidence_chunks(&[runbook], &term_model, 4);
        let title_candidates = (0..8)
            .map(|index| VersionedUpdateProcedureDocumentCandidate {
                document_id: Uuid::now_v7(),
                exact_title_identity: true,
                target_title_anchor: true,
                allow_head_fallback: true,
                requires_action_text_match: false,
                requires_subject_text_match: false,
                subject_identity_score: 3,
                focus_aligned_command_score: 0,
                priority: VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_IDENTITY_PRIORITY_BONUS
                    .saturating_add(1_000usize.saturating_sub(index)),
                seed_chunk_indices: Vec::new(),
                source_local_anchor_indices: Vec::new(),
            })
            .collect::<Vec<_>>();

        let merged = merge_versioned_update_procedure_document_candidates(
            title_candidates,
            evidence_candidates,
            4,
        );

        assert!(
            merged.iter().any(|candidate| {
                candidate.document_id == body_bound_document_id
                    && candidate.seed_chunk_indices == vec![12]
            }),
            "{merged:#?}"
        );
    }

    #[test]
    fn versioned_update_procedure_merge_prefers_seeded_exact_evidence_over_title_only_exact() {
        let command_document_id = Uuid::now_v7();
        let title_only_candidates = (0..2)
            .map(|index| VersionedUpdateProcedureDocumentCandidate {
                document_id: Uuid::now_v7(),
                exact_title_identity: true,
                target_title_anchor: false,
                allow_head_fallback: true,
                requires_action_text_match: false,
                requires_subject_text_match: false,
                subject_identity_score: 3,
                focus_aligned_command_score: 0,
                priority: VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_IDENTITY_PRIORITY_BONUS
                    .saturating_add(10_000usize.saturating_sub(index)),
                seed_chunk_indices: Vec::new(),
                source_local_anchor_indices: Vec::new(),
            })
            .collect::<Vec<_>>();
        let evidence_candidate = VersionedUpdateProcedureDocumentCandidate {
            document_id: command_document_id,
            exact_title_identity: true,
            target_title_anchor: false,
            allow_head_fallback: true,
            requires_action_text_match: true,
            requires_subject_text_match: false,
            subject_identity_score: 3,
            focus_aligned_command_score: 0,
            priority: VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_IDENTITY_PRIORITY_BONUS,
            seed_chunk_indices: vec![0],
            source_local_anchor_indices: Vec::new(),
        };

        let merged = merge_versioned_update_procedure_document_candidates(
            title_only_candidates,
            vec![evidence_candidate],
            2,
        );

        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].document_id, command_document_id, "{merged:#?}");
        assert_eq!(merged[0].seed_chunk_indices, vec![0]);
    }

    #[test]
    fn versioned_update_procedure_seeded_exact_bonus_is_bounded_by_priority() {
        let title_only_document_id = Uuid::now_v7();
        let seeded_document_id = Uuid::now_v7();
        let title_only_candidate = VersionedUpdateProcedureDocumentCandidate {
            document_id: title_only_document_id,
            exact_title_identity: true,
            target_title_anchor: false,
            allow_head_fallback: true,
            requires_action_text_match: false,
            requires_subject_text_match: false,
            subject_identity_score: 4,
            focus_aligned_command_score: 0,
            priority: VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_IDENTITY_PRIORITY_BONUS
                .saturating_add(VERSIONED_UPDATE_PROCEDURE_SEEDED_EXACT_PRIORITY_BONUS)
                .saturating_add(1),
            seed_chunk_indices: Vec::new(),
            source_local_anchor_indices: Vec::new(),
        };
        let seeded_candidate = VersionedUpdateProcedureDocumentCandidate {
            document_id: seeded_document_id,
            exact_title_identity: true,
            target_title_anchor: false,
            allow_head_fallback: true,
            requires_action_text_match: true,
            requires_subject_text_match: false,
            subject_identity_score: 4,
            focus_aligned_command_score: 0,
            priority: VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_IDENTITY_PRIORITY_BONUS,
            seed_chunk_indices: vec![0],
            source_local_anchor_indices: Vec::new(),
        };

        let merged = merge_versioned_update_procedure_document_candidates(
            vec![title_only_candidate],
            vec![seeded_candidate],
            2,
        );

        assert_eq!(merged[0].document_id, title_only_document_id, "{merged:#?}");
        assert_eq!(merged[1].document_id, seeded_document_id, "{merged:#?}");
    }

    #[test]
    fn versioned_update_procedure_reserves_multiple_exact_title_candidates() {
        let exact_candidates = (0..VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_RESERVE_CAP)
            .map(|index| VersionedUpdateProcedureDocumentCandidate {
                document_id: Uuid::now_v7(),
                exact_title_identity: true,
                target_title_anchor: false,
                allow_head_fallback: true,
                requires_action_text_match: false,
                requires_subject_text_match: false,
                subject_identity_score: 3,
                focus_aligned_command_score: 0,
                priority: VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_IDENTITY_PRIORITY_BONUS
                    .saturating_sub(index),
                seed_chunk_indices: Vec::new(),
                source_local_anchor_indices: Vec::new(),
            })
            .collect::<Vec<_>>();
        let reserved_ids =
            exact_candidates.iter().map(|candidate| candidate.document_id).collect::<Vec<_>>();
        let mut candidates = vec![VersionedUpdateProcedureDocumentCandidate {
            document_id: Uuid::now_v7(),
            exact_title_identity: false,
            target_title_anchor: false,
            allow_head_fallback: true,
            requires_action_text_match: false,
            requires_subject_text_match: false,
            subject_identity_score: 4,
            focus_aligned_command_score: 0,
            priority: VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_IDENTITY_PRIORITY_BONUS
                .saturating_add(10_000),
            seed_chunk_indices: Vec::new(),
            source_local_anchor_indices: Vec::new(),
        }];

        ensure_reserved_versioned_update_procedure_title_candidates(
            &mut candidates,
            exact_candidates,
            VERSIONED_UPDATE_PROCEDURE_DOCUMENT_CANDIDATE_CAP,
        );

        for reserved_id in reserved_ids {
            assert!(
                candidates.iter().any(|candidate| candidate.document_id == reserved_id),
                "{candidates:#?}"
            );
        }
    }

    #[test]
    fn versioned_update_procedure_command_signal_score_is_capped() {
        let command_text = (0..32)
            .map(|index| format!("{index}. sudo alpha-tool --apply"))
            .collect::<Vec<_>>()
            .join("\n");
        let procedure_terms = BTreeSet::from(["apply".to_string()]);

        let score = versioned_update_procedure_text_command_or_script_score(
            &command_text,
            &procedure_terms,
        );

        assert_eq!(score, VERSIONED_UPDATE_PROCEDURE_COMMAND_SIGNAL_SCORE_CAP);
    }

    #[test]
    fn versioned_update_procedure_command_signal_counts_inline_dense_runbook() {
        let command_text = concat!(
            "Alpha node update: sudo su sample-transfer ",
            "https://updates.example.invalid/alpha/update_secondary.sh -o /tmp/sample-runner.sh ",
            "sample-prepare +x /tmp/sample-runner.sh /tmp/sample-runner.sh"
        );
        let procedure_terms = BTreeSet::from(["update".to_string()]);

        let score =
            versioned_update_procedure_text_command_or_script_score(command_text, &procedure_terms);

        assert!(score >= 4, "dense inline command runbook should count shell steps: {score}");
    }

    #[test]
    fn versioned_update_procedure_command_signal_splits_fused_shell_prefix() {
        let command_text = concat!(
            "Alpha node update: sudo susample-transfer ",
            "https://updates.example.invalid/alpha/update.sh -o /tmp/sample-runner.sh ",
            "sample-prepare +x /tmp/sample-runner.sh/tmp/sample-runner.sh"
        );
        let procedure_terms = BTreeSet::from(["update".to_string()]);

        let score =
            versioned_update_procedure_text_command_or_script_score(command_text, &procedure_terms);

        assert!(score >= 4, "fused shell command tokens should still count: {score}");
        assert!(versioned_update_procedure_text_has_action_script(command_text, &procedure_terms));
    }

    #[test]
    fn versioned_update_procedure_focus_aligned_command_score_counts_dense_runbook() {
        let command_text = concat!(
            "Sample Target update: sample-runner --refresh sample-runner --apply ",
            "sudo sample-configure alpha-console sudo service alpha-console restart"
        );
        let query_ir = setup_query_ir("Sample Target");
        let term_model =
            versioned_update_procedure_term_model("how to update Sample Target?", Some(&query_ir));

        let score =
            versioned_update_procedure_text_focus_aligned_command_score(command_text, &term_model);

        assert!(
            score >= 4,
            "package refresh plus product maintenance commands should be a strong signal: {score}"
        );
        assert_eq!(
            versioned_update_procedure_text_focus_aligned_command_score(
                "Sample Target install: sample-install alpha-console",
                &term_model,
            ),
            0
        );
    }

    #[test]
    fn versioned_update_procedure_command_sequence_score_ignores_index_refresh_only() {
        let query_ir = setup_query_ir("Sample Target");
        let term_model =
            versioned_update_procedure_term_model("how to update Sample Target?", Some(&query_ir));
        assert_eq!(
            versioned_update_procedure_text_command_sequence_score(
                "sample-runner --refresh sudo sample-runner --install alpha-console",
                &term_model,
            ),
            0
        );
        assert!(
            versioned_update_procedure_text_command_sequence_score(
                concat!(
                    "Sample Target update procedure: ",
                    "sample-runner --refresh sample-runner --apply ",
                    "sudo sample-configure alpha-console"
                ),
                &term_model,
            ) >= 2
        );
    }

    #[test]
    fn versioned_update_exact_target_runbook_rejects_install_page_with_product_suffix() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types =
            vec!["artifact".to_string(), "procedure".to_string(), "version".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        query_ir.retrieval_query = Some("how to update Sample Target version?".to_string());

        let mut chunk = setup_focus_runtime_chunk(Uuid::now_v7(), 0, 1.0);
        chunk.document_label =
            "Install and update Sample Targets - Sample Target Guide".to_string();
        chunk.source_text = concat!(
            "Install Sample Targets:\n",
            "1. sample-runner --refresh\n",
            "2. sample-runner --install sample-unit\n",
            "3. sudo sample-service restart alpha-subject"
        )
        .to_string();

        let score = versioned_update_exact_target_runbook_score(
            "how to update Sample Target version?",
            &query_ir,
            &chunk,
        );

        assert!(score.is_none(), "install pages must not masquerade as update runbooks");
    }

    #[test]
    fn versioned_update_exact_target_runbook_accepts_structural_package_commands() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types =
            vec!["artifact".to_string(), "procedure".to_string(), "version".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        query_ir.retrieval_query = Some("how to update alpha sample console version?".to_string());

        let mut chunk = setup_focus_runtime_chunk(Uuid::now_v7(), 0, 1.0);
        chunk.document_label =
            "Sample Target update instructions from Platform 1.0 to 2.0".to_string();
        chunk.source_text = concat!(
            "To update Sample Target, run these steps:\n",
            "1. sample-runner --refresh\n",
            "2. sample-runner --apply\n",
            "3. sample-runner --migrate\n",
            "4. sample-platform-update\n",
            "5. sample-runner --pin sample-store\n",
            "6. sudo sample-configure alpha-rest\n",
            "7. sudo service alpha-rest restart"
        )
        .to_string();

        let score = versioned_update_exact_target_runbook_score(
            "how to update alpha sample console version?",
            &query_ir,
            &chunk,
        );

        assert!(score.is_some(), "exact-target package runbook should be accepted");
    }

    #[test]
    fn versioned_update_exact_target_runbook_accepts_body_aligned_generic_title() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Object }];
        query_ir.retrieval_query = Some("how to update Sample Target?".to_string());

        let mut chunk = setup_focus_runtime_chunk(Uuid::now_v7(), 0, 1.0);
        chunk.document_label = "Installation and maintenance".to_string();
        chunk.source_text = concat!(
            "Sample Target update procedure:\n",
            "1. sudo susample-transfer https://updates.example.invalid/alpha/update.sh -o /tmp/sample-runner.sh\n",
            "2. sample-prepare +x /tmp/sample-runner.sh/tmp/sample-runner.sh"
        )
        .to_string();

        let score = versioned_update_exact_target_runbook_score(
            "how to update Sample Target?",
            &query_ir,
            &chunk,
        );

        assert!(score.is_some(), "body-aligned generic runbook should be accepted");
    }

    #[test]
    fn versioned_update_exact_target_runbook_accepts_conceptual_procedure_runbook() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["procedure".to_string(), "concept".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        query_ir.retrieval_query = Some("how to update Sample Target?".to_string());
        query_ir.document_focus = None;

        let mut chunk = setup_focus_runtime_chunk(Uuid::now_v7(), 0, 1.0);
        chunk.document_label = "Installation and maintenance".to_string();
        chunk.source_text = concat!(
            "Sample Target update procedure:\n",
            "1. sample-get refresh\n",
            "2. sample-get upgrade\n",
            "3. sample-configure alpha-service\n",
            "4. sudo service alpha-service restart"
        )
        .to_string();

        let score = versioned_update_exact_target_runbook_score(
            "how to update Sample Target?",
            &query_ir,
            &chunk,
        );

        assert!(score.is_some(), "conceptual procedure runbook should be accepted");
    }

    #[test]
    fn versioned_update_exact_target_runbook_accepts_flattened_body_aligned_runbook() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["procedure".to_string(), "concept".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        query_ir.retrieval_query = Some("how to update Sample Target?".to_string());
        query_ir.document_focus = None;

        let mut chunk = setup_focus_runtime_chunk(Uuid::now_v7(), 0, 1.0);
        chunk.document_label = "Installation and maintenance".to_string();
        chunk.source_text = concat!(
            "Sample Target update procedure: ",
            "1. sample-get refresh ",
            "2. sample-get upgrade ",
            "3. sample-configure sample-target ",
            "4. sudo service sample-target restart"
        )
        .to_string();

        let score = versioned_update_exact_target_runbook_score(
            "how to update Sample Target?",
            &query_ir,
            &chunk,
        );

        assert!(score.is_some(), "flattened body-aligned runbook should be accepted");
    }

    #[test]
    fn versioned_update_exact_target_runbook_rejects_location_only_flattened_target() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["procedure".to_string(), "concept".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        query_ir.retrieval_query = Some("how to update Sample Target?".to_string());
        query_ir.document_focus = None;

        let mut chunk = setup_focus_runtime_chunk(Uuid::now_v7(), 0, 1.0);
        chunk.document_label = "Installation and maintenance".to_string();
        chunk.source_text = concat!(
            "Other Unit can be deployed on the same node as Sample Target. ",
            "Other Unit update procedure: ",
            "1. sample-get refresh ",
            "2. sample-get upgrade ",
            "3. sample-configure other-unit ",
            "4. sudo service other-unit restart"
        )
        .to_string();

        let score = versioned_update_exact_target_runbook_score(
            "how to update Sample Target?",
            &query_ir,
            &chunk,
        );

        assert!(score.is_none(), "location-only target mentions must not bind adjacent runbooks");
    }

    #[test]
    fn versioned_update_exact_target_runbook_rejects_location_only_multiline_target() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["procedure".to_string(), "concept".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        query_ir.retrieval_query = Some("how to update Sample Target?".to_string());
        query_ir.document_focus = None;

        let mut chunk = setup_focus_runtime_chunk(Uuid::now_v7(), 0, 1.0);
        chunk.document_label = "Installation and maintenance".to_string();
        chunk.source_text = concat!(
            "Other Unit can be deployed on the same node as Sample Target.\n",
            "Other Unit update procedure:\n",
            "1. sample-get refresh\n",
            "2. sample-get upgrade\n",
            "3. sample-configure other-unit\n",
            "4. sudo service other-unit restart"
        )
        .to_string();

        let score = versioned_update_exact_target_runbook_score(
            "how to update Sample Target?",
            &query_ir,
            &chunk,
        );

        assert!(score.is_none(), "line-adjacent target mentions must not bind other runbooks");
    }

    #[test]
    fn versioned_update_exact_target_runbook_rejects_unbound_body_mention_generic_title() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Object }];
        query_ir.retrieval_query = Some("how to update Sample Target?".to_string());

        let mut chunk = setup_focus_runtime_chunk(Uuid::now_v7(), 0, 1.0);
        chunk.document_label = "Installation and maintenance".to_string();
        chunk.source_text = concat!(
            "Sample Target can be installed on the same node.\n",
            "Deployment prerequisites apply to both products.\n",
            "Check shared capacity before maintenance.\n",
            "Other Unit update procedure:\n",
            "1. sudo sample-fetch https://updates.example.invalid/other/update.sh -o /tmp/sample-runner.sh\n",
            "2. sample-prepare +x /tmp/sample-runner.sh\n",
            "3. /tmp/sample-runner.sh"
        )
        .to_string();

        let score = versioned_update_exact_target_runbook_score(
            "how to update Sample Target?",
            &query_ir,
            &chunk,
        );

        assert!(score.is_none(), "unbound body mentions must not select adjacent runbooks");
    }

    #[test]
    fn versioned_update_exact_target_runbook_specificity_rejects_generic_script_for_version_query()
    {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types =
            vec!["artifact".to_string(), "procedure".to_string(), "version".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Object }];
        query_ir.retrieval_query = Some("how to update Sample Target version?".to_string());

        let mut chunk = setup_focus_runtime_chunk(Uuid::now_v7(), 0, 1.0);
        chunk.document_label = "Installation and maintenance".to_string();
        chunk.source_text = concat!(
            "Sample Target update procedure:\n",
            "1. sudo susample-transfer https://updates.example.invalid/alpha/update.sh -o /tmp/sample-runner.sh\n",
            "2. sample-prepare +x /tmp/sample-runner.sh/tmp/sample-runner.sh"
        )
        .to_string();

        assert!(
            versioned_update_exact_target_runbook_score(
                "how to update Sample Target version?",
                &query_ir,
                &chunk,
            )
            .is_some(),
            "the structural scorer should still recognize the runbook"
        );
        assert!(
            !versioned_update_exact_target_runbook_matches_query_specificity(
                "how to update Sample Target version?",
                &query_ir,
                &chunk,
            ),
            "version-specific queries should not be hijacked by generic script runbooks"
        );

        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.retrieval_query = Some("how to update Sample Target?".to_string());
        assert!(versioned_update_exact_target_runbook_matches_query_specificity(
            "how to update Sample Target?",
            &query_ir,
            &chunk,
        ));
    }

    #[test]
    fn versioned_update_exact_target_runbook_rejects_platform_only_migration() {
        let mut query_ir = setup_query_ir("Alpha console");
        query_ir.target_types =
            vec!["artifact".to_string(), "procedure".to_string(), "version".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Alpha console".to_string(), role: EntityRole::Subject }];
        query_ir.retrieval_query = Some("how to update Alpha console?".to_string());

        let mut chunk = setup_focus_runtime_chunk(Uuid::now_v7(), 0, 1.0);
        chunk.document_label = "Alpha console platform migration".to_string();
        chunk.source_text = concat!(
            "Alpha console platform migration:\n",
            "1. sample-runner --refresh\n",
            "2. sample-runner --migrate\n",
            "3. sample-platform-transition"
        )
        .to_string();

        let score = versioned_update_exact_target_runbook_score(
            "how to update Alpha console?",
            &query_ir,
            &chunk,
        );

        assert!(score.is_none(), "platform-only migration must not answer product update");
    }

    #[test]
    fn versioned_update_procedure_evidence_queries_include_action_only_probe() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        let term_model =
            versioned_update_procedure_term_model("how to update Sample Target?", Some(&query_ir));

        let queries = versioned_update_procedure_evidence_search_queries(
            "how to update Sample Target?",
            &term_model,
        );

        assert!(queries.iter().any(|query| query == "how update"));
        assert!(queries.iter().any(|query| {
            let tokens = query.split_whitespace().collect::<BTreeSet<_>>();
            ["sample", "target"].iter().all(|token| tokens.contains(token))
                && !tokens.contains("update")
        }));
        assert!(queries.iter().any(|query| query.contains("target")));
        assert!(queries.iter().all(|query| {
            !query.contains("sample-runner")
                && !query.contains("sample-configure")
                && !query.contains("service restart")
        }));
    }

    #[test]
    fn versioned_update_procedure_reference_queries_follow_quoted_section_titles() {
        let document_id = Uuid::now_v7();
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        let term_model =
            versioned_update_procedure_term_model("how to update Sample Target?", Some(&query_ir));
        let mut anchor = setup_focus_runtime_chunk(document_id, 0, 1.0);
        anchor.document_label = "Sample Target transition guide".to_string();
        anchor.source_text = concat!(
            "Sample Target update is performed by the maintenance script. ",
            "The command sequence is documented in «Install and update»."
        )
        .to_string();

        let queries = versioned_update_procedure_reference_search_queries(&[anchor], &term_model);

        assert!(queries.iter().any(|query| {
            let tokens = query.split_whitespace().collect::<BTreeSet<_>>();
            ["install", "update", "sample", "target"].iter().all(|token| tokens.contains(token))
        }));
    }

    #[test]
    fn versioned_update_procedure_reference_queries_ignore_unaligned_quotes() {
        let document_id = Uuid::now_v7();
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        let term_model =
            versioned_update_procedure_term_model("how to update Sample Target?", Some(&query_ir));
        let mut anchor = setup_focus_runtime_chunk(document_id, 0, 1.0);
        anchor.document_label = "Sample Target transition guide".to_string();
        anchor.source_text = concat!(
            "Sample Target update notes mention the UI button ",
            "«Show detailed activity»."
        )
        .to_string();

        let queries = versioned_update_procedure_reference_search_queries(&[anchor], &term_model);

        assert!(queries.is_empty(), "{queries:#?}");
    }

    #[test]
    fn versioned_update_procedure_reference_document_candidates_match_quoted_title() {
        let referenced = document_row("install-update.html", "Install and update");
        let unrelated = document_row("activity.html", "Show detailed activity");
        let document_index = HashMap::from([
            (referenced.document_id, referenced.clone()),
            (unrelated.document_id, unrelated),
        ]);
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        let term_model =
            versioned_update_procedure_term_model("how to update Sample Target?", Some(&query_ir));

        let candidates = versioned_update_procedure_reference_document_candidates(
            &["Install and update".to_string()],
            &term_model,
            &document_index,
            4,
        );

        assert_eq!(candidates.len(), 1, "{candidates:#?}");
        assert_eq!(candidates[0].document_id, referenced.document_id);
        assert!(candidates[0].requires_action_text_match);
        assert!(!candidates[0].requires_subject_text_match);
        assert!(
            candidates[0].subject_identity_score
                >= VERSIONED_UPDATE_PROCEDURE_STRONG_SUBJECT_IDENTITY_MIN
        );
    }

    #[test]
    fn versioned_update_procedure_reference_probe_keeps_generic_title_command_runbook() {
        let referenced = document_row("install-update.html", "Install and update");
        let document_id = referenced.document_id;
        let revision_id = Uuid::now_v7();
        let workspace_id = referenced.workspace_id;
        let library_id = referenced.library_id;
        let document_index = HashMap::from([(document_id, referenced)]);
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        let term_model =
            versioned_update_procedure_term_model("how to update Sample Target?", Some(&query_ir));
        let candidate = versioned_update_procedure_reference_document_candidates(
            &["Install and update".to_string()],
            &term_model,
            &document_index,
            4,
        )
        .into_iter()
        .next()
        .expect("referenced title candidate");
        let mut rows = (0..6)
            .map(|index| {
                chunk_row(
                    workspace_id,
                    library_id,
                    document_id,
                    revision_id,
                    index,
                    "General compatibility notes and background.",
                )
            })
            .collect::<Vec<_>>();
        rows.push(chunk_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            7,
            concat!(
                "Update procedure:\n",
                "1. sample-transfer https://updates.example.invalid/update.sh -o /tmp/sample-runner.sh\n",
                "2. sample-prepare +x /tmp/sample-runner.sh\n",
                "3. /tmp/sample-runner.sh"
            ),
        ));

        let selected = select_versioned_update_procedure_probe_seed_rows(
            rows,
            &candidate,
            &term_model,
            &document_index,
            &["install".to_string(), "update".to_string()],
            2,
        );

        let selected_indices = selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>();
        assert!(selected_indices.contains(&7), "{selected_indices:?}");
    }

    #[test]
    fn versioned_update_procedure_reference_selection_keeps_command_neighbors() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        let term_model =
            versioned_update_procedure_term_model("how to update Sample Target?", Some(&query_ir));
        let mut seed = setup_focus_runtime_chunk(document_id, 4, 1.0);
        seed.revision_id = revision_id;
        seed.document_label = "Sample Target install and update".to_string();
        seed.source_text = concat!(
            "Sample Target update\n",
            "1. sample-runner --refresh\n",
            "2. sample-runner --install sample-unit\n",
            "3. sudo sample-service restart alpha-node"
        )
        .to_string();
        let mut neighbor = setup_focus_runtime_chunk(document_id, 5, 0.8);
        neighbor.revision_id = revision_id;
        neighbor.document_label = seed.document_label.clone();
        neighbor.source_text = "4. sudo service alpha-node restart".to_string();
        let mut unrelated = setup_focus_runtime_chunk(Uuid::now_v7(), 0, 1.0);
        unrelated.document_label = "Sample Target release notes".to_string();
        unrelated.source_text = "Sample Target update history only.".to_string();

        let selected = select_versioned_update_procedure_reference_chunks(
            vec![unrelated, neighbor.clone(), seed.clone()],
            &[],
            &term_model,
            8,
        );

        let selected_ids = selected.iter().map(|chunk| chunk.chunk_id).collect::<BTreeSet<_>>();
        assert!(selected_ids.contains(&seed.chunk_id), "{selected:#?}");
        assert!(selected_ids.contains(&neighbor.chunk_id), "{selected:#?}");
        assert!(
            selected.iter().all(|chunk| chunk.score_kind == RuntimeChunkScoreKind::FocusedDocument)
        );
    }

    #[test]
    fn versioned_update_structural_source_accepts_literal_version_records() {
        let mut query_ir = setup_query_ir("Neutral Terminal");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities = vec![EntityMention {
            label: "Neutral Terminal".to_string(),
            role: EntityRole::Subject,
        }];
        let term_model =
            versioned_update_procedure_term_model("update Neutral Terminal", Some(&query_ir));
        let candidate = VersionedUpdateProcedureDocumentCandidate {
            document_id: Uuid::now_v7(),
            exact_title_identity: false,
            target_title_anchor: false,
            allow_head_fallback: false,
            requires_action_text_match: false,
            requires_subject_text_match: false,
            subject_identity_score: VERSIONED_UPDATE_PROCEDURE_STRONG_SUBJECT_IDENTITY_MIN,
            focus_aligned_command_score: 0,
            priority: 0,
            seed_chunk_indices: Vec::new(),
            source_local_anchor_indices: Vec::new(),
        };
        let mut chunk = setup_focus_runtime_chunk(candidate.document_id, 8, 1.0);
        chunk.document_label = "Neutral Terminal maintenance ledger".to_string();
        chunk.source_text = concat!(
            "Neutral Terminal\n",
            "- 2.4.0 -> 2.5.0\n",
            "- /opt/neutral/config\n",
            "- token_mode = strict"
        )
        .to_string();

        let score = versioned_update_procedure_structural_source_chunk_score(
            &chunk,
            &candidate,
            &term_model,
        );

        assert!(score.is_some(), "literal version records should be usable procedure context");
    }

    #[test]
    fn versioned_update_procedure_probe_rows_prefer_distant_command_seed_over_toc() {
        let document = document_row("install-update.html", "Install and update");
        let document_id = document.document_id;
        let revision_id = Uuid::now_v7();
        let workspace_id = document.workspace_id;
        let library_id = document.library_id;
        let document_index = HashMap::from([(document_id, document)]);
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        let term_model =
            versioned_update_procedure_term_model("how to update Sample Target?", Some(&query_ir));
        let candidate = VersionedUpdateProcedureDocumentCandidate {
            document_id,
            exact_title_identity: false,
            target_title_anchor: false,
            allow_head_fallback: false,
            requires_action_text_match: true,
            requires_subject_text_match: true,
            subject_identity_score: 0,
            focus_aligned_command_score: 0,
            priority: 0,
            seed_chunk_indices: Vec::new(),
            source_local_anchor_indices: Vec::new(),
        };
        let mut rows = Vec::new();
        rows.push(chunk_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            0,
            "Install and update - System requirements - Update Sample Target",
        ));
        rows.push(chunk_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            1,
            "System requirements for Sample Target installation.",
        ));
        for index in 2..22 {
            rows.push(chunk_row(
                workspace_id,
                library_id,
                document_id,
                revision_id,
                index,
                "Sample Target background information.",
            ));
        }
        rows.push(chunk_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            22,
            "Update Sample Target:\n\
             1. sudo alpha-update-runner --target subject-node\n\
             2. sudo sample-service restart alpha-subject-node",
        ));
        assert!(!versioned_update_procedure_line_has_command_start(
            "Install and update - System requirements - Update Sample Target"
        ));
        assert!(versioned_update_procedure_line_has_command_start(
            "1. sudo alpha-update-runner --target subject-node"
        ));

        let selected = select_versioned_update_procedure_probe_seed_rows(
            rows,
            &candidate,
            &term_model,
            &document_index,
            &["alpha".to_string(), "subject".to_string(), "update".to_string()],
            VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT,
        );

        let selected_indices = selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>();
        assert_eq!(selected_indices.first().copied(), Some(22), "{selected_indices:?}");
        assert!(selected_indices.contains(&22), "{selected_indices:?}");
    }

    #[test]
    fn versioned_update_procedure_probe_rows_prefer_dense_focus_aligned_seed() {
        let document = document_row("alpha-console-update.html", "Sample Target update");
        let document_id = document.document_id;
        let revision_id = Uuid::now_v7();
        let workspace_id = document.workspace_id;
        let library_id = document.library_id;
        let document_index = HashMap::from([(document_id, document)]);
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types =
            vec!["artifact".to_string(), "procedure".to_string(), "version".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        let term_model = versioned_update_procedure_term_model(
            "how to update Sample Target version?",
            Some(&query_ir),
        );
        let candidate = VersionedUpdateProcedureDocumentCandidate {
            document_id,
            exact_title_identity: false,
            target_title_anchor: false,
            allow_head_fallback: false,
            requires_action_text_match: true,
            requires_subject_text_match: true,
            subject_identity_score: 0,
            focus_aligned_command_score: 0,
            priority: 0,
            seed_chunk_indices: Vec::new(),
            source_local_anchor_indices: Vec::new(),
        };
        let mut rows = vec![
            chunk_row(
                workspace_id,
                library_id,
                document_id,
                revision_id,
                0,
                "Sample Target update overview and environment requirements.",
            ),
            chunk_row(
                workspace_id,
                library_id,
                document_id,
                revision_id,
                1,
                concat!(
                    "Sample Target update helper sudo su sample-transfer ",
                    "https://updates.example.invalid/alpha/update_secondary.sh -o /tmp/sample-runner.sh ",
                    "sample-prepare +x /tmp/sample-runner.sh /tmp/sample-runner.sh"
                ),
            ),
        ];
        for index in 2..18 {
            rows.push(chunk_row(
                workspace_id,
                library_id,
                document_id,
                revision_id,
                index,
                "Sample Target release history and compatibility notes.",
            ));
        }
        rows.push(chunk_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            18,
            concat!(
                "Sample Target product update: sample-runner --refresh sample-runner --apply ",
                "sudo sample-configure alpha-console sudo service alpha-console restart."
            ),
        ));

        let selected = select_versioned_update_procedure_probe_seed_rows(
            rows,
            &candidate,
            &term_model,
            &document_index,
            &["alpha".to_string(), "console".to_string(), "update".to_string()],
            VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT,
        );

        let selected_indices = selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>();
        assert_eq!(selected_indices.first().copied(), Some(18), "{selected_indices:?}");
    }

    #[test]
    fn versioned_update_procedure_exact_probe_selects_body_bound_generic_title_runbook() {
        let document = document_row("install-update.html", "Install and update");
        let document_id = document.document_id;
        let revision_id = Uuid::now_v7();
        let workspace_id = document.workspace_id;
        let library_id = document.library_id;
        let document_index = HashMap::from([(document_id, document)]);
        let mut query_ir = setup_query_ir("Sample Console");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Console".to_string(), role: EntityRole::Subject }];
        let rows = vec![
            chunk_row(
                workspace_id,
                library_id,
                document_id,
                revision_id,
                0,
                "Sample Console background and compatibility notes.",
            ),
            chunk_row(
                workspace_id,
                library_id,
                document_id,
                revision_id,
                5,
                concat!(
                    "Sample Console update procedure\n",
                    "1. sample-runner --refresh\n",
                    "2. sample-runner --apply sample-console-rest\n",
                    "3. sudo service sample-console-rest restart"
                ),
            ),
        ];

        let selected = select_versioned_update_procedure_exact_target_runbook_probe_rows(
            &rows,
            "how to update Sample Console?",
            Some(&query_ir),
            &document_index,
            &["sample".to_string(), "console".to_string(), "update".to_string()],
            4,
        );

        let selected_indices = selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>();
        assert_eq!(selected_indices, vec![5], "{selected_indices:?}");
    }

    #[test]
    fn versioned_update_procedure_exact_probe_rejects_unbound_body_identity_mention() {
        let document = document_row("install-update.html", "Install and update");
        let document_id = document.document_id;
        let revision_id = Uuid::now_v7();
        let workspace_id = document.workspace_id;
        let library_id = document.library_id;
        let document_index = HashMap::from([(document_id, document)]);
        let mut query_ir = setup_query_ir("Sample Console");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Console".to_string(), role: EntityRole::Subject }];
        let rows = vec![chunk_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            7,
            concat!(
                "Sample Console glossary entry.\n",
                "Compatibility background.\n",
                "General command examples:\n",
                "1. sample-runner --refresh\n",
                "2. sample-runner --apply beta-unit\n",
                "3. sudo service beta-unit restart"
            ),
        )];

        let selected = select_versioned_update_procedure_exact_target_runbook_probe_rows(
            &rows,
            "how to update Sample Console?",
            Some(&query_ir),
            &document_index,
            &["sample".to_string(), "console".to_string(), "update".to_string()],
            4,
        );

        assert!(selected.is_empty(), "{selected:#?}");
    }

    #[test]
    fn versioned_update_procedure_context_merge_reserves_prioritized_far_seed() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let seed = chunk_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            22,
            "Update Sample Target with sudo alpha-update-runner --target subject-node.",
        );
        let toc = chunk_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            0,
            "Install and update - system requirements - update procedure.",
        );
        let expanded = (0..30)
            .map(|index| {
                chunk_row(
                    workspace_id,
                    library_id,
                    document_id,
                    revision_id,
                    index,
                    &format!("context row {index}"),
                )
            })
            .collect::<Vec<_>>();

        let merged =
            merge_versioned_update_procedure_context_rows(vec![seed.clone(), toc], expanded, 8);

        let merged_indices = merged.iter().map(|row| row.chunk_index).collect::<Vec<_>>();
        assert!(merged_indices.contains(&22), "{merged_indices:?}");
    }

    #[test]
    fn versioned_update_procedure_command_start_accepts_structural_product_cli() {
        assert!(versioned_update_procedure_line_has_command_start(
            "1. alpha-admin migrate --target=/opt/alpha"
        ));
        assert!(versioned_update_procedure_line_has_command_start(
            "2. alpha_admin verify result=/var/tmp/alpha.json"
        ));
        assert!(!versioned_update_procedure_line_has_command_start(
            "Alpha-Suite target item update procedure"
        ));
    }

    #[test]
    fn command_dense_excerpt_accepts_structural_product_cli_lines() {
        let excerpt = command_dense_excerpt_for(
            concat!(
                "Sample Subject maintenance\n",
                "1. alpha-admin migrate --target=/opt/alpha\n",
                "2. alpha-admin verify --format=json"
            ),
            400,
        );

        assert!(excerpt.is_some());
    }

    #[test]
    fn versioned_update_procedure_evidence_accepts_ordered_version_transition_seed() {
        let evidence_document_id = Uuid::now_v7();
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        let term_model =
            versioned_update_procedure_term_model("how to update Sample Target?", Some(&query_ir));
        let mut transition = setup_focus_runtime_chunk(evidence_document_id, 2, 1.0);
        transition.document_label = "Alpha product migration guide".to_string();
        transition.source_text = concat!(
            "Sample Target migration plan\n",
            "1. Move the control component to version 9.8.7.6 or later.\n",
            "2. Move Sample Target artifact to version 9.8.7-3 or later.\n",
            "3. Move Sample Target artifact to version 10.4.2 or later."
        )
        .to_string();
        let mut history = setup_focus_runtime_chunk(Uuid::now_v7(), 1, 1.0);
        history.document_label = "Sample Target release history".to_string();
        history.source_text = concat!(
            "Build 1.0.40 - Added queue metrics for Sample Target.\n",
            "Build 1.0.39 - Fixed retry logging for Sample Target."
        )
        .to_string();

        let candidates = versioned_update_procedure_candidates_from_evidence_chunks(
            &[history, transition],
            &term_model,
            4,
        );

        assert_eq!(candidates.len(), 1, "{candidates:#?}");
        assert_eq!(candidates[0].document_id, evidence_document_id);
        assert_eq!(candidates[0].seed_chunk_indices, vec![2]);
    }

    #[test]
    fn versioned_update_procedure_evidence_requires_label_binding_for_version_only_seed() {
        let aligned_document_id = Uuid::now_v7();
        let environment_document_id = Uuid::now_v7();
        let mut query_ir = setup_query_ir("Sample Subject orchestrator");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities = vec![EntityMention {
            label: "Sample Subject orchestrator".to_string(),
            role: EntityRole::Subject,
        }];
        let term_model = versioned_update_procedure_term_model(
            "how to update Sample Subject orchestrator?",
            Some(&query_ir),
        );
        let mut aligned = setup_focus_runtime_chunk(aligned_document_id, 2, 1.0);
        aligned.document_label = "Sample Subject orchestrator transition guide".to_string();
        aligned.source_text = concat!(
            "Version transition\n",
            "1. Move AlphaSuiteControlCenter to version 7.2.0 or later.\n",
            "2. Move Sample Subject orchestrator artifact to version 8.3.0 or later.\n",
            "3. Move Sample Subject orchestrator artifact to version 9.4.0 or later."
        )
        .to_string();
        let mut environment = setup_focus_runtime_chunk(environment_document_id, 0, 1.0);
        environment.document_label = "Environment Variant platform transition guide".to_string();
        environment.source_text = concat!(
            "This transition applies when Sample Subject orchestrator runs on Environment Variant.\n",
            "1. Move the platform baseline to version 10.1.0.\n",
            "2. Move the platform baseline to version 10.2.0.\n",
            "3. Move the platform baseline to version 10.3.0."
        )
        .to_string();

        let candidates = versioned_update_procedure_candidates_from_evidence_chunks(
            &[environment, aligned],
            &term_model,
            4,
        );

        assert_eq!(candidates.len(), 1, "{candidates:#?}");
        assert_eq!(candidates[0].document_id, aligned_document_id);
        assert_eq!(candidates[0].seed_chunk_indices, vec![2]);
    }

    #[test]
    fn versioned_update_procedure_evidence_candidates_aggregate_neighbor_chunks() {
        let evidence_document_id = Uuid::now_v7();
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        let term_model =
            versioned_update_procedure_term_model("how to update Sample Target?", Some(&query_ir));
        let mut intro = setup_focus_runtime_chunk(evidence_document_id, 0, 1.0);
        intro.document_label = "Sample Target maintenance guide".to_string();
        intro.source_text = "Sample Target update overview and prerequisites.".to_string();
        let mut commands = setup_focus_runtime_chunk(evidence_document_id, 1, 1.0);
        commands.document_label = intro.document_label.clone();
        commands.source_text = concat!(
            "Procedure\n",
            "1. Remove /etc/alpha/sources.list\n",
            "2. sample-runner --install sample-update-unit\n",
            "3. sh ./update_alpha.sh --now"
        )
        .to_string();

        let candidates = versioned_update_procedure_candidates_from_evidence_chunks(
            &[intro, commands],
            &term_model,
            4,
        );

        assert_eq!(candidates.len(), 1, "{candidates:#?}");
        assert_eq!(candidates[0].document_id, evidence_document_id);
        assert_eq!(candidates[0].seed_chunk_indices, vec![1]);
    }

    #[test]
    fn versioned_update_procedure_evidence_accepts_source_local_procedure_anchor() {
        let document_id = Uuid::now_v7();
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        let term_model =
            versioned_update_procedure_term_model("how to update Sample Target?", Some(&query_ir));
        let mut anchor = setup_focus_runtime_chunk(document_id, 17, 1.0);
        anchor.document_label = "Install and update".to_string();
        anchor.source_text = concat!(
            "Sample Target deployment requirements.\n",
            "The update section describes prerequisites and compatibility before the runbook steps."
        )
        .to_string();

        let candidates =
            versioned_update_procedure_candidates_from_evidence_chunks(&[anchor], &term_model, 4);

        assert_eq!(candidates.len(), 1, "{candidates:#?}");
        assert_eq!(candidates[0].document_id, document_id);
        assert!(candidates[0].seed_chunk_indices.is_empty(), "{candidates:#?}");
        assert_eq!(candidates[0].source_local_anchor_indices, vec![17]);
        assert!(versioned_update_procedure_candidate_is_source_local_anchor_only(&candidates[0]));

        let strict_runbook_candidates =
            versioned_update_procedure_seeded_runbook_candidates(&candidates);
        assert_eq!(strict_runbook_candidates.len(), 1, "{strict_runbook_candidates:#?}");
        assert_eq!(strict_runbook_candidates[0].source_local_anchor_indices, vec![17]);
    }

    #[test]
    fn versioned_update_procedure_evidence_rejects_source_local_anchor_without_target_identity() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        let term_model =
            versioned_update_procedure_term_model("how to update Sample Target?", Some(&query_ir));
        let mut anchor = setup_focus_runtime_chunk(Uuid::now_v7(), 17, 1.0);
        anchor.document_label = "Install and update".to_string();
        anchor.source_text = concat!(
            "Adjacent Unit deployment requirements.\n",
            "The update section describes prerequisites and compatibility before the runbook steps."
        )
        .to_string();

        let candidates =
            versioned_update_procedure_candidates_from_evidence_chunks(&[anchor], &term_model, 4);

        assert!(candidates.is_empty(), "{candidates:#?}");
    }

    #[test]
    fn versioned_update_procedure_evidence_rejects_environment_seed_without_command_focus() {
        let aligned_document_id = Uuid::now_v7();
        let environment_document_id = Uuid::now_v7();
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        let term_model =
            versioned_update_procedure_term_model("how to update Sample Target?", Some(&query_ir));
        let mut aligned = setup_focus_runtime_chunk(aligned_document_id, 1, 1.0);
        aligned.document_label = "Sample Target update guide".to_string();
        aligned.source_text = concat!(
            "Procedure\n",
            "1. Remove /etc/alpha/sources.list\n",
            "2. sample-runner --install sample-update-unit\n",
            "3. sh ./update_alpha.sh --now"
        )
        .to_string();
        let mut environment = setup_focus_runtime_chunk(environment_document_id, 0, 1.0);
        environment.document_label = "Hosted application platform update guide".to_string();
        environment.source_text = concat!(
            "This application can be installed on an Sample Target.\n",
            "1. sample-runner --refresh\n",
            "2. sample-runner --apply\n",
            "3. sudo service hosted-app restart"
        )
        .to_string();

        let candidates = versioned_update_procedure_candidates_from_evidence_chunks(
            &[environment, aligned],
            &term_model,
            2,
        );

        assert_eq!(candidates.len(), 1, "{candidates:#?}");
        assert_eq!(candidates[0].document_id, aligned_document_id);
        assert_eq!(candidates[0].seed_chunk_indices, vec![1]);
    }

    #[test]
    fn versioned_update_procedure_evidence_rejects_bootstrap_script_seed() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        let term_model =
            versioned_update_procedure_term_model("how to update Sample Target?", Some(&query_ir));
        let mut bootstrap = setup_focus_runtime_chunk(Uuid::now_v7(), 0, 1.0);
        bootstrap.document_label = "Sample Target bootstrap guide".to_string();
        bootstrap.source_text = concat!(
            "Sample Target update helper\n",
            "1. sudo su\n",
            "2. sample-transfer https://example.invalid/bootstrap/install.sh -o /tmp/install.sh\n",
            "3. sample-prepare +x /tmp/install.sh\n",
            "4. /tmp/install.sh"
        )
        .to_string();

        let candidates = versioned_update_procedure_candidates_from_evidence_chunks(
            &[bootstrap],
            &term_model,
            4,
        );

        assert!(candidates.is_empty(), "{candidates:#?}");
    }

    #[test]
    fn versioned_update_procedure_evidence_accepts_action_named_script_seed() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        let term_model =
            versioned_update_procedure_term_model("how to refresh Sample Target?", Some(&query_ir));
        let mut update = setup_focus_runtime_chunk(Uuid::now_v7(), 0, 1.0);
        update.document_label = "Sample Target lifecycle guide".to_string();
        update.source_text = concat!(
            "Sample Target lifecycle\n",
            "1. sudo su\n",
            "2. sample-transfer https://example.invalid/alpha/refresh.sh -o /tmp/refresh.sh\n",
            "3. sample-prepare +x /tmp/refresh.sh\n",
            "4. /tmp/refresh.sh"
        )
        .to_string();

        let candidates =
            versioned_update_procedure_candidates_from_evidence_chunks(&[update], &term_model, 4);

        assert_eq!(candidates.len(), 1, "{candidates:#?}");
    }

    #[test]
    fn versioned_update_procedure_evidence_accepts_action_prose_with_script_commands() {
        let document_id = Uuid::now_v7();
        let mut query_ir = setup_query_ir("support node");
        query_ir.target_types =
            vec!["artifact".to_string(), "procedure".to_string(), "version".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "support node".to_string(), role: EntityRole::Subject }];
        query_ir.retrieval_query = Some("how to update support node?".to_string());
        let term_model =
            versioned_update_procedure_term_model("how to update support node?", Some(&query_ir));
        let mut update = setup_focus_runtime_chunk(document_id, 7, 1.0);
        update.document_label = "Support node installation and update".to_string();
        update.source_text = concat!(
            "Support node update\n",
            "- To update the support node, run: sudo su sample-transfer ",
            "https://updates.example.invalid/alpha/update_secondary.sh -o /tmp/sample-runner.sh ",
            "sample-prepare +x /tmp/sample-runner.sh /tmp/sample-runner.sh"
        )
        .to_string();

        let candidates =
            versioned_update_procedure_candidates_from_evidence_chunks(&[update], &term_model, 4);

        assert_eq!(candidates.len(), 1, "{candidates:#?}");
        assert_eq!(candidates[0].document_id, document_id);
        assert_eq!(candidates[0].seed_chunk_indices, vec![7]);
    }

    #[test]
    fn versioned_update_procedure_evidence_prefers_focus_aligned_procedure_over_helper_script() {
        let script_document_id = Uuid::now_v7();
        let maintenance_document_id = Uuid::now_v7();
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types =
            vec!["artifact".to_string(), "procedure".to_string(), "version".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        let term_model = versioned_update_procedure_term_model(
            "how to update Sample Target version?",
            Some(&query_ir),
        );
        let mut script = setup_focus_runtime_chunk(script_document_id, 3, 1.0);
        script.document_label = "Sample Target install and update".to_string();
        script.source_text = concat!(
            "Sample Target update helper sudo su sample-transfer ",
            "https://updates.example.invalid/alpha/update_secondary.sh -o /tmp/sample-runner.sh ",
            "sample-prepare +x /tmp/sample-runner.sh /tmp/sample-runner.sh"
        )
        .to_string();
        let mut maintenance = setup_focus_runtime_chunk(maintenance_document_id, 0, 1.0);
        maintenance.document_label = "Sample Target version update instruction".to_string();
        maintenance.source_text = concat!(
            "Sample Target product update: sample-runner --refresh sample-runner --apply ",
            "sudo sample-configure alpha-console sudo service alpha-console restart."
        )
        .to_string();

        let candidates = versioned_update_procedure_candidates_from_evidence_chunks(
            &[script, maintenance],
            &term_model,
            4,
        );

        assert_eq!(candidates.len(), 2, "{candidates:#?}");
        assert_eq!(candidates[0].document_id, maintenance_document_id, "{candidates:#?}");
        assert_eq!(candidates[0].seed_chunk_indices, vec![0]);
    }

    #[test]
    fn versioned_update_procedure_evidence_accepts_title_aligned_command_seed() {
        let document_id = Uuid::now_v7();
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        let term_model =
            versioned_update_procedure_term_model("how to update Sample Target?", Some(&query_ir));
        let mut update = setup_focus_runtime_chunk(document_id, 7, 1.0);
        update.document_label = "Sample Target update guide".to_string();
        update.source_text = concat!(
            "Procedure\n",
            "1. sudo su\n",
            "2. sample-transfer https://example.invalid/alpha/update.sh -o /tmp/sample-runner.sh\n",
            "3. sample-prepare +x /tmp/sample-runner.sh\n",
            "4. /tmp/sample-runner.sh"
        )
        .to_string();

        let candidates =
            versioned_update_procedure_candidates_from_evidence_chunks(&[update], &term_model, 4);

        assert_eq!(candidates.len(), 1, "{candidates:#?}");
        assert_eq!(candidates[0].document_id, document_id);
        assert_eq!(candidates[0].seed_chunk_indices, vec![7]);
    }

    #[test]
    fn versioned_update_procedure_evidence_candidates_outrank_title_only_distractors() {
        let title_distractor =
            document_row("alpha-node-ops.html", "Sample Target operations guide");
        let evidence_document = document_row("maintenance.html", "Lifecycle maintenance guide");
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        let document_index = HashMap::from([
            (title_distractor.document_id, title_distractor.clone()),
            (evidence_document.document_id, evidence_document.clone()),
        ]);
        let title_candidates = versioned_update_procedure_candidate_documents(
            "how to update Sample Target?",
            Some(&query_ir),
            &document_index,
            4,
        );
        let term_model =
            versioned_update_procedure_term_model("how to update Sample Target?", Some(&query_ir));
        let mut evidence_chunk = setup_focus_runtime_chunk(evidence_document.document_id, 2, 1.0);
        evidence_chunk.document_label = "Lifecycle maintenance guide".to_string();
        evidence_chunk.source_text = concat!(
            "Sample Target update steps\n",
            "1. Remove /etc/alpha/sources.list\n",
            "2. sh ./update_alpha.sh"
        )
        .to_string();
        let evidence_candidates = versioned_update_procedure_candidates_from_evidence_chunks(
            &[evidence_chunk],
            &term_model,
            4,
        );

        let merged = merge_versioned_update_procedure_document_candidates(
            title_candidates,
            evidence_candidates,
            1,
        );

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].document_id, evidence_document.document_id);
        assert_eq!(merged[0].seed_chunk_indices, vec![2]);
    }

    #[test]
    fn versioned_update_procedure_chunks_score_above_setup_focus_chunks() {
        let mut setup_chunk = setup_focus_runtime_chunk(Uuid::now_v7(), 0, 1.0);
        setup_chunk.document_label = "Sample Target configuration".to_string();
        setup_chunk.source_text = concat!(
            "Install the package with sample-runner --configure sample-unit.\n",
            "Settings are stored in /opt/alpha/alpha.conf.\n",
            "[Alpha]\n",
            "url = http://localhost"
        )
        .to_string();

        assert!(
            versioned_update_procedure_chunk_score(0, 0)
                > setup_focus_document_chunk_score(&setup_chunk),
            "procedure runbook chunks must not be ranked below generic setup/config anchors"
        );
    }

    #[test]
    fn setup_focus_selection_reserves_package_path_anchor_before_parameter_rows() {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mut rows = Vec::new();
        rows.push(chunk_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            1,
            "Module configuration\n\
             sample-runner --install sample-unit\n\
             sample-configure alpha-subject\n\
             Settings are stored in /opt/alpha/alpha-subject.conf in section [AlphaSubject].",
        ));
        for index in 2..8 {
            rows.push(chunk_row(
                workspace_id,
                library_id,
                document_id,
                revision_id,
                index,
                "endpointUrl = http://localhost\n\
                 timeoutSeconds = 60\n\
                 primaryKey = \"\"\n\
                 credentialToken = \"\"\n\
                 retryInterval = 10\n\
                 currency = \"USD\"",
            ));
        }

        let selected = select_setup_focus_document_rows(&rows);

        assert!(
            selected.iter().take(SETUP_VARIANT_CHUNKS_PER_DOCUMENT).any(|row| row.chunk_index == 1),
            "{selected:#?}"
        );
    }

    #[test]
    fn versioned_update_procedure_merge_keeps_title_aligned_candidate_ahead_of_body_only_evidence()
    {
        let title_document = document_row("target-update.html", "Sample Target update guide");
        let evidence_document =
            document_row("hosted-platform.html", "Hosted application platform update guide");
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        let document_index = HashMap::from([
            (title_document.document_id, title_document.clone()),
            (evidence_document.document_id, evidence_document.clone()),
        ]);
        let title_candidates = versioned_update_procedure_candidate_documents(
            "how to update Sample Target?",
            Some(&query_ir),
            &document_index,
            4,
        );
        let term_model =
            versioned_update_procedure_term_model("how to update Sample Target?", Some(&query_ir));
        let mut environment = setup_focus_runtime_chunk(evidence_document.document_id, 0, 1.0);
        environment.document_label = evidence_document.title.clone().unwrap_or_default();
        environment.source_text = concat!(
            "This application can be installed on an Sample Target.\n",
            "1. sample-runner --refresh\n",
            "2. sample-runner --apply\n",
            "3. sudo service hosted-app restart"
        )
        .to_string();
        let evidence_candidates = versioned_update_procedure_candidates_from_evidence_chunks(
            &[environment],
            &term_model,
            4,
        );

        let merged = merge_versioned_update_procedure_document_candidates(
            title_candidates,
            evidence_candidates,
            1,
        );

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].document_id, title_document.document_id);
        assert!(merged[0].allow_head_fallback);
        assert!(!merged[0].requires_action_text_match);
        assert!(!merged[0].requires_subject_text_match);
    }

    #[test]
    fn versioned_update_procedure_merge_preserves_seed_indices_when_title_priority_wins() {
        let document_id = Uuid::now_v7();
        let title_candidate = VersionedUpdateProcedureDocumentCandidate {
            document_id,
            exact_title_identity: false,
            target_title_anchor: false,
            allow_head_fallback: true,
            requires_action_text_match: false,
            requires_subject_text_match: false,
            subject_identity_score: 3,
            focus_aligned_command_score: 0,
            priority: VERSIONED_UPDATE_PROCEDURE_TITLE_ALIGNMENT_PRIORITY_BONUS,
            seed_chunk_indices: Vec::new(),
            source_local_anchor_indices: Vec::new(),
        };
        let evidence_candidate = VersionedUpdateProcedureDocumentCandidate {
            document_id,
            exact_title_identity: false,
            target_title_anchor: false,
            allow_head_fallback: false,
            requires_action_text_match: true,
            requires_subject_text_match: false,
            subject_identity_score: 0,
            focus_aligned_command_score: 0,
            priority: VERSIONED_UPDATE_PROCEDURE_EVIDENCE_PRIORITY_BONUS,
            seed_chunk_indices: vec![4, 5],
            source_local_anchor_indices: Vec::new(),
        };

        let merged = merge_versioned_update_procedure_document_candidates(
            vec![title_candidate],
            vec![evidence_candidate],
            1,
        );

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].document_id, document_id);
        assert_eq!(merged[0].seed_chunk_indices, vec![4, 5]);
        assert!(merged[0].allow_head_fallback);
        assert!(!merged[0].requires_action_text_match);
    }

    #[test]
    fn versioned_update_procedure_merge_reserves_strong_subject_title_candidate() {
        let title_document_id = Uuid::now_v7();
        let title_candidate = VersionedUpdateProcedureDocumentCandidate {
            document_id: title_document_id,
            exact_title_identity: false,
            target_title_anchor: false,
            allow_head_fallback: true,
            requires_action_text_match: false,
            requires_subject_text_match: false,
            subject_identity_score: 3,
            focus_aligned_command_score: 0,
            priority: 100,
            seed_chunk_indices: Vec::new(),
            source_local_anchor_indices: Vec::new(),
        };
        let evidence_candidates = (0..8)
            .map(|index| VersionedUpdateProcedureDocumentCandidate {
                document_id: Uuid::now_v7(),
                exact_title_identity: false,
                target_title_anchor: false,
                allow_head_fallback: false,
                requires_action_text_match: true,
                requires_subject_text_match: false,
                subject_identity_score: 1,
                focus_aligned_command_score: 0,
                priority: 20_000usize.saturating_sub(index),
                seed_chunk_indices: vec![index as i32],
                source_local_anchor_indices: Vec::new(),
            })
            .collect::<Vec<_>>();

        let merged = merge_versioned_update_procedure_document_candidates(
            vec![title_candidate],
            evidence_candidates,
            4,
        );

        assert_eq!(merged[0].document_id, title_document_id, "{merged:#?}");
        assert!(merged[0].allow_head_fallback);
        assert!(!merged[0].requires_action_text_match);
    }

    #[test]
    fn versioned_update_procedure_title_reserve_moves_preserved_candidate_to_front() {
        let title_document_id = Uuid::now_v7();
        let title_candidate = VersionedUpdateProcedureDocumentCandidate {
            document_id: title_document_id,
            exact_title_identity: true,
            target_title_anchor: false,
            allow_head_fallback: true,
            requires_action_text_match: false,
            requires_subject_text_match: false,
            subject_identity_score: 3,
            focus_aligned_command_score: 0,
            priority: 100,
            seed_chunk_indices: Vec::new(),
            source_local_anchor_indices: Vec::new(),
        };
        let mut candidates = (0..4)
            .map(|index| VersionedUpdateProcedureDocumentCandidate {
                document_id: Uuid::now_v7(),
                exact_title_identity: false,
                target_title_anchor: false,
                allow_head_fallback: false,
                requires_action_text_match: true,
                requires_subject_text_match: false,
                subject_identity_score: 1,
                focus_aligned_command_score: 0,
                priority: 20_000usize.saturating_sub(index),
                seed_chunk_indices: vec![index as i32],
                source_local_anchor_indices: Vec::new(),
            })
            .collect::<Vec<_>>();

        ensure_reserved_versioned_update_procedure_title_candidates(
            &mut candidates,
            vec![title_candidate],
            4,
        );

        assert_eq!(candidates.len(), 4);
        assert_eq!(candidates[0].document_id, title_document_id);
        assert!(candidates[0].allow_head_fallback);
        assert!(!candidates[0].requires_action_text_match);
    }

    #[test]
    fn versioned_update_procedure_title_reserve_keeps_exact_target_anchor() {
        let generic_a_id = Uuid::now_v7();
        let generic_b_id = Uuid::now_v7();
        let instruction_id = Uuid::now_v7();
        let make_candidate = |document_id: Uuid, priority: usize, target_title_anchor: bool| {
            VersionedUpdateProcedureDocumentCandidate {
                document_id,
                exact_title_identity: true,
                target_title_anchor,
                allow_head_fallback: true,
                requires_action_text_match: false,
                requires_subject_text_match: false,
                subject_identity_score: 3,
                focus_aligned_command_score: 0,
                priority,
                seed_chunk_indices: Vec::new(),
                source_local_anchor_indices: Vec::new(),
            }
        };
        let candidates = vec![
            (
                300,
                "Sample Target update overview".to_string(),
                make_candidate(generic_a_id, 300, false),
            ),
            (
                290,
                "Sample Target update compatibility".to_string(),
                make_candidate(generic_b_id, 290, false),
            ),
            (
                10,
                "Sample Target target runbook".to_string(),
                make_candidate(instruction_id, 10, true),
            ),
        ];

        let selected = select_versioned_update_procedure_title_candidates(candidates, 2);

        assert!(selected.iter().any(|candidate| candidate.document_id == instruction_id));
    }

    #[test]
    fn versioned_update_procedure_instruction_title_prefers_head_window_rows() {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let candidate = VersionedUpdateProcedureDocumentCandidate {
            document_id,
            exact_title_identity: true,
            target_title_anchor: true,
            allow_head_fallback: true,
            requires_action_text_match: false,
            requires_subject_text_match: false,
            subject_identity_score: 3,
            focus_aligned_command_score: 0,
            priority: VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_IDENTITY_PRIORITY_BONUS,
            seed_chunk_indices: Vec::new(),
            source_local_anchor_indices: Vec::new(),
        };
        let probe_rows = vec![chunk_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            20,
            "Late matching update note with no commands.",
        )];
        let head_rows = vec![
            chunk_row(
                workspace_id,
                library_id,
                document_id,
                revision_id,
                0,
                "Instruction for updating Sample Target from platform 1 to platform 2.",
            ),
            chunk_row(
                workspace_id,
                library_id,
                document_id,
                revision_id,
                1,
                "1. sample-runner --refresh\n2. sample-runner --apply\n3. sudo sample-configure alpha-rest\n4. sudo sample-service restart alpha-rest",
            ),
        ];

        let selected = if versioned_update_procedure_candidate_prefers_head_window(&candidate) {
            merge_versioned_update_procedure_context_rows(head_rows, probe_rows, 2)
        } else {
            merge_versioned_update_procedure_context_rows(probe_rows, head_rows, 2)
        };

        assert_eq!(selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>(), vec![0, 1]);
    }

    #[test]
    fn versioned_update_procedure_instruction_title_scan_finds_exact_target_runbook() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types =
            vec!["artifact".to_string(), "procedure".to_string(), "version".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        query_ir.retrieval_query = Some("Sample Target version update".to_string());
        let instruction_document = document_row(
            "alpha-console-platform-update.html",
            "Instruction for updating Sample Target from platform 1 to platform 2",
        );
        let instruction_document_id = instruction_document.document_id;
        let mut rows = (0..12)
            .map(|index| {
                document_row(
                    &format!("alpha-console-overview-{index}.html"),
                    &format!("Sample Target update overview {index}"),
                )
            })
            .collect::<Vec<_>>();
        rows.push(instruction_document);
        let document_index = rows.into_iter().map(|row| (row.document_id, row)).collect();

        let candidates = versioned_update_procedure_instruction_title_candidates(
            "how to update Sample Target version?",
            Some(&query_ir),
            &document_index,
            1,
        );

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].document_id, instruction_document_id);
        assert!(candidates[0].target_title_anchor);
        assert!(candidates[0].allow_head_fallback);
    }

    #[test]
    fn versioned_update_procedure_exact_action_title_reserve_prefers_target_runbook_title() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types =
            vec!["artifact".to_string(), "procedure".to_string(), "version".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        query_ir.retrieval_query = Some("Sample Target version update".to_string());
        let runbook_document = document_row(
            "alpha-console-platform-transition.html",
            "Instruction for updating Sample Target from platform 1 to platform 2",
        );
        let runbook_document_id = runbook_document.document_id;
        let mut rows = (0..48)
            .map(|index| {
                document_row(
                    &format!("alpha-console-reference-{index}.html"),
                    &format!("Sample Target reference {index}"),
                )
            })
            .collect::<Vec<_>>();
        rows.push(runbook_document);
        let document_index = rows.into_iter().map(|row| (row.document_id, row)).collect();

        let action_title_candidates = versioned_update_procedure_exact_action_title_candidates(
            "how to update Sample Target version?",
            Some(&query_ir),
            &document_index,
            1,
        );

        assert_eq!(action_title_candidates.len(), 1);
        assert_eq!(action_title_candidates[0].document_id, runbook_document_id);
        assert!(action_title_candidates[0].exact_title_identity);
        assert!(action_title_candidates[0].target_title_anchor);
    }

    #[test]
    fn versioned_update_procedure_exact_action_title_reserve_matches_action_title() {
        let mut query_ir = setup_query_ir("Alpha console");
        query_ir.target_types =
            vec!["artifact".to_string(), "procedure".to_string(), "version".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Alpha console".to_string(), role: EntityRole::Subject }];
        query_ir.retrieval_query = Some("how to update alpha console version?".to_string());
        let runbook_document = document_row(
            "alpha-console-localized-update.html",
            "Alpha console update instructions from Platform 1.0 to 2.0",
        );
        let runbook_document_id = runbook_document.document_id;
        let mut rows = (0..48)
            .map(|index| {
                document_row(
                    &format!("alpha-console-localized-reference-{index}.html"),
                    &format!("Alpha console reference {index}"),
                )
            })
            .collect::<Vec<_>>();
        rows.push(runbook_document);
        let document_index = rows.into_iter().map(|row| (row.document_id, row)).collect();

        let action_title_candidates = versioned_update_procedure_exact_action_title_candidates(
            "how to update alpha console version?",
            Some(&query_ir),
            &document_index,
            1,
        );

        assert_eq!(action_title_candidates.len(), 1);
        assert_eq!(action_title_candidates[0].document_id, runbook_document_id);
        assert!(action_title_candidates[0].exact_title_identity);
        assert!(action_title_candidates[0].target_title_anchor);
    }

    #[test]
    fn versioned_update_procedure_exact_title_scan_promotes_lower_ranked_command_runbook() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types =
            vec!["artifact".to_string(), "procedure".to_string(), "version".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        query_ir.retrieval_query = Some("Sample Target version update".to_string());
        let runbook_document =
            document_row("alpha-console-maintenance-note.html", "Sample Target maintenance note");
        let runbook_document_id = runbook_document.document_id;
        let mut rows = (0..VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_RESERVE_CAP)
            .map(|index| {
                document_row(
                    &format!("alpha-console-version-update-overview-{index}.html"),
                    &format!("Sample Target version update overview {index}"),
                )
            })
            .collect::<Vec<_>>();
        rows.push(runbook_document);
        let document_index = rows.into_iter().map(|row| (row.document_id, row)).collect();

        let reserved_candidates = versioned_update_procedure_instruction_title_candidates(
            "how to update Sample Target version?",
            Some(&query_ir),
            &document_index,
            VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_RESERVE_CAP,
        );
        assert!(
            !reserved_candidates
                .iter()
                .any(|candidate| candidate.document_id == runbook_document_id)
        );

        let scan_candidates = versioned_update_procedure_instruction_title_candidates(
            "how to update Sample Target version?",
            Some(&query_ir),
            &document_index,
            VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_RUNBOOK_SCAN_CAP,
        );
        assert!(
            scan_candidates.iter().any(|candidate| candidate.document_id == runbook_document_id)
        );

        let mut runbook_chunk = setup_focus_runtime_chunk(runbook_document_id, 0, 1.0);
        runbook_chunk.document_label = "Sample Target maintenance note".to_string();
        runbook_chunk.source_text = concat!(
            "Sample Target update procedure\n",
            "1. sample-runner --refresh\n",
            "2. sample-runner --apply\n",
            "3. sudo sample-configure alpha-console\n",
            "4. sudo service alpha-console restart"
        )
        .to_string();
        let runbook_evidence_score = versioned_update_exact_target_runbook_score(
            "how to update Sample Target version?",
            &query_ir,
            &runbook_chunk,
        )
        .expect("dense package-maintenance runbook should pass exact-target scoring");

        let mut title_only_chunk = setup_focus_runtime_chunk(Uuid::now_v7(), 0, 1.0);
        title_only_chunk.document_label = "Sample Target version update overview".to_string();
        title_only_chunk.source_text =
            "Overview of supported Sample Target release channels.".to_string();
        assert!(
            versioned_update_exact_target_runbook_score(
                "how to update Sample Target version?",
                &query_ir,
                &title_only_chunk,
            )
            .is_none()
        );

        let top_title_anchor_score = versioned_update_procedure_chunk_score(0, 0)
            + VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_CHUNK_SCORE_BONUS;
        let lower_ranked_runbook_score =
            versioned_update_procedure_exact_target_runbook_chunk_score(
                VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_RESERVE_CAP,
                0,
                runbook_evidence_score,
            );
        assert!(
            lower_ranked_runbook_score > top_title_anchor_score,
            "{lower_ranked_runbook_score} should beat {top_title_anchor_score}"
        );
    }

    #[test]
    fn truncate_bundle_retains_exact_target_versioned_update_runbook_anchor() {
        let runbook_document_id = Uuid::now_v7();
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types =
            vec!["artifact".to_string(), "procedure".to_string(), "version".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        query_ir.retrieval_query = Some("how to update Sample Target version?".to_string());
        let mut runbook = setup_focus_runtime_chunk(runbook_document_id, 0, 1.0);
        runbook.document_label = "Sample Target update instruction".to_string();
        runbook.score_kind = RuntimeChunkScoreKind::FocusedDocument;
        runbook.score = Some(1.0);
        runbook.source_text = concat!(
            "Sample Target update procedure\n",
            "1. sample-runner --refresh\n",
            "2. sample-runner --apply\n",
            "3. sudo sample-configure alpha-console\n",
            "4. sudo service alpha-console restart"
        )
        .to_string();
        let mut bundle = RetrievalBundle {
            entities: Vec::new(),
            relationships: Vec::new(),
            chunks: (0..8)
                .map(|index| {
                    let mut chunk = setup_focus_runtime_chunk(Uuid::now_v7(), index, 2_200_000.0);
                    chunk.document_label = format!("Sample Target changelog {index}");
                    chunk.score_kind = RuntimeChunkScoreKind::FocusedDocument;
                    chunk.source_text = "Release note with no update commands.".to_string();
                    chunk
                })
                .collect(),
        };
        bundle.chunks.push(runbook);

        truncate_bundle(&mut bundle, 4, Some(&query_ir), &std::collections::HashSet::new());

        assert!(bundle.chunks.iter().any(|chunk| chunk.document_id == runbook_document_id));
    }

    #[test]
    fn versioned_update_procedure_merge_reserves_action_only_title_candidate() {
        let action_document_id = Uuid::now_v7();
        let action_title_candidate = VersionedUpdateProcedureDocumentCandidate {
            document_id: action_document_id,
            exact_title_identity: false,
            target_title_anchor: false,
            allow_head_fallback: false,
            requires_action_text_match: true,
            requires_subject_text_match: true,
            subject_identity_score: 0,
            focus_aligned_command_score: 0,
            priority: 1,
            seed_chunk_indices: Vec::new(),
            source_local_anchor_indices: Vec::new(),
        };
        let evidence_candidates = (0..6)
            .map(|index| VersionedUpdateProcedureDocumentCandidate {
                document_id: Uuid::now_v7(),
                exact_title_identity: false,
                target_title_anchor: false,
                allow_head_fallback: false,
                requires_action_text_match: true,
                requires_subject_text_match: false,
                subject_identity_score: 0,
                focus_aligned_command_score: 0,
                priority: 10_000usize.saturating_sub(index),
                seed_chunk_indices: vec![index as i32],
                source_local_anchor_indices: Vec::new(),
            })
            .collect::<Vec<_>>();

        let merged = merge_versioned_update_procedure_document_candidates(
            vec![action_title_candidate],
            evidence_candidates,
            4,
        );

        assert!(merged.iter().any(|candidate| candidate.document_id == action_document_id));
    }

    #[test]
    fn versioned_update_procedure_exact_title_chunk_score_beats_reference_expansion() {
        let exact_candidate = VersionedUpdateProcedureDocumentCandidate {
            document_id: Uuid::now_v7(),
            exact_title_identity: true,
            target_title_anchor: false,
            allow_head_fallback: true,
            requires_action_text_match: false,
            requires_subject_text_match: false,
            subject_identity_score: 3,
            focus_aligned_command_score: 0,
            priority: VERSIONED_UPDATE_PROCEDURE_EXACT_TITLE_IDENTITY_PRIORITY_BONUS,
            seed_chunk_indices: Vec::new(),
            source_local_anchor_indices: Vec::new(),
        };
        let max_reference_evidence = VersionedUpdateProcedureChunkEvidence {
            subject_overlap: 8,
            procedure_overlap: 8,
            label_subject_overlap: 8,
            label_procedure_overlap: 8,
            structural_score: 8,
            version_transition_score: 8,
            subject_aligned_version_transition_score: 8,
            ordered_procedure_score: 8,
            command_or_script_score: 8,
            focus_aligned_command_score: 8,
            command_sequence_score: 8,
            unfocused_transition_score: 0,
            has_action_script: false,
            bound_target_runbook_score: 1,
            score: 20_000,
            has_setup_script_signature: false,
        };

        let exact_score = versioned_update_procedure_candidate_chunk_score(&exact_candidate, 7, 0);
        let reference_score =
            versioned_update_procedure_reference_chunk_score(max_reference_evidence, 0, 0);

        assert!(exact_score > reference_score);
    }

    #[test]
    fn versioned_update_procedure_context_merge_keeps_forward_neighbor_once() {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let matched_head =
            chunk_row(workspace_id, library_id, document_id, revision_id, 0, "Alpha update intro");
        let neighbor = chunk_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            1,
            "1. Stop Alpha\n2. Run ./upgrade_alpha.sh",
        );
        let late = chunk_row(workspace_id, library_id, document_id, revision_id, 2, "late note");

        let merged = merge_versioned_update_procedure_context_rows(
            vec![matched_head.clone()],
            vec![matched_head.clone(), neighbor.clone(), late],
            2,
        );

        assert_eq!(merged.iter().map(|row| row.chunk_index).collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!(merged[0].chunk_id, matched_head.chunk_id);
        assert_eq!(merged[1].chunk_id, neighbor.chunk_id);
    }

    #[test]
    fn versioned_update_procedure_context_windows_follow_existing_budget() {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let anchor = chunk_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            2,
            "Alpha update overview",
        );

        let windows = versioned_update_procedure_context_windows(
            &[anchor],
            VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT,
        );

        assert_eq!(windows, vec![(1, 8)]);
    }

    #[test]
    fn versioned_update_procedure_context_windows_use_forward_budget_at_document_start() {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let anchor = chunk_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            0,
            "Alpha update overview",
        );

        let windows = versioned_update_procedure_context_windows(
            &[anchor],
            VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT,
        );

        assert_eq!(windows, vec![(0, 7)]);
    }

    #[test]
    fn versioned_update_source_local_windows_expand_from_mid_document_anchor() {
        let windows = versioned_update_procedure_source_local_context_windows(
            &[17],
            VERSIONED_UPDATE_PROCEDURE_SOURCE_LOCAL_RUNBOOK_CONTEXT_LIMIT,
        );

        assert_eq!(windows, vec![(16, 31)]);
        assert!(windows.iter().any(|(start, end)| *start <= 22 && 22 <= *end));
    }

    #[test]
    fn versioned_update_exact_probe_accepts_broad_procedure_runbook_context() {
        let document = document_row("install-update.html", "Install and update");
        let document_id = document.document_id;
        let revision_id = Uuid::now_v7();
        let workspace_id = document.workspace_id;
        let library_id = document.library_id;
        let document_index = HashMap::from([(document_id, document)]);
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["procedure".to_string(), "concept".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        query_ir.document_focus = None;
        query_ir.retrieval_query = Some("how to update Sample Target?".to_string());
        let rows = vec![
            chunk_row(
                workspace_id,
                library_id,
                document_id,
                revision_id,
                17,
                "Sample Target background and compatibility notes.",
            ),
            chunk_row(
                workspace_id,
                library_id,
                document_id,
                revision_id,
                22,
                concat!(
                    "Sample Target update procedure\n",
                    "1. sample-runner --refresh\n",
                    "2. sample-runner --apply sample-target\n",
                    "3. sudo service sample-target restart"
                ),
            ),
        ];

        let selected = select_versioned_update_procedure_exact_target_runbook_probe_rows(
            &rows,
            "how to update Sample Target?",
            Some(&query_ir),
            &document_index,
            &["sample".to_string(), "target".to_string(), "update".to_string()],
            4,
        );

        assert_eq!(selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>(), vec![22]);
    }

    #[test]
    fn versioned_update_procedure_reservation_keeps_distinct_exact_target_documents() {
        let script_document_id = Uuid::now_v7();
        let transition_document_id = Uuid::now_v7();
        let mut query_ir = setup_query_ir("Sample Subject");
        query_ir.target_types =
            vec!["artifact".to_string(), "procedure".to_string(), "version".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Subject".to_string(), role: EntityRole::Subject }];
        query_ir.retrieval_query = Some("Sample Subject update version procedure".to_string());

        let mut script_a = setup_focus_runtime_chunk(script_document_id, 0, 50.0);
        script_a.document_label = "Sample Subject install and update guide".to_string();
        script_a.score_kind = RuntimeChunkScoreKind::FocusedDocument;
        script_a.source_text = concat!(
            "Sample Subject update script\n",
            "sudo su\n",
            "sample-transfer https://example.invalid/alpha/update.sh -o /tmp/sample-runner.sh\n",
            "sample-prepare +x /tmp/sample-runner.sh\n",
            "/tmp/sample-runner.sh"
        )
        .to_string();
        let mut script_b = setup_focus_runtime_chunk(script_document_id, 1, 49.0);
        script_b.document_label = script_a.document_label.clone();
        script_b.score_kind = RuntimeChunkScoreKind::FocusedDocument;
        script_b.source_text = script_a.source_text.clone();
        let mut transition = setup_focus_runtime_chunk(transition_document_id, 0, 10.0);
        transition.document_label = "Sample Subject update transition guide".to_string();
        transition.score_kind = RuntimeChunkScoreKind::FocusedDocument;
        transition.source_text = concat!(
            "Sample Subject update transition from version 1.0 to 2.0\n",
            "1. Refresh bundle metadata with sample-runner --refresh.\n",
            "2. Upgrade installed packages with sample-runner --apply.\n",
            "3. Reconfigure Alpha REST with sudo sample-configure alpha-rest.\n",
            "4. Restart Alpha REST with sudo service alpha-rest restart."
        )
        .to_string();
        let indexed = vec![(0, script_a), (1, script_b), (2, transition)];

        let reserved = reserved_versioned_update_procedure_chunks(&indexed, 4, &query_ir);

        assert!(
            reserved.iter().any(|(_, chunk)| chunk.document_id == transition_document_id),
            "{reserved:#?}"
        );
        assert_eq!(reserved.len(), 3, "{reserved:#?}");
        assert!(
            reserved.iter().take(2).any(|(_, chunk)| chunk.document_id == transition_document_id),
            "{reserved:#?}"
        );
        assert!(
            reserved.iter().take(2).any(|(_, chunk)| chunk.document_id == script_document_id),
            "{reserved:#?}"
        );
    }

    #[test]
    fn truncate_reserves_exact_versioned_update_anchor_against_focused_peers() {
        let target_document_id = Uuid::now_v7();
        let mut query_ir = setup_query_ir("Alpha Control");
        query_ir.target_types =
            vec!["artifact".to_string(), "procedure".to_string(), "version".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Alpha Control".to_string(), role: EntityRole::Subject }];
        query_ir.retrieval_query = Some("Alpha Control update version procedure".to_string());

        let mut chunks = (0..3)
            .map(|index| {
                let mut chunk = setup_focus_runtime_chunk(Uuid::now_v7(), 0, 200.0 - index as f32);
                chunk.document_label = format!("Beta Module {} update guide", index);
                chunk.score_kind = RuntimeChunkScoreKind::FocusedDocument;
                chunk.source_text = concat!(
                    "Beta Module update procedure\n",
                    "1. Refresh bundle metadata.\n",
                    "2. Restart the beta service."
                )
                .to_string();
                chunk
            })
            .collect::<Vec<_>>();
        let mut exact_anchor = setup_focus_runtime_chunk(target_document_id, 0, 1.0);
        exact_anchor.document_label = "Alpha Control update guide".to_string();
        exact_anchor.score_kind = RuntimeChunkScoreKind::FocusedDocument;
        exact_anchor.source_text = concat!(
            "Alpha Control update from version 1.0 to 2.0\n",
            "1. Refresh bundle metadata with pkgctl refresh.\n",
            "2. Upgrade packages with pkgctl upgrade.\n",
            "3. Reconfigure Alpha Control with pkgctl reconfigure alpha-control.\n",
            "4. Restart Alpha Control service."
        )
        .to_string();
        chunks.push(exact_anchor);

        truncate_chunks_for_context(&mut chunks, 3, Some(&query_ir), &HashSet::new());

        assert!(chunks.iter().any(|chunk| chunk.document_id == target_document_id), "{chunks:#?}");
    }

    #[test]
    fn truncate_reserves_exact_versioned_update_anchor_after_identity_merge() {
        let target_document_id = Uuid::now_v7();
        let mut query_ir = setup_query_ir("Sample Subject");
        query_ir.target_types =
            vec!["artifact".to_string(), "procedure".to_string(), "version".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Subject".to_string(), role: EntityRole::Subject }];
        query_ir.retrieval_query = Some("Sample Subject update version procedure".to_string());

        let mut chunks = (0..10)
            .map(|index| {
                let mut chunk = setup_focus_runtime_chunk(
                    Uuid::now_v7(),
                    index,
                    DOCUMENT_IDENTITY_SCORE_FLOOR + 100.0 - index as f32,
                );
                chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
                chunk.document_label = format!("Beta Module {index} reference");
                chunk.source_text = "Beta Module reference index".to_string();
                chunk
            })
            .collect::<Vec<_>>();
        let mut exact_anchor = setup_focus_runtime_chunk(target_document_id, 0, 1.0);
        exact_anchor.document_label = "Sample Subject update guide".to_string();
        exact_anchor.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
        exact_anchor.source_text = concat!(
            "Sample Subject update from version 1.0 to 2.0\n",
            "1. Refresh bundle metadata with sudo pkgctl refresh.\n",
            "2. Upgrade installed packages with sudo pkgctl upgrade.\n",
            "3. Reconfigure Sample Subject REST with sudo sample-configure alpha-rest.\n",
            "4. Restart Sample Subject REST with sudo service alpha-rest restart."
        )
        .to_string();
        let exact_anchor_id = exact_anchor.chunk_id;
        chunks.push(exact_anchor);

        truncate_chunks_for_context(&mut chunks, 8, Some(&query_ir), &HashSet::new());

        assert!(chunks.iter().any(|chunk| chunk.chunk_id == exact_anchor_id), "{chunks:#?}");
    }

    #[test]
    fn truncate_reserves_source_context_versioned_update_procedure_command_anchor() {
        let target_document_id = Uuid::now_v7();
        let mut query_ir = setup_query_ir("Sample Subject");
        query_ir.target_types =
            vec!["artifact".to_string(), "procedure".to_string(), "version".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Subject".to_string(), role: EntityRole::Subject }];
        query_ir.literal_constraints =
            vec![LiteralSpan { text: "Sample Subject".to_string(), kind: LiteralKind::Identifier }];
        query_ir.retrieval_query = Some("Sample Subject update version procedure".to_string());

        let mut chunks = (0..12)
            .map(|index| {
                let mut chunk =
                    setup_focus_runtime_chunk(Uuid::now_v7(), index, 300.0 - index as f32);
                chunk.score_kind = RuntimeChunkScoreKind::SourceContext;
                chunk.document_label = format!("Beta Module {index} reference table");
                chunk.source_text = "Sheet: Requirements | Row: table-only context".to_string();
                chunk
            })
            .collect::<Vec<_>>();
        let mut exact_anchor = setup_focus_runtime_chunk(target_document_id, 0, 1.0);
        exact_anchor.document_label = "Sample Subject update guide".to_string();
        exact_anchor.score_kind = RuntimeChunkScoreKind::SourceContext;
        exact_anchor.source_text = concat!(
            "Sample Subject update from version 1.0 to 2.0\n",
            "1. Refresh bundle metadata with sudo pkgctl refresh.\n",
            "2. Upgrade installed packages with sudo pkgctl upgrade.\n",
            "3. Reconfigure Sample Subject REST with sudo sample-configure alpha-rest.\n",
            "4. Restart Sample Subject REST with sudo service alpha-rest restart."
        )
        .to_string();
        let exact_anchor_id = exact_anchor.chunk_id;
        chunks.push(exact_anchor);

        truncate_chunks_for_context(&mut chunks, 8, Some(&query_ir), &HashSet::new());

        assert!(chunks.iter().any(|chunk| chunk.chunk_id == exact_anchor_id), "{chunks:#?}");
    }

    #[test]
    fn truncate_reserves_focused_document_chunks_against_structural_context() {
        let structural_document_id = Uuid::now_v7();
        let focused_document_id = Uuid::now_v7();
        let mut chunks = (0..6)
            .map(|index| {
                let mut chunk = setup_focus_runtime_chunk(
                    structural_document_id,
                    index,
                    DOCUMENT_IDENTITY_SCORE_FLOOR + 100.0 - index as f32,
                );
                chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
                chunk
            })
            .collect::<Vec<_>>();
        let mut focused_ids = Vec::new();
        for index in 0..2 {
            let mut chunk = setup_focus_runtime_chunk(focused_document_id, index, 500.0);
            chunk.score_kind = RuntimeChunkScoreKind::FocusedDocument;
            focused_ids.push(chunk.chunk_id);
            chunks.push(chunk);
        }
        let query_ir = setup_query_ir("Sample Manifest");

        truncate_chunks_for_context(&mut chunks, 4, Some(&query_ir), &HashSet::new());

        for chunk_id in focused_ids {
            assert!(chunks.iter().any(|chunk| chunk.chunk_id == chunk_id), "{chunks:#?}");
        }
        assert_eq!(chunks.len(), 4);
    }

    #[test]
    fn truncate_does_not_reserve_setup_anchor_for_versioned_procedure_ir() {
        let update_document_id = Uuid::now_v7();
        let setup_document_id = Uuid::now_v7();
        let mut chunks = Vec::new();
        for index in 0..2 {
            let mut chunk = setup_focus_runtime_chunk(
                update_document_id,
                index,
                DOCUMENT_IDENTITY_SCORE_FLOOR + 100.0 - index as f32,
            );
            chunk.document_label = "Sample Target update guide".to_string();
            chunk.source_text = format!("Step {}. Refresh package version 2.0.{index}.", index + 1);
            chunks.push(chunk);
        }
        let mut setup_anchor =
            setup_focus_runtime_chunk(setup_document_id, 0, DOCUMENT_IDENTITY_SCORE_FLOOR);
        setup_anchor.document_label = "Sample Target setup guide".to_string();
        setup_anchor.source_text =
            "sample-runner --install sample-link\nSettings file /opt/alpha/connector.conf"
                .to_string();
        let setup_anchor_id = setup_anchor.chunk_id;
        chunks.push(setup_anchor);
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        query_ir.retrieval_query = Some("how to update Sample Target?".to_string());

        truncate_chunks_for_context(&mut chunks, 2, Some(&query_ir), &HashSet::new());

        assert_eq!(chunks.len(), 2);
        assert!(
            !chunks.iter().any(|chunk| chunk.chunk_id == setup_anchor_id),
            "setup/package anchor must not evict versioned procedure evidence"
        );
    }

    fn document_row(file_name: &str, title: &str) -> KnowledgeDocumentRow {
        document_row_with_source(file_name, title, None, None)
    }

    fn document_row_with_source(
        file_name: &str,
        title: &str,
        source_uri: Option<&str>,
        document_hint: Option<&str>,
    ) -> KnowledgeDocumentRow {
        let document_id = Uuid::now_v7();
        KnowledgeDocumentRow {
            document_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            external_key: file_name.to_string(),
            file_name: Some(file_name.to_string()),
            title: Some(title.to_string()),
            source_uri: source_uri.map(str::to_string),
            document_hint: document_hint.map(str::to_string),
            document_state: "active".to_string(),
            active_revision_id: Some(Uuid::now_v7()),
            readable_revision_id: Some(Uuid::now_v7()),
            latest_revision_no: Some(1),
            parent_document_id: None,
            document_role: crate::domains::content::DOCUMENT_ROLE_PRIMARY.to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            deleted_at: None,
        }
    }

    fn revision_row(
        document_id: Uuid,
        revision_id: Uuid,
        revision_number: i64,
        text_state: &str,
    ) -> KnowledgeRevisionRow {
        let now = Utc::now();
        KnowledgeRevisionRow {
            revision_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id,
            revision_number,
            revision_state: "active".to_string(),
            revision_kind: "snapshot".to_string(),
            storage_ref: None,
            source_uri: None,
            document_hint: None,
            mime_type: "text/plain".to_string(),
            checksum: revision_id.to_string(),
            title: Some("Alpha Connector".to_string()),
            byte_size: 128,
            normalized_text: None,
            text_checksum: None,
            image_checksum: None,
            text_state: text_state.to_string(),
            vector_state: "ready".to_string(),
            graph_state: "ready".to_string(),
            text_readable_at: Some(now),
            vector_ready_at: Some(now),
            graph_ready_at: Some(now),
            superseded_by_revision_id: None,
            created_at: now,
        }
    }

    fn setup_query_ir(focus: &str) -> QueryIR {
        QueryIR {
            act: QueryAct::ConfigureHow,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::En,
            target_types: vec!["package".to_string(), "configuration_file".to_string()],
            target_entities: vec![EntityMention {
                label: "module settings".to_string(),
                role: EntityRole::Object,
            }],
            literal_constraints: Vec::<LiteralSpan>::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: Some(crate::domains::query_ir::DocumentHint {
                hint: focus.to_string(),
            }),
            conversation_refs: Vec::<UnresolvedRef>::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence: 1.0,
        }
    }

    fn low_confidence_untyped_query_ir() -> QueryIR {
        let mut query_ir = setup_query_ir("Sample Subject");
        query_ir.act = QueryAct::Describe;
        query_ir.confidence = 0.25;
        query_ir.target_types.clear();
        query_ir.target_entities.clear();
        query_ir.document_focus = None;
        query_ir
    }

    fn chunk_row(
        workspace_id: Uuid,
        library_id: Uuid,
        document_id: Uuid,
        revision_id: Uuid,
        chunk_index: i32,
        text: &str,
    ) -> KnowledgeChunkRow {
        let chunk_id = Uuid::now_v7();
        KnowledgeChunkRow {
            chunk_id,
            workspace_id,
            library_id,
            document_id,
            revision_id,
            chunk_index,
            chunk_kind: Some("code_block".to_string()),
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
            quality_score: None,
            window_text: None,
            raptor_level: None,
            occurred_at: None,
            occurred_until: None,
        }
    }

    #[test]
    fn chunk_answer_source_text_preserves_content_outside_window() {
        let workspace_id = Uuid::now_v7();
        let library_id = Uuid::now_v7();
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mut row = chunk_row(
            workspace_id,
            library_id,
            document_id,
            revision_id,
            0,
            "Overview line.\n1. Theta marker changed from state K to state L.",
        );
        row.chunk_kind = Some("paragraph".to_string());
        row.window_text = Some("Overview line.".to_string());

        let source_text = chunk_answer_source_text(&row);

        assert!(source_text.contains("Overview line."), "{source_text}");
        assert!(
            source_text.contains("Theta marker changed from state K to state L"),
            "{source_text}"
        );
    }

    fn setup_focus_runtime_chunk(
        document_id: Uuid,
        chunk_index: i32,
        score: f32,
    ) -> RuntimeMatchedChunk {
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index,
            chunk_kind: Some("paragraph".to_string()),
            document_id,
            document_label: format!("Document {document_id}"),
            excerpt: "setup evidence".to_string(),
            score: Some(score),
            score_kind: RuntimeChunkScoreKind::DocumentIdentity,
            source_text: "setup evidence".to_string(),
        }
    }

    fn latest_version_runtime_chunk(
        document_id: Uuid,
        chunk_index: i32,
        score: f32,
    ) -> RuntimeMatchedChunk {
        let mut chunk = setup_focus_runtime_chunk(document_id, chunk_index, score);
        chunk.score_kind = RuntimeChunkScoreKind::LatestVersion;
        chunk
    }
}

pub(crate) fn merge_chunks(
    left: Vec<RuntimeMatchedChunk>,
    right: Vec<RuntimeMatchedChunk>,
    top_k: usize,
) -> Vec<RuntimeMatchedChunk> {
    rrf_merge_chunks(left, right, top_k, RetrievalMergeLane::RrfFused)
}

pub(crate) fn merge_entity_bio_chunks(
    chunks: Vec<RuntimeMatchedChunk>,
    entity_bio_chunks: Vec<RuntimeMatchedChunk>,
    top_k: usize,
) -> Vec<RuntimeMatchedChunk> {
    rrf_merge_chunks(chunks, entity_bio_chunks, top_k, RetrievalMergeLane::EntityBio)
}

pub(crate) fn merge_graph_evidence_chunks(
    chunks: Vec<RuntimeMatchedChunk>,
    graph_evidence_chunks: Vec<RuntimeMatchedChunk>,
    top_k: usize,
) -> Vec<RuntimeMatchedChunk> {
    rrf_merge_chunks(chunks, graph_evidence_chunks, top_k, RetrievalMergeLane::GraphEvidence)
}

#[cfg(test)]
pub(crate) fn merge_query_ir_focus_chunks(
    chunks: Vec<RuntimeMatchedChunk>,
    query_ir_focus_chunks: Vec<RuntimeMatchedChunk>,
    top_k: usize,
) -> Vec<RuntimeMatchedChunk> {
    merge_query_ir_focus_chunks_for_query(chunks, query_ir_focus_chunks, top_k, None)
}

fn merge_query_ir_focus_chunks_for_query(
    chunks: Vec<RuntimeMatchedChunk>,
    query_ir_focus_chunks: Vec<RuntimeMatchedChunk>,
    top_k: usize,
    query_ir: Option<&QueryIR>,
) -> Vec<RuntimeMatchedChunk> {
    rrf_merge_chunks_with_query(
        chunks,
        query_ir_focus_chunks,
        top_k,
        RetrievalMergeLane::QueryIrFocus,
        query_ir,
    )
}

#[cfg(test)]
pub(crate) fn merge_versioned_update_procedure_chunks(
    chunks: Vec<RuntimeMatchedChunk>,
    procedure_chunks: Vec<RuntimeMatchedChunk>,
    top_k: usize,
) -> Vec<RuntimeMatchedChunk> {
    merge_versioned_update_procedure_chunks_for_query(chunks, procedure_chunks, top_k, None)
}

fn merge_versioned_update_procedure_chunks_for_query(
    chunks: Vec<RuntimeMatchedChunk>,
    procedure_chunks: Vec<RuntimeMatchedChunk>,
    top_k: usize,
    query_ir: Option<&QueryIR>,
) -> Vec<RuntimeMatchedChunk> {
    rrf_merge_chunks_with_query(
        chunks,
        procedure_chunks,
        top_k,
        RetrievalMergeLane::VersionedUpdateProcedure,
        query_ir,
    )
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum RetrievalMergeLane {
    RrfFused,
    EntityBio,
    GraphEvidence,
    QueryIrFocus,
    VersionedUpdateProcedure,
}

impl RetrievalMergeLane {
    fn score_kind(self) -> RuntimeChunkScoreKind {
        match self {
            Self::RrfFused => RuntimeChunkScoreKind::Relevance,
            Self::EntityBio => RuntimeChunkScoreKind::EntityBio,
            Self::GraphEvidence => RuntimeChunkScoreKind::GraphEvidence,
            Self::QueryIrFocus => RuntimeChunkScoreKind::QueryIrFocus,
            Self::VersionedUpdateProcedure => RuntimeChunkScoreKind::FocusedDocument,
        }
    }
}

fn score_kind_priority(kind: RuntimeChunkScoreKind) -> u8 {
    match kind {
        RuntimeChunkScoreKind::Relevance => 0,
        RuntimeChunkScoreKind::EntityBio
        | RuntimeChunkScoreKind::GraphEvidence
        | RuntimeChunkScoreKind::SourceContext
        | RuntimeChunkScoreKind::FocusedDocument => 1,
        RuntimeChunkScoreKind::QueryIrFocus => 2,
        RuntimeChunkScoreKind::ContentAnchor => 4,
        RuntimeChunkScoreKind::DocumentIdentity | RuntimeChunkScoreKind::LatestVersion => 3,
    }
}

pub(crate) fn query_ir_promotes_graph_evidence(query_ir: &QueryIR) -> bool {
    if query_ir.has_exact_technical_literal()
        || query_ir.requests_source_slice_context()
        || query_ir.is_follow_up()
    {
        return false;
    }
    if matches!(query_ir.scope, QueryScope::MultiDocument | QueryScope::CrossLibrary)
        || query_ir.comparison.is_some()
    {
        return true;
    }
    if matches!(query_ir.act, QueryAct::RetrieveValue | QueryAct::Enumerate) {
        return true;
    }

    let graph_target_min_entities = query_ir
        .target_types
        .iter()
        .map(|value| canonical_target_type_tag(value))
        .filter_map(|tag| graph_evidence_target_type_min_entities(tag.as_str()))
        .min();
    graph_target_min_entities
        .is_some_and(|min_entities| query_ir.target_entities.len() >= min_entities)
}

fn graph_evidence_target_type_min_entities(tag: &str) -> Option<usize> {
    match tag {
        "artifact" => Some(1),
        "relationship" | "entity" | "event" | "route" | "transition" => Some(2),
        _ => None,
    }
}

fn score_kind_preserves_absolute_score(kind: RuntimeChunkScoreKind) -> bool {
    kind != RuntimeChunkScoreKind::Relevance
}

fn effective_merge_score_kind(
    chunk: &RuntimeMatchedChunk,
    lane: RetrievalMergeLane,
    raw_score: f32,
) -> RuntimeChunkScoreKind {
    if chunk.score_kind != RuntimeChunkScoreKind::Relevance {
        return chunk.score_kind;
    }
    if raw_score >= DOCUMENT_IDENTITY_SCORE_FLOOR {
        return RuntimeChunkScoreKind::DocumentIdentity;
    }
    lane.score_kind()
}

/// Reciprocal Rank Fusion: merges two ranked lists into a single ranking.
/// Each document's score is `1/(k + rank_in_list)` summed across both lists.
/// This normalizes across different scoring scales (BM25 vs cosine similarity).
fn rrf_merge_chunks(
    vector_hits: Vec<RuntimeMatchedChunk>,
    lexical_hits: Vec<RuntimeMatchedChunk>,
    top_k: usize,
    right_lane: RetrievalMergeLane,
) -> Vec<RuntimeMatchedChunk> {
    rrf_merge_chunks_with_query(vector_hits, lexical_hits, top_k, right_lane, None)
}

fn rrf_merge_chunks_with_query(
    vector_hits: Vec<RuntimeMatchedChunk>,
    lexical_hits: Vec<RuntimeMatchedChunk>,
    top_k: usize,
    right_lane: RetrievalMergeLane,
    query_ir: Option<&QueryIR>,
) -> Vec<RuntimeMatchedChunk> {
    const RRF_K: f32 = 60.0;

    let mut rrf_scores: HashMap<Uuid, f32> = HashMap::new();
    let mut raw_scores: HashMap<Uuid, f32> = HashMap::new();
    let mut score_kinds: HashMap<Uuid, RuntimeChunkScoreKind> = HashMap::new();
    let mut chunks_by_id: HashMap<Uuid, RuntimeMatchedChunk> = HashMap::new();
    let mut record_hit = |rank: usize, chunk: RuntimeMatchedChunk, lane: RetrievalMergeLane| {
        let rrf_score = 1.0 / (RRF_K + rank as f32 + 1.0);
        *rrf_scores.entry(chunk.chunk_id).or_default() += rrf_score;
        let raw_score = score_value(chunk.score);
        let score_kind = effective_merge_score_kind(&chunk, lane, raw_score);
        score_kinds
            .entry(chunk.chunk_id)
            .and_modify(|existing| {
                if score_kind_priority(score_kind) > score_kind_priority(*existing) {
                    *existing = score_kind;
                }
            })
            .or_insert(score_kind);
        if raw_score.is_finite() {
            raw_scores
                .entry(chunk.chunk_id)
                .and_modify(|existing| {
                    if raw_score > *existing {
                        *existing = raw_score;
                    }
                })
                .or_insert(raw_score);
        }
        chunks_by_id.entry(chunk.chunk_id).or_insert(chunk);
    };

    // Score vector hits by their rank position
    for (rank, chunk) in vector_hits.into_iter().enumerate() {
        record_hit(rank, chunk, RetrievalMergeLane::RrfFused);
    }

    // Score lexical hits by their rank position
    for (rank, chunk) in lexical_hits.into_iter().enumerate() {
        record_hit(rank, chunk, right_lane);
    }

    let mut values: Vec<RuntimeMatchedChunk> = chunks_by_id
        .into_values()
        .map(|mut chunk| {
            let rrf_score = rrf_scores.get(&chunk.chunk_id).copied();
            let raw_score = raw_scores.get(&chunk.chunk_id).copied();
            let score_kind = score_kinds
                .get(&chunk.chunk_id)
                .copied()
                .unwrap_or(RuntimeChunkScoreKind::Relevance);
            chunk.score =
                if score_kind_preserves_absolute_score(score_kind) { raw_score } else { rrf_score };
            chunk.score_kind = score_kind;
            chunk
        })
        .collect();

    values.sort_by(|left, right| {
        let left_kind =
            score_kinds.get(&left.chunk_id).copied().unwrap_or(RuntimeChunkScoreKind::Relevance);
        let right_kind =
            score_kinds.get(&right.chunk_id).copied().unwrap_or(RuntimeChunkScoreKind::Relevance);
        score_kind_priority(right_kind)
            .cmp(&score_kind_priority(left_kind))
            .then_with(|| score_desc_chunks(left, right))
            .then_with(|| left.document_id.cmp(&right.document_id))
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    truncate_merged_chunks_for_lane(values, top_k, right_lane, query_ir)
}

fn truncate_merged_chunks_for_lane(
    values: Vec<RuntimeMatchedChunk>,
    top_k: usize,
    right_lane: RetrievalMergeLane,
    query_ir: Option<&QueryIR>,
) -> Vec<RuntimeMatchedChunk> {
    if top_k == 0 || values.is_empty() {
        return Vec::new();
    }
    let reserved_latest_version_count = reserved_latest_version_merge_count(&values, top_k);
    let versioned_update_runbook_anchor = query_ir.and_then(|ir| {
        let indexed = values.iter().cloned().enumerate().collect::<Vec<_>>();
        best_versioned_update_procedure_runbook_anchor_chunk(&indexed, ir).map(|(_, chunk)| chunk)
    });
    if right_lane == RetrievalMergeLane::VersionedUpdateProcedure {
        let mut selected = truncate_merged_chunks_with_reservations(
            values,
            top_k,
            &[
                (RuntimeChunkScoreKind::LatestVersion, reserved_latest_version_count),
                (
                    RuntimeChunkScoreKind::FocusedDocument,
                    VERSIONED_UPDATE_PROCEDURE_CONTEXT_RESERVATION_LIMIT,
                ),
            ],
        );
        if let Some(anchor) = versioned_update_runbook_anchor {
            ensure_versioned_update_procedure_runbook_chunk_retained(&mut selected, anchor, top_k);
        }
        return selected;
    }
    if right_lane != RetrievalMergeLane::QueryIrFocus {
        if reserved_latest_version_count > 0 {
            return truncate_merged_chunks_with_reservations(
                values,
                top_k,
                &[(RuntimeChunkScoreKind::LatestVersion, reserved_latest_version_count)],
            );
        }
        return values.into_iter().take(top_k).collect();
    }
    let reserved_focused_document_count = values
        .iter()
        .filter(|chunk| chunk.score_kind == RuntimeChunkScoreKind::FocusedDocument)
        .count()
        .min(top_k.saturating_sub(1))
        .min(DOCUMENT_EVIDENCE_CONTEXT_RESERVATION_LIMIT);
    let reserved_source_context_count = values
        .iter()
        .filter(|chunk| chunk.score_kind == RuntimeChunkScoreKind::SourceContext)
        .count()
        .min(top_k.saturating_sub(1).saturating_sub(reserved_focused_document_count))
        .min(QUERY_IR_FOCUS_SOURCE_CONTEXT_RESERVATION_LIMIT);
    let mut selected = truncate_merged_chunks_with_reservations(
        values,
        top_k,
        &[
            (RuntimeChunkScoreKind::LatestVersion, reserved_latest_version_count),
            (RuntimeChunkScoreKind::FocusedDocument, reserved_focused_document_count),
            (RuntimeChunkScoreKind::SourceContext, reserved_source_context_count),
        ],
    );
    if let Some(anchor) = versioned_update_runbook_anchor {
        ensure_versioned_update_procedure_runbook_chunk_retained(&mut selected, anchor, top_k);
    }
    selected
}

fn reserved_latest_version_merge_count(values: &[RuntimeMatchedChunk], top_k: usize) -> usize {
    values
        .iter()
        .filter(|chunk| chunk.score_kind == RuntimeChunkScoreKind::LatestVersion)
        .count()
        .min(top_k.saturating_sub(1))
}

fn truncate_merged_chunks_with_reservations(
    values: Vec<RuntimeMatchedChunk>,
    top_k: usize,
    reservations: &[(RuntimeChunkScoreKind, usize)],
) -> Vec<RuntimeMatchedChunk> {
    if top_k == 0 || values.is_empty() {
        return Vec::new();
    }
    let mut remaining = top_k.saturating_sub(1);
    let effective_reservations = reservations
        .iter()
        .filter_map(|(kind, requested)| {
            let count = values
                .iter()
                .filter(|chunk| chunk.score_kind == *kind)
                .count()
                .min(*requested)
                .min(remaining);
            remaining = remaining.saturating_sub(count);
            (count > 0).then_some((*kind, count))
        })
        .collect::<Vec<_>>();
    if effective_reservations.is_empty() {
        return values.into_iter().take(top_k).collect();
    }

    let mut selected = Vec::with_capacity(top_k);
    let mut selected_chunk_ids = HashSet::<Uuid>::new();
    for (kind, count) in effective_reservations {
        for chunk in values.iter().filter(|chunk| chunk.score_kind == kind).take(count) {
            selected_chunk_ids.insert(chunk.chunk_id);
            selected.push(chunk.clone());
        }
    }
    for chunk in values {
        if selected.len() >= top_k {
            break;
        }
        if selected_chunk_ids.insert(chunk.chunk_id) {
            selected.push(chunk);
        }
    }
    selected
}

pub(crate) fn score_desc_chunks(
    left: &RuntimeMatchedChunk,
    right: &RuntimeMatchedChunk,
) -> std::cmp::Ordering {
    score_value(right.score)
        .total_cmp(&score_value(left.score))
        .then_with(|| left.chunk_id.cmp(&right.chunk_id))
}

pub(crate) fn score_value(score: Option<f32>) -> f32 {
    score.unwrap_or(0.0)
}

pub(crate) fn truncate_bundle(
    bundle: &mut RetrievalBundle,
    top_k: usize,
    query_ir: Option<&QueryIR>,
    demoted_document_ids: &HashSet<Uuid>,
) {
    bundle.entities.truncate(entity_context_top_k(top_k, query_ir));
    bundle.relationships.truncate(top_k);
    truncate_chunks_for_context(&mut bundle.chunks, top_k, query_ir, demoted_document_ids);
}

fn entity_context_top_k(top_k: usize, query_ir: Option<&QueryIR>) -> usize {
    let Some(query_ir) = query_ir else {
        return top_k;
    };
    if matches!(query_ir.act, QueryAct::Enumerate | QueryAct::Meta)
        && (query_ir.scope == QueryScope::LibraryMeta || !query_ir.target_entities.is_empty())
    {
        return top_k.saturating_mul(3).clamp(top_k, 96);
    }
    top_k
}

fn truncate_chunks_for_context(
    chunks: &mut Vec<RuntimeMatchedChunk>,
    top_k: usize,
    query_ir: Option<&QueryIR>,
    demoted_document_ids: &HashSet<Uuid>,
) {
    if chunks.len() <= top_k {
        return;
    }
    let mut indexed = std::mem::take(chunks).into_iter().enumerate().collect::<Vec<_>>();
    // Attached-context documents (image attachments collapsed onto their parent
    // page) sink below every peer/primary chunk so they fill only the residual
    // budget after real content — they are searchable context, not competing
    // documents. The demotion is the FIRST sort key so a flood of one-chunk
    // image siblings can never displace the parent's procedural/graph evidence.
    // The set already excludes any document the query explicitly targets, so an
    // exact "show screenshot X" ask is not demoted.
    indexed.sort_by(|(left_index, left), (right_index, right)| {
        demoted_document_ids
            .contains(&left.document_id)
            .cmp(&demoted_document_ids.contains(&right.document_id))
            .then_with(|| {
                score_kind_priority(right.score_kind).cmp(&score_kind_priority(left.score_kind))
            })
            .then_with(|| score_desc_chunks(left, right))
            .then_with(|| left_index.cmp(right_index))
    });
    let reserved_document_evidence =
        reserved_document_evidence_anchor_chunks(&indexed, top_k, query_ir);
    let reserved_content_anchor_chunks = reserved_content_anchor_chunks(&indexed, top_k);
    // Reserve the focused-document setup anchor (the chunk carrying both a
    // command-object literal and a configuration path) before the score-ordered
    // truncation runs, so a confident single-document configure/how-to answer
    // never loses the "what to install" line to denser parameter chunks.
    let reserved_anchor = query_ir.and_then(|ir| best_setup_focus_anchor_chunk(&indexed, ir));
    let reserved_versioned_update_runbook_anchor =
        query_ir.and_then(|ir| best_versioned_update_procedure_runbook_anchor_chunk(&indexed, ir));
    let reserved_setup_variant_anchors =
        query_ir.map(|ir| setup_variant_anchor_chunks(&indexed, top_k, ir)).unwrap_or_default();
    let reserved_exact_literal_context_anchors = query_ir
        .map(|ir| reserved_exact_literal_context_chunks(&indexed, top_k, ir))
        .unwrap_or_default();
    let mut selected = if let Some(query_ir) = query_ir {
        if query_requests_latest_versions(query_ir) {
            truncate_chunks_with_latest_version_reservation(&indexed, top_k, query_ir)
        } else if matches!(query_ir.scope, QueryScope::MultiDocument) {
            truncate_chunks_for_multi_document_scope(&indexed, top_k, query_ir)
        } else if let Some(reserved_count) =
            source_context_reservation_count(&indexed, top_k, query_ir)
        {
            truncate_chunks_with_source_context_reservation(&indexed, top_k, reserved_count)
        } else {
            indexed.iter().take(top_k).cloned().collect::<Vec<_>>()
        }
    } else {
        indexed.iter().take(top_k).cloned().collect::<Vec<_>>()
    };
    ensure_document_evidence_anchors_retained(&mut selected, reserved_document_evidence, top_k);
    if let Some(anchor) = reserved_anchor {
        ensure_setup_focus_anchor_retained(&mut selected, anchor, top_k);
    }
    ensure_setup_variant_anchors_retained(&mut selected, reserved_setup_variant_anchors, top_k);
    ensure_exact_literal_context_anchors_retained(
        &mut selected,
        reserved_exact_literal_context_anchors,
        top_k,
    );
    ensure_content_anchor_chunks_retained(&mut selected, reserved_content_anchor_chunks, top_k);
    if let Some(anchor) = reserved_versioned_update_runbook_anchor {
        ensure_versioned_update_procedure_runbook_anchor_retained(&mut selected, anchor, top_k);
    }
    *chunks = selected.into_iter().map(|(_, chunk)| chunk).collect();
}

fn reserved_content_anchor_chunks(
    indexed: &[(usize, RuntimeMatchedChunk)],
    top_k: usize,
) -> Vec<(usize, RuntimeMatchedChunk)> {
    if top_k < 2 {
        return Vec::new();
    }
    indexed
        .iter()
        .filter(|(_, chunk)| chunk.score_kind == RuntimeChunkScoreKind::ContentAnchor)
        .take(CONTENT_ANCHOR_CONTEXT_RESERVATION_LIMIT.min(top_k.saturating_sub(1)))
        .cloned()
        .collect()
}

fn truncate_chunks_with_latest_version_reservation(
    chunks: &[(usize, RuntimeMatchedChunk)],
    top_k: usize,
    query_ir: &QueryIR,
) -> Vec<(usize, RuntimeMatchedChunk)> {
    if top_k == 0 {
        return Vec::new();
    }
    let latest_limit = requested_latest_version_count(query_ir).min(top_k);
    let mut selected = Vec::with_capacity(top_k);
    let mut selected_indices = HashSet::new();
    for (index, chunk) in
        chunks.iter().filter(|(_, chunk)| chunk.score_kind == RuntimeChunkScoreKind::LatestVersion)
    {
        if selected.len() >= latest_limit {
            break;
        }
        selected_indices.insert(*index);
        selected.push((*index, chunk.clone()));
    }
    for (index, chunk) in chunks {
        if selected.len() >= top_k {
            break;
        }
        if selected_indices.insert(*index) {
            selected.push((*index, chunk.clone()));
        }
    }
    selected
}

fn reserved_document_evidence_anchor_chunks(
    indexed: &[(usize, RuntimeMatchedChunk)],
    top_k: usize,
    query_ir: Option<&QueryIR>,
) -> Vec<(usize, RuntimeMatchedChunk)> {
    if top_k < 2
        || query_ir.is_some_and(|ir| {
            matches!(ir.scope, QueryScope::MultiDocument | QueryScope::LibraryMeta)
        })
    {
        return Vec::new();
    }
    if let Some(query_ir) = query_ir
        && query_ir_requests_versioned_update_procedure_context("", query_ir)
    {
        let limit = VERSIONED_UPDATE_PROCEDURE_DOCUMENT_EVIDENCE_RESERVATION_LIMIT
            .min(top_k.saturating_sub(1));
        let mut selected = reserved_versioned_update_procedure_chunks(indexed, top_k, query_ir);
        if selected.len() >= limit {
            return selected;
        }
        let seen = selected.iter().map(|(_, chunk)| chunk.chunk_id).collect::<BTreeSet<_>>();
        for anchor in indexed
            .iter()
            .filter(|(_, chunk)| chunk.score_kind == RuntimeChunkScoreKind::FocusedDocument)
        {
            if seen.contains(&anchor.1.chunk_id) {
                continue;
            }
            selected.push(anchor.clone());
            if selected.len() >= limit {
                break;
            }
        }
        return selected;
    }
    indexed
        .iter()
        .filter(|(_, chunk)| chunk.score_kind == RuntimeChunkScoreKind::FocusedDocument)
        .take(DOCUMENT_EVIDENCE_CONTEXT_RESERVATION_LIMIT.min(top_k.saturating_sub(1)))
        .cloned()
        .collect()
}

fn reserved_exact_literal_context_chunks(
    indexed: &[(usize, RuntimeMatchedChunk)],
    top_k: usize,
    query_ir: &QueryIR,
) -> Vec<(usize, RuntimeMatchedChunk)> {
    if top_k < 2 || !query_ir_requests_exact_literal_context_reservation(query_ir) {
        return Vec::new();
    }
    let focus_terms = exact_literal_context_focus_terms(query_ir);
    if focus_terms.is_empty() {
        return Vec::new();
    }

    let limit = EXACT_LITERAL_CONTEXT_RESERVATION_LIMIT.min(top_k.saturating_sub(1));
    let mut scored = indexed
        .iter()
        .filter_map(|(index, chunk)| {
            let score = exact_literal_context_chunk_score(chunk, &focus_terms)?;
            Some((score, score_value(chunk.score), *index, chunk.clone()))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.1.total_cmp(&left.1))
            .then_with(|| left.2.cmp(&right.2))
    });

    let mut selected = Vec::<(usize, RuntimeMatchedChunk)>::new();
    let mut seen_chunk_ids = HashSet::<Uuid>::new();
    let mut seen_document_ids = HashSet::<Uuid>::new();
    for (_, _, index, chunk) in &scored {
        if selected.len() >= limit {
            return selected;
        }
        if seen_document_ids.insert(chunk.document_id) && seen_chunk_ids.insert(chunk.chunk_id) {
            selected.push((*index, chunk.clone()));
        }
    }
    for (_, _, index, chunk) in scored {
        if selected.len() >= limit {
            break;
        }
        if seen_chunk_ids.insert(chunk.chunk_id) {
            selected.push((index, chunk));
        }
    }
    selected
}

fn query_ir_requests_exact_literal_context_reservation(query_ir: &QueryIR) -> bool {
    if query_ir_requests_versioned_update_procedure_context("", query_ir) {
        return false;
    }
    if matches!(query_ir.act, QueryAct::Compare) && query_ir.comparison.is_some() {
        return true;
    }
    if query_ir.is_exact_literal_technical() || query_ir.has_exact_technical_literal() {
        return true;
    }
    query_ir.target_types.iter().any(|target_type| {
        matches!(
            canonical_target_type_tag(target_type).as_str(),
            "configuration_file"
                | "config_key"
                | "connection"
                | "endpoint"
                | "filesystem_path"
                | "http_method"
                | "package"
                | "parameter"
                | "path"
                | "port"
                | "protocol"
                | "url"
                | "wsdl"
        )
    }) || query_ir
        .literal_constraints
        .iter()
        .any(|literal| literal_kind_has_exact_technical_shape(literal.kind, &literal.text))
}

fn exact_literal_context_focus_terms(query_ir: &QueryIR) -> Vec<String> {
    let mut terms = BTreeSet::<String>::new();
    if let Some(retrieval_query) = query_ir.retrieval_query.as_deref() {
        for segment in technical_literal_focus_keyword_segments(retrieval_query, Some(query_ir)) {
            terms.extend(segment);
        }
        terms.extend(normalized_alnum_tokens(retrieval_query, 3));
    }
    terms.extend(query_multi_document_relevance_terms(query_ir));
    terms.into_iter().collect()
}

fn exact_literal_context_chunk_score(
    chunk: &RuntimeMatchedChunk,
    focus_terms: &[String],
) -> Option<usize> {
    let literal_inventory = structural_literal_inventory_score(chunk);
    if literal_inventory == 0 {
        return None;
    }
    let haystack =
        format!("{} {} {}", chunk.document_label, chunk.excerpt, chunk.source_text).to_lowercase();
    let focus_score = focus_terms.iter().filter(|term| haystack.contains(term.as_str())).count();
    if focus_score == 0 {
        return None;
    }
    Some(literal_inventory.saturating_mul(128).saturating_add(focus_score.saturating_mul(16)))
}

fn structural_literal_inventory_score(chunk: &RuntimeMatchedChunk) -> usize {
    let text = format!("{} {}", chunk.excerpt, chunk.source_text);
    extract_parameter_literals(&text, 32).len().saturating_mul(4)
        + extract_explicit_path_literals(&text, 8).len().saturating_mul(3)
        + extract_package_command_literals(&text, 4).len().saturating_mul(3)
}

fn reserved_versioned_update_procedure_chunks(
    indexed: &[(usize, RuntimeMatchedChunk)],
    top_k: usize,
    query_ir: &QueryIR,
) -> Vec<(usize, RuntimeMatchedChunk)> {
    let question = query_ir.retrieval_query.as_deref().unwrap_or_default();
    let term_model = versioned_update_procedure_term_model(question, Some(query_ir));
    let target_identity_sequences =
        versioned_update_procedure_target_identity_token_sequences(question, Some(query_ir));
    let limit =
        VERSIONED_UPDATE_PROCEDURE_DOCUMENT_EVIDENCE_RESERVATION_LIMIT.min(top_k.saturating_sub(1));
    let mut selected = Vec::<(usize, RuntimeMatchedChunk)>::new();
    let mut seen_chunk_ids = HashSet::<Uuid>::new();
    let mut scored = indexed
        .iter()
        .filter(|(_, chunk)| versioned_update_procedure_reservation_candidate_kind(chunk))
        .filter_map(|(index, chunk)| {
            let evidence = versioned_update_procedure_chunk_evidence(chunk, &term_model);
            let has_subject_identity =
                evidence.subject_overlap > 0 || evidence.label_subject_overlap > 0;
            let has_action = evidence.procedure_overlap > 0;
            let has_label_aligned_version_transition =
                evidence.version_transition_score > 0 && evidence.label_subject_overlap > 0;
            let has_ordered_procedure_runbook = evidence.ordered_procedure_score > 0
                && (has_action
                    || evidence.label_procedure_overlap > 0
                    || evidence.version_transition_score > 0);
            let has_focus_aligned_command_runbook = evidence.focus_aligned_command_score > 0
                && (has_action
                    || evidence.label_procedure_overlap > 0
                    || evidence.version_transition_score > 0);
            if !has_subject_identity
                || (!has_action
                    && !has_label_aligned_version_transition
                    && !has_ordered_procedure_runbook
                    && !has_focus_aligned_command_runbook)
                || evidence.structural_score
                    < VERSIONED_UPDATE_PROCEDURE_EVIDENCE_STRUCTURAL_SCORE_FLOOR
                || (evidence.has_setup_script_signature
                    && !versioned_update_procedure_setup_signature_is_action_bound(evidence))
            {
                return None;
            }
            let score = evidence
                .score
                .saturating_add(evidence.label_subject_overlap.saturating_mul(512))
                .saturating_add(evidence.label_procedure_overlap.saturating_mul(512))
                .saturating_add(evidence.ordered_procedure_score.saturating_mul(4096))
                .saturating_add(evidence.focus_aligned_command_score.saturating_mul(8192));
            let label_sequence = normalized_alnum_token_sequence(&chunk.document_label, 1);
            let exact_target_label = versioned_update_procedure_label_has_target_identity_sequence(
                &label_sequence,
                &target_identity_sequences,
            ) && (evidence.label_procedure_overlap > 0
                || evidence.version_transition_score > 0
                || evidence.ordered_procedure_score > 0
                || evidence.focus_aligned_command_score > 0);
            let exact_target_runbook_score =
                versioned_update_exact_target_runbook_score(question, query_ir, chunk).unwrap_or(0);
            Some((
                score.saturating_add(exact_target_runbook_score),
                exact_target_label,
                exact_target_runbook_score > 0,
                has_focus_aligned_command_runbook,
                evidence.ordered_procedure_score,
                evidence.version_transition_score,
                evidence.focus_aligned_command_score,
                exact_target_runbook_score,
                *index,
                chunk.clone(),
            ))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| right.2.cmp(&left.2))
            .then_with(|| right.7.cmp(&left.7))
            .then_with(|| right.3.cmp(&left.3))
            .then_with(|| right.4.cmp(&left.4))
            .then_with(|| right.6.cmp(&left.6))
            .then_with(|| right.5.cmp(&left.5))
            .then_with(|| right.0.cmp(&left.0))
            .then_with(|| score_desc_chunks(&left.9, &right.9))
            .then_with(|| left.8.cmp(&right.8))
    });
    let mut seen_exact_document_ids = HashSet::<Uuid>::new();
    let exact_document_limit = VERSIONED_UPDATE_PROCEDURE_SUBJECT_TITLE_RESERVE_CAP.min(limit);
    for (_, exact_target_label, exact_target_runbook, _, _, _, _, _, index, chunk) in scored.iter()
    {
        if selected.len() >= exact_document_limit {
            break;
        }
        if (!*exact_target_label && !*exact_target_runbook)
            || !seen_exact_document_ids.insert(chunk.document_id)
        {
            continue;
        }
        if seen_chunk_ids.insert(chunk.chunk_id) {
            selected.push((*index, chunk.clone()));
        }
    }
    let mut title_anchors = indexed
        .iter()
        .filter(|(_, chunk)| {
            chunk_is_versioned_update_instruction_title_anchor_for_model(chunk, &term_model)
        })
        .map(|(index, chunk)| {
            let evidence = versioned_update_procedure_chunk_evidence(chunk, &term_model);
            let label_sequence = normalized_alnum_token_sequence(&chunk.document_label, 1);
            let exact_target_label = versioned_update_procedure_label_has_target_identity_sequence(
                &label_sequence,
                &target_identity_sequences,
            ) && (evidence.label_procedure_overlap > 0
                || evidence.version_transition_score > 0
                || evidence.ordered_procedure_score > 0
                || evidence.focus_aligned_command_score > 0);
            let has_ordered_procedure_runbook = evidence.ordered_procedure_score > 0
                && (evidence.procedure_overlap > 0
                    || evidence.label_procedure_overlap > 0
                    || evidence.version_transition_score > 0);
            let has_focus_aligned_command_runbook = evidence.focus_aligned_command_score > 0
                && (evidence.procedure_overlap > 0
                    || evidence.label_procedure_overlap > 0
                    || evidence.version_transition_score > 0);
            (
                exact_target_label,
                has_ordered_procedure_runbook,
                has_focus_aligned_command_runbook,
                evidence.ordered_procedure_score,
                evidence.focus_aligned_command_score,
                evidence.version_transition_score,
                evidence.score,
                *index,
                chunk.clone(),
            )
        })
        .collect::<Vec<_>>();
    title_anchors.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.1.cmp(&left.1))
            .then_with(|| right.2.cmp(&left.2))
            .then_with(|| right.3.cmp(&left.3))
            .then_with(|| right.4.cmp(&left.4))
            .then_with(|| right.5.cmp(&left.5))
            .then_with(|| right.6.cmp(&left.6))
            .then_with(|| score_desc_chunks(&left.8, &right.8))
            .then_with(|| left.7.cmp(&right.7))
    });
    let mut seen_title_anchor_document_ids =
        selected.iter().map(|(_, chunk)| chunk.document_id).collect::<HashSet<_>>();
    for (_, _, _, _, _, _, _, index, chunk) in title_anchors {
        if selected.len() >= limit {
            break;
        }
        if !seen_title_anchor_document_ids.insert(chunk.document_id) {
            continue;
        }
        if seen_chunk_ids.insert(chunk.chunk_id) {
            selected.push((index, chunk));
        }
    }
    for (_, _, _, _, _, _, _, _, index, chunk) in scored {
        if selected.len() >= limit {
            break;
        }
        if seen_chunk_ids.insert(chunk.chunk_id) {
            selected.push((index, chunk.clone()));
        }
        let mut neighbor_count = 0usize;
        for (neighbor_index, neighbor) in indexed.iter().filter(|(_, candidate)| {
            versioned_update_procedure_reservation_candidate_kind(candidate)
                && candidate.document_id == chunk.document_id
                && candidate.revision_id == chunk.revision_id
                && candidate.chunk_index > chunk.chunk_index
        }) {
            if selected.len() >= limit
                || neighbor_count >= VERSIONED_UPDATE_PROCEDURE_RESERVED_NEIGHBORS_PER_ANCHOR
            {
                break;
            }
            if seen_chunk_ids.insert(neighbor.chunk_id) {
                selected.push((*neighbor_index, neighbor.clone()));
                neighbor_count = neighbor_count.saturating_add(1);
            }
        }
    }
    selected
}

fn versioned_update_procedure_reservation_candidate_kind(chunk: &RuntimeMatchedChunk) -> bool {
    matches!(
        chunk.score_kind,
        RuntimeChunkScoreKind::FocusedDocument
            | RuntimeChunkScoreKind::DocumentIdentity
            | RuntimeChunkScoreKind::SourceContext
    )
}

fn ensure_document_evidence_anchors_retained(
    selected: &mut Vec<(usize, RuntimeMatchedChunk)>,
    anchors: Vec<(usize, RuntimeMatchedChunk)>,
    top_k: usize,
) {
    if top_k == 0 || anchors.is_empty() {
        return;
    }
    let protected_anchor_ids =
        anchors.iter().map(|(_, chunk)| chunk.chunk_id).collect::<BTreeSet<_>>();
    for anchor in anchors {
        if selected.iter().any(|(_, chunk)| chunk.chunk_id == anchor.1.chunk_id) {
            continue;
        }
        if selected.len() < top_k {
            selected.push(anchor);
            continue;
        }
        let evict_position = selected
            .iter()
            .rposition(|(_, chunk)| {
                !protected_anchor_ids.contains(&chunk.chunk_id)
                    && !matches!(
                        chunk.score_kind,
                        RuntimeChunkScoreKind::FocusedDocument
                            | RuntimeChunkScoreKind::SourceContext
                    )
                    && !chunk_is_setup_focus_command_path_anchor(chunk)
            })
            .or_else(|| {
                selected.iter().rposition(|(_, chunk)| {
                    !protected_anchor_ids.contains(&chunk.chunk_id)
                        && chunk.score_kind != RuntimeChunkScoreKind::FocusedDocument
                })
            })
            .or_else(|| {
                selected.iter().rposition(|(_, chunk)| {
                    !protected_anchor_ids.contains(&chunk.chunk_id)
                        && chunk.document_id != anchor.1.document_id
                })
            });
        if let Some(position) = evict_position {
            let anchor_chunk_id = anchor.1.chunk_id;
            selected[position] = anchor;
            tracing::info!(
                stage = "retrieval.document_evidence_anchor_reserved",
                chunk_id = %anchor_chunk_id,
                "document evidence anchor reserved past score-ordered truncation"
            );
        }
    }
}

/// Pick the highest-scoring focused-document setup anchor among the candidate
/// chunks for a confident single-document configure/how-to query. An anchor is a
/// chunk that carries both a command-object literal and a configuration path
/// and comes from a document whose label overlaps the query's `document_focus`.
fn best_setup_focus_anchor_chunk(
    indexed: &[(usize, RuntimeMatchedChunk)],
    query_ir: &QueryIR,
) -> Option<(usize, RuntimeMatchedChunk)> {
    if !matches!(query_ir.act, QueryAct::ConfigureHow)
        || query_ir.document_focus.is_none()
        || query_ir_requests_versioned_update_procedure_context("", query_ir)
    {
        return None;
    }
    let focus_tokens = setup_focus_anchor_query_tokens(query_ir);
    indexed
        .iter()
        .filter(|(_, chunk)| chunk_is_setup_focus_command_path_anchor(chunk))
        .filter(|(_, chunk)| {
            if focus_tokens.is_empty() {
                return true;
            }
            let label_tokens = normalized_alnum_tokens(&chunk.document_label, 3);
            focus_token_overlap_count(&focus_tokens, &label_tokens) > 0
        })
        .max_by(|(_, left), (_, right)| score_desc_chunks(right, left))
        .map(|(index, chunk)| (*index, chunk.clone()))
}

fn best_versioned_update_procedure_runbook_anchor_chunk(
    indexed: &[(usize, RuntimeMatchedChunk)],
    query_ir: &QueryIR,
) -> Option<(usize, RuntimeMatchedChunk)> {
    let scoring_question = versioned_update_procedure_scoring_question(query_ir);
    if scoring_question.trim().is_empty() {
        return None;
    }
    if !query_ir_requests_versioned_update_procedure_context(&scoring_question, query_ir) {
        return None;
    }
    indexed
        .iter()
        .filter_map(|(index, chunk)| {
            let runbook_score =
                versioned_update_exact_target_runbook_score(&scoring_question, query_ir, chunk)?;
            Some((runbook_score, score_value(chunk.score), *index, chunk))
        })
        .max_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.total_cmp(&right.1))
                .then_with(|| right.2.cmp(&left.2))
        })
        .map(|(runbook_score, score, index, chunk)| {
            tracing::debug!(
                stage = "retrieval.versioned_update_procedure_runbook_anchor_selected",
                chunk_id = %chunk.chunk_id,
                document_id = %chunk.document_id,
                runbook_score,
                score,
                index,
                "versioned update runbook anchor selected for retention"
            );
            (index, chunk.clone())
        })
}

fn versioned_update_procedure_scoring_question(query_ir: &QueryIR) -> String {
    query_ir.retrieval_query.clone().filter(|value| !value.trim().is_empty()).unwrap_or_else(|| {
        query_ir
            .target_entities
            .iter()
            .map(|entity| entity.label.as_str())
            .chain(query_ir.document_focus.iter().map(|focus| focus.hint.as_str()))
            .collect::<Vec<_>>()
            .join(" ")
    })
}

fn setup_focus_anchor_query_tokens(query_ir: &QueryIR) -> BTreeSet<String> {
    let mut tokens = BTreeSet::new();
    if let Some(document_focus) = query_ir.document_focus.as_ref() {
        tokens.extend(normalized_alnum_tokens(&document_focus.hint, 3));
    }
    if let Some(retrieval_query) = query_ir.retrieval_query.as_deref() {
        tokens.extend(normalized_alnum_tokens(retrieval_query, 3));
    }
    for entity in &query_ir.target_entities {
        tokens.extend(normalized_alnum_tokens(&entity.label, 3));
    }
    tokens
}

pub(crate) fn chunk_is_setup_focus_command_path_anchor(chunk: &RuntimeMatchedChunk) -> bool {
    !extract_package_command_literals(&chunk.source_text, 1).is_empty()
        && setup_focus_configuration_path_count(&chunk.source_text) > 0
}

fn setup_variant_anchor_chunks(
    indexed: &[(usize, RuntimeMatchedChunk)],
    top_k: usize,
    query_ir: &QueryIR,
) -> Vec<(usize, RuntimeMatchedChunk)> {
    if top_k < 2 || !query_ir_requests_setup_variant_anchor_reservation(query_ir) {
        return Vec::new();
    }
    let query_terms = setup_focus_anchor_query_tokens(query_ir);
    let anchor_labels = indexed
        .iter()
        .filter(|(_, chunk)| chunk_is_setup_focus_command_path_anchor(chunk))
        .map(|(_, chunk)| chunk.document_label.as_str())
        .collect::<Vec<_>>();
    if anchor_labels.len() < 2 {
        return Vec::new();
    }
    let family_model = setup_variant_anchor_family_model(&anchor_labels, &query_terms);
    let mut selected = Vec::new();
    let mut seen_families = BTreeSet::<String>::new();
    let reserve_limit = SETUP_VARIANT_DOCUMENT_CAP.min(top_k.saturating_sub(1));
    for (index, chunk) in
        indexed.iter().filter(|(_, chunk)| chunk_is_setup_focus_command_path_anchor(chunk))
    {
        let family =
            setup_variant_anchor_family(&chunk.document_label, &query_terms, &family_model);
        if seen_families.insert(family) {
            selected.push((*index, chunk.clone()));
            if selected.len() >= reserve_limit {
                break;
            }
        }
    }
    if selected.len() < 2 { Vec::new() } else { selected }
}

fn query_ir_requests_setup_variant_anchor_reservation(query_ir: &QueryIR) -> bool {
    matches!(query_ir.act, QueryAct::ConfigureHow | QueryAct::Describe)
        && query_ir.document_focus.is_some()
        && query_ir.source_slice.is_none()
        && query_ir.literal_constraints.is_empty()
        && !query_ir_requests_versioned_update_procedure_context("", query_ir)
}

#[derive(Default)]
struct SetupVariantAnchorFamilyModel {
    label_count: usize,
    token_frequency: BTreeMap<String, usize>,
}

fn setup_variant_anchor_family_model(
    labels: &[&str],
    query_terms: &BTreeSet<String>,
) -> SetupVariantAnchorFamilyModel {
    let mut token_frequency = BTreeMap::<String, usize>::new();
    let mut label_count = 0usize;
    for label in labels {
        let tokens = setup_variant_anchor_family_tokens(label, query_terms);
        if tokens.is_empty() {
            continue;
        }
        label_count = label_count.saturating_add(1);
        for token in tokens {
            *token_frequency.entry(token).or_default() += 1;
        }
    }
    SetupVariantAnchorFamilyModel { label_count, token_frequency }
}

fn setup_variant_anchor_family(
    label: &str,
    query_terms: &BTreeSet<String>,
    family_model: &SetupVariantAnchorFamilyModel,
) -> String {
    let tokens = setup_variant_anchor_family_tokens(label, query_terms);
    if let Some(token) = tokens.iter().find(|token| {
        family_model
            .token_frequency
            .get(*token)
            .is_some_and(|frequency| *frequency < family_model.label_count)
    }) {
        return token.clone();
    }
    if !tokens.is_empty() {
        return tokens.join(" ");
    }
    label.to_lowercase()
}

fn setup_variant_anchor_family_tokens(label: &str, query_terms: &BTreeSet<String>) -> Vec<String> {
    normalized_alnum_tokens(label, 3)
        .into_iter()
        .filter(|token| !query_terms.iter().any(|query_token| soft_token_match(query_token, token)))
        .collect()
}

fn ensure_setup_variant_anchors_retained(
    selected: &mut Vec<(usize, RuntimeMatchedChunk)>,
    anchors: Vec<(usize, RuntimeMatchedChunk)>,
    top_k: usize,
) {
    if top_k == 0 || anchors.is_empty() {
        return;
    }
    for anchor in anchors {
        if selected.iter().any(|(_, chunk)| chunk.chunk_id == anchor.1.chunk_id) {
            continue;
        }
        if selected.len() < top_k {
            selected.push(anchor);
            continue;
        }
        let evict_position = selected
            .iter()
            .rposition(|(_, chunk)| {
                !chunk_is_setup_focus_command_path_anchor(chunk)
                    && chunk.score_kind != RuntimeChunkScoreKind::FocusedDocument
            })
            .or_else(|| {
                selected
                    .iter()
                    .rposition(|(_, chunk)| !chunk_is_setup_focus_command_path_anchor(chunk))
            });
        if let Some(position) = evict_position {
            let anchor_chunk_id = anchor.1.chunk_id;
            selected[position] = anchor;
            tracing::info!(
                stage = "retrieval.setup_variant_anchor_reserved",
                chunk_id = %anchor_chunk_id,
                "setup variant anchor reserved past score-ordered truncation"
            );
        }
    }
}

fn ensure_exact_literal_context_anchors_retained(
    selected: &mut Vec<(usize, RuntimeMatchedChunk)>,
    anchors: Vec<(usize, RuntimeMatchedChunk)>,
    top_k: usize,
) {
    if top_k == 0 || anchors.is_empty() {
        return;
    }
    let protected_anchor_ids =
        anchors.iter().map(|(_, chunk)| chunk.chunk_id).collect::<BTreeSet<_>>();
    for anchor in anchors {
        if selected.iter().any(|(_, chunk)| chunk.chunk_id == anchor.1.chunk_id) {
            continue;
        }
        if selected.len() < top_k {
            selected.push(anchor);
            continue;
        }
        let evict_position = selected
            .iter()
            .rposition(|(_, chunk)| {
                !protected_anchor_ids.contains(&chunk.chunk_id)
                    && chunk.score_kind != RuntimeChunkScoreKind::LatestVersion
                    && structural_literal_inventory_score(chunk) == 0
            })
            .or_else(|| {
                selected.iter().rposition(|(_, chunk)| {
                    !protected_anchor_ids.contains(&chunk.chunk_id)
                        && !matches!(
                            chunk.score_kind,
                            RuntimeChunkScoreKind::LatestVersion
                                | RuntimeChunkScoreKind::FocusedDocument
                                | RuntimeChunkScoreKind::SourceContext
                        )
                })
            })
            .or_else(|| {
                selected.iter().rposition(|(_, chunk)| {
                    !protected_anchor_ids.contains(&chunk.chunk_id)
                        && chunk.score_kind != RuntimeChunkScoreKind::LatestVersion
                })
            });
        if let Some(position) = evict_position {
            let anchor_chunk_id = anchor.1.chunk_id;
            selected[position] = anchor;
            tracing::info!(
                stage = "retrieval.exact_literal_context_anchor_reserved",
                chunk_id = %anchor_chunk_id,
                "focus-aligned exact literal context anchor reserved past score-ordered truncation"
            );
        }
    }
}

fn ensure_content_anchor_chunks_retained(
    selected: &mut Vec<(usize, RuntimeMatchedChunk)>,
    anchors: Vec<(usize, RuntimeMatchedChunk)>,
    top_k: usize,
) {
    if top_k == 0 || anchors.is_empty() {
        return;
    }
    let protected_anchor_ids =
        anchors.iter().map(|(_, chunk)| chunk.chunk_id).collect::<BTreeSet<_>>();
    for anchor in anchors {
        if selected.iter().any(|(_, chunk)| chunk.chunk_id == anchor.1.chunk_id) {
            continue;
        }
        if selected.len() < top_k {
            selected.push(anchor);
            continue;
        }
        let evict_position = selected
            .iter()
            .rposition(|(_, chunk)| {
                !protected_anchor_ids.contains(&chunk.chunk_id)
                    && !matches!(
                        chunk.score_kind,
                        RuntimeChunkScoreKind::ContentAnchor
                            | RuntimeChunkScoreKind::LatestVersion
                            | RuntimeChunkScoreKind::FocusedDocument
                            | RuntimeChunkScoreKind::SourceContext
                    )
            })
            .or_else(|| {
                selected.iter().rposition(|(_, chunk)| {
                    !protected_anchor_ids.contains(&chunk.chunk_id)
                        && chunk.score_kind != RuntimeChunkScoreKind::ContentAnchor
                })
            });
        if let Some(position) = evict_position {
            let anchor_chunk_id = anchor.1.chunk_id;
            selected[position] = anchor;
            tracing::info!(
                stage = "retrieval.content_anchor_reserved",
                chunk_id = %anchor_chunk_id,
                "content-anchor evidence reserved past score-ordered truncation"
            );
        }
    }
}

/// Guarantee the reserved setup anchor is present in the truncated selection.
/// If the score-ordered truncation already kept it, this is a no-op; otherwise
/// the anchor displaces the lowest-priority non-anchor chunk so the context
/// budget is preserved.
fn ensure_setup_focus_anchor_retained(
    selected: &mut Vec<(usize, RuntimeMatchedChunk)>,
    anchor: (usize, RuntimeMatchedChunk),
    top_k: usize,
) {
    if top_k == 0 || selected.iter().any(|(_, chunk)| chunk.chunk_id == anchor.1.chunk_id) {
        return;
    }
    let evict_position =
        selected.iter().rposition(|(_, chunk)| !chunk_is_setup_focus_command_path_anchor(chunk));
    let anchor_chunk_id = anchor.1.chunk_id;
    match evict_position {
        Some(position) if selected.len() >= top_k => {
            selected[position] = anchor;
        }
        _ => selected.push(anchor),
    }
    tracing::info!(
        stage = "retrieval.setup_focus_anchor_reserved",
        chunk_id = %anchor_chunk_id,
        "focused-document setup anchor reserved past score-ordered truncation"
    );
}

fn ensure_versioned_update_procedure_runbook_anchor_retained(
    selected: &mut Vec<(usize, RuntimeMatchedChunk)>,
    anchor: (usize, RuntimeMatchedChunk),
    top_k: usize,
) {
    if top_k == 0 || selected.iter().any(|(_, chunk)| chunk.chunk_id == anchor.1.chunk_id) {
        return;
    }
    let evict_position = selected
        .iter()
        .rposition(|(_, chunk)| chunk.score_kind != RuntimeChunkScoreKind::LatestVersion);
    let anchor_chunk_id = anchor.1.chunk_id;
    match evict_position {
        Some(position) if selected.len() >= top_k => {
            selected[position] = anchor;
        }
        _ => selected.push(anchor),
    }
    tracing::info!(
        stage = "retrieval.versioned_update_procedure_runbook_anchor_reserved",
        chunk_id = %anchor_chunk_id,
        "versioned update runbook anchor reserved past score-ordered truncation"
    );
}

fn ensure_versioned_update_procedure_runbook_chunk_retained(
    selected: &mut Vec<RuntimeMatchedChunk>,
    anchor: RuntimeMatchedChunk,
    top_k: usize,
) {
    if top_k == 0 || selected.iter().any(|chunk| chunk.chunk_id == anchor.chunk_id) {
        return;
    }
    let evict_position =
        selected.iter().rposition(|chunk| chunk.score_kind != RuntimeChunkScoreKind::LatestVersion);
    let anchor_chunk_id = anchor.chunk_id;
    match evict_position {
        Some(position) if selected.len() >= top_k => {
            selected[position] = anchor;
        }
        _ => selected.push(anchor),
    }
    tracing::info!(
        stage = "retrieval.versioned_update_procedure_runbook_anchor_reserved",
        chunk_id = %anchor_chunk_id,
        "versioned update runbook anchor reserved during procedure merge truncation"
    );
}

fn source_context_reservation_count(
    chunks: &[(usize, RuntimeMatchedChunk)],
    top_k: usize,
    query_ir: &QueryIR,
) -> Option<usize> {
    let intents = classify_query_ir_intents(query_ir);
    let requests_error_code_context = has_question_intent(&intents, QuestionIntent::ErrorCode);
    let requests_transport_context = has_question_intent(&intents, QuestionIntent::Port)
        || has_question_intent(&intents, QuestionIntent::Protocol)
        || query_ir
            .target_types
            .iter()
            .any(|target_type| target_type.trim().eq_ignore_ascii_case("connection"));
    let requests_configuration_context = has_question_intent(&intents, QuestionIntent::ConfigKey)
        || has_question_intent(&intents, QuestionIntent::Parameter)
        || query_ir.target_types.iter().any(|target_type| {
            matches!(
                canonical_target_type_tag(target_type).as_str(),
                "configuration_file" | "config_key" | "parameter"
            )
        });
    let requests_table_context = query_ir_requests_table_source_context_reservation(query_ir);
    let requests_structured_fallback_context =
        query_ir_requests_structured_source_context_reservation(query_ir);
    let reserves_source_context = query_ir.is_exact_literal_technical()
        || requests_error_code_context
        || requests_transport_context
        || requests_configuration_context
        || requests_table_context
        || requests_structured_fallback_context;
    if top_k < 2 || !reserves_source_context {
        return None;
    }
    let source_context_count = chunks
        .iter()
        .filter(|(_, chunk)| chunk.score_kind == RuntimeChunkScoreKind::SourceContext)
        .count();
    if source_context_count == 0 {
        return None;
    }
    let max_reserved = if requests_error_code_context {
        2
    } else if requests_transport_context {
        8
    } else if requests_table_context || requests_structured_fallback_context {
        top_k.saturating_sub(4).clamp(8, 28)
    } else if requests_configuration_context {
        top_k.saturating_sub(2).clamp(4, 12)
    } else {
        4
    };
    Some(source_context_count.min(top_k.saturating_sub(1)).min(max_reserved))
}

fn query_ir_requests_table_source_context_reservation(query_ir: &QueryIR) -> bool {
    if query_ir.source_slice.is_some() || !matches!(query_ir.scope, QueryScope::SingleDocument) {
        return false;
    }
    let target_types = query_ir
        .target_types
        .iter()
        .map(|target_type| canonical_target_type_tag(target_type))
        .collect::<HashSet<_>>();
    target_types.contains("table_row") && target_types.contains("table_summary")
}

fn query_ir_requests_structured_source_context_reservation(query_ir: &QueryIR) -> bool {
    query_ir.confidence <= 0.3
        && matches!(query_ir.scope, QueryScope::SingleDocument)
        && matches!(query_ir.act, QueryAct::Describe | QueryAct::ConfigureHow)
        && query_ir.source_slice.is_none()
        && query_ir.document_focus.is_none()
        && query_ir.target_types.is_empty()
        && query_ir.literal_constraints.is_empty()
}

fn truncate_chunks_with_source_context_reservation(
    chunks: &[(usize, RuntimeMatchedChunk)],
    top_k: usize,
    reserved_count: usize,
) -> Vec<(usize, RuntimeMatchedChunk)> {
    if top_k == 0 {
        return Vec::new();
    }
    let mut selected = Vec::with_capacity(top_k);
    let mut selected_indices = HashSet::new();
    let mut selected_source_context_count = 0usize;

    for (index, chunk) in
        chunks.iter().filter(|(_, chunk)| chunk.score_kind == RuntimeChunkScoreKind::SourceContext)
    {
        if selected.len() >= reserved_count {
            break;
        }
        selected_indices.insert(*index);
        selected.push((*index, chunk.clone()));
        selected_source_context_count = selected_source_context_count.saturating_add(1);
    }

    for (index, chunk) in chunks {
        if selected.len() >= top_k {
            break;
        }
        if chunk.score_kind == RuntimeChunkScoreKind::SourceContext
            && selected_source_context_count >= reserved_count
        {
            continue;
        }
        if selected_indices.insert(*index) {
            if chunk.score_kind == RuntimeChunkScoreKind::SourceContext {
                selected_source_context_count = selected_source_context_count.saturating_add(1);
            }
            selected.push((*index, chunk.clone()));
        }
    }
    selected
}

fn truncate_chunks_for_multi_document_scope(
    chunks: &[(usize, RuntimeMatchedChunk)],
    top_k: usize,
    query_ir: &QueryIR,
) -> Vec<(usize, RuntimeMatchedChunk)> {
    if top_k == 0 || chunks.is_empty() {
        return Vec::new();
    }

    let mut selected = Vec::with_capacity(top_k);
    let mut selected_indices = HashSet::new();
    let relevant_documents = multi_document_relevant_document_ids(chunks, query_ir, top_k);

    for document_id in relevant_documents {
        let next_chunk = chunks
            .iter()
            .find(|(_, chunk)| chunk.document_id == document_id)
            .map(|(index, chunk)| (*index, chunk.clone()));
        if let Some((index, chunk)) = next_chunk {
            selected_indices.insert(index);
            selected.push((index, chunk));
        }
        if selected.len() >= top_k {
            return selected;
        }
    }

    for (index, chunk) in chunks {
        if selected.len() >= top_k {
            break;
        }
        if selected_indices.insert(*index) {
            selected.push((*index, chunk.clone()));
        }
    }
    selected
}

fn multi_document_relevant_document_ids(
    chunks: &[(usize, RuntimeMatchedChunk)],
    query_ir: &QueryIR,
    top_k: usize,
) -> Vec<Uuid> {
    let relevance_terms = query_multi_document_relevance_terms(query_ir);
    let mut relevant_documents = Vec::new();
    let mut scored_documents = HashSet::new();
    for (_, chunk) in chunks {
        let document_id = chunk.document_id;
        if scored_documents.insert(document_id)
            && query_chunk_relevance_score(chunk, &relevance_terms) > 0
        {
            relevant_documents.push(document_id);
            if relevant_documents.len() >= top_k {
                return relevant_documents;
            }
        }
    }

    if relevant_documents.len() < 2 && matches!(query_ir.act, QueryAct::Compare) {
        let mut selected_documents = relevant_documents.iter().copied().collect::<HashSet<_>>();
        for (_, chunk) in chunks {
            let document_id = chunk.document_id;
            if selected_documents.insert(document_id) {
                relevant_documents.push(document_id);
            }
            if relevant_documents.len() >= 2 {
                break;
            }
        }
    }

    if relevant_documents.is_empty() {
        let mut selected_documents = HashSet::new();
        for (_, chunk) in chunks {
            let document_id = chunk.document_id;
            if selected_documents.insert(document_id) {
                relevant_documents.push(document_id);
            }
            if relevant_documents.len() >= top_k {
                break;
            }
        }
    }

    if relevant_documents.len() > top_k {
        relevant_documents.truncate(top_k);
    }
    relevant_documents
}

fn query_chunk_relevance_score(chunk: &RuntimeMatchedChunk, terms: &[String]) -> usize {
    if terms.is_empty() {
        return 0;
    }

    let haystack =
        format!("{} {} {}", chunk.document_label, chunk.excerpt, chunk.source_text).to_lowercase();
    terms.iter().filter(|term| haystack.contains(term.as_str())).count()
}

fn query_multi_document_relevance_terms(query_ir: &QueryIR) -> Vec<String> {
    let mut terms = BTreeSet::new();
    let push_terms = |value: &str, terms: &mut BTreeSet<String>| {
        for token in normalized_alnum_tokens(value, 3) {
            if !token.is_empty() {
                terms.insert(token);
            }
        }
    };

    for entity in &query_ir.target_entities {
        push_terms(&entity.label, &mut terms);
    }
    if let Some(comparison) = &query_ir.comparison {
        if let Some(value) = &comparison.a {
            push_terms(value, &mut terms);
        }
        if let Some(value) = &comparison.b {
            push_terms(value, &mut terms);
        }
        push_terms(&comparison.dimension, &mut terms);
    }
    if let Some(document_focus) = &query_ir.document_focus {
        push_terms(&document_focus.hint, &mut terms);
    }
    for literal in &query_ir.literal_constraints {
        push_terms(&literal.text, &mut terms);
    }
    for target_type in &query_ir.target_types {
        push_terms(target_type, &mut terms);
    }

    terms.into_iter().collect()
}

pub(crate) fn excerpt_for(content: &str, max_chars: usize) -> String {
    let trimmed = content.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }

    let excerpt = trimmed.chars().take(max_chars).collect::<String>();
    format!("{excerpt}...")
}

pub(crate) fn focused_excerpt_for(content: &str, keywords: &[String], max_chars: usize) -> String {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let lines = trimmed.lines().map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>();
    if lines.is_empty() {
        return String::new();
    }

    let normalized_keywords = keywords
        .iter()
        .map(|keyword| keyword.trim())
        .filter(|keyword| keyword.chars().count() >= 3)
        .map(|keyword| keyword.to_lowercase())
        .collect::<Vec<_>>();
    if normalized_keywords.is_empty() {
        return excerpt_for(trimmed, max_chars);
    }

    let mut best_index = None;
    let mut best_score = 0usize;
    for (index, line) in lines.iter().enumerate() {
        let lowered = line.to_lowercase();
        let score = normalized_keywords
            .iter()
            .filter(|keyword| lowered.contains(keyword.as_str()))
            .map(|keyword| keyword.chars().count().min(24))
            .sum::<usize>();
        if score > best_score {
            best_score = score;
            best_index = Some(index);
        }
    }

    let Some(center_index) = best_index else {
        return excerpt_for(trimmed, max_chars);
    };
    if best_score == 0 {
        return excerpt_for(trimmed, max_chars);
    }

    let max_focus_lines = 5usize;
    let mut selected = BTreeSet::from([center_index]);
    let mut radius = 1usize;
    loop {
        let excerpt =
            selected.iter().copied().map(|index| lines[index]).collect::<Vec<_>>().join(" ");
        if excerpt.chars().count() >= max_chars
            || selected.len() >= max_focus_lines
            || selected.len() == lines.len()
        {
            return excerpt_for(&excerpt, max_chars);
        }

        let mut expanded = false;
        if center_index >= radius {
            expanded |= selected.insert(center_index - radius);
        }
        if center_index + radius < lines.len() {
            expanded |= selected.insert(center_index + radius);
        }
        if !expanded {
            return excerpt_for(&excerpt, max_chars);
        }
        radius += 1;
    }
}

pub(crate) fn command_dense_excerpt_for(content: &str, max_chars: usize) -> Option<String> {
    if !content_is_command_dense(content) {
        return None;
    }
    let repaired = repair_technical_layout_noise(content);
    let excerpt = excerpt_for(&repaired, max_chars);
    (!excerpt.trim().is_empty()).then_some(excerpt)
}

fn content_is_command_dense(content: &str) -> bool {
    let mut non_empty_lines = 0usize;
    let mut command_lines = 0usize;
    let mut artifact_lines = 0usize;
    for line in content.lines().map(str::trim).filter(|line| !line.is_empty()) {
        non_empty_lines = non_empty_lines.saturating_add(1);
        if versioned_update_procedure_line_has_command_start(line) {
            command_lines = command_lines.saturating_add(1);
        }
        if procedure_artifact_token_count(line) > 0 {
            artifact_lines = artifact_lines.saturating_add(1);
        }
    }
    command_lines >= 2
        || (command_lines >= 1 && artifact_lines >= 2)
        || (non_empty_lines >= 3 && command_lines >= 1 && artifact_lines >= 1)
}

#[cfg(test)]
#[path = "retrieve_tests.rs"]
mod tests;
