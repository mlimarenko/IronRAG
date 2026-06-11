use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::Context;
use chrono::{DateTime, Datelike, Utc};
use futures::future::join_all;
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
        arangodb::document_store::{
            KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeLibraryGenerationRow,
            KnowledgeRevisionRow,
        },
        repositories,
    },
    services::{
        content::document_hint::resolve_document_hint,
        knowledge::runtime_read::load_active_runtime_graph_projection,
        query::{
            effective_query::structured_current_question_segment,
            latest_versions::{
                LATEST_VERSION_CHUNKS_PER_DOCUMENT, compare_version_desc,
                extract_semver_like_version, latest_version_chunk_score,
                latest_version_context_top_k, latest_version_family_key,
                latest_version_scope_terms, query_requests_latest_versions,
                requested_latest_version_count, text_has_release_version_marker,
            },
            planner::{RuntimeQueryPlan, strip_leading_question_marker},
            text_match::{
                common_prefix_char_count, near_token_overlap_count,
                normalized_alnum_token_sequence, normalized_alnum_tokens, token_sequence_contains,
            },
            vector_dimensions::{
                library_vector_index_dimensions, validate_embedding_vector_dimensions,
            },
        },
    },
    shared::extraction::text_render::repair_technical_layout_noise,
};

use super::question_intent::{
    QuestionIntent, canonical_target_type_tag, classify_query_ir_intents, has_question_intent,
    query_ir_has_focused_document_answer_intent,
};
use super::source_profile::is_source_profile_chunk_row;
use super::technical_literals::{
    extract_explicit_path_literals, extract_package_command_literals, extract_parameter_literals,
    technical_literal_focus_keyword_segments,
};
use super::tuning::{DOCUMENT_IDENTITY_SCORE_FLOOR, FOCUS_BROADEN_MIN_CHUNKS};
use super::types::*;
use super::{
    GraphTargetEntityCoverageField, GraphTargetEntityCoverageFieldKind, GraphTargetEntityProfile,
    associative_edges_for_entities, focus_token_overlap_count, graph_target_entity_coverage_score,
    load_initial_table_rows_for_documents, load_table_rows_for_documents,
    load_table_summary_chunks_for_documents, merge_canonical_table_aggregation_chunks,
    query_ir_document_focus_tokens, query_relevant_graph_evidence_target_hits,
    question_asks_table_aggregation, requested_initial_table_row_count,
    resolve_scoped_target_document_ids,
};

const DIRECT_TABLE_AGGREGATION_SUMMARY_LIMIT: usize = 32;
const DIRECT_TABLE_AGGREGATION_ROW_LIMIT: usize = 24;
const DIRECT_TABLE_AGGREGATION_CHUNK_LIMIT: usize = 32;
const DOCUMENT_IDENTITY_CHUNKS_PER_DOCUMENT: usize = 3;
const DOCUMENT_IDENTITY_FOCUSED_CHUNKS_PER_DOCUMENT: usize = 4;
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
        key: row.document_id.to_string(),
        arango_id: None,
        arango_rev: None,
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
            .arango_search_store
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
    // Post-filter: ArangoSearch BM25 stems tokens, so a surname like
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
        .arango_document_store
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
        .arango_document_store
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
            .arango_search_store
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
    let raw_question_tokens =
        (!typed_setup_focus).then(|| raw_question_setup_focus_tokens(question));
    let mut candidate_document_ids = if typed_setup_focus || structural_setup_focus {
        let candidate_document_ids = setup_focus_candidate_document_ids(
            query_ir,
            document_index,
            SETUP_FOCUS_DOCUMENT_CANDIDATE_CAP,
        );
        if candidate_document_ids.is_empty() && structural_setup_focus {
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
            .arango_document_store
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
        let mut document_score = setup_focus_document_candidate_score(&selected_rows, query_ir);
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
    if query_ir.source_slice.is_some() || !query_ir_has_setup_focus_identity(query_ir) {
        return false;
    }
    // A configure/how-to intent qualifies for the focused-document lane when it
    // either declares a package/config/parameter target type, or carries an
    // explicit document_focus. The latter covers questions the compiler tagged
    // only as a generic procedure (target_types=["procedure"]) but that still
    // point at one specific document — without it, the focused document's own
    // install/config chunks get diluted by a multi-document salad. The lane is
    // a soft RRF boost that already demotes standalone-image documents, so a
    // broad configure query (no focus, no package/config target) still skips it.
    if matches!(query_ir.act, QueryAct::ConfigureHow)
        && (query_ir_has_setup_focus_target(query_ir) || query_ir.document_focus.is_some())
    {
        return true;
    }
    let mut has_package_target = false;
    let mut has_configuration_target = false;
    for target_type in &query_ir.target_types {
        match canonical_target_type_tag(target_type).as_str() {
            "package" => has_package_target = true,
            "configuration_file" | "config_key" => has_configuration_target = true,
            _ => {}
        }
    }
    has_package_target && has_configuration_target
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
        || query_ir.target_entities.iter().any(|entity| {
            matches!(entity.role, EntityRole::Subject)
                && normalized_alnum_tokens(entity.label.trim(), 3).len() >= 2
        })
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
    let focus_terms = setup_focus_query_identity_terms(query_ir)
        .into_iter()
        .filter_map(|term| {
            let tokens = normalized_alnum_tokens(&term, 3);
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
                            let value_tokens = normalized_alnum_tokens(&value, 3);
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

fn raw_question_setup_focus_tokens(question: &str) -> BTreeSet<String> {
    normalized_alnum_tokens(strip_leading_question_marker(question), 3)
}

fn raw_question_setup_focus_candidate_document_ids(
    question_tokens: Option<&BTreeSet<String>>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    limit: usize,
) -> Vec<Uuid> {
    let Some(question_tokens) = question_tokens else {
        return Vec::new();
    };
    if question_tokens.is_empty() || limit == 0 {
        return Vec::new();
    }
    let distinctive_tokens =
        raw_setup_focus_distinctive_question_tokens(question_tokens, document_index);
    if distinctive_tokens.is_empty() {
        return Vec::new();
    }
    let mut candidates = document_index
        .values()
        .filter(|document| !setup_focus_document_is_standalone_image(document))
        .filter_map(|document| {
            let score = raw_question_setup_focus_document_score(&distinctive_tokens, document);
            (score > 0).then_some((score, document.document_id))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|(left_score, left_id), (right_score, right_id)| {
        right_score.cmp(left_score).then_with(|| left_id.cmp(right_id))
    });
    candidates.into_iter().map(|(_, document_id)| document_id).take(limit).collect()
}

fn raw_question_setup_focus_document_score(
    question_tokens: &BTreeSet<String>,
    document: &KnowledgeDocumentRow,
) -> usize {
    setup_focus_document_identity_values(document)
        .into_iter()
        .map(|value| {
            let value_tokens = normalized_alnum_tokens(&value, 3);
            let overlap = question_tokens.intersection(&value_tokens).count();
            if overlap < 2 {
                return 0;
            }
            overlap.saturating_mul(32).saturating_add(value_tokens.len().min(16))
        })
        .max()
        .unwrap_or(0)
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
            for token in normalized_alnum_tokens(&value, 3) {
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
    let package_count = extract_package_command_literals(&text, 2).len();
    let configuration_path_count = setup_focus_configuration_path_count(&text);
    let assignment_count = setup_focus_parameter_assignment_count(&text);
    let parameter_literal_count = setup_focus_parameter_literal_count(&text);
    let section_count = setup_focus_section_header_count(&text);
    let url_count = setup_focus_url_count(&text);

    package_count
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
    for entity in &query_ir.target_entities {
        let label = entity.label.trim();
        if !label.is_empty() && seen.insert(label.to_lowercase()) {
            terms.push(label.to_string());
        }
    }
    if let Some(document_focus) = &query_ir.document_focus {
        let focus = document_focus.hint.trim();
        if !focus.is_empty() && seen.insert(focus.to_lowercase()) {
            terms.push(focus.to_string());
        }
    }
    for literal in &query_ir.literal_constraints {
        let literal = literal.text.trim();
        if !literal.is_empty() && seen.insert(literal.to_lowercase()) {
            terms.push(literal.to_string());
        }
    }
    terms
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
            candidates.push((score, row));
        }
    }
    for anchor in rows.iter().filter(|row| setup_focus_row_has_package_and_path(row)) {
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

fn setup_focus_row_has_package_and_path(row: &KnowledgeChunkRow) -> bool {
    let text = setup_focus_row_text(row);
    let package_count = extract_package_command_literals(&text, 2).len();
    let configuration_path_count = setup_focus_configuration_path_count(&text);
    let setup_signal_count = package_count
        .saturating_add(setup_focus_parameter_assignment_count(&text))
        .saturating_add(setup_focus_parameter_literal_count(&text))
        .saturating_add(setup_focus_section_header_count(&text))
        .saturating_add(setup_focus_url_count(&text));
    configuration_path_count > 0 && setup_signal_count > 0
}

fn setup_focus_row_score(row: &KnowledgeChunkRow) -> usize {
    let text = setup_focus_row_text(row);
    let package_count = extract_package_command_literals(&text, 2).len();
    let configuration_path_count = setup_focus_configuration_path_count(&text);
    let assignment_count = setup_focus_parameter_assignment_count(&text);
    let parameter_literal_count = setup_focus_parameter_literal_count(&text);
    let section_count = setup_focus_section_header_count(&text);
    let url_count = setup_focus_url_count(&text);
    package_count
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
    let package_count = extract_package_command_literals(&chunk.source_text, 2).len() as f32;
    let path_count = setup_focus_configuration_path_count(&chunk.source_text) as f32;
    let assignment_count = setup_focus_parameter_assignment_count(&chunk.source_text) as f32;
    let parameter_literal_count = setup_focus_parameter_literal_count(&chunk.source_text) as f32;
    let section_count = setup_focus_section_header_count(&chunk.source_text) as f32;
    let url_count = setup_focus_url_count(&chunk.source_text) as f32;
    SETUP_FOCUS_DOCUMENT_SCORE_BASE
        + package_count * 1_000.0
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
            .arango_search_store
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
            .arango_document_store
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
        for row in state
            .arango_document_store
            .list_chunks_by_revisions_windows(&windows)
            .await
            .with_context(|| {
                format!(
                    "failed to load artifact sibling source head chunks for document {}",
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

    if !focus_terms.is_empty() && rows.len() < chunk_cap {
        for row in state
            .arango_document_store
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
            .arango_document_store
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
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let initial_table_row_count = requested_initial_table_row_count(query_ir);
    let targeted_table_aggregation =
        question_asks_table_aggregation(question, query_ir) && !targeted_document_ids.is_empty();
    let query_ir_focus_queries = query_ir.map(query_ir_lexical_focus_queries).unwrap_or_default();
    let lexical_queries = build_lexical_queries(question, plan, &query_ir_focus_queries, query_ir);
    let lexical_limit = limit.saturating_mul(2).max(24);
    let plan_keywords = &plan.keywords;
    let targeted_document_ids_ref = &targeted_document_ids;
    // Resolved temporal bounds — applied as AQL hard-filter on every
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
            .arango_search_store
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
        // Batch-hydrate all hits in one `list_chunks_by_ids` call to
        // avoid an N+1 Arango round-trip per vector match.
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

    // Run lexical queries concurrently so the Arango coordinator can
    // fan them out; the RRF merge below preserves output order.
    let lexical_future = async {
        let started = std::time::Instant::now();
        let lexical_query_count = lexical_queries.len();
        // Fan the AQL searches out in parallel — same as before — but
        // hydrate each query's hits through `batch_hydrate_hits` to
        // replace the per-hit `get_chunk` N+1 with a single
        // `list_chunks_by_ids` round-trip. With 4 lexical queries × ~20
        // hits each the old path fired ~80 serial chunk loads per
        // request; now it's at most 4 batched reads.
        let per_query_futures = lexical_queries.into_iter().map(|lexical_query| async move {
            let hits = state
                .arango_search_store
                .search_chunks(library_id, &lexical_query, lexical_limit, temporal_start, temporal_end)
                .await
                .with_context(|| {
                    format!(
                        "failed to run lexical Arango chunk search for runtime query: {lexical_query}"
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
    let latest_version_document_ids = latest_version_documents
        .iter()
        .map(|document| document.document_id)
        .collect::<BTreeSet<_>>();
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
    let (
        document_identity_result,
        latest_version_result,
        entity_bio_result,
        query_ir_focus_chunks_result,
        setup_focus_document_chunks_result,
    ) = tokio::join!(
        document_identity_future,
        latest_version_future,
        entity_bio_future,
        query_ir_focus_future,
        setup_focus_document_future,
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
    let latest_version_chunks = latest_version_result?;
    if !latest_version_chunks.is_empty() {
        let latest_version_context_limit = if latest_version_selection
            .inferred_from_retrieved_evidence
        {
            limit.max(
                latest_version_requested_count.saturating_mul(LATEST_VERSION_CHUNKS_PER_DOCUMENT),
            )
        } else {
            // query_ir is always Some when explicit latest-version chunks are
            // non-empty (guarded by select_latest_version_documents).
            #[allow(clippy::expect_used)]
            latest_version_context_top_k(query_ir.expect("latest chunks require QueryIR"), limit)
        };
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
            chunks = merge_query_ir_focus_chunks(
                chunks,
                query_ir_focus_chunks,
                query_ir_focus_context_top_k(limit),
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
    let mut setup_focus_document_ids = BTreeSet::new();
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
            setup_focus_document_ids
                .extend(setup_focus_document_chunks.iter().map(|chunk| chunk.document_id));
            if !setup_focus_document_chunks.is_empty() {
                chunks = merge_query_ir_focus_chunks(
                    chunks,
                    setup_focus_document_chunks,
                    query_ir_focus_context_top_k(limit),
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
            chunks = merge_query_ir_focus_chunks(
                chunks,
                linked_anchor_context_chunks,
                query_ir_focus_context_top_k(limit),
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
            chunks = merge_query_ir_focus_chunks(
                chunks,
                artifact_sibling_source_chunks,
                query_ir_focus_context_top_k(limit),
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
    // Setup-focused evidence is already globally bounded by
    // `SETUP_FOCUS_DOCUMENT_CHUNK_CAP`; keep that inventory intact so a
    // how-to answer can see package, config path, table rows, and example
    // blocks. The global setup-focus budget prevents broad variant
    // discovery from multiplying the cap by document count.
    chunks =
        diversify_chunks_by_document(chunks, max_chunks_per_document, &setup_focus_document_ids);
    retain_scoped_documents(&mut chunks, &targeted_document_ids, &latest_version_document_ids);
    // Post-retrieval temporal hard-filter. The lexical and vector lanes
    // already FILTER on `occurred_at` at query time, but companion paths
    // (source_context focused/neighbor expansion, graph entity hydration,
    // RAPTOR / table summary loaders, query-IR focus chunks) bypass that
    // filter and pull chunks regardless of date. When the user explicitly
    // scopes a question to a window we drop any chunk whose underlying
    // `KnowledgeChunkRow.occurred_at` is null OR falls outside the bounds.
    // RuntimeMatchedChunk does not carry temporal data, so we re-query
    // `list_chunks_by_ids` once over the surviving set — single Arango
    // round-trip, no per-chunk lookup. Verified necessary on stage
    // 2026-05-03: image-OCR chunks (no occurred_at) were leaking into
    // "messages in March 2026" answers via source_context companions.
    if temporal_start.is_some() && temporal_end.is_some() && !chunks.is_empty() {
        let chunk_ids: Vec<Uuid> = chunks.iter().map(|c| c.chunk_id).collect();
        let rows = state
            .arango_document_store
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
        ))
        .await;
    }

    Ok(chunks)
}

pub(crate) fn retain_scoped_documents(
    chunks: &mut Vec<RuntimeMatchedChunk>,
    targeted_document_ids: &BTreeSet<Uuid>,
    latest_version_document_ids: &BTreeSet<Uuid>,
) {
    let mut scoped_document_ids = targeted_document_ids.clone();
    scoped_document_ids.extend(latest_version_document_ids.iter().copied());
    if scoped_document_ids.is_empty() {
        return;
    }
    chunks.retain(|chunk| scoped_document_ids.contains(&chunk.document_id));
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
            .arango_document_store
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
            .arango_document_store
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
            .arango_document_store
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
    compare_version_desc(&left.version, &right.version).then_with(|| left.title.cmp(&right.title))
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
/// `RuntimeMatchedChunk` rows with exactly ONE Arango round-trip.
/// The previous `join_all(get_chunk)` pattern turned every hit into a
/// separate coordinator call — on a typical 16-hit vector + 4×20-hit
/// lexical fan-out that was ~100 sequential Arango fetches per
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
    // want to waste network bytes on duplicate filter args.
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
        .arango_document_store
        .list_chunks_by_ids(&chunk_ids)
        .await
        .context("failed to batch-load runtime query chunks")?;
    let mut mapped: Vec<RuntimeMatchedChunk> = Vec::with_capacity(chunk_rows.len());
    for chunk in chunk_rows {
        let Some(score) = score_by_chunk.get(&chunk.chunk_id).copied() else {
            continue;
        };
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

/// Hard cap on the number of lexical AQL searches dispatched to
/// Arango per query. Every additional query is a full
/// `search_chunks` round-trip; with a ~500 ms p50 per query and a
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

    // Priority 1 — the raw user question. Arango's full-text
    // analyser already splits it into relevant tokens; this is the
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
        excerpt: focused_excerpt_for(&source_text, keywords, 280),
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
    if let Some(window) = chunk.window_text.as_deref() {
        if !window.trim().is_empty() {
            return repair_technical_layout_noise(window);
        }
    }
    if chunk.content_text.trim().is_empty() && !chunk.normalized_text.trim().is_empty() {
        return repair_technical_layout_noise(&chunk.normalized_text);
    }
    repair_technical_layout_noise(&chunk.content_text)
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
    fn setup_focus_candidates_match_focused_title_without_unique_document_target() {
        let target = document_row("target.md", "Provider Alpha setup manual");
        let screenshot = document_row("image.png", "Provider Alpha: screenshot");
        let unrelated = document_row("other.md", "Provider Beta setup manual");
        let document_index = HashMap::from([
            (target.document_id, target.clone()),
            (screenshot.document_id, screenshot.clone()),
            (unrelated.document_id, unrelated),
        ]);
        let mut query_ir = setup_query_ir("Provider Alpha");

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
        let focused = document_row("provider-beta-admin-guide.md", "Provider Beta setup reference");
        let generic = document_row("alpha-suite-admin-guide.md", "Alpha Suite administration");
        let screenshot = document_row("provider-beta.png", "Provider Beta checkout");
        let document_index = HashMap::from([
            (focused.document_id, focused.clone()),
            (generic.document_id, generic.clone()),
            (screenshot.document_id, screenshot),
        ]);
        let mut query_ir = setup_query_ir("Alpha Suite");
        query_ir.target_entities =
            vec![EntityMention { label: "Provider Beta".to_string(), role: EntityRole::Object }];

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
        let mut query_ir = setup_query_ir("Alpha Suite");
        query_ir.target_types = vec!["procedure".to_string()];

        assert!(query_ir_requests_setup_focus_document_candidates(&query_ir));
    }

    #[test]
    fn configure_procedure_queries_without_focus_or_setup_target_skip_setup_focus() {
        // Without an explicit document_focus and without a package/config target
        // type, a bare procedure-tagged configure query stays out of the
        // focused-document lane so broad how-to questions are not over-scoped.
        let mut query_ir = setup_query_ir("Alpha Suite");
        query_ir.target_types = vec!["procedure".to_string()];
        query_ir.document_focus = None;
        query_ir.target_entities =
            vec![EntityMention { label: "settings".to_string(), role: EntityRole::Object }];

        assert!(!query_ir_requests_setup_focus_document_candidates(&query_ir));
    }

    #[test]
    fn setup_focus_candidates_use_target_entities_without_document_focus() {
        let mut query_ir = setup_query_ir("Alpha Suite");
        query_ir.document_focus = None;
        query_ir.target_entities =
            vec![EntityMention { label: "Alpha Suite".to_string(), role: EntityRole::Subject }];

        assert!(query_ir_requests_setup_focus_document_candidates(&query_ir));
        assert_eq!(setup_focus_query_identity_terms(&query_ir), vec!["Alpha Suite".to_string()]);
    }

    #[test]
    fn setup_focus_candidates_ignore_single_token_entity_without_document_focus() {
        let mut query_ir = setup_query_ir("Alpha Suite");
        query_ir.document_focus = None;
        query_ir.target_entities =
            vec![EntityMention { label: "settings".to_string(), role: EntityRole::Object }];

        assert!(!query_ir_requests_setup_focus_document_candidates(&query_ir));
    }

    #[test]
    fn setup_focus_candidates_ignore_object_entity_without_document_focus() {
        let mut query_ir = setup_query_ir("Alpha Suite");
        query_ir.document_focus = None;
        query_ir.target_entities =
            vec![EntityMention { label: "retry timeout".to_string(), role: EntityRole::Object }];

        assert!(!query_ir_requests_setup_focus_document_candidates(&query_ir));
    }

    #[test]
    fn low_confidence_untyped_ir_allows_raw_question_setup_focus_fallback() {
        let mut query_ir = setup_query_ir("Alpha Suite");
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
        let target = document_row("alpha-suite-x9.md", "Alpha Suite X9 setup reference");
        let screenshot = document_row("alpha-suite-x9.png", "Alpha Suite X9 screenshot");
        let weaker = document_row("alpha-suite-y1.md", "Alpha Suite Y1 setup reference");
        let unrelated = document_row("beta-suite.md", "Beta Suite setup reference");
        let unrelated_id = unrelated.document_id;
        let document_index = HashMap::from([
            (target.document_id, target.clone()),
            (screenshot.document_id, screenshot),
            (weaker.document_id, weaker.clone()),
            (unrelated.document_id, unrelated),
        ]);
        let question_tokens = raw_question_setup_focus_tokens("Alpha Suite X9");

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
    fn retrieved_latest_version_fallback_selects_dominant_semver_family() {
        let alpha_new = document_row("alpha-2-4.md", "Alpha Suite 2.4.0 changelog");
        let alpha_mid = document_row("alpha-2-3.md", "Alpha Suite 2.3.0 changelog");
        let alpha_old = document_row("alpha-2-2.md", "Alpha Suite 2.2.0 changelog");
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
            vec!["Alpha Suite 2.4.0 changelog", "Alpha Suite 2.3.0 changelog"]
        );
    }

    #[test]
    fn retrieved_latest_version_fallback_defaults_to_max_inventory_window() {
        let documents = (1..=11)
            .map(|minor| {
                document_row(
                    &format!("alpha-2-{minor}.md"),
                    &format!("Alpha Suite 2.{minor}.0 changelog"),
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
            Some("Alpha Suite 2.11.0 changelog")
        );
    }

    #[test]
    fn retrieved_latest_version_fallback_repairs_moderate_confidence_untyped_ir() {
        let alpha_new = document_row("alpha-2-4.md", "Alpha Suite 2.4.0 changelog");
        let alpha_mid = document_row("alpha-2-3.md", "Alpha Suite 2.3.0 changelog");
        let alpha_old = document_row("alpha-2-2.md", "Alpha Suite 2.2.0 changelog");
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
        let alpha_new = document_row("alpha-2-4.md", "Alpha Suite 2.4.0 changelog");
        let alpha_mid = document_row("alpha-2-3.md", "Alpha Suite 2.3.0 changelog");
        let alpha_old = document_row("alpha-2-2.md", "Alpha Suite 2.2.0 changelog");
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
            label: "Alpha Suite records".to_string(),
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
        let alpha_new = document_row("alpha-2-4.md", "Alpha Suite 2.4.0 changelog");
        let alpha_mid = document_row("alpha-2-3.md", "Alpha Suite 2.3.0 changelog");
        let alpha_old = document_row("alpha-2-2.md", "Alpha Suite 2.2.0 changelog");
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
        let alpha_new = document_row("alpha-2-4.md", "Alpha Suite 2.4.0 changelog");
        let alpha_mid = document_row("alpha-2-3.md", "Alpha Suite 2.3.0 changelog");
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
        let alpha_new = document_row("alpha-2-4.md", "Alpha Suite 2.4.0 changelog");
        let alpha_mid = document_row("alpha-2-3.md", "Alpha Suite 2.3.0 changelog");
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
        let alpha_new = document_row("alpha-2-4.md", "Alpha Suite 2.4.0 changelog");
        let alpha_mid = document_row("alpha-2-3.md", "Alpha Suite 2.3.0 changelog");
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
                "scope: Provider Alpha configuration literals\nquestion: explain these settings",
                &chunks,
                &index,
            )
            .is_none()
        );
    }

    #[test]
    fn retrieved_latest_version_query_ir_repair_ignores_plain_semver_relevance_chunks() {
        let alpha_new = document_row("alpha-2-4.md", "Alpha Suite 2.4.0 changelog");
        let alpha_mid = document_row("alpha-2-3.md", "Alpha Suite 2.3.0 changelog");
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
        let mut query_ir = setup_query_ir("Provider Alpha");
        query_ir.target_types = vec!["configuration_file".to_string(), "parameter".to_string()];

        let mut chunks = (0..6)
            .map(|index| setup_focus_runtime_chunk(Uuid::now_v7(), index, 1.0))
            .collect::<Vec<_>>();
        chunks.extend(
            [
                "[Main] url = https://alpha.local/api",
                "| fillDetails | boolean | true false | Fill detail fields |",
                "[Display] visible = true",
                "| visible | boolean | true false | Display code |",
                "printSlip = true",
                "secretKey = value",
            ]
            .into_iter()
            .enumerate()
            .map(|(offset, text)| {
                let mut chunk = setup_focus_runtime_chunk(document_id, 20 + offset as i32, 0.1);
                chunk.score_kind = RuntimeChunkScoreKind::SourceContext;
                chunk.document_label = "Provider Alpha setup manual".to_string();
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
        assert!(retained_source_text.iter().any(|text| text.contains("secretKey")));
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
                "[Main] endpointUrl = https://alpha.local/api",
                "| fillDetails | boolean | true false | Fill detail fields |",
                "[Display] visible = true",
                "| visible | boolean | true false | Display code |",
                "printSlip = false",
                "secretKey = value",
            ]
            .into_iter()
            .enumerate()
            .map(|(offset, text)| {
                let mut chunk = setup_focus_runtime_chunk(document_id, 20 + offset as i32, 0.1);
                chunk.score_kind = RuntimeChunkScoreKind::SourceContext;
                chunk.document_label = "Provider Alpha setup manual".to_string();
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
            chunk.document_label = "Provider Alpha setup manual".to_string();
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
        let alpha_new = document_row("alpha-2-4.md", "Alpha Suite 2.4.0 changelog");
        let alpha_mid = document_row("alpha-2-3.md", "Alpha Suite 2.3.0 changelog");
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
        let alpha_new = document_row("alpha-2-4.md", "Alpha Suite 2.4.0 changelog");
        let generic = document_row("overview.md", "Alpha Suite overview");
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
                "Provider Alpha overview",
            ),
            chunk_row(
                workspace_id,
                library_id,
                document_id,
                revision_id,
                1,
                "Install the module:\naptitude install alpha-connector\n\nConfigure it:\ndpkg-reconfigure alpha-connector\n\nSettings are stored in /opt/alpha/connector/connector.conf.",
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
                "Provider Alpha overview",
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
                "Install the module:\naptitude install alpha-connector\nSettings are stored in /opt/alpha/connector/connector.conf.",
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
                "Sheet: Connector settings | Row 2 | Name: secretKey | Type: string | Description: Shared secret",
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
            "Install the module:\naptitude install alpha-connector\nSettings are stored in /opt/alpha/connector/connector.conf.",
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
            "| sendDetails | boolean | Send detailed payment payload | Default true |",
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
            excerpt: "mode\nsecretKey\nmerchantId".to_string(),
            score: Some(1.0),
            score_kind: RuntimeChunkScoreKind::Relevance,
            source_text: "mode\nsecretKey\nmerchantId".to_string(),
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
            "Sheet: Connector settings | Table: Payment details | Row 1 | Name: fillPaymentDetails | Type: boolean | Values: true false | Description: Fill paymentDetails | Default: true",
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
                "Install with aptitude install alpha-connector. Configure /opt/alpha/alpha.conf.",
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
                "Install with aptitude install alpha-connector. Configure /opt/alpha/alpha.conf.",
            ),
            chunk_row(
                workspace_id,
                library_id,
                second_document,
                second_revision,
                2,
                "Sheet: Settings | Row 2 | Name: merchantId | Type: string",
            ),
            chunk_row(
                workspace_id,
                library_id,
                second_document,
                second_revision,
                3,
                "Sheet: Settings | Row 3 | Name: secretKey | Type: string",
            ),
        ];
        let mut query_ir = setup_query_ir("Provider Alpha");
        query_ir.literal_constraints = vec![
            LiteralSpan {
                text: "merchant".to_string(),
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
    fn setup_focus_candidates_keep_long_manual_after_many_short_matches() {
        let target = document_row(
            "provider-alpha-admin-guide.md",
            "Provider Alpha - Administrator setup manual",
        );
        let mut documents = vec![target.clone()];
        for index in 0..64 {
            documents.push(document_row(
                &format!("image-{index}.png"),
                &format!("Provider Alpha: checkout screenshot {index}"),
            ));
        }
        let document_index = documents
            .into_iter()
            .map(|document| (document.document_id, document))
            .collect::<HashMap<_, _>>();
        let query_ir = setup_query_ir("Provider Alpha");

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
    fn structural_setup_focus_fallback_uses_provider_free_entity_identity() {
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
        let document = document_row("cash-desk.md", "Cash desk operator reference");
        let question_tokens = BTreeSet::from(["desks".to_string()]);

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
        anchor_chunk.document_label = "Provider Alpha setup reference".to_string();
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
        primary_chunk.document_label = "General payment operations".to_string();

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
        setup_chunk.document_label = "Provider Alpha setup reference".to_string();
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
            key: document_id.to_string(),
            arango_id: None,
            arango_rev: None,
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
            key: revision_id.to_string(),
            arango_id: None,
            arango_rev: None,
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
        let mut query_ir = setup_query_ir("Alpha Suite");
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
            key: chunk_id.to_string(),
            arango_id: None,
            arango_rev: None,
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

pub(crate) fn merge_query_ir_focus_chunks(
    chunks: Vec<RuntimeMatchedChunk>,
    query_ir_focus_chunks: Vec<RuntimeMatchedChunk>,
    top_k: usize,
) -> Vec<RuntimeMatchedChunk> {
    rrf_merge_chunks(chunks, query_ir_focus_chunks, top_k, RetrievalMergeLane::QueryIrFocus)
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum RetrievalMergeLane {
    RrfFused,
    EntityBio,
    GraphEvidence,
    QueryIrFocus,
}

impl RetrievalMergeLane {
    fn score_kind(self) -> RuntimeChunkScoreKind {
        match self {
            Self::RrfFused => RuntimeChunkScoreKind::Relevance,
            Self::EntityBio => RuntimeChunkScoreKind::EntityBio,
            Self::GraphEvidence => RuntimeChunkScoreKind::GraphEvidence,
            Self::QueryIrFocus => RuntimeChunkScoreKind::QueryIrFocus,
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
        RuntimeChunkScoreKind::DocumentIdentity | RuntimeChunkScoreKind::LatestVersion => 3,
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
    if chunk.score_kind == RuntimeChunkScoreKind::LatestVersion {
        return RuntimeChunkScoreKind::LatestVersion;
    }
    if raw_score >= DOCUMENT_IDENTITY_SCORE_FLOOR {
        return RuntimeChunkScoreKind::DocumentIdentity;
    }
    if chunk.score_kind != RuntimeChunkScoreKind::Relevance {
        return chunk.score_kind;
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
    truncate_merged_chunks_for_lane(values, top_k, right_lane)
}

fn truncate_merged_chunks_for_lane(
    values: Vec<RuntimeMatchedChunk>,
    top_k: usize,
    right_lane: RetrievalMergeLane,
) -> Vec<RuntimeMatchedChunk> {
    if top_k == 0 || values.is_empty() {
        return Vec::new();
    }
    if right_lane != RetrievalMergeLane::QueryIrFocus {
        return values.into_iter().take(top_k).collect();
    }
    let reserved_source_context_count = values
        .iter()
        .filter(|chunk| chunk.score_kind == RuntimeChunkScoreKind::SourceContext)
        .count()
        .min(top_k.saturating_sub(1))
        .min(QUERY_IR_FOCUS_SOURCE_CONTEXT_RESERVATION_LIMIT);
    if reserved_source_context_count == 0 {
        return values.into_iter().take(top_k).collect();
    }

    let mut selected = Vec::with_capacity(top_k);
    let mut selected_chunk_ids = HashSet::<Uuid>::new();
    for chunk in values
        .iter()
        .filter(|chunk| chunk.score_kind == RuntimeChunkScoreKind::SourceContext)
        .take(reserved_source_context_count)
    {
        selected_chunk_ids.insert(chunk.chunk_id);
        selected.push(chunk.clone());
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
    score_value(right.score).total_cmp(&score_value(left.score))
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
    // Reserve the focused-document setup anchor (the chunk carrying both a
    // package-install command and a configuration path) before the score-ordered
    // truncation runs, so a confident single-document configure/how-to answer
    // never loses the "what to install" line to denser parameter chunks.
    let reserved_anchor = query_ir.and_then(|ir| best_setup_focus_anchor_chunk(&indexed, ir));
    let mut selected = if let Some(query_ir) = query_ir {
        if matches!(query_ir.scope, QueryScope::MultiDocument) {
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
    if let Some(anchor) = reserved_anchor {
        ensure_setup_focus_anchor_retained(&mut selected, anchor, top_k);
    }
    *chunks = selected.into_iter().map(|(_, chunk)| chunk).collect();
}

/// Pick the highest-scoring focused-document setup anchor among the candidate
/// chunks for a confident single-document configure/how-to query. An anchor is a
/// chunk that carries both a package-install command and a configuration path
/// and comes from a document whose label overlaps the query's `document_focus`.
fn best_setup_focus_anchor_chunk(
    indexed: &[(usize, RuntimeMatchedChunk)],
    query_ir: &QueryIR,
) -> Option<(usize, RuntimeMatchedChunk)> {
    if !matches!(query_ir.act, QueryAct::ConfigureHow)
        || !matches!(query_ir.scope, QueryScope::SingleDocument)
        || query_ir.document_focus.is_none()
    {
        return None;
    }
    let focus_tokens = query_ir_document_focus_tokens(query_ir)?;
    indexed
        .iter()
        .filter(|(_, chunk)| chunk_is_setup_focus_package_path_anchor(chunk))
        .filter(|(_, chunk)| {
            let label_tokens = normalized_alnum_tokens(&chunk.document_label, 3);
            focus_token_overlap_count(&focus_tokens, &label_tokens) > 0
        })
        .max_by(|(_, left), (_, right)| score_desc_chunks(right, left))
        .map(|(index, chunk)| (*index, chunk.clone()))
}

pub(crate) fn chunk_is_setup_focus_package_path_anchor(chunk: &RuntimeMatchedChunk) -> bool {
    !extract_package_command_literals(&chunk.source_text, 1).is_empty()
        && setup_focus_configuration_path_count(&chunk.source_text) > 0
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
        selected.iter().rposition(|(_, chunk)| !chunk_is_setup_focus_package_path_anchor(chunk));
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
    let requests_structured_fallback_context =
        query_ir_requests_structured_source_context_reservation(query_ir);
    let reserves_source_context = query_ir.is_exact_literal_technical()
        || requests_error_code_context
        || requests_transport_context
        || requests_configuration_context
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
    } else if requests_structured_fallback_context {
        top_k.saturating_sub(4).clamp(8, 28)
    } else if requests_configuration_context {
        top_k.saturating_sub(2).clamp(4, 12)
    } else {
        4
    };
    Some(source_context_count.min(top_k.saturating_sub(1)).min(max_reserved))
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

#[cfg(test)]
#[path = "retrieve_tests.rs"]
mod tests;
