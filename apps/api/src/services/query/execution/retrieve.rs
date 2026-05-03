use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::Context;
use chrono::{DateTime, Datelike, Utc};
use futures::future::join_all;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        provider_profiles::EffectiveProviderProfile,
        query::RuntimeQueryMode,
        query_ir::{QueryIR, TemporalConstraint},
    },
    infra::{
        arangodb::document_store::{
            KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeLibraryGenerationRow,
        },
        repositories::{self, ai_repository},
    },
    services::{
        knowledge::runtime_read::load_active_runtime_graph_projection,
        query::{
            latest_versions::{
                LATEST_VERSION_CHUNKS_PER_DOCUMENT, compare_version_desc,
                extract_semver_like_version, latest_version_chunk_score,
                latest_version_context_top_k, latest_version_family_key,
                latest_version_scope_terms, query_requests_latest_versions,
                requested_latest_version_count, text_has_release_version_marker,
            },
            planner::RuntimeQueryPlan,
            text_match::{
                normalized_alnum_token_sequence, normalized_alnum_tokens, token_sequence_contains,
            },
        },
    },
    shared::extraction::text_render::repair_technical_layout_noise,
};

use super::technical_literals::technical_literal_focus_keyword_segments;
use super::tuning::DOCUMENT_IDENTITY_SCORE_FLOOR;
use super::types::*;
use super::{
    GraphTargetEntityCoverageField, GraphTargetEntityCoverageFieldKind, GraphTargetEntityProfile,
    graph_target_entity_coverage_score, graph_target_entity_profiles,
    load_initial_table_rows_for_documents, load_table_rows_for_documents,
    load_table_summary_chunks_for_documents, merge_canonical_table_aggregation_chunks,
    query_relevant_entity_hits, question_asks_table_aggregation, related_edges_for_entities,
    requested_initial_table_row_count,
};

const DIRECT_TABLE_AGGREGATION_SUMMARY_LIMIT: usize = 32;
const DIRECT_TABLE_AGGREGATION_ROW_LIMIT: usize = 24;
const DIRECT_TABLE_AGGREGATION_CHUNK_LIMIT: usize = 32;
const DOCUMENT_IDENTITY_CHUNKS_PER_DOCUMENT: usize = 3;
const GRAPH_EVIDENCE_CONTEXT_FETCH_MULTIPLIER: usize = 4;
const GRAPH_EVIDENCE_CONTEXT_SCORE_TOKEN_MIN_CHARS: usize = 4;
const GRAPH_EVIDENCE_CONTEXT_EVIDENCE_FIELD_WEIGHT: usize = 4;
const GRAPH_EVIDENCE_CONTEXT_TARGET_FIELD_WEIGHT: usize = 2;
const GRAPH_EVIDENCE_CONTEXT_SOURCE_FIELD_WEIGHT: usize = 1;

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
            d.document_state::text as document_state,
            h.active_revision_id,
            h.readable_revision_id,
            latest_revision.latest_revision_no,
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
    document_state: String,
    active_revision_id: Option<Uuid>,
    readable_revision_id: Option<Uuid>,
    latest_revision_no: Option<i64>,
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
        document_state: row.document_state,
        active_revision_id: row.active_revision_id,
        readable_revision_id: row.readable_revision_id,
        latest_revision_no: row.latest_revision_no,
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
const QUERY_IR_FOCUS_CHUNKS_PER_QUERY: usize = 12;

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
    let mut all_evidence_chunk_ids: Vec<Uuid> = Vec::new();
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
            if !seen_nodes.insert(node.id) {
                continue;
            }
            let evidence = repositories::list_runtime_graph_evidence_by_target(
                &state.persistence.postgres,
                library_id,
                "node",
                node.id,
            )
            .await
            .context("failed to list graph evidence for entity-bio retrieval")?;
            for row in evidence {
                if let Some(chunk_id) = row.chunk_id {
                    if all_evidence_chunk_ids.len() >= ENTITY_BIO_CHUNK_CAP {
                        break;
                    }
                    if !all_evidence_chunk_ids.contains(&chunk_id) {
                        all_evidence_chunk_ids.push(chunk_id);
                    }
                }
            }
            if all_evidence_chunk_ids.len() >= ENTITY_BIO_CHUNK_CAP {
                break;
            }
        }
        if all_evidence_chunk_ids.len() >= ENTITY_BIO_CHUNK_CAP {
            break;
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
    let empty_targets: BTreeSet<Uuid> = BTreeSet::new();
    let candidates =
        batch_hydrate_hits(state, hits, document_index, plan_keywords, &empty_targets).await?;
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
    graph_index: &QueryGraphIndex,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
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
        graph_index,
        graph_evidence_context_fetch_cap(),
    )
    .await
    .context("failed to load ranked graph evidence rows for chunk hydration")?;
    let context_lines = graph_evidence_context_lines_from_rows(&ranked_evidence.rows, graph_index);
    let (hits, evidence_texts_by_chunk) =
        graph_evidence_chunk_hits_from_rows(&ranked_evidence.rows);
    if hits.is_empty() {
        tracing::info!(
            stage = "retrieval.graph_evidence",
            graph_target_count = ranked_evidence.graph_target_count,
            text_query_count = ranked_evidence.text_query_count,
            text_evidence_count = ranked_evidence.text_evidence_count,
            target_evidence_count = ranked_evidence.target_evidence_count,
            ranked_evidence_count = ranked_evidence.rows.len(),
            graph_evidence_line_count = context_lines.len(),
            evidence_chunk_count = 0usize,
            "graph evidence rows loaded without hydratable chunks",
        );
        return Ok(RuntimeGraphEvidenceRetrieval { chunks: Vec::new(), context_lines });
    }

    let empty_targets = BTreeSet::new();
    let mut chunks = batch_hydrate_hits(state, hits, document_index, plan_keywords, &empty_targets)
        .await
        .context("failed to hydrate graph evidence chunks")?;
    apply_graph_evidence_texts_to_chunks(&mut chunks, &evidence_texts_by_chunk, plan_keywords);
    for chunk in &mut chunks {
        chunk.score_kind = RuntimeChunkScoreKind::GraphEvidence;
    }
    tracing::info!(
        stage = "retrieval.graph_evidence",
        graph_target_count = ranked_evidence.graph_target_count,
        text_query_count = ranked_evidence.text_query_count,
        text_evidence_count = ranked_evidence.text_evidence_count,
        target_evidence_count = ranked_evidence.target_evidence_count,
        ranked_evidence_count = ranked_evidence.rows.len(),
        graph_evidence_line_count = context_lines.len(),
        evidence_chunk_count = chunks.len(),
        "graph evidence chunks hydrated from ranked graph evidence rows",
    );
    Ok(RuntimeGraphEvidenceRetrieval { chunks, context_lines })
}

fn graph_evidence_context_lines_from_rows(
    rows: &[repositories::RuntimeGraphEvidenceRow],
    graph_index: &QueryGraphIndex,
) -> Vec<String> {
    let mut lines = Vec::with_capacity(rows.len());
    for row in rows.iter().take(GRAPH_EVIDENCE_CONTEXT_LINE_CAP) {
        let Some(line) = graph_evidence_context_line(row, graph_index) else {
            continue;
        };
        lines.push(line);
    }
    lines
}

#[derive(Debug)]
struct RankedGraphEvidenceRows {
    rows: Vec<repositories::RuntimeGraphEvidenceRow>,
    graph_target_count: usize,
    text_query_count: usize,
    text_evidence_count: usize,
    target_evidence_count: usize,
}

async fn load_ranked_graph_evidence_rows_for_query(
    state: &AppState,
    library_id: Uuid,
    question: &str,
    entities: &[RuntimeMatchedEntity],
    relationships: &[RuntimeMatchedRelationship],
    plan: &RuntimeQueryPlan,
    query_ir: Option<&QueryIR>,
    graph_index: &QueryGraphIndex,
    limit: usize,
) -> anyhow::Result<RankedGraphEvidenceRows> {
    let targets =
        graph_evidence_targets_for_query(entities, relationships, plan, query_ir, graph_index);
    let target_entity_profiles = graph_target_entity_profiles(query_ir, graph_index);
    let query_ir_focus_queries = query_ir.map(query_ir_lexical_focus_queries).unwrap_or_default();
    let text_queries =
        build_graph_evidence_text_queries(question, plan, &query_ir_focus_queries, query_ir);

    let text_evidence = repositories::search_runtime_graph_evidence_by_text(
        &state.persistence.postgres,
        library_id,
        &text_queries,
        GRAPH_EVIDENCE_CONTEXT_LINE_CAP as i64,
    )
    .await
    .context("failed to search graph evidence context by evidence text")?;

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

    let rows = rank_graph_evidence_context_rows(
        &text_evidence,
        &target_evidence,
        question,
        &text_queries,
        graph_index,
        &target_entity_profiles,
        limit,
    );

    Ok(RankedGraphEvidenceRows {
        rows,
        graph_target_count: targets.len(),
        text_query_count: text_queries.len(),
        text_evidence_count: text_evidence.len(),
        target_evidence_count: target_evidence.len(),
    })
}

pub(crate) fn graph_evidence_chunk_hits_from_rows(
    rows: &[repositories::RuntimeGraphEvidenceRow],
) -> (Vec<(Uuid, f32)>, HashMap<Uuid, Vec<String>>) {
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
) -> Option<String> {
    let evidence_text = graph_evidence_text_for_context(&row.evidence_text);
    if evidence_text.is_empty() {
        return None;
    }

    let mut attrs = Vec::new();
    if let Some(target_label) =
        graph_evidence_target_label(&row.target_kind, row.target_id, graph_index)
    {
        attrs.push(("target", target_label));
    }
    if let Some(source) =
        row.source_file_name.as_deref().map(str::trim).filter(|value| !value.is_empty())
    {
        attrs.push(("source", source.to_string()));
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
) {
    for chunk in chunks {
        let Some(evidence_texts) = evidence_texts_by_chunk.get(&chunk.chunk_id) else {
            continue;
        };
        let evidence_source_text = graph_evidence_source_text(&chunk.source_text, evidence_texts);
        if evidence_source_text.trim().is_empty() {
            continue;
        }
        chunk.source_text = evidence_source_text;
        chunk.excerpt = focused_excerpt_for(&chunk.source_text, plan_keywords, 280);
    }
}

fn graph_evidence_source_text(chunk_source_text: &str, evidence_texts: &[String]) -> String {
    let mut parts = Vec::new();
    let mut seen = BTreeSet::new();
    for text in evidence_texts {
        let normalized = graph_evidence_text_for_context(text);
        if normalized.is_empty() || !seen.insert(normalized.to_lowercase()) {
            continue;
        }
        parts.push(normalized);
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
    graph_index: &QueryGraphIndex,
) -> Vec<(String, Uuid)> {
    let query_entities =
        query_relevant_entity_hits(plan, query_ir, graph_index, GRAPH_EVIDENCE_TARGET_CAP / 2);
    let query_relationships = related_edges_for_entities(
        &query_entities,
        graph_index,
        plan,
        query_ir,
        GRAPH_EVIDENCE_TARGET_CAP / 2,
    );

    let mut seen = HashSet::<(String, Uuid)>::new();
    let mut targets = Vec::new();
    for target in graph_evidence_targets(entities, relationships)
        .into_iter()
        .chain(graph_evidence_targets(&query_entities, &query_relationships))
    {
        if targets.len() >= GRAPH_EVIDENCE_TARGET_CAP {
            break;
        }
        if seen.insert(target.clone()) {
            targets.push(target);
        }
    }
    targets
}

async fn load_query_ir_focus_chunks(
    state: &AppState,
    library_id: Uuid,
    question: &str,
    focus_queries: &[String],
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
            .with_context(|| format!("failed to run query-IR focus chunk search: {focus_query}"))
    });
    let per_query_results: Vec<Result<Vec<_>, anyhow::Error>> = join_all(per_query_futures).await;
    let mut hits = Vec::new();
    let mut seen = HashSet::new();
    for result in per_query_results {
        for hit in result? {
            if hits.len() >= QUERY_IR_FOCUS_CHUNK_CAP {
                break;
            }
            if seen.insert(hit.chunk_id) {
                let fallback_score = query_ir_focus_chunk_score(hits.len());
                let raw_score = hit.score as f32;
                let score = if raw_score.is_finite() && raw_score > 0.0 {
                    raw_score
                } else {
                    fallback_score
                };
                hits.push((hit.chunk_id, score));
            }
        }
        if hits.len() >= QUERY_IR_FOCUS_CHUNK_CAP {
            break;
        }
    }
    if hits.is_empty() {
        return Ok(Vec::new());
    }

    let empty_targets = BTreeSet::new();
    let mut chunks = batch_hydrate_hits(state, hits, document_index, plan_keywords, &empty_targets)
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
    let targeted_document_ids = forced_target_document_ids
        .filter(|ids| !ids.is_empty())
        .cloned()
        .unwrap_or_else(|| explicit_target_document_ids(question, document_index));
    let initial_table_row_count = requested_initial_table_row_count(question);
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
        let raw_hits = state
            .arango_search_store
            .search_chunk_vectors_by_similarity(
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
        Ok((hits, started.elapsed().as_millis()))
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
        let mut lexical_hits: Vec<RuntimeMatchedChunk> = Vec::new();
        for result in per_query_results {
            let query_hits = result?;
            lexical_hits = merge_chunks(lexical_hits, query_hits, lexical_limit);
        }
        Ok::<(Vec<RuntimeMatchedChunk>, usize, u128), anyhow::Error>((
            lexical_hits,
            lexical_query_count,
            started.elapsed().as_millis(),
        ))
    };

    let ((vector_hits, vector_elapsed_ms), (lexical_hits, lexical_query_count, lexical_elapsed_ms)) =
        tokio::try_join!(vector_future, lexical_future)?;
    tracing::info!(
        stage = "retrieval.chunks_fanout",
        vector_elapsed_ms,
        vector_hits = vector_hits.len(),
        lexical_elapsed_ms,
        lexical_query_count,
        lexical_hits = lexical_hits.len(),
        "vector + lexical chunk fan-out"
    );
    let mut chunks =
        merge_chunks(vector_hits, lexical_hits, limit.max(initial_table_row_count.unwrap_or(0)));
    let document_identity_chunks = load_document_identity_chunks_for_targets(
        state,
        document_index,
        &targeted_document_ids,
        plan_keywords,
    )
    .await?;
    if !document_identity_chunks.is_empty() {
        chunks = merge_chunks(
            chunks,
            document_identity_chunks,
            limit.max(initial_table_row_count.unwrap_or(0)).saturating_add(
                targeted_document_ids.len() * DOCUMENT_IDENTITY_CHUNKS_PER_DOCUMENT,
            ),
        );
    }
    let latest_version_chunks =
        load_latest_version_document_chunks(state, query_ir, document_index, plan_keywords).await?;
    if !latest_version_chunks.is_empty() {
        chunks = merge_chunks(
            chunks,
            latest_version_chunks,
            // query_ir is always Some when latest_version_chunks is non-empty (guarded by caller).
            #[allow(clippy::expect_used)]
            latest_version_context_top_k(query_ir.expect("latest chunks require QueryIR"), limit),
        );
    }
    let entity_bio_chunks =
        load_entity_bio_chunks(state, library_id, query_ir, document_index, plan_keywords).await?;
    if !entity_bio_chunks.is_empty() {
        // Cap at limit + the bio budget so entity-bio hits are additive
        // rather than pushing other high-score chunks off the top-K.
        let merged_limit = limit.saturating_add(ENTITY_BIO_CHUNK_CAP);
        chunks = merge_entity_bio_chunks(chunks, entity_bio_chunks, merged_limit);
    }
    let query_ir_focus_chunks = load_query_ir_focus_chunks(
        state,
        library_id,
        question,
        &query_ir_focus_queries,
        document_index,
        plan_keywords,
        temporal_start,
        temporal_end,
    )
    .await?;
    if !query_ir_focus_chunks.is_empty() {
        chunks = merge_query_ir_focus_chunks(
            chunks,
            query_ir_focus_chunks,
            query_ir_focus_context_top_k(limit),
        );
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
    chunks = diversify_chunks_by_document(chunks, max_chunks_per_document);
    if !targeted_document_ids.is_empty() {
        chunks.retain(|chunk| targeted_document_ids.contains(&chunk.document_id));
    }
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

    Ok(chunks)
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
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if targeted_document_ids.is_empty() {
        return Ok(Vec::new());
    }

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
    }
    Ok(chunks)
}

fn document_identity_chunk_score(document_rank: usize, chunk_rank: usize) -> f32 {
    DOCUMENT_IDENTITY_SCORE_FLOOR + 10_000.0 - document_rank as f32 * 100.0 - chunk_rank as f32
}

async fn load_latest_version_document_chunks(
    state: &AppState,
    query_ir: Option<&QueryIR>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let Some(ir) = query_ir else {
        return Ok(Vec::new());
    };
    if !query_requests_latest_versions(ir) {
        return Ok(Vec::new());
    }
    let requested_count = requested_latest_version_count(ir);
    let scope_terms = latest_version_scope_terms(ir);
    let documents = latest_version_documents(document_index, requested_count, &scope_terms);
    if documents.is_empty() {
        return Ok(Vec::new());
    }

    let mut chunks = Vec::new();
    for (rank, document) in documents.into_iter().enumerate() {
        let rows = state
            .arango_document_store
            .list_chunks_by_revision_range(
                document.revision_id,
                0,
                LATEST_VERSION_CHUNKS_PER_DOCUMENT.saturating_sub(1) as i32,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to load latest-version chunks for document {} revision {}",
                    document.document_id, document.revision_id
                )
            })?;
        for (chunk_rank, row) in
            rows.into_iter().take(LATEST_VERSION_CHUNKS_PER_DOCUMENT).enumerate()
        {
            let score = latest_version_chunk_score(
                DOCUMENT_IDENTITY_SCORE_FLOOR,
                requested_count,
                rank,
                chunk_rank,
            );
            if let Some(mut chunk) = map_chunk_hit(row, score, document_index, plan_keywords) {
                chunk.score = Some(score);
                chunk.score_kind = RuntimeChunkScoreKind::DocumentIdentity;
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
        .filter(|document| document.document_state == "active")
        .filter_map(|document| {
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
            let identity_text =
                format!("{primary_title_lower} {}", document.external_key.to_lowercase());
            Some((
                LatestVersionDocument {
                    document_id: document.document_id,
                    revision_id,
                    version,
                    title: primary_title.to_string(),
                    family_key: latest_version_family_key(primary_title),
                },
                identity_text,
            ))
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
    rows.sort_by(|left, right| {
        compare_version_desc(&left.version, &right.version)
            .then_with(|| left.title.cmp(&right.title))
    });
    rows.dedup_by(|left, right| {
        left.version == right.version && left.title.eq_ignore_ascii_case(&right.title)
    });
    rows.truncate(count);
    rows
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
    provider_profile: &EffectiveProviderProfile,
) -> anyhow::Result<Option<RuntimeVectorSearchContext>> {
    let providers = ai_repository::list_provider_catalog(&state.persistence.postgres)
        .await
        .context("failed to list provider catalog for runtime vector search")?;
    let Some(provider) = providers
        .into_iter()
        .find(|row| row.provider_kind == provider_profile.embedding.provider_kind.as_str())
    else {
        return Ok(None);
    };
    let models = ai_repository::list_model_catalog(&state.persistence.postgres, Some(provider.id))
        .await
        .context("failed to list model catalog for runtime vector search")?;
    let Some(model) =
        models.into_iter().find(|row| row.model_name == provider_profile.embedding.model_name)
    else {
        return Ok(None);
    };

    let Some(generation) = load_latest_library_generation(state, library_id).await? else {
        return Ok(None);
    };
    if generation.active_vector_generation <= 0 {
        return Ok(None);
    }

    Ok(Some(RuntimeVectorSearchContext { model_catalog_id: model.id }))
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
) -> Vec<RuntimeMatchedChunk> {
    if max_per_doc == 0 {
        return chunks;
    }
    let mut counts: std::collections::HashMap<Uuid, usize> =
        std::collections::HashMap::with_capacity(chunks.len());
    let mut out = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        let count = counts.entry(chunk.document_id).or_insert(0);
        if *count >= max_per_doc {
            continue;
        }
        *count += 1;
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
    push_query(question.trim().to_string(), &mut queries);

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
        for segment in technical_literal_focus_keyword_segments(question, query_ir) {
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

    push_query(question.trim().to_string(), &mut queries);

    for focus in query_ir_focus_queries {
        push_query(focus.clone(), &mut queries);
    }

    if plan.intent_profile.exact_literal_technical {
        for segment in technical_literal_focus_keyword_segments(question, query_ir) {
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

    let compound_values = query_ir_compound_focus_values(query_ir);
    let mut compounds = adjacent_query_ir_focus_compounds(&compound_values);
    sort_query_ir_focus_values_by_specificity(&mut compounds);
    for compound in &compounds {
        push_focus(&compound, &mut queries);
    }
    let mut focus_values = query_ir_focus_values(query_ir);
    sort_query_ir_focus_values_by_specificity(&mut focus_values);
    for focus in &focus_values {
        push_focus(focus, &mut queries);
    }
    if queries.is_empty()
        && let Some(document_focus) = &query_ir.document_focus
    {
        push_focus(&document_focus.hint, &mut queries);
    }
    queries
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

    for literal in &query_ir.literal_constraints {
        push_value(&literal.text, &mut values);
    }
    for entity in &query_ir.target_entities {
        push_value(&entity.label, &mut values);
    }
    values
}

fn query_ir_focus_values(query_ir: &QueryIR) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut values = Vec::new();
    let mut push_value = |value: &str, values: &mut Vec<String>| {
        let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if !is_usable_query_ir_focus(&normalized) || !seen.insert(normalized.to_lowercase()) {
            return;
        }
        values.push(normalized);
    };

    for literal in &query_ir.literal_constraints {
        push_value(&literal.text, &mut values);
    }
    for temporal in &query_ir.temporal_constraints {
        for focus in temporal_constraint_focus_values(temporal) {
            push_value(&focus, &mut values);
        }
    }
    for entity in &query_ir.target_entities {
        push_value(&entity.label, &mut values);
    }
    values
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
    // strict equality silently hides up to ~80% of historical chunks
    // (verified on stage 2026-05-03: 18207 of 22275 leader_lm chunks
    // dropped, 114682 of 277088 across all libraries). Downstream
    // dedup by `chunk_id` keeps the result set clean against any cross-
    // revision duplicates. Sprint T-future will resolve the underlying
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
        document_index
            .get(&chunk.document_id)
            .and_then(canonical_document_revision_id)
            .is_some()
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

fn explicit_target_document_ids(
    question: &str,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> BTreeSet<Uuid> {
    super::explicit_target_document_ids_from_values(
        question,
        document_index.values().flat_map(|document| {
            [
                document.file_name.as_deref(),
                document.title.as_deref(),
                Some(document.external_key.as_str()),
            ]
            .into_iter()
            .flatten()
            .map(move |value| (document.document_id, value))
        }),
    )
}

pub(crate) fn canonical_document_revision_id(document: &KnowledgeDocumentRow) -> Option<Uuid> {
    document.readable_revision_id.or(document.active_revision_id)
}

#[cfg(test)]
mod document_index_tests {
    use chrono::TimeZone;

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
            document_state: "active".to_string(),
            active_revision_id: Some(active_revision_id),
            readable_revision_id: Some(readable_revision_id),
            latest_revision_no: Some(4),
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
        assert_eq!(canonical_document_revision_id(&row), Some(readable_revision_id));
        assert_eq!(row.active_revision_id, Some(active_revision_id));
        assert_eq!(row.latest_revision_no, Some(4));
        assert_eq!(row.updated_at, updated_at);
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
        RuntimeChunkScoreKind::DocumentIdentity => 3,
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
    values.truncate(top_k);
    values
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

pub(crate) fn truncate_bundle(bundle: &mut RetrievalBundle, top_k: usize) {
    bundle.entities.truncate(top_k);
    bundle.relationships.truncate(top_k);
    truncate_chunks_for_context(&mut bundle.chunks, top_k);
}

fn truncate_chunks_for_context(chunks: &mut Vec<RuntimeMatchedChunk>, top_k: usize) {
    if chunks.len() <= top_k {
        return;
    }
    let mut indexed = std::mem::take(chunks).into_iter().enumerate().collect::<Vec<_>>();
    indexed.sort_by(|(left_index, left), (right_index, right)| {
        score_kind_priority(right.score_kind)
            .cmp(&score_kind_priority(left.score_kind))
            .then_with(|| left_index.cmp(right_index))
    });
    indexed.truncate(top_k);
    *chunks = indexed.into_iter().map(|(_, chunk)| chunk).collect();
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
