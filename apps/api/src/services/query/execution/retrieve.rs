use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use anyhow::Context;
use chrono::{DateTime, Datelike, Utc};
use futures::{StreamExt, future::join_all, stream};
use uuid::Uuid;

use super::chunk_support::chunk_answer_source_text;
pub(crate) use super::chunk_support::{
    canonical_document_revision_id, chunk_is_setup_focus_command_path_anchor,
    command_dense_excerpt_for, excerpt_for, focused_excerpt_for, map_chunk_hit,
};
use super::command_shape::{
    procedure_artifact_token_count,
    procedure_line_has_command_start as versioned_update_procedure_line_has_command_start,
    shellish_inline_token_starts_command, shellish_token_has_artifact_preparation_signal,
    shellish_token_has_external_artifact, shellish_token_is_local_artifact,
    shellish_tokens_from_text, strip_leading_procedure_order_marker,
};
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
    merge_canonical_table_aggregation_chunks, query_ir_requests_table_section_siblings,
    query_relevant_graph_evidence_target_hits, question_asks_table_aggregation,
    requested_initial_table_row_count, resolve_scoped_target_document_ids,
};
use crate::{
    app::state::AppState,
    domains::{
        ai::AiBindingPurpose,
        content::{attachment_parent_page_id, revision_text_state_is_readable, source_page_id},
        query::RuntimeQueryMode,
        query_ir::{
            EntityMention, EntityRole, LiteralKind, QueryAct, QueryIR, QueryScope, QueryTargetKind,
            TemporalConstraint, literal_kind_has_exact_technical_shape,
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

/// Ceiling on entity-bio fan-out chunks so vector and lexical hits stay within
/// the context window for entities that appear across dozens of documents.
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
const VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT: usize = 8;
const VERSIONED_UPDATE_PROCEDURE_MATCH_PROBE_CHUNKS_PER_DOCUMENT: usize =
    VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT * 4;
const VERSIONED_UPDATE_PROCEDURE_CONTEXT_BACKWARD_CHUNKS: i32 = 1;
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
const VERSIONED_UPDATE_PROCEDURE_EVIDENCE_SEARCH_QUERY_CAP: usize = 6;
const VERSIONED_UPDATE_PROCEDURE_EVIDENCE_SEARCH_HIT_LIMIT: usize = 96;
const VERSIONED_UPDATE_PROCEDURE_SUBJECT_TITLE_RESERVE_CAP: usize = 2;
const VERSIONED_UPDATE_PROCEDURE_EVIDENCE_STRUCTURAL_SCORE_FLOOR: usize = 3;
const VERSIONED_UPDATE_PROCEDURE_COMMAND_SIGNAL_SCORE_CAP: usize = 8;
const SETUP_FOCUS_CONFIG_PATH_EXTENSIONS: [&str; 8] =
    [".conf", ".ini", ".cfg", ".properties", ".yaml", ".yml", ".toml", ".json"];
const RAW_SETUP_FOCUS_CANDIDATE_TOKEN_MAX_DOC_FREQUENCY_FLOOR: usize = 8;
const RAW_SETUP_FOCUS_CANDIDATE_TOKEN_MAX_DOC_FREQUENCY_CAP: usize = 512;
const RAW_SETUP_FOCUS_CANDIDATE_TOKEN_DOCUMENT_DIVISOR: usize = 50;
const RAW_SETUP_FOCUS_STRUCTURAL_DOCUMENT_SCORE_FLOOR: usize = 24;
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

pub(crate) fn entity_bio_target_mentions(query_ir: &QueryIR) -> Vec<&EntityMention> {
    query_ir.target_entities.iter().filter(|mention| !mention.label.trim().is_empty()).collect()
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

async fn entity_bio_evidence_chunk_ids(
    state: &AppState,
    library_id: Uuid,
    projection_version: i64,
    target_mentions: &[&EntityMention],
) -> anyhow::Result<(Vec<Uuid>, usize)> {
    let mut seen_nodes = HashSet::new();
    let mut evidence_targets = Vec::new();
    for mention in target_mentions {
        let nodes = repositories::search_admitted_runtime_graph_entities_by_query_text(
            &state.persistence.postgres,
            library_id,
            projection_version,
            &mention.label,
            4,
        )
        .await
        .context("failed to search graph entities by label for entity-bio retrieval")?;
        for node in nodes {
            if seen_nodes.insert(node.id) {
                evidence_targets.push(("node".to_string(), node.id));
            }
        }
    }
    if evidence_targets.is_empty() {
        return Ok((Vec::new(), seen_nodes.len()));
    }
    let evidence_limit =
        evidence_targets.len().saturating_mul(ENTITY_BIO_CHUNK_CAP).min(i64::MAX as usize) as i64;
    let evidence = repositories::list_runtime_graph_evidence_by_targets(
        &state.persistence.postgres,
        library_id,
        &evidence_targets,
        evidence_limit,
    )
    .await
    .context("failed to list graph evidence for entity-bio retrieval")?;
    let mut chunk_ids = Vec::new();
    for chunk_id in evidence.into_iter().filter_map(|row| row.chunk_id) {
        if chunk_ids.len() >= ENTITY_BIO_CHUNK_CAP {
            break;
        }
        if !chunk_ids.contains(&chunk_id) {
            chunk_ids.push(chunk_id);
        }
    }
    Ok((chunk_ids, seen_nodes.len()))
}

async fn entity_bio_lexical_chunk_ids(
    state: &AppState,
    library_id: Uuid,
    target_mentions: &[&EntityMention],
    evidence_chunk_ids: &[Uuid],
) -> anyhow::Result<Vec<Uuid>> {
    let mut lexical_chunk_ids = Vec::new();
    for mention in target_mentions {
        let remaining =
            ENTITY_BIO_CHUNK_CAP.saturating_sub(evidence_chunk_ids.len() + lexical_chunk_ids.len());
        if remaining == 0 {
            break;
        }
        let hits = state
            .search_store
            .search_chunks(library_id, mention.label.trim(), remaining.max(4), None, None)
            .await
            .context("failed to run lexical entity-label search for entity-bio retrieval")?;
        for hit in hits {
            if lexical_chunk_ids.len() + evidence_chunk_ids.len() >= ENTITY_BIO_CHUNK_CAP {
                break;
            }
            if !evidence_chunk_ids.contains(&hit.chunk_id)
                && !lexical_chunk_ids.contains(&hit.chunk_id)
            {
                lexical_chunk_ids.push(hit.chunk_id);
            }
        }
    }
    Ok(lexical_chunk_ids)
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

    // The compiler already decided which spans are entities. Script and
    // capitalization carry no additional domain-neutral meaning here.
    let target_mentions = entity_bio_target_mentions(ir);
    if target_mentions.is_empty() {
        return Ok(Vec::new());
    }

    let (all_evidence_chunk_ids, evidence_node_count) = entity_bio_evidence_chunk_ids(
        state,
        library_id,
        snapshot.projection_version,
        &target_mentions,
    )
    .await?;

    // Graph-evidence is bounded by what the `extract_graph` stage
    // captured — low-confidence or oblique-case mentions often miss
    // that pass. Complement the graph lookup with a dedicated lexical
    // search over the entity label itself so every chunk where the
    // label appears as plain text contributes, not just the ones that
    // became evidence rows.
    let lexical_chunk_ids =
        entity_bio_lexical_chunk_ids(state, library_id, &target_mentions, &all_evidence_chunk_ids)
            .await?;

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
    let label_tokens: Vec<String> = target_mentions
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
        evidence_node_count,
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
    let db_text_queries = graph_evidence_db_text_queries(&text_queries, query_ir);
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
        target_entity_profiles,
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

fn graph_evidence_context_candidates(
    text_evidence: &[repositories::RuntimeGraphEvidenceRow],
    target_evidence: &[repositories::RuntimeGraphEvidenceRow],
    graph_index: &QueryGraphIndex,
) -> Vec<GraphEvidenceContextCandidate> {
    let mut candidates = Vec::new();
    let mut seen_row_ids = BTreeSet::new();
    for (source_ordinal, rows) in [text_evidence, target_evidence].into_iter().enumerate() {
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
    candidates
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
    let mut candidates =
        graph_evidence_context_candidates(text_evidence, target_evidence, graph_index);

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

fn push_graph_evidence_source_field(
    fields: &mut Vec<GraphEvidenceContextCandidateField>,
    row: &repositories::RuntimeGraphEvidenceRow,
) {
    let source_parts = [row.source_file_name.as_deref(), row.page_ref.as_deref()]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if !source_parts.is_empty() {
        push_graph_evidence_context_candidate_field(
            fields,
            source_parts.join(" "),
            GRAPH_EVIDENCE_CONTEXT_SOURCE_FIELD_WEIGHT,
            GraphTargetEntityCoverageFieldKind::Summary,
        );
    }
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
    push_graph_evidence_source_field(&mut fields, row);
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

fn graph_evidence_focus_relevance_score(
    candidate_fields: &[GraphEvidenceContextCandidateField],
    focus_text: &str,
    tokens: &BTreeSet<String>,
    token_frequencies: &HashMap<String, usize>,
    candidate_count: usize,
    weight: usize,
) -> usize {
    let overlapping = tokens
        .iter()
        .filter_map(|token| {
            let field_weight = candidate_fields
                .iter()
                .filter(|field| field.tokens.contains(token))
                .map(|field| field.weight)
                .max()
                .unwrap_or_default();
            (field_weight > 0).then(|| {
                let frequency = token_frequencies.get(token).copied().unwrap_or(candidate_count);
                candidate_count
                    .saturating_sub(frequency)
                    .saturating_add(1)
                    .saturating_mul(field_weight)
            })
        })
        .collect::<Vec<_>>();
    if overlapping.is_empty() {
        return 0;
    }
    let overlap_score = overlapping.iter().sum::<usize>();
    let full_overlap_bonus = if overlapping.len() == tokens.len() {
        16usize.saturating_mul(weight).saturating_add(overlap_score)
    } else {
        0
    };
    let sequence_score = candidate_fields
        .iter()
        .filter(|field| {
            token_sequence_contains(
                &field.text,
                focus_text,
                GRAPH_EVIDENCE_CONTEXT_SCORE_TOKEN_MIN_CHARS,
            )
        })
        .map(|field| 32usize.saturating_mul(weight).saturating_mul(field.weight))
        .sum::<usize>();
    overlap_score
        .saturating_mul(weight)
        .saturating_add(full_overlap_bonus)
        .saturating_add(sequence_score)
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

    let score = focus_tokens
        .iter()
        .enumerate()
        .map(|(ordinal, (focus_text, tokens))| {
            graph_evidence_focus_relevance_score(
                candidate_fields,
                focus_text,
                tokens,
                token_frequencies,
                candidate_count,
                focus_tokens.len().saturating_sub(ordinal).max(1),
            )
        })
        .sum::<usize>();
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

fn collect_content_anchor_query_ir_values(values: &mut Vec<String>, query_ir: &QueryIR) {
    query_ir
        .retrieval_query
        .as_deref()
        .into_iter()
        .chain(query_ir.document_focus.as_ref().map(|focus| focus.hint.as_str()))
        .chain(query_ir.target_entities.iter().map(|entity| entity.label.as_str()))
        .chain(query_ir.literal_constraints.iter().map(|literal| literal.text.as_str()))
        .for_each(|value| collect_content_anchor_text_value(values, value));
}

fn content_anchor_focus(values: &[String]) -> (BTreeSet<String>, Vec<Vec<String>>) {
    let mut focus_tokens = BTreeSet::new();
    let mut phrase_sequences = Vec::new();
    let mut seen_sequences = BTreeSet::new();
    for value in values {
        focus_tokens.extend(normalized_alnum_tokens(value, CONTENT_ANCHOR_TOKEN_MIN_CHARS));
        for sequence in quoted_content_anchor_sequences(value)
            .into_iter()
            .chain(adjacent_content_anchor_sequences(value))
        {
            push_content_anchor_sequence(&mut phrase_sequences, &mut seen_sequences, sequence);
        }
    }
    (focus_tokens, phrase_sequences)
}

impl RetrievalContentAnchorModel {
    pub(crate) fn new(question: &str, query_ir: Option<&QueryIR>) -> Self {
        let mut values = Vec::new();
        collect_content_anchor_text_value(&mut values, question);
        if let Some(query_ir) = query_ir {
            collect_content_anchor_query_ir_values(&mut values, query_ir);
        }
        let (focus_tokens, phrase_sequences) = content_anchor_focus(&values);
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
        .filter(|sequence| prefix_token_sequence_contains_tokens(&text_sequence, sequence, 5))
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
        .filter(|sequence| prefix_token_sequence_contains_tokens(&text_sequence, sequence, 5))
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
    if !query_ir_requests_setup_focus_document_candidates(query_ir) {
        return Ok(Vec::new());
    }
    let mut candidate_document_ids = setup_focus_candidate_document_ids(
        query_ir,
        document_index,
        SETUP_FOCUS_DOCUMENT_CANDIDATE_CAP,
    );
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
        let document_score = setup_focus_document_candidate_score(&selected_rows, query_ir)
            .saturating_add(
                setup_focus_document_label_identity_score(document, query_ir).saturating_mul(4096),
            );
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

fn setup_variant_fetch_inputs(
    candidate_document_ids: &[Uuid],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> Vec<(usize, Uuid, Uuid)> {
    candidate_document_ids
        .iter()
        .take(SETUP_VARIANT_DOCUMENT_FETCH_CAP)
        .enumerate()
        .filter_map(|(document_rank, document_id)| {
            let document = document_index.get(document_id)?;
            canonical_document_revision_id(document)
                .map(|revision_id| (document_rank, *document_id, revision_id))
        })
        .collect()
}

fn append_setup_variant_chunks(
    selected_rows: Vec<KnowledgeChunkRow>,
    selected_document_count: usize,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    chunks: &mut Vec<RuntimeMatchedChunk>,
) {
    for (chunk_rank, row) in
        selected_rows.into_iter().take(SETUP_VARIANT_CHUNKS_PER_DOCUMENT).enumerate()
    {
        let score = setup_variant_document_chunk_score(selected_document_count - 1, chunk_rank);
        if let Some(mut chunk) = map_chunk_hit(row, score, document_index, plan_keywords) {
            chunk.score = Some(score);
            chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
            chunks.push(chunk);
        }
        if chunks.len() >= SETUP_VARIANT_CHUNK_CAP {
            break;
        }
    }
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

    let fetch_inputs = setup_variant_fetch_inputs(&candidate_document_ids, document_index);
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
        append_setup_variant_chunks(
            selected_rows,
            selected_document_count,
            document_index,
            plan_keywords,
            &mut chunks,
        );
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
    query_ir_has_setup_focus_target(query_ir) && query_ir_has_setup_focus_identity(query_ir)
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
    if !query_ir.targets_any(&[
        QueryTargetKind::Artifact,
        QueryTargetKind::Document,
        QueryTargetKind::Entity,
    ]) {
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
    for document in document_index.values().filter(|document| {
        document.document_role == crate::domains::content::DOCUMENT_ROLE_PRIMARY
            && !setup_focus_document_is_standalone_image(document)
    }) {
        let matched = setup_focus_document_identity_values(document)
            .into_iter()
            .flat_map(|value| normalized_alnum_tokens(&value, 3))
            .flat_map(|value_token| {
                query_terms
                    .iter()
                    .filter(move |query_term| soft_token_match(query_term, &value_token))
                    .cloned()
            })
            .collect::<BTreeSet<_>>();
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

#[derive(Clone)]
struct VersionedUpdateProcedureFocusModel {
    focus_terms: BTreeSet<String>,
    target_identity_sequences: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Copy)]
struct VersionedUpdateProcedureChunkEvidence {
    target_identity_score: usize,
    label_has_target_identity: bool,
    ordered_step_score: usize,
    command_score: usize,
    version_transition_score: usize,
    structural_score: usize,
    score: usize,
    has_setup_script_signature: bool,
}

fn versioned_update_title_candidate_ids(
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    focus_model: &VersionedUpdateProcedureFocusModel,
) -> Vec<Uuid> {
    let mut candidates = document_index
        .values()
        .filter(|document| {
            document.document_role == crate::domains::content::DOCUMENT_ROLE_PRIMARY
                && !setup_focus_document_is_standalone_image(document)
        })
        .filter_map(|document| {
            let label = document.title.as_deref().unwrap_or(&document.external_key);
            versioned_update_procedure_text_has_target_identity_sequence(label, focus_model)
                .then_some((label.to_lowercase(), document.document_id))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    candidates.into_iter().map(|(_, document_id)| document_id).collect()
}

async fn versioned_update_discovered_document_ids(
    state: &AppState,
    library_id: Uuid,
    focus_model: &VersionedUpdateProcedureFocusModel,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    focus_terms: &[String],
) -> anyhow::Result<Vec<Uuid>> {
    let mut discovered_hits = Vec::new();
    for target_sequence in focus_model
        .target_identity_sequences
        .iter()
        .take(VERSIONED_UPDATE_PROCEDURE_EVIDENCE_SEARCH_QUERY_CAP)
    {
        let search_query = target_sequence.join(" ");
        if search_query.is_empty() {
            continue;
        }
        match state
            .search_store
            .search_chunks(
                library_id,
                &search_query,
                VERSIONED_UPDATE_PROCEDURE_EVIDENCE_SEARCH_HIT_LIMIT,
                None,
                None,
            )
            .await
        {
            Ok(rows) => {
                discovered_hits.extend(rows.into_iter().map(|row| (row.chunk_id, row.score as f32)))
            }
            Err(error) => tracing::warn!(
                stage = "retrieval.versioned_update_procedure_identity_probe_failed",
                error = %error,
                retrieval_degraded = true,
                "typed procedure identity probe failed; exact-title candidates remain available"
            ),
        }
    }
    let chunks =
        batch_hydrate_hits(state, discovered_hits, document_index, focus_terms, &BTreeSet::new())
            .await
            .context("failed to hydrate typed procedure identity probes")?;
    Ok(chunks
        .into_iter()
        .filter(|chunk| {
            let text = format!("{}\n{}", chunk.document_label, chunk.source_text);
            versioned_update_procedure_text_has_target_identity_sequence(&text, focus_model)
        })
        .map(|chunk| chunk.document_id)
        .collect())
}

fn versioned_update_fetch_inputs(
    document_ids: &[Uuid],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> Vec<(usize, Uuid, Uuid)> {
    document_ids
        .iter()
        .enumerate()
        .filter_map(|(rank, document_id)| {
            let revision_id = canonical_document_revision_id(document_index.get(document_id)?)?;
            Some((rank, *document_id, revision_id))
        })
        .collect()
}

fn append_unique_document_ids(
    source: impl IntoIterator<Item = Uuid>,
    seen: &mut BTreeSet<Uuid>,
    destination: &mut Vec<Uuid>,
) {
    destination.extend(source.into_iter().filter(|document_id| seen.insert(*document_id)));
}

async fn load_versioned_update_candidate_rows(
    state: &AppState,
    fetch_inputs: Vec<(usize, Uuid, Uuid)>,
    focus_terms: &[String],
) -> anyhow::Result<Vec<(usize, Uuid, Vec<KnowledgeChunkRow>)>> {
    let fetches = fetch_inputs.into_iter().map(|(document_rank, document_id, revision_id)| {
        let focus_terms = focus_terms.to_vec();
        async move {
            let matching_rows = state
                .document_store
                .list_chunks_by_revision_matching_terms(
                    revision_id,
                    &focus_terms,
                    VERSIONED_UPDATE_PROCEDURE_MATCH_PROBE_CHUNKS_PER_DOCUMENT,
                )
                .await
                .with_context(|| {
                    format!("failed to load typed procedure identity chunks for document {document_id}")
                })?;
            let windows = matching_rows
                .iter()
                .map(|row| {
                    (
                        row.chunk_index
                            .saturating_sub(VERSIONED_UPDATE_PROCEDURE_CONTEXT_BACKWARD_CHUNKS)
                            .max(0),
                        row.chunk_index.saturating_add(3),
                    )
                })
                .collect::<Vec<_>>();
            let context_rows = if windows.is_empty() {
                Vec::new()
            } else {
                state
                    .document_store
                    .list_chunks_by_revision_windows(revision_id, &windows)
                    .await
                    .with_context(|| {
                        format!("failed to load typed procedure context windows for document {document_id}")
                    })?
            };
            let head_rows = state
                .document_store
                .list_chunks_by_revision_range(
                    revision_id,
                    0,
                    VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT.saturating_sub(1) as i32,
                )
                .await
                .with_context(|| {
                    format!("failed to load typed procedure document head for document {document_id}")
                })?;
            Ok::<_, anyhow::Error>((
                document_rank,
                document_id,
                merge_versioned_update_procedure_rows(matching_rows, context_rows, head_rows),
            ))
        }
    });
    stream::iter(fetches)
        .buffer_unordered(VERSIONED_UPDATE_PROCEDURE_DOCUMENT_FETCH_CONCURRENCY)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect()
}

async fn load_versioned_update_procedure_chunks(
    state: &AppState,
    library_id: Uuid,
    _question: &str,
    query_ir: Option<&QueryIR>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    _plan_keywords: &[String],
    temporal_start: Option<DateTime<Utc>>,
    temporal_end: Option<DateTime<Utc>>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let Some(query_ir) = query_ir else {
        return Ok(Vec::new());
    };
    if temporal_start.is_some()
        || temporal_end.is_some()
        || !query_ir_requests_versioned_update_procedure_context("", query_ir)
    {
        return Ok(Vec::new());
    }
    let Some(focus_model) = versioned_update_procedure_focus_model(query_ir) else {
        return Ok(Vec::new());
    };
    let focus_terms = focus_model.focus_terms.iter().cloned().collect::<Vec<_>>();
    let mut candidate_document_ids = Vec::<Uuid>::new();
    let mut seen_document_ids = BTreeSet::<Uuid>::new();
    append_unique_document_ids(
        versioned_update_title_candidate_ids(document_index, &focus_model),
        &mut seen_document_ids,
        &mut candidate_document_ids,
    );
    let discovered_document_ids = versioned_update_discovered_document_ids(
        state,
        library_id,
        &focus_model,
        document_index,
        &focus_terms,
    )
    .await?;
    append_unique_document_ids(
        discovered_document_ids,
        &mut seen_document_ids,
        &mut candidate_document_ids,
    );
    candidate_document_ids.truncate(VERSIONED_UPDATE_PROCEDURE_DOCUMENT_CANDIDATE_CAP);
    if candidate_document_ids.is_empty() {
        return Ok(Vec::new());
    }

    let fetched = load_versioned_update_candidate_rows(
        state,
        versioned_update_fetch_inputs(&candidate_document_ids, document_index),
        &focus_terms,
    )
    .await?;

    let mut chunks = Vec::<RuntimeMatchedChunk>::new();
    for (document_rank, document_id, rows) in fetched {
        let mut mapped = rows
            .into_iter()
            .enumerate()
            .filter_map(|(chunk_rank, row)| {
                let score = versioned_update_procedure_chunk_score(document_rank, chunk_rank);
                map_chunk_hit(row, score, document_index, &focus_terms)
            })
            .collect::<Vec<_>>();
        mapped.sort_by(|left, right| {
            left.chunk_index
                .cmp(&right.chunk_index)
                .then_with(|| left.chunk_id.cmp(&right.chunk_id))
        });
        let Some(first) = mapped.first() else {
            continue;
        };
        let combined_text =
            mapped.iter().map(|chunk| chunk.source_text.as_str()).collect::<Vec<_>>().join("\n");
        let evidence = versioned_update_procedure_text_evidence(
            &first.document_label,
            &combined_text,
            &focus_model,
        );
        if !versioned_update_procedure_evidence_supports_runbook(evidence) {
            continue;
        }
        let label_has_target_identity = evidence.label_has_target_identity;
        for (chunk_rank, mut chunk) in mapped.into_iter().enumerate() {
            chunk.score = Some(
                versioned_update_procedure_chunk_score(document_rank, chunk_rank)
                    + evidence.score.min(65_536) as f32 / 32.0,
            );
            chunk.score_kind = if label_has_target_identity {
                RuntimeChunkScoreKind::DocumentIdentity
            } else {
                RuntimeChunkScoreKind::FocusedDocument
            };
            chunks.push(chunk);
        }
        tracing::debug!(
            stage = "retrieval.versioned_update_procedure_document",
            %document_id,
            structural_score = evidence.structural_score,
            ordered_step_score = evidence.ordered_step_score,
            command_score = evidence.command_score,
            "accepted exact typed-identity procedure document"
        );
    }
    chunks.sort_by(|left, right| {
        score_desc_chunks(left, right)
            .then_with(|| left.document_id.cmp(&right.document_id))
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
    });
    chunks.truncate(VERSIONED_UPDATE_PROCEDURE_CONTEXT_RESERVATION_LIMIT);
    Ok(chunks)
}

fn merge_versioned_update_procedure_rows(
    matching_rows: Vec<KnowledgeChunkRow>,
    context_rows: Vec<KnowledgeChunkRow>,
    head_rows: Vec<KnowledgeChunkRow>,
) -> Vec<KnowledgeChunkRow> {
    let mut rows = BTreeMap::<Uuid, KnowledgeChunkRow>::new();
    for row in matching_rows.into_iter().chain(context_rows).chain(head_rows) {
        rows.entry(row.chunk_id).or_insert(row);
    }
    let mut rows = rows.into_values().collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left.chunk_index.cmp(&right.chunk_index).then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    rows.truncate(VERSIONED_UPDATE_PROCEDURE_MATCH_PROBE_CHUNKS_PER_DOCUMENT);
    rows
}

async fn load_versioned_update_procedure_source_local_runbook_chunks(
    _state: &AppState,
    anchor_chunks: &[RuntimeMatchedChunk],
    _document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    _focus_terms: &[String],
    _question: &str,
    query_ir: Option<&QueryIR>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let Some(query_ir) = query_ir else {
        return Ok(Vec::new());
    };
    if !query_ir_requests_versioned_update_procedure_context("", query_ir) {
        return Ok(Vec::new());
    }
    let mut chunks = anchor_chunks
        .iter()
        .filter_map(|chunk| {
            let score = versioned_update_exact_target_runbook_score("", query_ir, chunk)?;
            let mut chunk = chunk.clone();
            chunk.score = Some(
                versioned_update_procedure_chunk_score(0, 0) + score.min(65_536) as f32 / 32.0,
            );
            chunk.score_kind = RuntimeChunkScoreKind::SourceContext;
            Some(chunk)
        })
        .collect::<Vec<_>>();
    chunks.sort_by(score_desc_chunks);
    chunks.truncate(VERSIONED_UPDATE_PROCEDURE_CONTEXT_RESERVATION_LIMIT);
    Ok(chunks)
}

fn question_requests_versioned_update_procedure_evidence(
    _question: &str,
    query_ir: Option<&QueryIR>,
) -> bool {
    query_ir
        .is_some_and(|query_ir| query_ir_requests_versioned_update_procedure_context("", query_ir))
}

pub(super) fn query_ir_requests_versioned_update_procedure_context(
    _question: &str,
    query_ir: &QueryIR,
) -> bool {
    matches!(query_ir.act, QueryAct::ConfigureHow | QueryAct::Describe)
        && query_ir.source_slice.is_none()
        && query_ir.needs_clarification.is_none()
        && !query_ir_requires_remediation_synthesis(query_ir)
        && !query_ir_has_setup_configuration_target(query_ir)
        && query_ir.targets(QueryTargetKind::Procedure)
        && query_ir.targets_any(&[QueryTargetKind::Version, QueryTargetKind::Release])
        && !query_ir.targets(QueryTargetKind::Concept)
        && versioned_update_procedure_focus_model(query_ir).is_some()
}

fn versioned_update_procedure_focus_model(
    query_ir: &QueryIR,
) -> Option<VersionedUpdateProcedureFocusModel> {
    let mut sequences = Vec::<Vec<String>>::new();
    let mut seen_sequences = BTreeSet::<Vec<String>>::new();
    for label in
        query_ir.target_entities.iter().map(|entity| entity.label.as_str()).chain(
            query_ir.document_focus.iter().map(|document_focus| document_focus.hint.as_str()),
        )
    {
        let sequence = normalized_alnum_token_sequence(label, 1);
        if versioned_update_procedure_named_identity_is_usable(&sequence)
            && seen_sequences.insert(sequence.clone())
        {
            sequences.push(sequence);
        }
    }
    for literal in &query_ir.literal_constraints {
        if !matches!(literal.kind, LiteralKind::Identifier | LiteralKind::Other) {
            continue;
        }
        let sequence = normalized_alnum_token_sequence(&literal.text, 1);
        if !sequence.is_empty() && seen_sequences.insert(sequence.clone()) {
            sequences.push(sequence);
        }
    }
    if sequences.is_empty() {
        return None;
    }
    let focus_terms = sequences.iter().flatten().cloned().collect::<BTreeSet<_>>();
    (!focus_terms.is_empty()).then_some(VersionedUpdateProcedureFocusModel {
        focus_terms,
        target_identity_sequences: sequences,
    })
}

fn versioned_update_procedure_named_identity_is_usable(sequence: &[String]) -> bool {
    sequence.len() >= 2
        && sequence.iter().map(|token| token.chars().count()).sum::<usize>() >= 7
        && sequence.iter().any(|token| token.chars().count() >= 3)
}

fn versioned_update_procedure_text_has_target_identity_sequence(
    text: &str,
    focus_model: &VersionedUpdateProcedureFocusModel,
) -> bool {
    let text_sequence = normalized_alnum_token_sequence(text, 1);
    focus_model
        .target_identity_sequences
        .iter()
        .any(|target_sequence| token_sequence_contains_tokens(&text_sequence, target_sequence))
}

fn versioned_update_procedure_label_has_exact_target_identity(
    label: &str,
    focus_model: &VersionedUpdateProcedureFocusModel,
) -> bool {
    versioned_update_procedure_text_has_target_identity_sequence(label, focus_model)
}

fn versioned_update_procedure_text_evidence(
    document_label: &str,
    text: &str,
    focus_model: &VersionedUpdateProcedureFocusModel,
) -> VersionedUpdateProcedureChunkEvidence {
    let label_has_target_identity =
        versioned_update_procedure_label_has_exact_target_identity(document_label, focus_model);
    let body_has_target_identity =
        versioned_update_procedure_text_has_target_identity_sequence(text, focus_model);
    let target_identity_score = usize::from(label_has_target_identity).saturating_mul(2)
        + usize::from(body_has_target_identity);
    let ordered_step_score = versioned_update_procedure_formal_ordered_step_score(text);
    let command_score = versioned_update_procedure_formal_command_score(text);
    let version_transition_score =
        versioned_update_procedure_ordered_version_transition_score(text);
    let structural_score = ordered_step_score
        .saturating_add(command_score)
        .saturating_add(version_transition_score.saturating_mul(2))
        .saturating_add(procedure_artifact_token_count(text).min(4));
    let has_setup_script_signature =
        versioned_update_procedure_text_has_setup_script_signature(text);
    let score = target_identity_score
        .saturating_mul(4096)
        .saturating_add(ordered_step_score.saturating_mul(2048))
        .saturating_add(command_score.saturating_mul(1536))
        .saturating_add(version_transition_score.saturating_mul(1024))
        .saturating_add(structural_score.saturating_mul(128));
    VersionedUpdateProcedureChunkEvidence {
        target_identity_score,
        label_has_target_identity,
        ordered_step_score,
        command_score,
        version_transition_score,
        structural_score,
        score,
        has_setup_script_signature,
    }
}

fn versioned_update_procedure_chunk_evidence(
    chunk: &RuntimeMatchedChunk,
    focus_model: &VersionedUpdateProcedureFocusModel,
) -> VersionedUpdateProcedureChunkEvidence {
    versioned_update_procedure_text_evidence(
        &chunk.document_label,
        &format!("{}\n{}", chunk.source_text, chunk.excerpt),
        focus_model,
    )
}

fn versioned_update_procedure_chunk_runbook_evidence(
    chunk: &RuntimeMatchedChunk,
    focus_model: &VersionedUpdateProcedureFocusModel,
) -> Option<VersionedUpdateProcedureChunkEvidence> {
    let evidence = versioned_update_procedure_chunk_evidence(chunk, focus_model);
    if !versioned_update_procedure_evidence_supports_runbook(evidence) {
        return None;
    }
    let is_unordered_setup_anchor = chunk_is_setup_focus_command_path_anchor(chunk)
        && evidence.ordered_step_score == 0
        && evidence.version_transition_score == 0;
    (!is_unordered_setup_anchor).then_some(evidence)
}

fn versioned_update_procedure_evidence_supports_runbook(
    evidence: VersionedUpdateProcedureChunkEvidence,
) -> bool {
    let formal_step_count = evidence.ordered_step_score.saturating_add(evidence.command_score);
    evidence.target_identity_score > 0
        && formal_step_count >= 2
        && evidence.structural_score >= VERSIONED_UPDATE_PROCEDURE_EVIDENCE_STRUCTURAL_SCORE_FLOOR
        && versioned_update_procedure_setup_signature_allows_runbook(evidence)
}

fn versioned_update_procedure_setup_signature_allows_runbook(
    evidence: VersionedUpdateProcedureChunkEvidence,
) -> bool {
    !evidence.has_setup_script_signature
        || (evidence.ordered_step_score >= 2 && evidence.command_score > 0)
}

fn versioned_update_procedure_formal_ordered_step_score(text: &str) -> usize {
    text.lines()
        .filter(|line| {
            ordered_step_marker_count(line) > 0 && !ordered_step_looks_like_sequential_record(line)
        })
        .count()
        .min(VERSIONED_UPDATE_PROCEDURE_COMMAND_SIGNAL_SCORE_CAP)
}

fn versioned_update_procedure_formal_command_score(text: &str) -> usize {
    let line_commands =
        text.lines().filter(|line| versioned_update_procedure_line_has_command_start(line)).count();
    let inline_commands = text
        .lines()
        .map(|line| {
            let tokens = shellish_tokens_from_text(line);
            (0..tokens.len())
                .filter(|index| shellish_inline_token_starts_command(&tokens, *index))
                .count()
        })
        .sum::<usize>();
    line_commands.max(inline_commands).min(VERSIONED_UPDATE_PROCEDURE_COMMAND_SIGNAL_SCORE_CAP)
}

fn ordered_step_looks_like_sequential_record(line: &str) -> bool {
    let body = strip_leading_procedure_order_marker(line).trim_start_matches(['-', '*', '•', ' ']);
    body.split_whitespace().next().is_some_and(formal_semver_field)
}

fn formal_semver_field(field: &str) -> bool {
    let field =
        field.trim_matches(|character: char| character.is_ascii_punctuation() && character != '.');
    let numeric = field.strip_prefix('v').or_else(|| field.strip_prefix('V')).unwrap_or(field);
    let mut component_count = 0usize;
    let syntax_is_valid = numeric.split('.').all(|component| {
        component_count = component_count.saturating_add(1);
        !component.is_empty() && component.chars().all(|character| character.is_ascii_digit())
    });
    syntax_is_valid && component_count >= 2 && extract_semver_like_version(numeric).is_some()
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
            index = index.saturating_add(1);
            continue;
        }
        let mut end = index.saturating_add(1);
        while end < chars.len() && chars[end].is_ascii_digit() {
            end = end.saturating_add(1);
        }
        if end < chars.len()
            && matches!(chars[end], '.' | ')')
            && chars.get(end.saturating_add(1)).is_some_and(|character| character.is_whitespace())
        {
            count = count.saturating_add(1);
            index = end.saturating_add(1);
            continue;
        }
        index = end;
    }
    count
}

fn versioned_update_procedure_text_has_setup_script_signature(text: &str) -> bool {
    let tokens = shellish_tokens_from_text(text);
    let has_external_artifact =
        tokens.iter().any(|token| shellish_token_has_external_artifact(token));
    let has_local_artifact = tokens.iter().any(|token| shellish_token_is_local_artifact(token));
    let has_preparation_signal =
        tokens.iter().any(|token| shellish_token_has_artifact_preparation_signal(token));
    has_external_artifact && has_local_artifact && has_preparation_signal
}

pub(crate) fn versioned_update_exact_target_runbook_score(
    _question: &str,
    query_ir: &QueryIR,
    chunk: &RuntimeMatchedChunk,
) -> Option<usize> {
    if !query_ir_requests_versioned_update_procedure_context("", query_ir) {
        return None;
    }
    let focus_model = versioned_update_procedure_focus_model(query_ir)?;
    versioned_update_procedure_chunk_runbook_evidence(chunk, &focus_model)
        .map(|evidence| evidence.score)
}

pub(crate) fn versioned_update_procedure_runbook_anchor_score(
    _question: &str,
    query_ir: &QueryIR,
    chunk: &RuntimeMatchedChunk,
) -> Option<usize> {
    versioned_update_exact_target_runbook_score("", query_ir, chunk)
}

pub(crate) fn chunk_is_versioned_update_instruction_title_anchor(
    _question: &str,
    query_ir: &QueryIR,
    chunk: &RuntimeMatchedChunk,
) -> bool {
    let Some(focus_model) = versioned_update_procedure_focus_model(query_ir) else {
        return false;
    };
    query_ir_requests_versioned_update_procedure_context("", query_ir)
        && chunk_is_versioned_update_instruction_title_anchor_for_model(chunk, &focus_model)
}

fn chunk_is_versioned_update_instruction_title_anchor_for_model(
    chunk: &RuntimeMatchedChunk,
    focus_model: &VersionedUpdateProcedureFocusModel,
) -> bool {
    versioned_update_procedure_chunk_runbook_evidence(chunk, focus_model)
        .is_some_and(|evidence| evidence.label_has_target_identity)
}

fn versioned_update_procedure_focus_terms(
    _question: &str,
    query_ir: Option<&QueryIR>,
    _plan_keywords: &[String],
) -> Vec<String> {
    query_ir
        .and_then(versioned_update_procedure_focus_model)
        .map(|focus_model| focus_model.focus_terms.into_iter().collect())
        .unwrap_or_default()
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
        match target_type {
            QueryTargetKind::Package => has_command_object_target = true,
            QueryTargetKind::ConfigurationFile | QueryTargetKind::ConfigKey => {
                has_configuration_target = true;
            }
            _ => {}
        }
    }
    has_command_object_target && has_configuration_target
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
    query_ir.targets_any(&[
        QueryTargetKind::Package,
        QueryTargetKind::ConfigurationFile,
        QueryTargetKind::ConfigKey,
        QueryTargetKind::Parameter,
    ])
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

fn document_evidence_anchor_candidate_document_ids(
    question: &str,
    query_ir: Option<&QueryIR>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    limit: usize,
) -> Vec<Uuid> {
    if limit == 0 {
        return Vec::new();
    }
    if let Some(query_ir) = query_ir
        && (query_ir.source_slice.is_some()
            || query_ir.comparison.is_some()
            || matches!(query_ir.scope, QueryScope::MultiDocument | QueryScope::LibraryMeta))
    {
        return Vec::new();
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

fn raw_setup_focus_text_structural_score(text: &str) -> usize {
    let command_literal_count = extract_package_command_literals(text, 2).len();
    let configuration_path_count = setup_focus_configuration_path_count(text);
    let assignment_count = setup_focus_parameter_assignment_count(text);
    let parameter_literal_count = setup_focus_parameter_literal_count(text);
    let section_count = setup_focus_section_header_count(text);
    let url_count = setup_focus_url_count(text);

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

fn extend_unique_chunk_rows(
    source: impl IntoIterator<Item = KnowledgeChunkRow>,
    chunk_cap: usize,
    seen: &mut BTreeSet<Uuid>,
    rows: &mut Vec<KnowledgeChunkRow>,
) -> bool {
    for row in source {
        if seen.insert(row.chunk_id) {
            rows.push(row);
        }
        if rows.len() >= chunk_cap {
            return true;
        }
    }
    false
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
        let head_rows =
            state.document_store.list_chunks_by_revisions_windows(&windows).await.with_context(
                || {
                    format!(
                        "failed to load artifact sibling source head chunks for document {}",
                        document.document_id
                    )
                },
            )?;
        if extend_unique_chunk_rows(head_rows, chunk_cap, &mut seen, &mut rows) {
            return Ok(rows);
        }
    }

    if !focus_terms.is_empty() && rows.len() < chunk_cap {
        let focused_rows = state
            .document_store
            .list_chunks_by_revisions_matching_terms(&revision_ids, focus_terms, chunk_cap)
            .await
            .with_context(|| {
                format!(
                    "failed to load artifact sibling source focused chunks for document {}",
                    document.document_id
                )
            })?;
        if extend_unique_chunk_rows(focused_rows, chunk_cap, &mut seen, &mut rows) {
            return Ok(rows);
        }
    }

    if rows.len() < chunk_cap {
        let fallback_max_index = ARTIFACT_SIBLING_SOURCE_CHUNKS_PER_DOCUMENT.saturating_sub(1);
        let fallback_rows = state
            .document_store
            .list_chunks_by_revision_range(canonical_revision_id, 0, fallback_max_index)
            .await
            .with_context(|| {
                format!(
                    "failed to load artifact sibling source fallback chunks for document {} revision {}",
                    document.document_id, canonical_revision_id
                )
            })?;
        extend_unique_chunk_rows(fallback_rows, chunk_cap, &mut seen, &mut rows);
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
    let mut seen = BTreeSet::<Vec<String>>::new();
    chunks
        .iter()
        .filter_map(|chunk| document_index.get(&chunk.document_id))
        .filter(|document| setup_focus_document_is_standalone_image(document))
        .flat_map(setup_focus_document_identity_values)
        .flat_map(|value| artifact_sibling_identity_prefixes(&value))
        .map(|prefix| normalized_alnum_tokens(&prefix, 3))
        .filter(|tokens| tokens.len() >= 2)
        .filter(|tokens| seen.insert(tokens.iter().cloned().collect()))
        .collect()
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

struct RetrievalExecutionRequest<'a> {
    state: &'a AppState,
    library_id: Uuid,
    question: &'a str,
    targeted_document_ids: BTreeSet<Uuid>,
    runtime_plan: &'a RuntimeQueryPlan,
    limit: usize,
    question_embedding: &'a [f32],
    vector_search_context: Option<&'a RuntimeVectorSearchContext>,
    document_index: &'a HashMap<Uuid, KnowledgeDocumentRow>,
    query_ir: Option<&'a QueryIR>,
    allow_broaden: bool,
    text_search_config: &'a str,
    reusable_primary: Option<ChunkRetrievalLaneOutcome>,
}

struct InitialCompanionRequest<'a> {
    state: &'a AppState,
    library_id: Uuid,
    question: &'a str,
    targeted_document_ids: &'a BTreeSet<Uuid>,
    plan_keywords: &'a [String],
    document_index: &'a HashMap<Uuid, KnowledgeDocumentRow>,
    query_ir: Option<&'a QueryIR>,
    query_ir_focus_queries: &'a [String],
    temporal_start: Option<DateTime<Utc>>,
    temporal_end: Option<DateTime<Utc>>,
    primary_evidence_available: bool,
}

struct InitialCompanionChunks {
    retrieval_plan: CompanionRetrievalPlan,
    latest_version_document_ids: BTreeSet<Uuid>,
    document_identity: Vec<RuntimeMatchedChunk>,
    latest_version: Vec<RuntimeMatchedChunk>,
    latest_version_semantic: Vec<RuntimeMatchedChunk>,
    entity_bio: Vec<RuntimeMatchedChunk>,
    query_ir_focus: Vec<RuntimeMatchedChunk>,
    content_anchor: Vec<RuntimeMatchedChunk>,
    document_evidence_anchor: Vec<RuntimeMatchedChunk>,
    versioned_update_procedure: Vec<RuntimeMatchedChunk>,
    setup_focus_document: Vec<RuntimeMatchedChunk>,
    setup_variant_document: Vec<RuntimeMatchedChunk>,
}

struct MergedCompanionChunks {
    chunks: Vec<RuntimeMatchedChunk>,
    retrieval_plan: CompanionRetrievalPlan,
    latest_version_document_ids: BTreeSet<Uuid>,
    protected_document_ids: BTreeSet<Uuid>,
}

struct PostMergeCompanionRequest<'a> {
    state: &'a AppState,
    library_id: Uuid,
    question: &'a str,
    document_index: &'a HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &'a [String],
    query_ir: Option<&'a QueryIR>,
    temporal_start: Option<DateTime<Utc>>,
    temporal_end: Option<DateTime<Utc>>,
    limit: usize,
    primary_evidence_available: bool,
}

struct FinalChunkShapingRequest<'a> {
    state: &'a AppState,
    targeted_document_ids: &'a BTreeSet<Uuid>,
    document_index: &'a HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &'a [String],
    query_ir: Option<&'a QueryIR>,
    temporal_start: Option<DateTime<Utc>>,
    temporal_end: Option<DateTime<Utc>>,
    limit: usize,
    initial_table_row_count: Option<usize>,
    targeted_table_aggregation: bool,
}

enum PlannedLaneRun<T> {
    Skipped,
    Launched { spec: super::retrieval_plan::LaneSpec, result: anyhow::Result<T> },
}

async fn run_planned_chunk_lane<Fut>(
    plan: &CompanionRetrievalPlan,
    lane: RetrievalLane,
    future: Fut,
) -> PlannedLaneRun<Vec<RuntimeMatchedChunk>>
where
    Fut: std::future::Future<Output = anyhow::Result<Vec<RuntimeMatchedChunk>>>,
{
    let Some(spec) = plan.spec(lane) else {
        return PlannedLaneRun::Skipped;
    };
    tracing::debug!(
        stage = "retrieval.companion_lane_launched",
        lane = lane.name(),
        criticality = ?spec.criticality,
        "launched planned companion retrieval lane"
    );
    PlannedLaneRun::Launched { spec, result: timed_lane(lane.span_name(), future).await }
}

fn resolve_planned_chunk_lane(
    run: PlannedLaneRun<Vec<RuntimeMatchedChunk>>,
    primary_evidence_available: bool,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let PlannedLaneRun::Launched { spec, result } = run else {
        return Ok(Vec::new());
    };
    match resolve_lane_result(spec, result, primary_evidence_available)? {
        LaneResolution::Ready(chunks) => Ok(chunks),
        LaneResolution::Degraded(error) => {
            tracing::warn!(
                stage = "retrieval.companion_lane_degraded",
                lane = spec.lane.name(),
                criticality = ?spec.criticality,
                error = %format!("{error:#}"),
                retrieval_degraded = true,
                primary_evidence_available,
                "optional companion retrieval lane failed; retaining primary evidence"
            );
            Ok(Vec::new())
        }
    }
}

pub(crate) async fn retrieve_document_chunks(
    state: &AppState,
    library_id: Uuid,
    question: &str,
    forced_target_document_ids: Option<&BTreeSet<Uuid>>,
    plan: &RuntimeQueryPlan,
    limit: usize,
    question_embedding: &[f32],
    vector_search_context: Option<&RuntimeVectorSearchContext>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    query_ir: Option<&QueryIR>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    // Load the library's retrieval config to derive the lexical text-search
    // config for the Postgres FTS lane. Falls back to the default ("simple")
    // when the row is not found or the JSON cannot be deserialized, so that
    // the caller is never blocked by a missing library.
    let stored_retrieval_config = repositories::catalog_repository::get_library_by_id(
        &state.persistence.postgres,
        library_id,
    )
    .await
    .context("failed to load library retrieval config for chunk search")?
    .map(|row| row.retrieval_config);
    let text_search_config = resolve_text_search_config(stored_retrieval_config)
        .context("failed to plan lexical retrieval from stored library configuration")?;
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
    Box::pin(retrieve_document_chunks_with_targets(RetrievalExecutionRequest {
        state,
        library_id,
        question,
        targeted_document_ids,
        runtime_plan: plan,
        limit,
        question_embedding,
        vector_search_context,
        document_index,
        query_ir,
        allow_broaden: pin_is_inferred,
        text_search_config: &text_search_config,
        reusable_primary: None,
    }))
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
/// the pin should be dropped for a single broad replan.
///
/// Fires only when the pin was compiler-inferred AND the narrowed retrieval
/// is at or below a small coverage floor — a genuinely thin stub, not merely
/// a small-but-sufficient document. Bounding it to near-empty results keeps
/// the fallback to at most one companion-lane replan inside the tool-call SLO.
/// The lexical/vector primary snapshot is reused and never searched twice.
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

async fn load_initial_companion_chunks(
    request: InitialCompanionRequest<'_>,
) -> anyhow::Result<InitialCompanionChunks> {
    let InitialCompanionRequest {
        state,
        library_id,
        question,
        targeted_document_ids,
        plan_keywords,
        document_index,
        query_ir,
        query_ir_focus_queries,
        temporal_start,
        temporal_end,
        primary_evidence_available,
        ..
    } = request;
    let latest_version_selection = select_latest_version_documents(query_ir, document_index);
    let latest_version_requested_count = latest_version_selection.requested_count;
    let latest_version_documents = latest_version_selection.documents;
    let latest_selection_kind = if query_ir.is_some_and(query_requests_latest_versions) {
        LatestSelectionKind::Explicit
    } else {
        LatestSelectionKind::None
    };
    let has_explicit_content_anchor =
        explicit_content_anchor_requested(query_ir, !targeted_document_ids.is_empty());
    let versioned_update_intent =
        question_requests_versioned_update_procedure_evidence(question, query_ir);
    let setup_intent = query_ir.is_some_and(|query_ir| {
        (matches!(query_ir.act, QueryAct::ConfigureHow)
            || query_ir_has_setup_configuration_target(query_ir))
            && (query_ir_requests_setup_focus_document_candidates(query_ir)
                || question_requests_setup_variant_evidence(question, Some(query_ir)))
    });
    let retrieval_plan = CompanionRetrievalPlan::compile(RetrievalPlanningContext {
        query_ir,
        has_target_documents: !targeted_document_ids.is_empty(),
        has_focus_queries: !query_ir_focus_queries.is_empty(),
        has_content_anchor: has_explicit_content_anchor,
        has_document_evidence_anchor: !targeted_document_ids.is_empty()
            || query_ir.is_some_and(|query_ir| query_ir.document_focus.is_some()),
        latest_selection: latest_selection_kind,
        versioned_update_intent,
        setup_intent,
    });
    let latest_version_document_ids =
        latest_version_scoped_document_ids(&latest_version_documents, &[]);
    tracing::info!(
        stage = "retrieval.plan",
        planned_lane_count = retrieval_plan.planned_count(),
        skipped_lane_count = retrieval_plan.skipped_count(),
        planned_lanes = %retrieval_plan.planned_names(),
        skipped_lanes = %retrieval_plan.skipped_names(),
        primary_evidence_available,
        "compiled structural companion retrieval plan"
    );
    let latest_version_scope_terms = query_ir.map(latest_version_scope_terms).unwrap_or_default();
    let has_explicit_source_tail =
        query_ir.and_then(|query_ir| query_ir.source_slice.as_ref()).is_some_and(|source_slice| {
            matches!(source_slice.direction, crate::domains::query_ir::SourceSliceDirection::Tail)
        });
    let prefers_source_tail =
        has_explicit_source_tail || query_ir.is_some_and(query_requests_latest_versions);
    let allows_unscoped_density_fallback = prefers_source_tail && !has_explicit_source_tail;
    let (
        document_identity,
        latest_version,
        latest_version_semantic,
        entity_bio,
        query_ir_focus,
        content_anchor,
        document_evidence_anchor,
        versioned_update_procedure,
        setup_focus_document,
        setup_variant_document,
    ) = tokio::join!(
        run_planned_chunk_lane(
            &retrieval_plan,
            RetrievalLane::DocumentIdentity,
            load_document_identity_chunks_for_targets(
                state,
                document_index,
                targeted_document_ids,
                plan_keywords,
                query_ir,
            ),
        ),
        run_planned_chunk_lane(
            &retrieval_plan,
            RetrievalLane::LatestVersion,
            load_latest_version_document_chunks(
                state,
                document_index,
                plan_keywords,
                latest_version_requested_count,
                &latest_version_documents,
            ),
        ),
        run_planned_chunk_lane(
            &retrieval_plan,
            RetrievalLane::LatestVersionSemantic,
            load_latest_version_semantic_document_chunks(
                state,
                document_index,
                plan_keywords,
                latest_version_requested_count,
                &latest_version_scope_terms,
                query_ir.is_some_and(query_requests_latest_versions),
                prefers_source_tail,
                allows_unscoped_density_fallback,
            ),
        ),
        run_planned_chunk_lane(
            &retrieval_plan,
            RetrievalLane::EntityBio,
            load_entity_bio_chunks(
                state,
                library_id,
                query_ir,
                document_index,
                plan_keywords,
                targeted_document_ids,
            ),
        ),
        run_planned_chunk_lane(
            &retrieval_plan,
            RetrievalLane::QueryIrFocus,
            load_query_ir_focus_chunks(
                state,
                library_id,
                question,
                query_ir_focus_queries,
                targeted_document_ids,
                document_index,
                plan_keywords,
                temporal_start,
                temporal_end,
            ),
        ),
        run_planned_chunk_lane(
            &retrieval_plan,
            RetrievalLane::ContentAnchor,
            load_content_anchor_chunks(
                state,
                question,
                query_ir,
                targeted_document_ids,
                document_index,
                plan_keywords,
                temporal_start,
                temporal_end,
            ),
        ),
        run_planned_chunk_lane(
            &retrieval_plan,
            RetrievalLane::DocumentEvidenceAnchor,
            load_document_evidence_anchor_chunks(
                state,
                question,
                query_ir,
                document_index,
                plan_keywords,
                temporal_start,
                temporal_end,
            ),
        ),
        run_planned_chunk_lane(
            &retrieval_plan,
            RetrievalLane::VersionedUpdateProcedure,
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
        ),
        run_planned_chunk_lane(
            &retrieval_plan,
            RetrievalLane::SetupFocus,
            load_setup_focus_document_chunks(
                state,
                query_ir,
                document_index,
                plan_keywords,
                temporal_start,
                temporal_end,
            ),
        ),
        run_planned_chunk_lane(
            &retrieval_plan,
            RetrievalLane::SetupVariant,
            load_setup_variant_document_chunks(
                state,
                question,
                query_ir,
                document_index,
                plan_keywords,
                temporal_start,
                temporal_end,
            ),
        ),
    );
    Ok(InitialCompanionChunks {
        retrieval_plan,
        latest_version_document_ids,
        document_identity: resolve_planned_chunk_lane(
            document_identity,
            primary_evidence_available,
        )?,
        latest_version: resolve_planned_chunk_lane(latest_version, primary_evidence_available)?,
        latest_version_semantic: resolve_planned_chunk_lane(
            latest_version_semantic,
            primary_evidence_available,
        )?,
        entity_bio: resolve_planned_chunk_lane(entity_bio, primary_evidence_available)?,
        query_ir_focus: resolve_planned_chunk_lane(query_ir_focus, primary_evidence_available)?,
        content_anchor: resolve_planned_chunk_lane(content_anchor, primary_evidence_available)?,
        document_evidence_anchor: resolve_planned_chunk_lane(
            document_evidence_anchor,
            primary_evidence_available,
        )?,
        versioned_update_procedure: resolve_planned_chunk_lane(
            versioned_update_procedure,
            primary_evidence_available,
        )?,
        setup_focus_document: resolve_planned_chunk_lane(
            setup_focus_document,
            primary_evidence_available,
        )?,
        setup_variant_document: resolve_planned_chunk_lane(
            setup_variant_document,
            primary_evidence_available,
        )?,
    })
}

fn merge_optional_focus_chunks(
    chunks: Vec<RuntimeMatchedChunk>,
    companion_chunks: Vec<RuntimeMatchedChunk>,
    merged_limit: usize,
    query_ir: Option<&QueryIR>,
    protected_document_ids: &mut BTreeSet<Uuid>,
) -> Vec<RuntimeMatchedChunk> {
    if companion_chunks.is_empty() {
        return chunks;
    }
    protected_document_ids.extend(companion_chunks.iter().map(|chunk| chunk.document_id));
    merge_query_ir_focus_chunks_for_query(chunks, companion_chunks, merged_limit, query_ir)
}

fn merge_initial_companion_chunks(
    mut chunks: Vec<RuntimeMatchedChunk>,
    initial: InitialCompanionChunks,
    targeted_document_ids: &BTreeSet<Uuid>,
    query_ir: Option<&QueryIR>,
    limit: usize,
    initial_table_row_count: Option<usize>,
) -> anyhow::Result<MergedCompanionChunks> {
    let InitialCompanionChunks {
        retrieval_plan,
        mut latest_version_document_ids,
        document_identity,
        mut latest_version,
        latest_version_semantic,
        entity_bio,
        query_ir_focus,
        content_anchor,
        document_evidence_anchor,
        versioned_update_procedure,
        setup_focus_document,
        setup_variant_document,
    } = initial;
    if !document_identity.is_empty() {
        let identity_budget_per_document =
            DOCUMENT_IDENTITY_CHUNKS_PER_DOCUMENT + DOCUMENT_IDENTITY_FOCUSED_CHUNKS_PER_DOCUMENT;
        chunks = merge_chunks(
            chunks,
            document_identity,
            limit
                .max(initial_table_row_count.unwrap_or(0))
                .saturating_add(targeted_document_ids.len() * identity_budget_per_document),
        );
    }
    latest_version_document_ids
        .extend(latest_version_scoped_document_ids(&[], &latest_version_semantic));
    if !latest_version_semantic.is_empty() {
        let merged_limit = query_ir.map_or(limit, |ir| latest_version_context_top_k(ir, limit));
        latest_version = merge_chunks(latest_version, latest_version_semantic, merged_limit);
    }
    if !latest_version.is_empty() {
        let query_ir = query_ir
            .context("explicit latest-version lane returned chunks without a structured query")?;
        let latest_version_context_limit = limit
            .max(initial_table_row_count.unwrap_or(0))
            .saturating_add(latest_version_context_top_k(query_ir, 0));
        chunks = merge_chunks(chunks, latest_version, latest_version_context_limit);
    }
    if !entity_bio.is_empty() {
        chunks =
            merge_entity_bio_chunks(chunks, entity_bio, limit.saturating_add(ENTITY_BIO_CHUNK_CAP));
    }
    if !query_ir_focus.is_empty() {
        chunks = merge_query_ir_focus_chunks_for_query(
            chunks,
            query_ir_focus,
            query_ir_focus_context_top_k(limit),
            query_ir,
        );
    }
    let mut protected_document_ids = BTreeSet::new();
    chunks = merge_optional_focus_chunks(
        chunks,
        content_anchor,
        query_ir_focus_context_top_k(limit).saturating_add(CONTENT_ANCHOR_CHUNK_CAP),
        query_ir,
        &mut protected_document_ids,
    );
    chunks = merge_optional_focus_chunks(
        chunks,
        document_evidence_anchor,
        query_ir_focus_context_top_k(limit),
        query_ir,
        &mut protected_document_ids,
    );
    if !versioned_update_procedure.is_empty() {
        protected_document_ids
            .extend(versioned_update_procedure.iter().map(|chunk| chunk.document_id));
        chunks = merge_versioned_update_procedure_chunks_for_query(
            chunks,
            versioned_update_procedure,
            query_ir_focus_context_top_k(limit),
            query_ir,
        );
    }
    chunks = merge_optional_focus_chunks(
        chunks,
        setup_focus_document,
        query_ir_focus_context_top_k(limit),
        query_ir,
        &mut protected_document_ids,
    );
    chunks = merge_optional_focus_chunks(
        chunks,
        setup_variant_document,
        query_ir_focus_context_top_k(limit).max(SETUP_VARIANT_CHUNK_CAP),
        query_ir,
        &mut protected_document_ids,
    );
    Ok(MergedCompanionChunks {
        chunks,
        retrieval_plan,
        latest_version_document_ids,
        protected_document_ids,
    })
}

async fn load_post_merge_companion_chunks(
    mut merged: MergedCompanionChunks,
    request: PostMergeCompanionRequest<'_>,
) -> anyhow::Result<MergedCompanionChunks> {
    let PostMergeCompanionRequest {
        state,
        library_id,
        question,
        document_index,
        plan_keywords,
        query_ir,
        temporal_start,
        temporal_end,
        limit,
        primary_evidence_available,
    } = request;
    let focus_terms = versioned_update_procedure_focus_terms(question, query_ir, plan_keywords);
    let source_local_result = run_planned_chunk_lane(
        &merged.retrieval_plan,
        RetrievalLane::VersionedUpdateSourceLocal,
        load_versioned_update_procedure_source_local_runbook_chunks(
            state,
            &merged.chunks,
            document_index,
            &focus_terms,
            question,
            query_ir,
        ),
    )
    .await;
    let source_local_chunks =
        resolve_planned_chunk_lane(source_local_result, primary_evidence_available)?;
    if !source_local_chunks.is_empty() {
        merged
            .protected_document_ids
            .extend(source_local_chunks.iter().map(|chunk| chunk.document_id));
        merged.chunks = merge_versioned_update_procedure_chunks_for_query(
            merged.chunks,
            source_local_chunks,
            query_ir_focus_context_top_k(limit),
            query_ir,
        );
    }
    let linked_anchor_result = run_planned_chunk_lane(
        &merged.retrieval_plan,
        RetrievalLane::LinkedAnchorContext,
        load_linked_anchor_context_chunks(
            state,
            library_id,
            question,
            query_ir,
            &merged.chunks,
            document_index,
            plan_keywords,
            temporal_start,
            temporal_end,
        ),
    )
    .await;
    let linked_anchor_chunks =
        resolve_planned_chunk_lane(linked_anchor_result, primary_evidence_available)?;
    if !linked_anchor_chunks.is_empty() {
        merged.chunks = merge_query_ir_focus_chunks_for_query(
            merged.chunks,
            linked_anchor_chunks,
            query_ir_focus_context_top_k(limit),
            query_ir,
        );
    }
    let artifact_sibling_result = run_planned_chunk_lane(
        &merged.retrieval_plan,
        RetrievalLane::ArtifactSiblingSource,
        load_artifact_sibling_source_chunks(state, &merged.chunks, document_index, plan_keywords),
    )
    .await;
    let artifact_sibling_chunks =
        resolve_planned_chunk_lane(artifact_sibling_result, primary_evidence_available)?;
    if !artifact_sibling_chunks.is_empty() {
        merged.chunks = merge_query_ir_focus_chunks_for_query(
            merged.chunks,
            artifact_sibling_chunks,
            query_ir_focus_context_top_k(limit),
            query_ir,
        );
    }
    Ok(merged)
}

async fn apply_temporal_post_filter(
    state: &AppState,
    mut chunks: Vec<RuntimeMatchedChunk>,
    temporal_start: Option<DateTime<Utc>>,
    temporal_end: Option<DateTime<Utc>>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if temporal_start.is_none() || temporal_end.is_none() || chunks.is_empty() {
        return Ok(chunks);
    }
    let chunk_ids: Vec<Uuid> = chunks.iter().map(|chunk| chunk.chunk_id).collect();
    let rows = state
        .document_store
        .list_chunks_by_ids(&chunk_ids)
        .await
        .context("failed to look up chunks for temporal post-filter")?;
    let allowed: HashSet<Uuid> = rows
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
    Ok(chunks)
}

async fn apply_final_chunk_shaping(
    mut chunks: Vec<RuntimeMatchedChunk>,
    request: FinalChunkShapingRequest<'_>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let FinalChunkShapingRequest {
        state,
        targeted_document_ids,
        document_index,
        plan_keywords,
        query_ir,
        temporal_start,
        temporal_end,
        limit,
        initial_table_row_count,
        targeted_table_aggregation,
    } = request;
    chunks = apply_temporal_post_filter(state, chunks, temporal_start, temporal_end).await?;
    if let Some(row_count) = initial_table_row_count {
        let initial_rows = load_initial_table_rows_for_documents(
            state,
            document_index,
            targeted_document_ids,
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
            targeted_document_ids,
            DIRECT_TABLE_AGGREGATION_SUMMARY_LIMIT,
            plan_keywords,
        )
        .await?;
        let direct_row_chunks = load_table_rows_for_documents(
            state,
            document_index,
            targeted_document_ids,
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
    Ok(chunks)
}

async fn retrieve_document_chunks_with_targets(
    request: RetrievalExecutionRequest<'_>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let RetrievalExecutionRequest {
        state,
        library_id,
        question,
        targeted_document_ids,
        runtime_plan: plan,
        limit,
        question_embedding,
        vector_search_context,
        document_index,
        query_ir,
        allow_broaden,
        text_search_config,
        reusable_primary,
    } = request;
    let initial_table_row_count = requested_initial_table_row_count(query_ir);
    let targeted_table_aggregation =
        question_asks_table_aggregation(question, query_ir) && !targeted_document_ids.is_empty();
    let query_ir_focus_queries = query_ir.map(query_ir_lexical_focus_queries).unwrap_or_default();
    let lexical_queries = build_lexical_queries(question, plan, &query_ir_focus_queries, query_ir);
    let lexical_limit = limit.saturating_mul(2).max(24);
    let plan_keywords = &plan.keywords;
    let targeted_document_ids_ref = &targeted_document_ids;
    // Inferred focus is the only path that may broaden. Hydrate its primary
    // results without the provisional target filter once, then retain a
    // scoped view for the first pass. A broaden replan can therefore reuse
    // the same lexical/vector searches instead of paying for them twice.
    let broad_primary_scope = BTreeSet::new();
    let primary_hydration_document_ids_ref =
        if allow_broaden { &broad_primary_scope } else { targeted_document_ids_ref };
    // Resolved temporal bounds — applied as a hard filter on every
    // chunk-touching search lane. None when QueryIR has no temporal
    // constraints or none parsed as RFC3339.
    let (temporal_start, temporal_end) =
        query_ir.map_or((None, None), |ir| ir.resolved_temporal_bounds());

    let (lane_outcome, primary_reused) = reuse_or_execute_primary(reusable_primary, || {
        execute_primary_retrieval(PrimaryRetrievalRequest {
            state,
            library_id,
            limit,
            question_embedding,
            vector_search_context,
            document_index,
            plan_keywords,
            hydration_document_ids: primary_hydration_document_ids_ref,
            lexical_queries,
            lexical_limit,
            text_search_config,
            temporal_start,
            temporal_end,
        })
    })
    .await?;
    if primary_reused {
        tracing::info!(
            stage = "retrieval.primary_reused",
            vector_hits = lane_outcome.vector_hits.len(),
            lexical_hits = lane_outcome.lexical_hits.len(),
            "reused lexical/vector primary results for focus-broaden replan"
        );
    }
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
    let reusable_primary = lane_outcome.clone();
    let mut chunks = merge_chunks(
        lane_outcome.vector_hits,
        lane_outcome.lexical_hits,
        limit.max(initial_table_row_count.unwrap_or(0)),
    );
    if allow_broaden && !targeted_document_ids.is_empty() {
        chunks.retain(|chunk| targeted_document_ids.contains(&chunk.document_id));
    }
    let primary_evidence_available = !chunks.is_empty();
    let initial_companions = load_initial_companion_chunks(InitialCompanionRequest {
        state,
        library_id,
        question,
        targeted_document_ids: &targeted_document_ids,
        plan_keywords,
        document_index,
        query_ir,
        query_ir_focus_queries: &query_ir_focus_queries,
        temporal_start,
        temporal_end,
        primary_evidence_available,
    })
    .await?;
    let MergedCompanionChunks {
        chunks,
        retrieval_plan,
        latest_version_document_ids,
        protected_document_ids,
    } = merge_initial_companion_chunks(
        chunks,
        initial_companions,
        &targeted_document_ids,
        query_ir,
        limit,
        initial_table_row_count,
    )?;

    let MergedCompanionChunks {
        mut chunks,
        latest_version_document_ids,
        protected_document_ids,
        ..
    } = load_post_merge_companion_chunks(
        MergedCompanionChunks {
            chunks,
            retrieval_plan,
            latest_version_document_ids,
            protected_document_ids,
        },
        PostMergeCompanionRequest {
            state,
            library_id,
            question,
            document_index,
            plan_keywords,
            query_ir,
            temporal_start,
            temporal_end,
            limit,
            primary_evidence_available,
        },
    )
    .await?;
    let max_chunks_per_document = if targeted_document_ids.is_empty() {
        MAX_CHUNKS_PER_DOCUMENT
    } else {
        MAX_CHUNKS_PER_DOCUMENT.max(DOCUMENT_IDENTITY_CHUNKS_PER_DOCUMENT)
    };
    chunks = diversify_chunks_by_document(chunks, max_chunks_per_document, &protected_document_ids);
    retain_scoped_documents(
        &mut chunks,
        &targeted_document_ids,
        &latest_version_document_ids,
        &protected_document_ids,
    );
    chunks = apply_final_chunk_shaping(
        chunks,
        FinalChunkShapingRequest {
            state,
            targeted_document_ids: &targeted_document_ids,
            document_index,
            plan_keywords,
            query_ir,
            temporal_start,
            temporal_end,
            limit,
            initial_table_row_count,
            targeted_table_aggregation,
        },
    )
    .await?;

    // P3 coverage fallback. A compiler-inferred single-document focus pin
    // can hard-lock onto a thin same-titled stub and exclude the real
    // procedure docs. When the narrowed retrieval comes back at/below the
    // coverage floor, drop the pin once and re-broaden to the whole library
    // so the answer step has material to work with. `allow_broaden` is false
    // on the recursive call; the cached lexical/vector primary result is
    // reused and only the structurally changed companion plan runs again.
    if should_broaden_focus(allow_broaden, chunks.len()) {
        tracing::info!(
            stage = "retrieval.focus_broaden_fallback",
            library_id = %library_id,
            pinned_document_count = targeted_document_ids.len(),
            narrowed_chunk_count = chunks.len(),
            coverage_floor = FOCUS_BROADEN_MIN_CHUNKS,
            "inferred focus pin returned too few chunks; dropping pin and re-broadening retrieval"
        );
        return Box::pin(retrieve_document_chunks_with_targets(RetrievalExecutionRequest {
            state,
            library_id,
            question,
            targeted_document_ids: BTreeSet::new(),
            runtime_plan: plan,
            limit,
            question_embedding,
            vector_search_context,
            document_index,
            query_ir,
            allow_broaden: false,
            text_search_config,
            reusable_primary: Some(reusable_primary),
        }))
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

fn append_query_ir_focus_hits(
    query_hits: Vec<(Uuid, f32)>,
    seen: &mut HashSet<Uuid>,
    hits: &mut Vec<(Uuid, f32)>,
) {
    for (chunk_id, raw_score) in query_hits {
        if hits.len() >= QUERY_IR_FOCUS_CHUNK_CAP {
            break;
        }
        if seen.insert(chunk_id) {
            let fallback_score = query_ir_focus_chunk_score(hits.len());
            let score =
                if raw_score.is_finite() && raw_score > 0.0 { raw_score } else { fallback_score };
            hits.push((chunk_id, score));
        }
    }
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
                append_query_ir_focus_hits(query_hits, &mut seen, &mut hits);
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

#[derive(Clone)]
struct ChunkRetrievalLaneOutcome {
    vector_hits: Vec<RuntimeMatchedChunk>,
    vector_elapsed_ms: u128,
    lexical_hits: Vec<RuntimeMatchedChunk>,
    lexical_query_count: usize,
    lexical_elapsed_ms: u128,
    degraded_lane_count: usize,
}

struct PrimaryRetrievalRequest<'a> {
    state: &'a AppState,
    library_id: Uuid,
    limit: usize,
    question_embedding: &'a [f32],
    vector_search_context: Option<&'a RuntimeVectorSearchContext>,
    document_index: &'a HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &'a [String],
    hydration_document_ids: &'a BTreeSet<Uuid>,
    lexical_queries: Vec<String>,
    lexical_limit: usize,
    text_search_config: &'a str,
    temporal_start: Option<DateTime<Utc>>,
    temporal_end: Option<DateTime<Utc>>,
}

async fn execute_primary_retrieval(
    request: PrimaryRetrievalRequest<'_>,
) -> anyhow::Result<ChunkRetrievalLaneOutcome> {
    let PrimaryRetrievalRequest {
        state,
        library_id,
        limit,
        question_embedding,
        vector_search_context,
        document_index,
        plan_keywords,
        hydration_document_ids,
        lexical_queries,
        lexical_limit,
        text_search_config,
        temporal_start,
        temporal_end,
    } = request;
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
        let context = vector_search_context.ok_or_else(|| QueryServiceError::StateConflict {
            message: format!(
                "runtime query for library {library_id} has a vector without a ready exact-profile preflight; retry the query"
            ),
        })?;
        let _vector_guard =
            state.canonical_services.search.vector_plane_read_guard(state, library_id).await?;
        validate_runtime_vector_search_context(state, library_id, context).await?;
        validate_embedding_vector_dimensions(
            context.dimensions,
            question_embedding,
            "runtime chunk search",
        )?;
        let raw_hits = state
            .search_store
            .search_chunk_vectors_by_similarity(
                context.dimensions,
                library_id,
                &context.embedding_profile_key,
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
        let hits = batch_hydrate_hits(
            state,
            raw_hits.iter().map(|hit| (hit.chunk_id, hit.score as f32)).collect(),
            document_index,
            plan_keywords,
            hydration_document_ids,
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
    let lexical_future = async {
        let started = std::time::Instant::now();
        let lexical_query_count = lexical_queries.len();
        let text_search_config_owned = text_search_config.to_owned();
        let per_query_futures =
            lexical_queries.into_iter().enumerate().map(|(query_index, lexical_query)| {
                let text_search_config_owned = text_search_config_owned.clone();
                async move {
                    let search_result = state
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
                        });
                    let result = match search_result {
                        Ok(hits) => {
                            batch_hydrate_hits(
                                state,
                                hits.into_iter()
                                    .map(|hit| (hit.chunk_id, hit.score as f32))
                                    .collect(),
                                document_index,
                                plan_keywords,
                                hydration_document_ids,
                            )
                            .await
                        }
                        Err(error) => Err(error),
                    };
                    (query_index, result)
                }
            });
        let mut indexed_results = stream::iter(per_query_futures)
            .buffer_unordered(MAX_CONCURRENT_LEXICAL_QUERIES)
            .collect::<Vec<_>>()
            .await;
        indexed_results.sort_by_key(|(query_index, _)| *query_index);
        let per_query_results =
            indexed_results.into_iter().map(|(_, result)| result).collect::<Vec<_>>();
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
    combine_chunk_retrieval_lanes(vector_result, lexical_result)
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

fn push_document_identity_focus_term(
    value: &str,
    seen: &mut BTreeSet<String>,
    terms: &mut Vec<String>,
) {
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
        if token.chars().count() > DOCUMENT_IDENTITY_FOCUS_PREFIX_CHARS {
            let prefix =
                token.chars().take(DOCUMENT_IDENTITY_FOCUS_PREFIX_CHARS).collect::<String>();
            if seen.insert(prefix.clone()) {
                terms.push(prefix);
            }
        }
    }
}

fn document_identity_focus_terms(
    plan_keywords: &[String],
    query_ir: Option<&QueryIR>,
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut terms = Vec::new();
    for keyword in plan_keywords {
        push_document_identity_focus_term(keyword, &mut seen, &mut terms);
    }
    if let Some(query_ir) = query_ir {
        let values = query_ir
            .document_focus
            .as_ref()
            .map(|focus| focus.hint.as_str())
            .into_iter()
            .chain(query_ir.target_entities.iter().map(|entity| entity.label.as_str()))
            .chain(query_ir.literal_constraints.iter().map(|literal| literal.text.as_str()));
        for value in values {
            push_document_identity_focus_term(value, &mut seen, &mut terms);
        }
    }
    terms
}

struct LatestVersionSelection {
    requested_count: usize,
    documents: Vec<LatestVersionDocument>,
}

fn select_latest_version_documents(
    query_ir: Option<&QueryIR>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> LatestVersionSelection {
    let Some(query_ir) = query_ir.filter(|query_ir| query_requests_latest_versions(query_ir))
    else {
        return LatestVersionSelection { requested_count: 0, documents: Vec::new() };
    };
    let requested_count = requested_latest_version_count(query_ir);
    let scope_terms = latest_version_scope_terms(query_ir);
    LatestVersionSelection {
        requested_count,
        documents: latest_version_documents(document_index, requested_count, &scope_terms),
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

async fn latest_version_semantic_candidates(
    state: &AppState,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    scope_terms: &[String],
    requested_count: usize,
    prefer_source_tail: bool,
    allow_unscoped_density_fallback: bool,
) -> anyhow::Result<Vec<LatestVersionSemanticDocument>> {
    let mut candidates = latest_version_semantic_candidate_documents(
        document_index,
        scope_terms,
        LATEST_VERSION_SEMANTIC_DOCUMENT_CANDIDATE_CAP,
    );
    let semantic_document_ids =
        candidates.iter().map(|document| document.document_id).collect::<BTreeSet<_>>();
    let mut candidate_document_ids = semantic_document_ids;
    if prefer_source_tail {
        let mut density_candidates = latest_version_structural_density_candidate_documents(
            state,
            document_index,
            scope_terms,
            &candidate_document_ids,
            LATEST_VERSION_STRUCTURAL_DENSITY_DOCUMENT_CAP,
        )
        .await?;
        candidate_document_ids
            .extend(density_candidates.iter().map(|document| document.document_id));
        if allow_unscoped_density_fallback
            && !scope_terms.is_empty()
            && density_candidates.len() < requested_count.min(4)
        {
            let remaining = LATEST_VERSION_STRUCTURAL_DENSITY_DOCUMENT_CAP
                .saturating_sub(density_candidates.len());
            let unscoped = latest_version_structural_density_candidate_documents(
                state,
                document_index,
                &[],
                &candidate_document_ids,
                remaining,
            )
            .await?;
            candidate_document_ids.extend(unscoped.iter().map(|document| document.document_id));
            density_candidates.extend(unscoped);
        }
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
    Ok(candidates)
}

struct LatestVersionSemanticScan {
    rows: Vec<LatestVersionSemanticRow>,
    dense_distinct_version_count: usize,
    deep_scan_document_count: usize,
}

async fn scan_latest_version_semantic_rows(
    state: &AppState,
    candidates: &[LatestVersionSemanticDocument],
    requested_count: usize,
    prefer_source_tail: bool,
    scope_terms: &[String],
) -> anyhow::Result<LatestVersionSemanticScan> {
    let mut rows = Vec::new();
    let mut dense_rows = Vec::new();
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
            dense_rows.extend(candidate_rows.into_iter().map(|mut row| {
                row.structural_density_score = structural_score;
                row
            }));
        } else {
            rows.extend(candidate_rows);
        }
    }
    let dense_distinct_version_count =
        dense_rows.iter().map(|row| row.version.clone()).collect::<BTreeSet<_>>().len();
    if dense_distinct_version_count
        >= requested_count.min(LATEST_VERSION_STRUCTURAL_MIN_DISTINCT_VERSIONS)
        && !(scope_terms.is_empty() && prefer_source_tail)
    {
        rows = dense_rows;
    } else {
        rows.extend(dense_rows);
    }
    Ok(LatestVersionSemanticScan { rows, dense_distinct_version_count, deep_scan_document_count })
}

fn latest_version_semantic_chunks_from_rows(
    rows: Vec<LatestVersionSemanticRow>,
    source_tail_inventory: bool,
    requested_count: usize,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
) -> Vec<RuntimeMatchedChunk> {
    let mut chunks = Vec::new();
    let mut seen_versions = BTreeSet::<Vec<u32>>::new();
    let mut seen_chunk_ids = HashSet::<Uuid>::new();
    let limit = requested_count.max(LATEST_VERSION_SEMANTIC_CHUNK_CAP);
    for semantic_row in rows {
        let version_seen =
            !source_tail_inventory && !seen_versions.insert(semantic_row.version.clone());
        if version_seen || !seen_chunk_ids.insert(semantic_row.row.chunk_id) {
            continue;
        }
        let score = latest_version_chunk_score(
            DOCUMENT_IDENTITY_SCORE_FLOOR + 256.0,
            requested_count,
            chunks.len(),
            0,
        );
        if let Some(mut chunk) =
            map_chunk_hit(semantic_row.row, score, document_index, plan_keywords)
        {
            chunk.score = Some(score);
            chunk.score_kind = RuntimeChunkScoreKind::LatestVersion;
            chunks.push(chunk);
        }
        if chunks.len() >= limit {
            break;
        }
    }
    chunks.truncate(LATEST_VERSION_SEMANTIC_CHUNK_CAP);
    chunks
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
    let candidates = latest_version_semantic_candidates(
        state,
        document_index,
        scope_terms,
        requested_count,
        prefer_source_tail,
        allow_unscoped_density_fallback,
    )
    .await?;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let scan = scan_latest_version_semantic_rows(
        state,
        &candidates,
        requested_count,
        prefer_source_tail,
        scope_terms,
    )
    .await?;
    let mut rows = scan.rows;
    let dense_distinct_version_count = scan.dense_distinct_version_count;
    let deep_scan_document_count = scan.deep_scan_document_count;
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

    Ok(latest_version_semantic_chunks_from_rows(
        rows,
        source_tail_inventory,
        requested_count,
        document_index,
        plan_keywords,
    ))
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
        compare_version_desc(&left.version, &right.version)
            .then_with(|| left.document_rank.cmp(&right.document_rank))
            .then_with(|| right.row.chunk_index.cmp(&left.row.chunk_index))
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
            compare_version_desc(&left.version, &right.version)
                .then_with(|| right.row.chunk_index.cmp(&left.row.chunk_index))
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
        document.source_uri.as_deref(),
        Some(document.external_key.as_str()),
    ]
    .into_iter()
    .flatten()
    .any(latest_version_identity_value_is_image_like)
}

fn latest_version_identity_value_is_image_like(value: &str) -> bool {
    let path = value.split(['?', '#']).next().unwrap_or(value).trim();
    matches!(
        std::path::Path::new(path)
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
    rows.sort_by(compare_latest_version_documents);
    rows.truncate(count);
    rows
}

fn latest_version_document_from_index_row(
    document: &KnowledgeDocumentRow,
) -> Option<LatestVersionDocument> {
    if !latest_version_semantic_document_is_content_candidate(document) {
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
    // Titles can carry one release across multiple parallel version channels.
    // A generic semver scanner returns the first literal and can therefore rank
    // the newest release as ancient.
    // Reuse the release-context parser used for chunk bodies so the version
    // adjacent to the release marker, including a direct compound group, is
    // interpreted consistently on both retrieval paths.
    let version = extract_release_context_version(primary_title)?;
    let revision_id = canonical_document_revision_id(document)?;
    Some(LatestVersionDocument {
        document_id: document.document_id,
        revision_id,
        version,
        title: primary_title.to_string(),
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
    let mut seen_sources = HashSet::<ReleaseSourceIdentity>::with_capacity(rows.len());
    rows.into_iter()
        .filter(|document| {
            seen_sources
                .insert(ReleaseSourceIdentity::new(document.document_id, document.revision_id))
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

pub(crate) async fn prepare_runtime_vector_search_context(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<Option<RuntimeVectorSearchContext>> {
    let _vector_guard =
        state.canonical_services.search.vector_plane_read_guard(state, library_id).await?;
    let version = load_embedding_profile_inventory_version(state, library_id).await?;
    let binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
        .await
        .context("failed to resolve embed_chunk binding for runtime vector search")?
        .ok_or_else(|| QueryServiceError::StateConflict {
            message: format!(
                "active embed_chunk binding is unavailable while proving the exact vector inventory for library {library_id}; configure the binding and rebuild before querying"
            ),
        })?;

    let embedding_profile_key = binding.embedding_execution_profile_key();
    let index_state = ensure_library_embedding_profile_indexed(
        state,
        library_id,
        &embedding_profile_key,
        version,
    )
    .await?;

    let dimensions = match index_state {
        EmbeddingProfileIndexState::Empty => {
            ensure_embedding_profile_inventory_version_current(state, library_id, version).await?;
            tracing::info!(
                stage = "embed.skip",
                reason = "empty_vector_inventory",
                library_id = %library_id,
                "query embedding and ANN lookup skipped for an empty library"
            );
            return Ok(None);
        }
        EmbeddingProfileIndexState::Ready { dimensions } => dimensions,
    };

    Ok(Some(RuntimeVectorSearchContext {
        embedding_profile_key,
        dimensions,
        active_vector_generation: version.active_vector_generation,
        source_truth_version: version.source_truth_version,
    }))
}

/// Cheap request-boundary fence after provider work. The exact inventory was
/// already validated once by [`prepare_runtime_vector_search_context`]; this
/// recheck only verifies that generation/profile identity did not change
/// before either ANN lane consumes the shared preflight.
pub(crate) async fn validate_runtime_vector_search_context(
    state: &AppState,
    library_id: Uuid,
    context: &RuntimeVectorSearchContext,
) -> anyhow::Result<()> {
    ensure_embedding_profile_inventory_version_current(
        state,
        library_id,
        EmbeddingProfileInventoryVersion {
            active_vector_generation: context.active_vector_generation,
            source_truth_version: context.source_truth_version,
            has_ready_vector: true,
        },
    )
    .await?;
    ensure_active_embedding_profile_key(state, library_id, &context.embedding_profile_key).await?;
    Ok(())
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
/// the bounded four-wide fan-out keeps wall-clock inside the coordinator's
/// budget without allowing one turn to monopolize the pool. Anything above 8 returned diminishing
/// recall for order-of-magnitude more latency.
const MAX_LEXICAL_QUERIES: usize = 8;
const MAX_CONCURRENT_LEXICAL_QUERIES: usize = 4;
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

pub(crate) fn graph_evidence_db_text_queries(
    text_queries: &[String],
    query_ir: Option<&QueryIR>,
) -> Vec<repositories::RuntimeGraphEvidenceSearchQuery> {
    let literal_or_formal_keys = query_ir
        .into_iter()
        .flat_map(|query_ir| {
            query_ir
                .literal_constraints
                .iter()
                .map(|literal| literal.text.as_str())
                .chain(query_ir.target_entities.iter().map(|entity| entity.label.as_str()))
                .chain(query_ir.document_focus.iter().map(|focus| focus.hint.as_str()))
        })
        .map(|query| query.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase())
        .collect::<BTreeSet<_>>();

    text_queries
        .iter()
        .take(MAX_GRAPH_EVIDENCE_DB_TEXT_QUERIES)
        .map(|query| {
            let normalized = query.split_whitespace().collect::<Vec<_>>().join(" ");
            if literal_or_formal_keys.contains(&normalized.to_lowercase()) {
                repositories::RuntimeGraphEvidenceSearchQuery::LiteralOrFormal(normalized)
            } else {
                repositories::RuntimeGraphEvidenceSearchQuery::Lexical(normalized)
            }
        })
        .collect()
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
        push_focus(compound, &mut queries);
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

fn push_unique_query_ir_focus(value: &str, seen: &mut BTreeSet<String>, values: &mut Vec<String>) {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if is_usable_query_ir_focus(&normalized) && seen.insert(normalized.to_lowercase()) {
        values.push(normalized);
    }
}

fn push_query_ir_focus_values(
    values_to_add: impl IntoIterator<Item = String>,
    seen: &mut BTreeSet<String>,
    values: &mut Vec<String>,
) {
    for value in values_to_add {
        push_unique_query_ir_focus(&value, seen, values);
    }
}

fn query_ir_focus_value_groups(query_ir: &QueryIR) -> (Vec<String>, Vec<String>) {
    let mut seen = BTreeSet::new();
    let mut primary_values = Vec::new();
    let mut modifier_values = Vec::new();
    push_query_ir_focus_values(
        query_ir.temporal_constraints.iter().flat_map(temporal_constraint_focus_values),
        &mut seen,
        &mut primary_values,
    );
    let focus_uses_target_entities = query_ir_has_focused_document_answer_intent(query_ir)
        && !query_ir.target_entities.is_empty();
    if !focus_uses_target_entities {
        push_query_ir_focus_values(
            query_ir.literal_constraints.iter().map(|literal| literal.text.clone()),
            &mut seen,
            &mut primary_values,
        );
    }
    let (primary_entity_values, modifier_entity_values) =
        query_ir_entity_focus_value_groups(query_ir);
    push_query_ir_focus_values(primary_entity_values, &mut seen, &mut primary_values);
    push_query_ir_focus_values(modifier_entity_values, &mut seen, &mut modifier_values);
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

fn linked_anchor_scored_labels(
    chunks: &[RuntimeMatchedChunk],
    focus_tokens: &BTreeSet<String>,
) -> Vec<(usize, String)> {
    let mut seen = BTreeSet::new();
    let mut scored = chunks
        .iter()
        .flat_map(|chunk| {
            markdown_link_labels(&chunk.source_text)
                .into_iter()
                .chain(markdown_link_labels(&chunk.excerpt))
        })
        .map(|label| label.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|label| {
            is_usable_query_ir_focus(label)
                && label.chars().count() <= 120
                && seen.insert(label.to_lowercase())
        })
        .filter_map(|label| {
            let overlap = linked_anchor_token_overlap(&label, focus_tokens);
            (overlap > 0).then_some((overlap, label))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    scored
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

    let scored_labels = linked_anchor_scored_labels(chunks, &focus_tokens);
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
    fn semantic_latest_unscoped_tail_orders_each_document_by_version_not_chunk_index() {
        let history = document_row("neutral-history.html", "Neutral release history");
        let revision_id = canonical_document_revision_id(&history).unwrap();
        let rows = vec![
            LatestVersionSemanticRow {
                version: vec![1, 0, 1],
                document_rank: 1,
                structural_density_score: 80,
                from_structural_inventory: true,
                row: chunk_row(
                    history.workspace_id,
                    history.library_id,
                    history.document_id,
                    revision_id,
                    100,
                    "Version 1.0.1 | older change",
                ),
            },
            LatestVersionSemanticRow {
                version: vec![1, 0, 9],
                document_rank: 1,
                structural_density_score: 80,
                from_structural_inventory: true,
                row: chunk_row(
                    history.workspace_id,
                    history.library_id,
                    history.document_id,
                    revision_id,
                    10,
                    "Version 1.0.9 | newer change",
                ),
            },
        ];

        let ordered = order_latest_version_semantic_rows(rows, true, 2);

        assert_eq!(ordered[0].version, vec![1, 0, 9]);
        assert_eq!(ordered[1].version, vec![1, 0, 1]);
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
        query_ir.target_types = vec![QueryTargetKind::Procedure];

        assert!(query_ir_requests_setup_focus_document_candidates(&query_ir));
    }

    #[test]
    fn configure_procedure_queries_without_focus_or_setup_target_skip_setup_focus() {
        // Without an explicit document_focus and without a command-object/configuration target
        // type, a bare procedure-tagged configure query stays out of the
        // focused-document lane so broad how-to questions are not over-scoped.
        let mut query_ir = setup_query_ir("Sample Subject");
        query_ir.target_types = vec![QueryTargetKind::Procedure];
        query_ir.document_focus = None;
        query_ir.target_entities =
            vec![EntityMention { label: "settings".to_string(), role: EntityRole::Object }];

        assert!(!query_ir_requests_setup_focus_document_candidates(&query_ir));
    }

    #[test]
    fn configure_subject_queries_without_setup_target_request_setup_focus_candidates() {
        let mut query_ir = setup_query_ir("Sample Subject");
        query_ir.target_types = vec![QueryTargetKind::Concept, QueryTargetKind::Procedure];
        query_ir.document_focus = None;
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Subject".to_string(), role: EntityRole::Subject }];

        assert!(query_ir_requests_setup_focus_document_candidates(&query_ir));
        assert_eq!(setup_focus_query_identity_terms(&query_ir), ["Sample Subject".to_string()]);
    }

    #[test]
    fn configure_subject_setup_focus_skips_ambiguous_subject_context() {
        let mut query_ir = setup_query_ir("Sample Subject");
        query_ir.target_types = vec![QueryTargetKind::Concept, QueryTargetKind::Procedure];
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
        query_ir.target_types = vec![QueryTargetKind::Concept, QueryTargetKind::Procedure];
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
        query_ir.target_types = vec![QueryTargetKind::Concept, QueryTargetKind::Procedure];
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
        query_ir.target_types = vec![QueryTargetKind::Concept, QueryTargetKind::Procedure];
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
        query_ir.target_types = vec![QueryTargetKind::Concept];
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
            vec![QueryTargetKind::Concept, QueryTargetKind::Artifact, QueryTargetKind::Procedure];
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
        query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Version];
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
        query_ir.target_types = vec![
            QueryTargetKind::Package,
            QueryTargetKind::ConfigurationFile,
            QueryTargetKind::Procedure,
        ];
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
            vec![QueryTargetKind::Concept, QueryTargetKind::Artifact, QueryTargetKind::Procedure];
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
    fn low_confidence_untyped_ir_does_not_enable_setup_focus_lane() {
        let mut query_ir = setup_query_ir("Sample Subject");
        query_ir.act = QueryAct::Describe;
        query_ir.confidence = 0.25;
        query_ir.target_types.clear();
        query_ir.target_entities.clear();
        query_ir.document_focus = None;

        assert!(!query_ir_requests_setup_focus_document_candidates(&query_ir));
    }

    #[test]
    fn untyped_query_does_not_activate_latest_version_selection_from_retrieved_evidence() {
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
        let _retrieved_chunks = [
            setup_focus_runtime_chunk(alpha_mid.document_id, 0, 1.0),
            setup_focus_runtime_chunk(alpha_new.document_id, 0, 1.0),
            setup_focus_runtime_chunk(beta_new.document_id, 0, 1.0),
        ];
        let query_ir = low_confidence_untyped_query_ir();

        let selection = select_latest_version_documents(Some(&query_ir), &index);

        assert_eq!(selection.requested_count, 0);
        assert!(selection.documents.is_empty());
    }

    #[test]
    fn typed_latest_version_query_selects_requested_inventory_window() {
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
        let mut query_ir = low_confidence_untyped_query_ir();
        query_ir.act = QueryAct::Enumerate;
        query_ir.target_types = vec![QueryTargetKind::Release, QueryTargetKind::Version];
        query_ir.source_slice = Some(crate::domains::query_ir::SourceSliceSpec {
            direction: crate::domains::query_ir::SourceSliceDirection::Tail,
            count: Some(10),
            filter: crate::domains::query_ir::SourceSliceFilter::ReleaseMarker,
        });

        let selection = select_latest_version_documents(Some(&query_ir), &index);

        assert_eq!(selection.requested_count, 10);
        assert_eq!(selection.documents.len(), 10);
        assert_eq!(
            selection.documents.first().map(|document| document.title.as_str()),
            Some("Sample Subject 2.11.0 changelog")
        );
    }

    #[test]
    fn typed_latest_version_query_keeps_title_variants_from_distinct_documents() {
        let titles = [
            "Delta Suite 9.0.5",
            "Delta Suite 9.0.5 - Delta Suite Administration",
            "Delta Suite 9.0.4",
            "Delta Suite 9.0.3 - Delta Suite Administration",
            "Delta Suite 9.0.2 - Delta Suite Administration",
            "Delta Suite 9.0.1 - Delta Suite Administration",
        ];
        let documents = titles
            .into_iter()
            .enumerate()
            .map(|(index, title)| document_row(&format!("delta-{index}.md"), title))
            .collect::<Vec<_>>();
        let index = documents
            .into_iter()
            .map(|document| (document.document_id, document))
            .collect::<HashMap<_, _>>();
        let mut query_ir = low_confidence_untyped_query_ir();
        query_ir.act = QueryAct::Enumerate;
        query_ir.target_types = vec![QueryTargetKind::Release, QueryTargetKind::Version];
        query_ir.source_slice = Some(crate::domains::query_ir::SourceSliceSpec {
            direction: crate::domains::query_ir::SourceSliceDirection::Tail,
            count: Some(5),
            filter: crate::domains::query_ir::SourceSliceFilter::ReleaseMarker,
        });

        let selection = select_latest_version_documents(Some(&query_ir), &index);
        let versions =
            selection.documents.iter().map(|document| document.version.clone()).collect::<Vec<_>>();

        assert_eq!(
            versions,
            vec![vec![9, 0, 5], vec![9, 0, 5], vec![9, 0, 4], vec![9, 0, 3], vec![9, 0, 2]]
        );
    }

    #[test]
    fn typed_latest_version_query_does_not_infer_dominance_from_titles() {
        let titles = [
            "Delta Suite 3.0.3",
            "Delta Suite 3.0.2 - Delta Suite Administration",
            "Delta Suite 3.0.1 - Delta Suite Administration",
            "Omega Stack 99.0.2",
            "Omega Stack 99.0.1",
        ];
        let documents = titles
            .into_iter()
            .enumerate()
            .map(|(index, title)| document_row(&format!("family-{index}.md"), title))
            .collect::<Vec<_>>();
        let index = documents
            .into_iter()
            .map(|document| (document.document_id, document))
            .collect::<HashMap<_, _>>();
        let selected = latest_version_documents(&index, 3, &[]);

        assert_eq!(selected.len(), 3);
        assert!(selected.iter().any(|document| document.title.starts_with("Delta Suite")));
        assert!(selected.iter().any(|document| document.title.starts_with("Omega Stack")));
        assert_eq!(selected[0].version, vec![99, 0, 2]);
        assert_eq!(selected[2].version, vec![3, 0, 3]);
    }

    #[test]
    fn latest_version_document_dedupe_does_not_collapse_title_extensions() {
        let version = vec![7, 7, 7];
        let extended_title = "Family 0 Suite 7.7.7 - Family Administration";
        let mut rows = vec![LatestVersionDocument {
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            version: version.clone(),
            title: extended_title.to_string(),
        }];
        rows.extend((0..2_048).map(|index| {
            let title = format!("Family {index} Suite 7.7.7");
            LatestVersionDocument {
                document_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                version: version.clone(),
                title,
            }
        }));

        let deduped = dedupe_latest_version_documents(rows);

        assert_eq!(deduped.len(), 2_049);
        assert_eq!(
            deduped.iter().filter(|document| document.title.contains("Family 0 Suite")).count(),
            2
        );
    }

    #[test]
    fn latest_version_document_dedupe_keeps_exact_titles_from_distinct_documents() {
        let title = "Delta Suite 9.0.5";
        let rows = vec![
            LatestVersionDocument {
                document_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                version: vec![9, 0, 5],
                title: title.to_string(),
            },
            LatestVersionDocument {
                document_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                version: vec![9, 0, 5],
                title: title.to_string(),
            },
        ];

        let deduped = dedupe_latest_version_documents(rows);

        assert_eq!(deduped.len(), 2);
    }

    #[test]
    fn latest_version_document_dedupe_collapses_same_document_revision() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let rows = vec![
            LatestVersionDocument {
                document_id,
                revision_id,
                version: vec![9, 0, 5],
                title: "Delta 9.0.5".to_string(),
            },
            LatestVersionDocument {
                document_id,
                revision_id,
                version: vec![9, 0, 5],
                title: "Delta 9.0.5 - Administration".to_string(),
            },
        ];

        let deduped = dedupe_latest_version_documents(rows);

        assert_eq!(deduped.len(), 1);
    }

    #[test]
    fn config_queries_reserve_source_context_chunks_during_truncation() {
        let document_id = Uuid::now_v7();
        let mut query_ir = setup_query_ir("Subject Alpha");
        query_ir.target_types =
            vec![QueryTargetKind::ConfigurationFile, QueryTargetKind::Parameter];

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
        query_ir.target_types = vec![QueryTargetKind::TableRow, QueryTargetKind::TableSummary];
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
    fn untyped_entity_identity_does_not_activate_setup_focus_lane() {
        let mut query_ir = setup_query_ir("");
        query_ir.act = QueryAct::Describe;
        query_ir.scope = QueryScope::MultiDocument;
        query_ir.target_types.clear();
        query_ir.target_entities =
            vec![EntityMention { label: "Alpha Connector".to_string(), role: EntityRole::Subject }];
        query_ir.document_focus = None;
        query_ir.confidence = 0.25;

        assert!(!query_ir_requests_setup_focus_document_candidates(&query_ir));
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
            revision_row(document.document_id, older_revision_id, 1, "text_readable"),
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
        };
        let mut semantic_chunk =
            latest_version_runtime_chunk(semantic_document_id, 0, DOCUMENT_IDENTITY_SCORE_FLOOR);
        semantic_chunk.document_label = "Sample Subject changelog".to_string();

        let ids = latest_version_scoped_document_ids(&[structural_document], &[semantic_chunk]);

        assert!(ids.contains(&structural_document_id));
        assert!(ids.contains(&semantic_document_id));
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
        query_ir.target_types = vec![QueryTargetKind::Service];
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
    fn troubleshooting_request_skips_generic_update_runbook_lanes() {
        let mut query_ir = setup_query_ir("Sample operation was already completed");
        query_ir.target_types = vec![
            QueryTargetKind::Procedure,
            QueryTargetKind::Troubleshooting,
            QueryTargetKind::Remediation,
            QueryTargetKind::ErrorMessage,
        ];
        query_ir.literal_constraints = vec![LiteralSpan {
            text: "sample operation was already completed".to_string(),
            kind: LiteralKind::Other,
        }];

        assert!(!question_requests_versioned_update_procedure_evidence(
            "What should I do when the error says sample operation was already completed?",
            Some(&query_ir),
        ));
        assert!(!query_ir_requests_versioned_update_procedure_context("", &query_ir));
    }

    #[test]
    fn typed_procedure_without_typed_subject_does_not_infer_command_seed() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Version];
        query_ir.target_entities.clear();
        query_ir.document_focus = None;
        query_ir.retrieval_query = Some("opaque raw prose".to_string());

        assert!(versioned_update_procedure_focus_model(&query_ir).is_none());
        assert!(!query_ir_requests_versioned_update_procedure_context(
            "opaque raw prose",
            &query_ir,
        ));
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
    fn concept_only_procedure_query_fails_closed_without_version_signal() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.document_focus = None;
        query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Concept];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        query_ir.retrieval_query = Some("how to update Sample Target".to_string());

        assert!(!question_requests_versioned_update_procedure_evidence(
            "how to update Sample Target?",
            Some(&query_ir)
        ));
    }

    #[test]
    fn raw_question_does_not_activate_procedure_runbook_without_typed_identity() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Version];
        query_ir.target_entities.clear();
        query_ir.document_focus = None;
        query_ir.retrieval_query = Some("how to update Sample Target?".to_string());

        assert!(versioned_update_procedure_focus_model(&query_ir).is_none());
        assert!(!question_requests_versioned_update_procedure_evidence(
            "how to update Sample Target?",
            Some(&query_ir),
        ));
    }

    #[test]
    fn identifier_literal_is_an_explicit_procedure_target_identity() {
        let mut query_ir = setup_query_ir("unfocused");
        query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Release];
        query_ir.target_entities.clear();
        query_ir.document_focus = None;
        query_ir.literal_constraints =
            vec![LiteralSpan { text: "Sample Process".to_string(), kind: LiteralKind::Identifier }];

        let focus_model = versioned_update_procedure_focus_model(&query_ir).unwrap();

        assert!(
            focus_model
                .target_identity_sequences
                .iter()
                .any(|sequence| sequence == &vec!["sample".to_string(), "process".to_string()])
        );
        assert!(query_ir_requests_versioned_update_procedure_context("", &query_ir));

        query_ir.literal_constraints[0].kind = LiteralKind::Other;
        assert!(versioned_update_procedure_focus_model(&query_ir).is_some());

        query_ir.literal_constraints[0].kind = LiteralKind::Version;
        assert!(versioned_update_procedure_focus_model(&query_ir).is_none());
    }

    #[test]
    fn typed_versioned_procedure_accepts_exact_identity_and_formal_steps() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Version];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        let focus_model = versioned_update_procedure_focus_model(&query_ir).unwrap();
        let evidence = versioned_update_procedure_text_evidence(
            "Sample Target reference",
            concat!(
                "1. alpha-admin prepare --target=/srv/alpha\n",
                "2. alpha-admin verify --format=json",
            ),
            &focus_model,
        );

        assert!(query_ir_requests_versioned_update_procedure_context("", &query_ir));
        assert!(evidence.label_has_target_identity);
        assert_eq!(evidence.ordered_step_score, 2);
        assert!(versioned_update_procedure_evidence_supports_runbook(evidence));
    }

    #[test]
    fn typed_versioned_procedure_rejects_fuzzy_identity_and_unstructured_prose() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Release];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        let focus_model = versioned_update_procedure_focus_model(&query_ir).unwrap();
        let fuzzy_identity = versioned_update_procedure_text_evidence(
            "Sample Targets reference",
            "1. alpha-admin prepare\n2. alpha-admin verify",
            &focus_model,
        );
        let prose_only = versioned_update_procedure_text_evidence(
            "Sample Target reference",
            "A paragraph describes a possible lifecycle transition without formal steps.",
            &focus_model,
        );

        assert!(!fuzzy_identity.label_has_target_identity);
        assert!(!versioned_update_procedure_text_has_target_identity_sequence(
            "1. alpha-admin prepare\n2. alpha-admin verify",
            &focus_model,
        ));
        assert!(!versioned_update_procedure_evidence_supports_runbook(fuzzy_identity,));
        assert!(!versioned_update_procedure_evidence_supports_runbook(prose_only,));
    }

    #[test]
    fn typed_versioned_procedure_rejects_unbound_materialization_script() {
        let mut query_ir = setup_query_ir("Sample Target");
        query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Version];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        let focus_model = versioned_update_procedure_focus_model(&query_ir).unwrap();
        let evidence = versioned_update_procedure_text_evidence(
            "Sample Target reference",
            concat!(
                "curl https://example.invalid/runner.sh -o /tmp/runner.sh\n",
                "chmod +x /tmp/runner.sh\n",
                "/tmp/runner.sh",
            ),
            &focus_model,
        );

        assert!(evidence.has_setup_script_signature);
        assert_eq!(evidence.ordered_step_score, 0);
        assert!(!versioned_update_procedure_evidence_supports_runbook(evidence,));
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
        query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Version];
        query_ir.target_entities =
            vec![EntityMention { label: "Sample Target".to_string(), role: EntityRole::Subject }];
        query_ir.retrieval_query = Some("how to update Sample Target?".to_string());

        let setup_anchor = chunks
            .iter()
            .find(|chunk| chunk.chunk_id == setup_anchor_id)
            .expect("setup anchor fixture");
        assert!(
            versioned_update_exact_target_runbook_score("", &query_ir, setup_anchor).is_none(),
            "an unordered package/config anchor is setup evidence, not a versioned runbook"
        );

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
            target_types: vec![QueryTargetKind::Package, QueryTargetKind::ConfigurationFile],
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
    rrf_merge_chunks(left, right, top_k, RetrievalMergeLane::Default)
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
        .filter_map(|target| graph_evidence_target_type_min_entities(*target))
        .min();
    graph_target_min_entities
        .is_some_and(|min_entities| query_ir.target_entities.len() >= min_entities)
}

const fn graph_evidence_target_type_min_entities(target: QueryTargetKind) -> Option<usize> {
    match target {
        QueryTargetKind::Artifact => Some(1),
        QueryTargetKind::Relationship
        | QueryTargetKind::Entity
        | QueryTargetKind::Event
        | QueryTargetKind::Route
        | QueryTargetKind::Transition => Some(2),
        _ => None,
    }
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
    let values = fuse_rrf_chunks(vector_hits, lexical_hits, right_lane);
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

#[cfg(test)]
pub(crate) fn truncate_bundle(
    bundle: &mut RetrievalBundle,
    top_k: usize,
    query_ir: Option<&QueryIR>,
    demoted_document_ids: &HashSet<Uuid>,
) {
    truncate_bundle_with_semantic_chunk_ranks(
        bundle,
        top_k,
        query_ir,
        demoted_document_ids,
        &HashMap::new(),
    );
}

pub(crate) fn truncate_bundle_with_semantic_chunk_ranks(
    bundle: &mut RetrievalBundle,
    top_k: usize,
    query_ir: Option<&QueryIR>,
    demoted_document_ids: &HashSet<Uuid>,
    semantic_chunk_ranks: &HashMap<Uuid, usize>,
) {
    bundle.entities.truncate(entity_context_top_k(top_k, query_ir));
    bundle.relationships.truncate(top_k);
    truncate_chunks_for_context_with_semantic_ranks(
        &mut bundle.chunks,
        top_k,
        query_ir,
        demoted_document_ids,
        semantic_chunk_ranks,
    );
}

fn semantic_chunk_rank_order(
    left: &RuntimeMatchedChunk,
    right: &RuntimeMatchedChunk,
    semantic_chunk_ranks: &HashMap<Uuid, usize>,
) -> std::cmp::Ordering {
    match (semantic_chunk_ranks.get(&left.chunk_id), semantic_chunk_ranks.get(&right.chunk_id)) {
        (Some(left_rank), Some(right_rank)) => left_rank.cmp(right_rank),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
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

#[cfg(test)]
fn truncate_chunks_for_context(
    chunks: &mut Vec<RuntimeMatchedChunk>,
    top_k: usize,
    query_ir: Option<&QueryIR>,
    demoted_document_ids: &HashSet<Uuid>,
) {
    truncate_chunks_for_context_with_semantic_ranks(
        chunks,
        top_k,
        query_ir,
        demoted_document_ids,
        &HashMap::new(),
    );
}

fn truncate_chunks_for_context_with_semantic_ranks(
    chunks: &mut Vec<RuntimeMatchedChunk>,
    top_k: usize,
    query_ir: Option<&QueryIR>,
    demoted_document_ids: &HashSet<Uuid>,
    semantic_chunk_ranks: &HashMap<Uuid, usize>,
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
            // Semantic rank is a selection signal, not a replacement score.
            // Canonical demotion and protected evidence lanes remain stronger;
            // within one lane, the scored provider prefix precedes raw-score
            // fallback while every provenance score stays untouched.
            .then_with(|| semantic_chunk_rank_order(left, right, semantic_chunk_ranks))
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
    query_ir.targets_any(&[
        QueryTargetKind::ConfigurationFile,
        QueryTargetKind::ConfigKey,
        QueryTargetKind::Connection,
        QueryTargetKind::Endpoint,
        QueryTargetKind::FilesystemPath,
        QueryTargetKind::HttpMethod,
        QueryTargetKind::Package,
        QueryTargetKind::Parameter,
        QueryTargetKind::Path,
        QueryTargetKind::Port,
        QueryTargetKind::Protocol,
        QueryTargetKind::Url,
        QueryTargetKind::Wsdl,
    ]) || query_ir
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

fn append_reserved_procedure_neighbors(
    indexed: &[(usize, RuntimeMatchedChunk)],
    anchor: &RuntimeMatchedChunk,
    limit: usize,
    seen_chunk_ids: &mut HashSet<Uuid>,
    selected: &mut Vec<(usize, RuntimeMatchedChunk)>,
) {
    let neighbors = indexed.iter().filter(|(_, candidate)| {
        versioned_update_procedure_reservation_candidate_kind(candidate)
            && candidate.document_id == anchor.document_id
            && candidate.revision_id == anchor.revision_id
            && candidate.chunk_index > anchor.chunk_index
    });
    for (neighbor_index, neighbor) in
        neighbors.take(VERSIONED_UPDATE_PROCEDURE_RESERVED_NEIGHBORS_PER_ANCHOR)
    {
        if selected.len() >= limit {
            break;
        }
        if seen_chunk_ids.insert(neighbor.chunk_id) {
            selected.push((*neighbor_index, neighbor.clone()));
        }
    }
}

fn reserved_versioned_update_procedure_chunks(
    indexed: &[(usize, RuntimeMatchedChunk)],
    top_k: usize,
    query_ir: &QueryIR,
) -> Vec<(usize, RuntimeMatchedChunk)> {
    if top_k < 2 || !query_ir_requests_versioned_update_procedure_context("", query_ir) {
        return Vec::new();
    }
    let Some(focus_model) = versioned_update_procedure_focus_model(query_ir) else {
        return Vec::new();
    };
    let limit =
        VERSIONED_UPDATE_PROCEDURE_DOCUMENT_EVIDENCE_RESERVATION_LIMIT.min(top_k.saturating_sub(1));
    let mut scored = indexed
        .iter()
        .filter(|(_, chunk)| versioned_update_procedure_reservation_candidate_kind(chunk))
        .filter_map(|(index, chunk)| {
            let evidence = versioned_update_procedure_chunk_runbook_evidence(chunk, &focus_model)?;
            Some((
                evidence.label_has_target_identity,
                evidence.score,
                evidence.ordered_step_score,
                evidence.command_score,
                evidence.version_transition_score,
                *index,
                chunk.clone(),
            ))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.1.cmp(&left.1))
            .then_with(|| right.2.cmp(&left.2))
            .then_with(|| right.3.cmp(&left.3))
            .then_with(|| right.4.cmp(&left.4))
            .then_with(|| score_desc_chunks(&left.6, &right.6))
            .then_with(|| left.5.cmp(&right.5))
    });

    let mut selected = Vec::<(usize, RuntimeMatchedChunk)>::new();
    let mut seen_chunk_ids = HashSet::<Uuid>::new();
    let mut seen_document_ids = HashSet::<Uuid>::new();
    for (_, _, _, _, _, index, chunk) in &scored {
        if selected.len() >= VERSIONED_UPDATE_PROCEDURE_SUBJECT_TITLE_RESERVE_CAP.min(limit) {
            break;
        }
        if seen_document_ids.insert(chunk.document_id) && seen_chunk_ids.insert(chunk.chunk_id) {
            selected.push((*index, chunk.clone()));
        }
    }
    for (_, _, _, _, _, index, chunk) in scored {
        if selected.len() >= limit {
            break;
        }
        if seen_chunk_ids.insert(chunk.chunk_id) {
            selected.push((index, chunk.clone()));
        }
        append_reserved_procedure_neighbors(
            indexed,
            &chunk,
            limit,
            &mut seen_chunk_ids,
            &mut selected,
        );
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
    if !query_ir_requests_versioned_update_procedure_context("", query_ir) {
        return None;
    }
    indexed
        .iter()
        .filter_map(|(index, chunk)| {
            let runbook_score = versioned_update_exact_target_runbook_score("", query_ir, chunk)?;
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
        || query_ir.targets(QueryTargetKind::Connection);
    let requests_configuration_context = has_question_intent(&intents, QuestionIntent::ConfigKey)
        || has_question_intent(&intents, QuestionIntent::Parameter)
        || query_ir.targets_any(&[
            QueryTargetKind::ConfigurationFile,
            QueryTargetKind::ConfigKey,
            QueryTargetKind::Parameter,
        ]);
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
    query_ir.targets(QueryTargetKind::TableRow) && query_ir.targets(QueryTargetKind::TableSummary)
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

fn append_distinct_document_ids(
    chunks: &[(usize, RuntimeMatchedChunk)],
    documents: &mut Vec<Uuid>,
    limit: usize,
) {
    let mut selected = documents.iter().copied().collect::<HashSet<_>>();
    for document_id in chunks.iter().map(|(_, chunk)| chunk.document_id) {
        if selected.insert(document_id) {
            documents.push(document_id);
        }
        if documents.len() >= limit {
            break;
        }
    }
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
        append_distinct_document_ids(chunks, &mut relevant_documents, 2);
    }
    if relevant_documents.is_empty() {
        append_distinct_document_ids(chunks, &mut relevant_documents, top_k);
    }
    relevant_documents.truncate(top_k);
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
    terms.into_iter().collect()
}

#[cfg(test)]
#[path = "retrieve_tests.rs"]
mod tests;
