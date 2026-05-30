use std::collections::HashMap;

use anyhow::Context;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::query_ir::{
        QueryAct, QueryIR, QueryScope, SourceSliceDirection, SourceSliceFilter, SourceSliceSpec,
    },
    infra::arangodb::document_store::{
        KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeStructuredBlockRow,
    },
    shared::extraction::text_render::repair_technical_layout_noise,
};

use super::{
    RuntimeMatchedChunk,
    question_intent::{
        QuestionIntent, canonical_target_type_tag, classify_query_ir_intents, has_question_intent,
    },
    retrieve::{
        canonical_document_revision_id, excerpt_for, focused_excerpt_for, map_chunk_hit,
        score_value,
    },
    source_profile::{
        SOURCE_PROFILE_CHUNK_KIND, is_record_stream_source_profile_row,
        is_source_profile_chunk_row, is_source_profile_runtime_chunk,
    },
    technical_literals::{
        detect_technical_literal_intent_from_query_ir, extract_explicit_path_literals,
        extract_package_command_literals, technical_chunk_selection_score,
        technical_literal_focus_keywords,
    },
};

const SOURCE_CONTEXT_DOCUMENT_LIMIT: usize = 3;
const SOURCE_CONTEXT_ANCHOR_LIMIT_PER_DOCUMENT: usize = 2;
const SOURCE_CONTEXT_DEFAULT_NEIGHBOR_BACKWARD: i32 = 1;
const SOURCE_CONTEXT_DEFAULT_NEIGHBOR_FORWARD: i32 = 1;
const SOURCE_CONTEXT_PROCEDURAL_NEIGHBOR_BACKWARD: i32 = 3;
const SOURCE_CONTEXT_PROFILE_HEADROOM: usize = 1;
const SOURCE_CONTEXT_FOCUSED_MATCH_LIMIT_PER_DOCUMENT: usize = 2;
const SOURCE_CONTEXT_FOCUSED_MATCH_CANDIDATE_LIMIT_PER_DOCUMENT: usize = 64;
const SOURCE_CONTEXT_FOCUSED_MATCH_SCORE_BONUS: f32 = 1.0;
const SOURCE_CONTEXT_PATH_MATCH_LIMIT_PER_DOCUMENT: usize = 2;
const SOURCE_CONTEXT_PATH_MATCH_CANDIDATE_LIMIT_PER_DOCUMENT: usize = 16;
const SOURCE_CONTEXT_PATH_MATCH_SCORE_BONUS: f32 = 0.8;
const SOURCE_CONTEXT_SETUP_PATH_SCORE_BONUS: f32 = 4.0;
const SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_FORWARD: i32 = 16;
const SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_LIMIT_PER_DOCUMENT: usize = 12;
const SOURCE_CONTEXT_PROCEDURAL_SETUP_LIMIT_PER_DOCUMENT: usize = 6;
const SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_SCORE_BONUS: f32 = 0.6;
const SOURCE_CONTEXT_CODE_PATTERN_TERM_LIMIT: usize = 10;
const SOURCE_CONTEXT_CODE_PATTERN_HIT_LIMIT: usize = 8;
const SOURCE_CONTEXT_CODE_PATTERN_SCORE_BONUS: f32 = 3.0;
const SOURCE_CONTEXT_TRANSPORT_PATTERN_TERM_LIMIT: usize = 10;
const SOURCE_CONTEXT_TRANSPORT_PATTERN_HIT_LIMIT: usize = 16;
const SOURCE_CONTEXT_TRANSPORT_PATTERN_SCORE_BONUS: f32 = 2.5;
pub(crate) const SOURCE_SLICE_DEFAULT_COUNT: usize = 12;
pub(crate) const SOURCE_SLICE_MAX_COUNT: usize = 30;
pub(crate) const SOURCE_UNIT_CHUNK_KIND: &str = "source_unit";
const TABLE_ROW_CHUNK_KIND: &str = "table_row";
const CODE_BLOCK_CHUNK_KIND: &str = "code_block";
const KEY_VALUE_BLOCK_CHUNK_KIND: &str = "key_value_block";
const METADATA_BLOCK_CHUNK_KIND: &str = "metadata_block";
const SOURCE_SLICE_CONTEXT_CHARS_PER_UNIT: usize = 1_600;
const SOURCE_SLICE_CONTEXT_MAX_CHARS: usize = 64_000;
const SOURCE_CONTEXT_SELECTED_PROFILE_BONUS: f32 = 2.0;
const SOURCE_CONTEXT_LIBRARY_PROFILE_BONUS: f32 = 1.5;
const SOURCE_CONTEXT_NEIGHBOR_PENALTY: f32 = 0.01;
const SOURCE_CONTEXT_SLICE_PROFILE_BONUS: f32 = 4.0;
const SOURCE_CONTEXT_SLICE_BONUS: f32 = 3.0;

const SOURCE_CONTEXT_EXCERPT_CHARS: usize = 720;
const SOURCE_CONTEXT_GRAPH_EVIDENCE_BONUS: f32 = 0.75;
const SOURCE_CONTEXT_DOCUMENT_HEAD_CHUNK_INDEX: i32 = 0;

pub(crate) fn source_anchor_window(anchor: i32, backward: i32, forward: i32) -> (i32, i32) {
    (anchor.saturating_sub(backward.max(0)), anchor.saturating_add(forward.max(0)))
}

#[derive(Debug, Clone, Default, serde::Serialize, utoipa::ToSchema)]
pub(crate) struct StructuredSourceContextDiagnostics {
    pub(crate) eligible_document_count: usize,
    pub(crate) source_profile_count: usize,
    pub(crate) neighbor_count: usize,
    pub(crate) focused_match_count: usize,
    pub(crate) procedural_structured_sibling_count: usize,
    pub(crate) library_profile_count: usize,
    pub(crate) source_slice_count: usize,
}

#[derive(Debug, Clone)]
struct SourceContextCandidate {
    document_id: Uuid,
    revision_id: Uuid,
    first_rank: usize,
    best_score: f32,
    anchors: Vec<SourceContextAnchor>,
}

#[derive(Debug, Clone, Copy)]
struct SourceContextAnchor {
    chunk_index: i32,
    score: f32,
    first_rank: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceContextNeighborSpan {
    backward: i32,
    forward: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StructuredSourceCompanionKind {
    SourceProfile,
    Neighbor,
    FocusedMatch,
    ProceduralStructuredSibling,
    LibrarySourceProfile,
}

#[derive(Debug, Clone)]
struct StructuredSourceCompanion {
    chunk: RuntimeMatchedChunk,
    kind: StructuredSourceCompanionKind,
}

pub(crate) async fn augment_structured_source_context(
    state: &AppState,
    library_id: Uuid,
    question: &str,
    query_ir: Option<&QueryIR>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    graph_evidence_source_document_ids: &[Uuid],
    chunks: &mut Vec<RuntimeMatchedChunk>,
) -> anyhow::Result<StructuredSourceContextDiagnostics> {
    let mut companions = Vec::<StructuredSourceCompanion>::new();
    let mut candidates = collect_source_context_candidates(chunks);
    let focus_keywords = source_context_focus_keywords(question, query_ir, plan_keywords);
    let requests_expanded_source_context = query_ir.is_some_and(requests_expanded_source_context);
    if requests_expanded_source_context {
        candidates = merge_graph_evidence_source_context_candidates(
            candidates,
            graph_evidence_source_document_ids,
            document_index,
            chunks,
        );
        seed_document_head_source_context_anchors(&mut candidates);
    }
    // T2: source-slice loader now honours `temporal_constraints` via the
    // AQL substring filter on `occurred_at=ISO` headers, so we no longer
    // skip the slice path when bounds are present. Tail-N inside a
    // bounded window now returns the chronological tail within the
    // window instead of the unconditional tail of the file.
    let (slice_temporal_start, slice_temporal_end) =
        query_ir.map_or((None, None), |ir| ir.resolved_temporal_bounds());
    if let Some(slice) = query_ir.and_then(|ir| ir.source_slice.as_ref())
        && let Some(diagnostics) = apply_ordered_source_slice_context(
            state,
            library_id,
            document_index,
            plan_keywords,
            chunks,
            &candidates,
            slice,
            slice_temporal_start,
            slice_temporal_end,
        )
        .await?
    {
        return Ok(StructuredSourceContextDiagnostics {
            eligible_document_count: candidates.len(),
            ..diagnostics
        });
    }
    for candidate in &candidates {
        let focused_rows = state
            .arango_document_store
            .list_chunks_by_revision_matching_terms(
                candidate.revision_id,
                &focus_keywords,
                SOURCE_CONTEXT_FOCUSED_MATCH_CANDIDATE_LIMIT_PER_DOCUMENT,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to load query-focused source context chunks for revision {}",
                    candidate.revision_id
                )
            })?;
        let focused_rows = select_query_focused_source_rows(
            &focused_rows,
            &focus_keywords,
            false,
            &candidate.anchors,
            SOURCE_CONTEXT_FOCUSED_MATCH_LIMIT_PER_DOCUMENT,
        );
        let mut neighbor_anchors = candidate.anchors.clone();
        for (rank, row) in focused_rows.into_iter().enumerate() {
            let score = candidate.best_score + SOURCE_CONTEXT_FOCUSED_MATCH_SCORE_BONUS
                - rank as f32 * 0.01;
            push_unique_source_context_anchor(
                &mut neighbor_anchors,
                SourceContextAnchor {
                    chunk_index: row.chunk_index,
                    score,
                    first_rank: usize::MAX.saturating_sub(rank),
                },
            );
            if let Some(focused) = map_companion_chunk(row, score, document_index, &focus_keywords)
            {
                companions.push(StructuredSourceCompanion {
                    chunk: focused,
                    kind: StructuredSourceCompanionKind::FocusedMatch,
                });
            }
        }

        if query_ir.is_some_and(requests_path_source_context) {
            let configuration_path_context =
                query_ir.is_some_and(requests_configuration_file_path_source_context);
            let path_terms = ["/".to_string()];
            let mut path_rows = state
                .arango_document_store
                .list_chunks_by_revision_matching_terms(
                    candidate.revision_id,
                    &path_terms,
                    SOURCE_CONTEXT_PATH_MATCH_CANDIDATE_LIMIT_PER_DOCUMENT,
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to load path-bearing source context chunks for revision {}",
                        candidate.revision_id
                    )
                })?;
            if configuration_path_context {
                let head_path_rows = state
                    .arango_document_store
                    .list_chunks_by_revision_range(
                        candidate.revision_id,
                        SOURCE_CONTEXT_DOCUMENT_HEAD_CHUNK_INDEX,
                        SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_FORWARD,
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "failed to load setup path source context chunks for revision {}",
                            candidate.revision_id
                        )
                    })?;
                path_rows.extend(head_path_rows);
            }
            let path_rows = select_path_source_rows(
                &path_rows,
                &neighbor_anchors,
                SOURCE_CONTEXT_PATH_MATCH_LIMIT_PER_DOCUMENT,
                configuration_path_context,
            );
            for (rank, row) in path_rows.into_iter().enumerate() {
                let setup_score_bonus = if configuration_path_context {
                    setup_path_source_score_bonus(&row)
                } else {
                    0.0
                };
                let score = candidate.best_score
                    + SOURCE_CONTEXT_PATH_MATCH_SCORE_BONUS
                    + setup_score_bonus
                    - rank as f32 * 0.01;
                push_unique_source_context_anchor(
                    &mut neighbor_anchors,
                    SourceContextAnchor {
                        chunk_index: row.chunk_index,
                        score,
                        first_rank: usize::MAX.saturating_sub(rank),
                    },
                );
                if let Some(path_match) =
                    map_companion_chunk(row, score, document_index, &focus_keywords)
                {
                    companions.push(StructuredSourceCompanion {
                        chunk: path_match,
                        kind: StructuredSourceCompanionKind::FocusedMatch,
                    });
                }
            }
        }

        if query_ir.is_some_and(requests_procedural_source_context) {
            let structured_windows = procedural_structured_sibling_windows(&neighbor_anchors);
            let rows = state
                .arango_document_store
                .list_chunks_by_revision_windows(candidate.revision_id, &structured_windows)
                .await
                .with_context(|| {
                    format!(
                        "failed to load procedural structured source siblings for revision {}",
                        candidate.revision_id
                    )
                })?;
            let rows = select_procedural_structured_sibling_rows(
                &rows,
                &neighbor_anchors,
                SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_LIMIT_PER_DOCUMENT,
            );
            for (rank, row) in rows.into_iter().enumerate() {
                let score = candidate.best_score + SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_SCORE_BONUS
                    - rank as f32 * 0.01;
                if let Some(sibling) =
                    map_companion_chunk(row, score, document_index, plan_keywords)
                {
                    companions.push(StructuredSourceCompanion {
                        chunk: sibling,
                        kind: StructuredSourceCompanionKind::ProceduralStructuredSibling,
                    });
                }
            }
        }

        let profile_rows = state
            .arango_document_store
            .list_chunks_by_revision_range(candidate.revision_id, 0, 0)
            .await
            .with_context(|| {
                format!(
                    "failed to load source profile chunk for revision {}",
                    candidate.revision_id
                )
            })?;
        if let Some(profile_row) = profile_rows.into_iter().find(is_source_profile_chunk_row) {
            let profile_score = candidate.best_score + SOURCE_CONTEXT_SELECTED_PROFILE_BONUS;
            if let Some(profile) =
                map_companion_chunk(profile_row, profile_score, document_index, plan_keywords)
            {
                companions.push(StructuredSourceCompanion {
                    chunk: profile,
                    kind: StructuredSourceCompanionKind::SourceProfile,
                });
            }
        }

        let neighbor_span = source_context_neighbor_span(query_ir);
        let neighbor_windows = source_context_neighbor_windows(&neighbor_anchors, neighbor_span);
        let rows = state
            .arango_document_store
            .list_chunks_by_revision_windows(candidate.revision_id, &neighbor_windows)
            .await
            .with_context(|| {
                format!(
                    "failed to load structured source neighbor chunks for revision {}",
                    candidate.revision_id
                )
            })?;
        for row in rows {
            if is_source_profile_chunk_row(&row) {
                continue;
            }
            let Some(score) = source_context_best_neighbor_score(
                &neighbor_anchors,
                row.chunk_index,
                neighbor_span,
            ) else {
                continue;
            };
            if let Some(neighbor) = map_companion_chunk(row, score, document_index, plan_keywords) {
                companions.push(StructuredSourceCompanion {
                    chunk: neighbor,
                    kind: StructuredSourceCompanionKind::Neighbor,
                });
            }
        }
    }

    if query_ir.is_some_and(requests_error_code_source_context) {
        let code_pattern_companions = load_code_pattern_source_context(
            state,
            library_id,
            document_index,
            &focus_keywords,
            chunks,
            &companions,
        )
        .await?;
        companions.extend(code_pattern_companions);
    }

    if query_ir.is_some_and(requests_transport_source_context) {
        let transport_pattern_companions = load_transport_pattern_source_context(
            state,
            library_id,
            document_index,
            &focus_keywords,
            chunks,
            &companions,
        )
        .await?;
        companions.extend(transport_pattern_companions);
    }

    if query_ir.is_some_and(requests_library_source_profile_context) {
        let global_best_score =
            chunks.iter().map(|chunk| score_value(chunk.score)).fold(0.0, f32::max);
        let revision_ids = canonical_source_profile_revision_ids(
            document_index,
            SOURCE_CONTEXT_DOCUMENT_LIMIT * 4,
        );
        let rows = state
            .arango_document_store
            .list_source_profile_chunks_by_revisions(
                library_id,
                &revision_ids,
                SOURCE_CONTEXT_DOCUMENT_LIMIT,
            )
            .await
            .context("failed to load library source profile chunks for source coverage")?;
        for (rank, row) in rows.into_iter().enumerate() {
            let score =
                global_best_score + SOURCE_CONTEXT_LIBRARY_PROFILE_BONUS - rank as f32 * 0.01;
            if let Some(profile) = map_companion_chunk(row, score, document_index, plan_keywords) {
                companions.push(StructuredSourceCompanion {
                    chunk: profile,
                    kind: StructuredSourceCompanionKind::LibrarySourceProfile,
                });
            }
        }
    }

    let mut diagnostics = apply_structured_source_companions(chunks, companions);
    diagnostics.eligible_document_count = candidates.len();
    Ok(diagnostics)
}

#[must_use]
pub(crate) fn source_slice_context_top_k(query_ir: &QueryIR, base_top_k: usize) -> usize {
    let Some(slice) = query_ir.source_slice.as_ref() else {
        return base_top_k;
    };
    base_top_k.max(source_slice_count(slice).saturating_add(1))
}

#[must_use]
pub(crate) fn structured_source_context_top_k(query_ir: &QueryIR, base_top_k: usize) -> usize {
    let top_k = source_slice_context_top_k(query_ir, base_top_k);
    if !requests_expanded_source_context(query_ir) {
        return top_k;
    }
    top_k.max(procedural_source_context_chunk_floor())
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

async fn apply_ordered_source_slice_context(
    state: &AppState,
    library_id: Uuid,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
    chunks: &mut Vec<RuntimeMatchedChunk>,
    candidates: &[SourceContextCandidate],
    slice: &SourceSliceSpec,
    temporal_start: Option<chrono::DateTime<chrono::Utc>>,
    temporal_end: Option<chrono::DateTime<chrono::Utc>>,
) -> anyhow::Result<Option<StructuredSourceContextDiagnostics>> {
    let Some((candidate, profile_row)) =
        first_record_stream_candidate_profile(state, candidates, library_id, document_index)
            .await?
    else {
        return Ok(None);
    };
    let count = source_slice_count(slice);
    let release_marker_required = matches!(slice.filter, SourceSliceFilter::ReleaseMarker);
    let unit_blocks = match slice.direction {
        SourceSliceDirection::Head | SourceSliceDirection::All => state
            .arango_document_store
            .list_head_source_unit_blocks_by_revision(
                candidate.revision_id,
                count,
                temporal_start,
                temporal_end,
                release_marker_required,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to load ordered source-unit head slice for revision {}",
                    candidate.revision_id
                )
            })?,
        SourceSliceDirection::Tail => state
            .arango_document_store
            .list_tail_source_unit_blocks_by_revision(
                candidate.revision_id,
                count,
                temporal_start,
                temporal_end,
                release_marker_required,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to load ordered source-unit tail slice for revision {}",
                    candidate.revision_id
                )
            })?,
    };

    let mut selected = Vec::with_capacity(count.saturating_add(1));
    let profile_score = candidate.best_score + SOURCE_CONTEXT_SLICE_PROFILE_BONUS;
    if let Some(profile) =
        map_companion_chunk(profile_row, profile_score, document_index, plan_keywords)
    {
        selected.push(profile);
    }
    let slice_score = candidate.best_score + SOURCE_CONTEXT_SLICE_BONUS;
    for block in unit_blocks.into_iter().take(count) {
        if let Some(unit) = map_source_unit_block(block, slice_score, document_index) {
            selected.push(unit);
        }
    }

    if selected.len() <= 1 {
        return Ok(None);
    }
    selected.sort_by(|left, right| {
        score_value(right.score)
            .total_cmp(&score_value(left.score))
            .then_with(|| left.document_id.cmp(&right.document_id))
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    let source_slice_count = selected.len().saturating_sub(1);
    *chunks = selected;
    Ok(Some(StructuredSourceContextDiagnostics {
        eligible_document_count: 1,
        source_profile_count: 1,
        neighbor_count: 0,
        focused_match_count: 0,
        procedural_structured_sibling_count: 0,
        library_profile_count: 0,
        source_slice_count,
    }))
}

async fn first_record_stream_candidate_profile(
    state: &AppState,
    candidates: &[SourceContextCandidate],
    library_id: Uuid,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> anyhow::Result<Option<(SourceContextCandidate, KnowledgeChunkRow)>> {
    for candidate in candidates {
        let profile_rows = state
            .arango_document_store
            .list_chunks_by_revision_range(candidate.revision_id, 0, 0)
            .await
            .with_context(|| {
                format!(
                    "failed to load ordered source profile chunk for revision {}",
                    candidate.revision_id
                )
            })?;
        if let Some(profile) = profile_rows.into_iter().find(is_record_stream_source_profile_row) {
            return Ok(Some((candidate.clone(), profile)));
        }
    }
    // Library-scoped fallback: when no candidate from top-K is a record stream
    // (e.g. date-anchored queries where ranking surfaces unrelated documents),
    // scan the library for any canonical-revision record_jsonl source profile.
    // Without this step, an ordered-source slice request would never reach
    // `apply_ordered_source_slice_context` for the canonical record-stream
    // document when BM25/vector lose to denser non-stream text.
    let canonical_revision_ids =
        canonical_source_profile_revision_ids(document_index, SOURCE_CONTEXT_DOCUMENT_LIMIT * 4);
    if canonical_revision_ids.is_empty() {
        return Ok(None);
    }
    let library_rows = state
        .arango_document_store
        .list_source_profile_chunks_by_revisions(
            library_id,
            &canonical_revision_ids,
            SOURCE_CONTEXT_DOCUMENT_LIMIT * 4,
        )
        .await
        .context("failed to load library record-stream source profile chunks for ordered slice")?;
    let Some(profile) = library_rows.into_iter().find(is_record_stream_source_profile_row) else {
        return Ok(None);
    };
    let synthetic = SourceContextCandidate {
        document_id: profile.document_id,
        revision_id: profile.revision_id,
        first_rank: usize::MAX,
        best_score: 0.0,
        anchors: Vec::new(),
    };
    Ok(Some((synthetic, profile)))
}

fn source_slice_count(slice: &SourceSliceSpec) -> usize {
    slice
        .count
        .map(usize::from)
        .unwrap_or(SOURCE_SLICE_DEFAULT_COUNT)
        .clamp(1, SOURCE_SLICE_MAX_COUNT)
}

fn requests_library_source_profile_context(query_ir: &QueryIR) -> bool {
    query_ir.requests_source_coverage_context()
        && matches!(
            query_ir.scope,
            QueryScope::LibraryMeta | QueryScope::MultiDocument | QueryScope::CrossLibrary
        )
}

fn collect_source_context_candidates(
    chunks: &[RuntimeMatchedChunk],
) -> Vec<SourceContextCandidate> {
    let mut ordered_document_ids = Vec::<Uuid>::new();
    let mut candidates = HashMap::<Uuid, SourceContextCandidate>::new();
    let mut anchor_ranks = HashMap::<Uuid, HashMap<i32, SourceContextAnchor>>::new();
    for (rank, chunk) in chunks.iter().enumerate() {
        if is_source_profile_runtime_chunk(chunk) {
            continue;
        }
        let score = score_value(chunk.score);
        let entry = candidates.entry(chunk.document_id).or_insert_with(|| {
            ordered_document_ids.push(chunk.document_id);
            SourceContextCandidate {
                document_id: chunk.document_id,
                revision_id: chunk.revision_id,
                first_rank: rank,
                best_score: f32::MIN,
                anchors: Vec::new(),
            }
        });
        if score > entry.best_score {
            entry.best_score = score;
            entry.revision_id = chunk.revision_id;
        }
        anchor_ranks
            .entry(chunk.document_id)
            .or_default()
            .entry(chunk.chunk_index)
            .and_modify(|existing| {
                if source_context_anchor_is_better(score, rank, existing) {
                    *existing = SourceContextAnchor {
                        chunk_index: chunk.chunk_index,
                        score,
                        first_rank: rank,
                    };
                }
            })
            .or_insert(SourceContextAnchor {
                chunk_index: chunk.chunk_index,
                score,
                first_rank: rank,
            });
    }

    let mut selected = ordered_document_ids
        .into_iter()
        .filter_map(|document_id| {
            let mut candidate = candidates.remove(&document_id)?;
            let mut anchors = anchor_ranks.remove(&document_id)?.into_values().collect::<Vec<_>>();
            anchors.sort_by(|left, right| {
                right
                    .score
                    .total_cmp(&left.score)
                    .then_with(|| left.first_rank.cmp(&right.first_rank))
                    .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            });
            anchors.truncate(SOURCE_CONTEXT_ANCHOR_LIMIT_PER_DOCUMENT);
            candidate.anchors = anchors;
            (!candidate.anchors.is_empty()).then_some(candidate)
        })
        .collect::<Vec<_>>();
    selected.sort_by(|left, right| {
        left.first_rank
            .cmp(&right.first_rank)
            .then_with(|| left.document_id.cmp(&right.document_id))
    });
    selected.truncate(SOURCE_CONTEXT_DOCUMENT_LIMIT);
    selected
}

fn merge_graph_evidence_source_context_candidates(
    candidates: Vec<SourceContextCandidate>,
    graph_evidence_source_document_ids: &[Uuid],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    chunks: &[RuntimeMatchedChunk],
) -> Vec<SourceContextCandidate> {
    if graph_evidence_source_document_ids.is_empty() {
        return candidates;
    }

    let global_best_score = chunks.iter().map(|chunk| score_value(chunk.score)).fold(0.0, f32::max);
    let mut by_document = candidates
        .iter()
        .cloned()
        .map(|candidate| (candidate.document_id, candidate))
        .collect::<HashMap<_, _>>();
    let mut seen_graph_document_ids = std::collections::BTreeSet::new();
    let mut promoted_document_ids = std::collections::BTreeSet::new();
    let mut merged = Vec::new();

    for (rank, document_id) in graph_evidence_source_document_ids.iter().enumerate() {
        if !seen_graph_document_ids.insert(*document_id) {
            continue;
        }
        let Some(document) = document_index.get(document_id) else {
            continue;
        };
        if document.document_state != "active" {
            continue;
        }
        let Some(revision_id) = canonical_document_revision_id(document) else {
            continue;
        };
        let graph_score =
            global_best_score + SOURCE_CONTEXT_GRAPH_EVIDENCE_BONUS - rank as f32 * 0.01;
        promoted_document_ids.insert(*document_id);
        if let Some(mut candidate) = by_document.remove(document_id) {
            candidate.first_rank = rank.min(candidate.first_rank);
            candidate.revision_id = revision_id;
            candidate.best_score = candidate.best_score.max(graph_score);
            merged.push(candidate);
        } else {
            merged.push(SourceContextCandidate {
                document_id: *document_id,
                revision_id,
                first_rank: rank,
                best_score: graph_score,
                anchors: Vec::new(),
            });
        }
    }

    for candidate in candidates {
        if !promoted_document_ids.contains(&candidate.document_id) {
            merged.push(candidate);
        }
    }
    merged.truncate(SOURCE_CONTEXT_DOCUMENT_LIMIT);
    merged
}

fn seed_document_head_source_context_anchors(candidates: &mut [SourceContextCandidate]) {
    for (rank, candidate) in candidates.iter_mut().enumerate() {
        push_unique_source_context_anchor(
            &mut candidate.anchors,
            SourceContextAnchor {
                chunk_index: SOURCE_CONTEXT_DOCUMENT_HEAD_CHUNK_INDEX,
                score: candidate.best_score,
                first_rank: candidate.first_rank.min(rank),
            },
        );
    }
}

fn source_context_anchor_is_better(
    score: f32,
    rank: usize,
    existing: &SourceContextAnchor,
) -> bool {
    score > existing.score || (score == existing.score && rank < existing.first_rank)
}

fn push_unique_source_context_anchor(
    anchors: &mut Vec<SourceContextAnchor>,
    anchor: SourceContextAnchor,
) {
    if let Some(existing) =
        anchors.iter_mut().find(|existing| existing.chunk_index == anchor.chunk_index)
    {
        if source_context_anchor_is_better(anchor.score, anchor.first_rank, existing) {
            *existing = anchor;
        }
    } else {
        anchors.push(anchor);
    }
}

fn source_context_neighbor_span(query_ir: Option<&QueryIR>) -> SourceContextNeighborSpan {
    let mut span = SourceContextNeighborSpan {
        backward: SOURCE_CONTEXT_DEFAULT_NEIGHBOR_BACKWARD,
        forward: SOURCE_CONTEXT_DEFAULT_NEIGHBOR_FORWARD,
    };
    if query_ir.is_some_and(requests_expanded_source_context) {
        span.backward = SOURCE_CONTEXT_PROCEDURAL_NEIGHBOR_BACKWARD;
    }
    span
}

fn requests_expanded_source_context(query_ir: &QueryIR) -> bool {
    requests_procedural_source_context(query_ir)
        || requests_error_code_source_context(query_ir)
        || requests_transport_source_context(query_ir)
}

fn requests_procedural_source_context(query_ir: &QueryIR) -> bool {
    matches!(query_ir.scope, QueryScope::SingleDocument)
        && query_ir.source_slice.is_none()
        && (matches!(query_ir.act, QueryAct::ConfigureHow)
            || query_ir_requests_focused_configuration_source_context(query_ir))
}

fn query_ir_requests_focused_configuration_source_context(query_ir: &QueryIR) -> bool {
    matches!(query_ir.act, QueryAct::Describe | QueryAct::RetrieveValue)
        && query_ir
            .target_types
            .iter()
            .map(|value| canonical_target_type_tag(value))
            .any(|tag| matches!(tag.as_str(), "configuration_file" | "config_key"))
        && query_ir_has_strong_source_context_anchor(query_ir)
}

fn query_ir_has_strong_source_context_anchor(query_ir: &QueryIR) -> bool {
    query_ir.document_focus.is_some() || !query_ir.literal_constraints.is_empty()
}

fn requests_error_code_source_context(query_ir: &QueryIR) -> bool {
    query_ir.source_slice.is_none()
        && has_question_intent(&classify_query_ir_intents(query_ir), QuestionIntent::ErrorCode)
}

fn requests_transport_source_context(query_ir: &QueryIR) -> bool {
    if query_ir.source_slice.is_some() || !query_ir.literal_constraints.is_empty() {
        return false;
    }
    let intents = classify_query_ir_intents(query_ir);
    has_question_intent(&intents, QuestionIntent::Port)
        || has_question_intent(&intents, QuestionIntent::Protocol)
        || query_ir
            .target_types
            .iter()
            .any(|target_type| target_type.trim().eq_ignore_ascii_case("connection"))
}

fn requests_path_source_context(query_ir: &QueryIR) -> bool {
    query_ir.source_slice.is_none()
        && (detect_technical_literal_intent_from_query_ir("", query_ir).wants_paths
            || requests_configuration_file_path_source_context(query_ir))
}

fn requests_configuration_file_path_source_context(query_ir: &QueryIR) -> bool {
    query_ir
        .target_types
        .iter()
        .map(|value| canonical_target_type_tag(value))
        .any(|tag| tag == "configuration_file")
}

async fn load_code_pattern_source_context(
    state: &AppState,
    library_id: Uuid,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    focus_keywords: &[String],
    chunks: &[RuntimeMatchedChunk],
    _companions: &[StructuredSourceCompanion],
) -> anyhow::Result<Vec<StructuredSourceCompanion>> {
    let terms = code_pattern_query_terms(focus_keywords, SOURCE_CONTEXT_CODE_PATTERN_TERM_LIMIT);
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let global_best_score = chunks.iter().map(|chunk| score_value(chunk.score)).fold(0.0, f32::max);
    let rows = state
        .arango_document_store
        .search_code_pattern_chunks_by_terms(
            library_id,
            &terms,
            SOURCE_CONTEXT_CODE_PATTERN_HIT_LIMIT,
        )
        .await?;
    let row_count = rows.len();
    let mut companion_document_index = document_index.clone();
    hydrate_missing_companion_documents(state, &rows, &mut companion_document_index).await?;
    let mut mapped = rows
        .into_iter()
        .enumerate()
        .filter_map(|(rank, row)| {
            let score =
                global_best_score + SOURCE_CONTEXT_CODE_PATTERN_SCORE_BONUS - rank as f32 * 0.01;
            map_companion_chunk(row, score, &companion_document_index, focus_keywords).map(
                |chunk| StructuredSourceCompanion {
                    chunk,
                    kind: StructuredSourceCompanionKind::FocusedMatch,
                },
            )
        })
        .collect::<Vec<_>>();
    tracing::info!(
        stage = "retrieval.structured_source_context.code_pattern",
        term_count = terms.len(),
        row_count = row_count,
        mapped_count = mapped.len(),
        "code-pattern source context candidates mapped"
    );
    mapped.sort_by(|left, right| {
        score_value(right.chunk.score)
            .total_cmp(&score_value(left.chunk.score))
            .then_with(|| left.chunk.document_id.cmp(&right.chunk.document_id))
            .then_with(|| left.chunk.chunk_index.cmp(&right.chunk.chunk_index))
            .then_with(|| left.chunk.chunk_id.cmp(&right.chunk.chunk_id))
    });
    Ok(mapped)
}

async fn load_transport_pattern_source_context(
    state: &AppState,
    library_id: Uuid,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    focus_keywords: &[String],
    chunks: &[RuntimeMatchedChunk],
    _companions: &[StructuredSourceCompanion],
) -> anyhow::Result<Vec<StructuredSourceCompanion>> {
    let terms =
        code_pattern_query_terms(focus_keywords, SOURCE_CONTEXT_TRANSPORT_PATTERN_TERM_LIMIT);
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let global_best_score = chunks.iter().map(|chunk| score_value(chunk.score)).fold(0.0, f32::max);
    let rows = state
        .arango_document_store
        .search_transport_pattern_chunks_by_terms(
            library_id,
            &terms,
            SOURCE_CONTEXT_TRANSPORT_PATTERN_HIT_LIMIT,
        )
        .await?;
    let row_count = rows.len();
    let mut companion_document_index = document_index.clone();
    hydrate_missing_companion_documents(state, &rows, &mut companion_document_index).await?;
    let mut mapped = rows
        .into_iter()
        .enumerate()
        .filter_map(|(rank, row)| {
            let score = global_best_score + SOURCE_CONTEXT_TRANSPORT_PATTERN_SCORE_BONUS
                - rank as f32 * 0.01;
            map_companion_chunk(row, score, &companion_document_index, focus_keywords).map(
                |chunk| StructuredSourceCompanion {
                    chunk,
                    kind: StructuredSourceCompanionKind::FocusedMatch,
                },
            )
        })
        .collect::<Vec<_>>();
    tracing::info!(
        stage = "retrieval.structured_source_context.transport_pattern",
        term_count = terms.len(),
        row_count = row_count,
        mapped_count = mapped.len(),
        "transport-pattern source context candidates mapped"
    );
    mapped.sort_by(|left, right| {
        score_value(right.chunk.score)
            .total_cmp(&score_value(left.chunk.score))
            .then_with(|| left.chunk.document_id.cmp(&right.chunk.document_id))
            .then_with(|| left.chunk.chunk_index.cmp(&right.chunk.chunk_index))
            .then_with(|| left.chunk.chunk_id.cmp(&right.chunk.chunk_id))
    });
    Ok(mapped)
}

async fn hydrate_missing_companion_documents(
    state: &AppState,
    rows: &[KnowledgeChunkRow],
    document_index: &mut HashMap<Uuid, KnowledgeDocumentRow>,
) -> anyhow::Result<()> {
    let mut missing_document_ids = rows
        .iter()
        .map(|row| row.document_id)
        .filter(|document_id| !document_index.contains_key(document_id))
        .collect::<Vec<_>>();
    missing_document_ids.sort_unstable();
    missing_document_ids.dedup();
    if missing_document_ids.is_empty() {
        return Ok(());
    }
    let documents = state
        .arango_document_store
        .list_documents_by_ids(&missing_document_ids)
        .await
        .context("failed to hydrate source-context companion documents")?;
    for document in documents {
        document_index.insert(document.document_id, document);
    }
    Ok(())
}

fn code_pattern_query_terms(focus_keywords: &[String], limit: usize) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut terms = Vec::new();
    for keyword in focus_keywords {
        let term = keyword.trim().to_lowercase();
        let alphabetic_count = term.chars().filter(|ch| ch.is_alphabetic()).count();
        let digit_count = term.chars().filter(|ch| ch.is_ascii_digit()).count();
        if term.chars().count() < 2
            || (alphabetic_count < 2 && digit_count < 2)
            || !seen.insert(term.clone())
        {
            continue;
        }
        terms.push(term);
        if terms.len() >= limit {
            break;
        }
    }
    terms
}

fn procedural_source_context_chunk_floor() -> usize {
    let span = SOURCE_CONTEXT_PROCEDURAL_NEIGHBOR_BACKWARD
        .saturating_add(SOURCE_CONTEXT_DEFAULT_NEIGHBOR_FORWARD)
        .saturating_add(1)
        .max(0) as usize;
    SOURCE_CONTEXT_PROFILE_HEADROOM
        .saturating_add(SOURCE_CONTEXT_ANCHOR_LIMIT_PER_DOCUMENT.saturating_mul(span))
        .saturating_add(SOURCE_CONTEXT_FOCUSED_MATCH_LIMIT_PER_DOCUMENT)
        .saturating_add(SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_LIMIT_PER_DOCUMENT)
}

fn source_context_neighbor_windows(
    anchors: &[SourceContextAnchor],
    span: SourceContextNeighborSpan,
) -> Vec<(i32, i32)> {
    anchors
        .iter()
        .map(|anchor| source_anchor_window(anchor.chunk_index, span.backward, span.forward))
        .collect()
}

fn procedural_structured_sibling_windows(anchors: &[SourceContextAnchor]) -> Vec<(i32, i32)> {
    anchors
        .iter()
        .map(|anchor| {
            source_anchor_window(
                anchor.chunk_index,
                0,
                SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_FORWARD,
            )
        })
        .collect()
}

fn source_context_best_neighbor_score(
    anchors: &[SourceContextAnchor],
    chunk_index: i32,
    span: SourceContextNeighborSpan,
) -> Option<f32> {
    anchors
        .iter()
        .filter_map(|anchor| {
            let min_index = anchor.chunk_index.saturating_sub(span.backward.max(0));
            let max_index = anchor.chunk_index.saturating_add(span.forward.max(0));
            (chunk_index >= min_index && chunk_index <= max_index).then(|| {
                let distance = chunk_index.abs_diff(anchor.chunk_index) as f32;
                source_context_neighbor_score(anchor.score, distance, chunk_index)
            })
        })
        .max_by(f32::total_cmp)
}

fn source_context_focus_keywords(
    question: &str,
    query_ir: Option<&QueryIR>,
    plan_keywords: &[String],
) -> Vec<String> {
    let mut keywords = technical_literal_focus_keywords(question, query_ir);
    if let Some(query_ir) = query_ir {
        if let Some(document_focus) = query_ir.document_focus.as_ref() {
            keywords.push(document_focus.hint.clone());
        }
        keywords.extend(query_ir.target_entities.iter().map(|entity| entity.label.clone()));
        keywords.extend(query_ir.literal_constraints.iter().map(|literal| literal.text.clone()));
    }
    keywords.extend(plan_keywords.iter().cloned());
    let mut seen = std::collections::BTreeSet::new();
    keywords
        .into_iter()
        .filter_map(|keyword| {
            let normalized = keyword.split_whitespace().collect::<Vec<_>>().join(" ");
            if normalized.is_empty() {
                return None;
            }
            let key = normalized.to_lowercase();
            seen.insert(key).then_some(normalized)
        })
        .collect()
}

fn select_query_focused_source_rows(
    rows: &[KnowledgeChunkRow],
    focus_keywords: &[String],
    pagination_requested: bool,
    anchors: &[SourceContextAnchor],
    limit: usize,
) -> Vec<KnowledgeChunkRow> {
    if limit == 0 || rows.is_empty() || focus_keywords.is_empty() {
        return Vec::new();
    }
    let anchor_indexes = anchors.iter().map(|anchor| anchor.chunk_index).collect::<Vec<_>>();
    let mut candidates = rows
        .iter()
        .filter(|row| !is_source_profile_chunk_row(row))
        .filter(|row| !anchor_indexes.contains(&row.chunk_index))
        .filter_map(|row| {
            let score = technical_chunk_selection_score(
                &format!(
                    "{}\n{}",
                    row.content_text,
                    row.window_text.as_deref().unwrap_or_default()
                ),
                focus_keywords,
                pagination_requested,
            );
            (score > 0).then_some((score, row))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    candidates.into_iter().take(limit).map(|(_, row)| row.clone()).collect()
}

fn select_procedural_structured_sibling_rows(
    rows: &[KnowledgeChunkRow],
    anchors: &[SourceContextAnchor],
    limit: usize,
) -> Vec<KnowledgeChunkRow> {
    if limit == 0 || rows.is_empty() || anchors.is_empty() {
        return Vec::new();
    }
    let anchor_indexes = anchors.iter().map(|anchor| anchor.chunk_index).collect::<Vec<_>>();
    let mut eligible = rows
        .iter()
        .filter(|row| !is_source_profile_chunk_row(row))
        .filter(|row| !anchor_indexes.contains(&row.chunk_index))
        .filter(|row| is_procedural_structured_sibling_row(row))
        .filter(|row| {
            !row.content_text.trim().is_empty()
                || row.window_text.as_deref().is_some_and(|text| !text.trim().is_empty())
        })
        .collect::<Vec<_>>();
    eligible.sort_by(|left, right| {
        left.chunk_index.cmp(&right.chunk_index).then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });

    let mut selected = Vec::<&KnowledgeChunkRow>::new();
    let mut selected_ids = std::collections::BTreeSet::<Uuid>::new();
    if anchors.iter().any(|anchor| anchor.chunk_index == SOURCE_CONTEXT_DOCUMENT_HEAD_CHUNK_INDEX) {
        let setup_limit = limit.min(SOURCE_CONTEXT_PROCEDURAL_SETUP_LIMIT_PER_DOCUMENT);
        for row in eligible
            .iter()
            .copied()
            .filter(|row| {
                row.chunk_index > SOURCE_CONTEXT_DOCUMENT_HEAD_CHUNK_INDEX
                    && row.chunk_index
                        <= SOURCE_CONTEXT_DOCUMENT_HEAD_CHUNK_INDEX
                            .saturating_add(SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_FORWARD)
            })
            .take(setup_limit)
        {
            if selected_ids.insert(row.chunk_id) {
                selected.push(row);
            }
        }
    }

    let mut candidates = eligible
        .iter()
        .copied()
        .filter(|row| !selected_ids.contains(&row.chunk_id))
        .filter_map(|row| {
            let distance = anchors
                .iter()
                .filter_map(|anchor| {
                    (row.chunk_index >= anchor.chunk_index)
                        .then_some(row.chunk_index.saturating_sub(anchor.chunk_index))
                })
                .min()?;
            Some((distance, row))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|(left_distance, left), (right_distance, right)| {
        left_distance
            .cmp(right_distance)
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    for (_, row) in candidates {
        if selected.len() >= limit {
            break;
        }
        if selected_ids.insert(row.chunk_id) {
            selected.push(row);
        }
    }
    selected.into_iter().cloned().collect()
}

fn is_procedural_structured_sibling_row(row: &KnowledgeChunkRow) -> bool {
    matches!(
        row.chunk_kind.as_deref(),
        Some(
            TABLE_ROW_CHUNK_KIND
                | CODE_BLOCK_CHUNK_KIND
                | KEY_VALUE_BLOCK_CHUNK_KIND
                | METADATA_BLOCK_CHUNK_KIND
        )
    )
}

fn select_path_source_rows(
    rows: &[KnowledgeChunkRow],
    anchors: &[SourceContextAnchor],
    limit: usize,
    prioritize_module_setup_paths: bool,
) -> Vec<KnowledgeChunkRow> {
    if limit == 0 || rows.is_empty() {
        return Vec::new();
    }
    let anchor_indexes = anchors.iter().map(|anchor| anchor.chunk_index).collect::<Vec<_>>();
    let mut candidates = rows
        .iter()
        .filter(|row| !is_source_profile_chunk_row(row))
        .filter(|row| prioritize_module_setup_paths || !anchor_indexes.contains(&row.chunk_index))
        .filter_map(|row| {
            let text =
                format!("{}\n{}", row.content_text, row.window_text.as_deref().unwrap_or_default());
            let path_count = extract_explicit_path_literals(&text, 4).len();
            let setup_score = usize::from(
                prioritize_module_setup_paths
                    && path_count > 0
                    && !extract_package_command_literals(&text, 1).is_empty(),
            );
            (path_count > 0).then_some((setup_score, path_count, row))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|(left_setup, left_count, left), (right_setup, right_count, right)| {
        right_setup
            .cmp(left_setup)
            .then_with(|| right_count.cmp(left_count))
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    candidates.into_iter().take(limit).map(|(_, _, row)| row.clone()).collect()
}

fn setup_path_source_score_bonus(row: &KnowledgeChunkRow) -> f32 {
    let text = format!("{}\n{}", row.content_text, row.window_text.as_deref().unwrap_or_default());
    let has_package_command = !extract_package_command_literals(&text, 1).is_empty();
    let has_configuration_path = extract_explicit_path_literals(&text, 8).into_iter().any(|path| {
        let lowered = path.to_ascii_lowercase();
        lowered.ends_with(".conf") || lowered.ends_with(".ini")
    });
    if has_package_command && has_configuration_path {
        SOURCE_CONTEXT_SETUP_PATH_SCORE_BONUS
    } else {
        0.0
    }
}

fn source_context_neighbor_score(anchor_score: f32, distance: f32, chunk_index: i32) -> f32 {
    // Source companions expand the evidence around an anchor; they must not become
    // stronger anchors than the retrieval hit that caused the expansion.
    anchor_score
        - SOURCE_CONTEXT_NEIGHBOR_PENALTY
        - distance * 0.001
        - chunk_index.max(0) as f32 * 0.000_001
}

fn map_companion_chunk(
    row: KnowledgeChunkRow,
    score: f32,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    plan_keywords: &[String],
) -> Option<RuntimeMatchedChunk> {
    let is_source_profile = is_source_profile_chunk_row(&row);
    let content_text = row.content_text.clone();
    let mut chunk = map_chunk_hit(row, score, document_index, plan_keywords)?;
    if is_source_profile {
        chunk.chunk_kind = Some(SOURCE_PROFILE_CHUNK_KIND.to_string());
    }
    let repaired_content_text = repair_technical_layout_noise(&content_text);
    if source_context_content_preserves_missing_paths(&repaired_content_text, &chunk.source_text) {
        chunk.source_text = repaired_content_text;
    }
    chunk.score = Some(score);
    chunk.score_kind = crate::services::query::execution::RuntimeChunkScoreKind::SourceContext;
    chunk.excerpt = if is_source_profile_runtime_chunk(&chunk) {
        source_profile_excerpt(&chunk.source_text)
    } else {
        let excerpt =
            focused_excerpt_for(&chunk.source_text, plan_keywords, SOURCE_CONTEXT_EXCERPT_CHARS);
        if excerpt.trim().is_empty() {
            excerpt_for(&chunk.source_text, SOURCE_CONTEXT_EXCERPT_CHARS)
        } else {
            excerpt
        }
    };
    Some(chunk)
}

fn source_context_content_preserves_missing_paths(content_text: &str, source_text: &str) -> bool {
    let paths = extract_explicit_path_literals(content_text, 8);
    !paths.is_empty() && paths.iter().any(|path| !source_text.contains(path))
}

fn map_source_unit_block(
    block: KnowledgeStructuredBlockRow,
    score: f32,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> Option<RuntimeMatchedChunk> {
    let document = document_index.get(&block.document_id)?;
    let canonical_revision_id = canonical_document_revision_id(document)?;
    if block.revision_id != canonical_revision_id {
        return None;
    }
    let document_label = document
        .title
        .clone()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| document.file_name.clone())
        .unwrap_or_else(|| document.external_key.clone());
    let source_text = if block.text.trim().is_empty() {
        block.normalized_text.clone()
    } else {
        block.text.clone()
    };
    Some(RuntimeMatchedChunk {
        chunk_id: block.block_id,
        document_id: block.document_id,
        revision_id: block.revision_id,
        chunk_index: block.ordinal,
        chunk_kind: Some(SOURCE_UNIT_CHUNK_KIND.to_string()),
        document_label,
        excerpt: excerpt_for(&source_text, SOURCE_CONTEXT_EXCERPT_CHARS),
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::SourceContext,
        score: Some(score),
        source_text,
    })
}

fn apply_structured_source_companions(
    chunks: &mut Vec<RuntimeMatchedChunk>,
    companions: Vec<StructuredSourceCompanion>,
) -> StructuredSourceContextDiagnostics {
    if companions.is_empty() {
        return StructuredSourceContextDiagnostics::default();
    }

    let original_rank = chunks
        .iter()
        .enumerate()
        .map(|(rank, chunk)| (chunk.chunk_id, rank))
        .collect::<HashMap<_, _>>();
    let mut merged =
        chunks.drain(..).map(|chunk| (chunk.chunk_id, chunk)).collect::<HashMap<_, _>>();
    let mut diagnostics = StructuredSourceContextDiagnostics::default();

    for companion in companions {
        match companion.kind {
            StructuredSourceCompanionKind::SourceProfile => {
                diagnostics.source_profile_count += 1;
            }
            StructuredSourceCompanionKind::Neighbor => {
                diagnostics.neighbor_count += 1;
            }
            StructuredSourceCompanionKind::FocusedMatch => {
                diagnostics.focused_match_count += 1;
            }
            StructuredSourceCompanionKind::ProceduralStructuredSibling => {
                diagnostics.procedural_structured_sibling_count += 1;
            }
            StructuredSourceCompanionKind::LibrarySourceProfile => {
                diagnostics.source_profile_count += 1;
                diagnostics.library_profile_count += 1;
            }
        }
        merged
            .entry(companion.chunk.chunk_id)
            .and_modify(|existing| {
                if score_value(companion.chunk.score) > score_value(existing.score) {
                    *existing = companion.chunk.clone();
                } else if source_context_content_preserves_missing_paths(
                    &companion.chunk.source_text,
                    &existing.source_text,
                ) {
                    existing.source_text = companion.chunk.source_text.clone();
                    existing.excerpt = companion.chunk.excerpt.clone();
                }
            })
            .or_insert(companion.chunk);
    }

    let mut values = merged.into_values().collect::<Vec<_>>();
    values.sort_by(|left, right| {
        score_value(right.score)
            .total_cmp(&score_value(left.score))
            .then_with(|| {
                let left_rank = original_rank.get(&left.chunk_id).copied().unwrap_or(usize::MAX);
                let right_rank = original_rank.get(&right.chunk_id).copied().unwrap_or(usize::MAX);
                left_rank.cmp(&right_rank)
            })
            .then_with(|| left.document_id.cmp(&right.document_id))
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    *chunks = values;
    diagnostics
}

fn canonical_source_profile_revision_ids(
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    limit: usize,
) -> Vec<Uuid> {
    if limit == 0 {
        return Vec::new();
    }
    let mut rows = document_index
        .values()
        .filter(|document| document.document_state == "active")
        .filter_map(|document| {
            document
                .readable_revision_id
                .or(document.active_revision_id)
                .map(|revision_id| (document.updated_at, document.document_id, revision_id))
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| right.1.cmp(&left.1)));
    rows.truncate(limit);
    rows.into_iter().map(|(_, _, revision_id)| revision_id).collect()
}

fn source_profile_excerpt(text: &str) -> String {
    text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or(text.trim()).to_string()
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use crate::domains::query_ir::{QueryAct, QueryLanguage, QueryScope};

    use super::*;

    fn runtime_chunk(document_id: Uuid, revision_id: Uuid, index: i32) -> RuntimeMatchedChunk {
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id,
            revision_id,
            chunk_index: index,
            chunk_kind: Some("metadata_block".to_string()),
            document_label: "event-stream.jsonl".to_string(),
            excerpt: format!("unit {index}"),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.5 - index as f32 * 0.01),
            source_text: format!("[unit_id=u-{index}] unit {index}"),
        }
    }

    fn companion_chunk(
        document_id: Uuid,
        revision_id: Uuid,
        index: i32,
        kind: &str,
        score: f32,
    ) -> RuntimeMatchedChunk {
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id,
            revision_id,
            chunk_index: index,
            chunk_kind: Some(kind.to_string()),
            document_label: "event-stream.jsonl".to_string(),
            excerpt: format!("{kind} {index}"),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(score),
            source_text: format!("{kind} {index}"),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn chunk_row(
        document_id: Uuid,
        revision_id: Uuid,
        index: i32,
        kind: &str,
        text: &str,
    ) -> KnowledgeChunkRow {
        KnowledgeChunkRow {
            key: Uuid::now_v7().to_string(),
            arango_id: None,
            arango_rev: None,
            chunk_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id,
            revision_id,
            chunk_index: index,
            chunk_kind: Some(kind.to_string()),
            content_text: text.to_string(),
            normalized_text: text.to_string(),
            span_start: Some(0),
            span_end: Some(text.len() as i32),
            token_count: Some(1),
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

    fn document_row(document_id: Uuid, revision_id: Uuid) -> KnowledgeDocumentRow {
        KnowledgeDocumentRow {
            key: document_id.to_string(),
            arango_id: None,
            arango_rev: None,
            document_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            external_key: "event-stream.jsonl".to_string(),
            file_name: Some("event-stream.jsonl".to_string()),
            title: Some("event-stream.jsonl".to_string()),
            document_state: "active".to_string(),
            active_revision_id: Some(revision_id),
            readable_revision_id: Some(revision_id),
            latest_revision_no: Some(1),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            deleted_at: None,
        }
    }

    fn source_unit_block(
        document_id: Uuid,
        revision_id: Uuid,
        ordinal: i32,
        text: &str,
    ) -> KnowledgeStructuredBlockRow {
        KnowledgeStructuredBlockRow {
            key: Uuid::now_v7().to_string(),
            arango_id: None,
            arango_rev: None,
            block_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id,
            revision_id,
            ordinal,
            block_kind: SOURCE_UNIT_CHUNK_KIND.to_string(),
            text: text.to_string(),
            normalized_text: text.to_string(),
            heading_trail: Vec::new(),
            section_path: Vec::new(),
            page_number: None,
            span_start: Some(0),
            span_end: Some(text.len() as i32),
            parent_block_id: None,
            table_coordinates_json: None,
            code_language: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn source_slice_ir(direction: SourceSliceDirection, count: Option<u16>) -> QueryIR {
        QueryIR {
            act: QueryAct::Enumerate,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec!["record".to_string()],
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: Some(SourceSliceSpec {
                direction,
                count,
                filter: SourceSliceFilter::None,
            }),
            retrieval_query: None,
            confidence: 0.8,
        }
    }

    fn latest_release_source_slice_ir(count: Option<u16>) -> QueryIR {
        let mut ir = source_slice_ir(SourceSliceDirection::Tail, count);
        ir.act = QueryAct::Describe;
        ir.scope = QueryScope::LibraryMeta;
        ir.target_types = vec!["release".to_string()];
        if let Some(slice) = ir.source_slice.as_mut() {
            slice.filter = SourceSliceFilter::ReleaseMarker;
        }
        ir
    }

    fn source_context_ir(act: QueryAct, scope: QueryScope) -> QueryIR {
        QueryIR {
            act,
            scope,
            language: QueryLanguage::Auto,
            target_types: Vec::new(),
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence: 0.9,
        }
    }

    #[test]
    fn structural_source_profile_marker_is_recognized_without_kind() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let document_index = HashMap::from([(document_id, document_row(document_id, revision_id))]);
        let row = chunk_row(
            document_id,
            revision_id,
            0,
            "metadata_block",
            "[source_profile unit_count=3]",
        );

        let mapped = map_companion_chunk(row, 1.0, &document_index, &[]).unwrap();

        assert_eq!(mapped.chunk_kind.as_deref(), Some("source_profile"));
        assert_eq!(mapped.excerpt, "[source_profile unit_count=3]");
    }

    #[test]
    fn map_companion_chunk_preserves_paths_lost_from_window_text() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let document_index = HashMap::from([(document_id, document_row(document_id, revision_id))]);
        let mut row = chunk_row(
            document_id,
            revision_id,
            1,
            "code_block",
            "aptitude install alpha-connector\n\
             dpkg-reconfigure alpha-connector\n\
             module configuration: /opt/alpha/modules/connector/connector.conf",
        );
        row.window_text = Some(
            "aptitude install alpha-connector\n\
             dpkg-reconfigure alpha-connector\n\
             module configuration:"
                .to_string(),
        );

        let mapped = map_companion_chunk(row, 1.0, &document_index, &[]).unwrap();

        assert!(mapped.source_text.contains("/opt/alpha/modules/connector/connector.conf"));
        assert!(mapped.excerpt.contains("/opt/alpha/modules/connector/connector.conf"));
    }

    #[test]
    fn query_focused_source_rows_select_late_matching_chunk_inside_selected_document() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let rows = vec![
            chunk_row(document_id, revision_id, 0, "paragraph", "Alpha service overview"),
            chunk_row(
                document_id,
                revision_id,
                4,
                "paragraph",
                "RareNeedle setting controls the payment confirmation format.",
            ),
            chunk_row(document_id, revision_id, 5, "paragraph", "Unrelated appendix"),
        ];
        let anchors = vec![SourceContextAnchor { chunk_index: 0, score: 10.0, first_rank: 0 }];

        let selected = select_query_focused_source_rows(
            &rows,
            &["rareneedle".to_string(), "payment".to_string()],
            false,
            &anchors,
            1,
        );

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].chunk_index, 4);
    }

    #[test]
    fn path_source_rows_select_path_literals_inside_selected_document() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let rows = vec![
            chunk_row(document_id, revision_id, 0, "paragraph", "Alpha service overview"),
            chunk_row(
                document_id,
                revision_id,
                3,
                "paragraph",
                "The connector parameters are stored in /opt/provider-alpha/connector.conf.",
            ),
            chunk_row(document_id, revision_id, 4, "paragraph", "Unrelated appendix"),
        ];
        let anchors = vec![SourceContextAnchor { chunk_index: 0, score: 10.0, first_rank: 0 }];

        let selected = select_path_source_rows(&rows, &anchors, 1, false);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].chunk_index, 3);
    }

    #[test]
    fn path_source_rows_skip_existing_anchor_chunks() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let rows = vec![
            chunk_row(
                document_id,
                revision_id,
                0,
                "paragraph",
                "The connector parameters are stored in /opt/provider-alpha/connector.conf.",
            ),
            chunk_row(
                document_id,
                revision_id,
                2,
                "paragraph",
                "The audit output is written to /var/log/provider-alpha/audit.log.",
            ),
        ];
        let anchors = vec![SourceContextAnchor { chunk_index: 0, score: 10.0, first_rank: 0 }];

        let selected = select_path_source_rows(&rows, &anchors, 1, false);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].chunk_index, 2);
    }

    #[test]
    fn configuration_path_source_rows_prefer_module_setup_commands() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let rows = vec![
            chunk_row(document_id, revision_id, 0, "paragraph", "Alpha service overview"),
            chunk_row(
                document_id,
                revision_id,
                1,
                "code_block",
                "Install the module:\naptitude install alpha-connector\n\nConfigure it:\ndpkg-reconfigure alpha-connector\n\nSettings are stored in /opt/alpha/modules/connector/connector.conf.",
            ),
            chunk_row(
                document_id,
                revision_id,
                8,
                "code_block",
                "Example paths: /opt/alpha/ui.ini /opt/alpha/display.ini /opt/alpha/log.ini.",
            ),
        ];
        let anchors = vec![SourceContextAnchor { chunk_index: 0, score: 10.0, first_rank: 0 }];

        let selected = select_path_source_rows(&rows, &anchors, 1, true);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].chunk_index, 1);
    }

    #[test]
    fn configuration_path_source_rows_keep_setup_anchor_chunks() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let rows = vec![
            chunk_row(
                document_id,
                revision_id,
                1,
                "code_block",
                "Install the module:\naptitude install alpha-connector\n\nConfigure it:\ndpkg-reconfigure alpha-connector\n\nSettings are stored in /opt/alpha/modules/connector/connector.conf.",
            ),
            chunk_row(
                document_id,
                revision_id,
                2,
                "table_row",
                "| url | string | Server URL | Default http://localhost |",
            ),
        ];
        let anchors = vec![SourceContextAnchor { chunk_index: 1, score: 10.0, first_rank: 0 }];

        let selected = select_path_source_rows(&rows, &anchors, 1, true);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].chunk_index, 1);
    }

    #[test]
    fn configuration_file_targets_request_path_source_context() {
        let mut query_ir = source_context_ir(QueryAct::ConfigureHow, QueryScope::SingleDocument);
        query_ir.target_types = vec!["configuration_file".to_string()];

        assert!(
            requests_path_source_context(&query_ir),
            "setup answers that ask for a configuration file need path-bearing chunks even without a path literal"
        );
    }

    #[test]
    fn procedural_structured_siblings_select_rows_after_anchors_without_query_terms() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let rows = vec![
            chunk_row(document_id, revision_id, 0, "metadata_block", "[source_profile units=5]"),
            chunk_row(
                document_id,
                revision_id,
                1,
                "code_block",
                "install package alpha-connector\nconfig /opt/alpha/connector.conf",
            ),
            chunk_row(document_id, revision_id, 2, "table_row", "merchantId | partner id"),
            chunk_row(document_id, revision_id, 3, "paragraph", "Narrative detail"),
            chunk_row(document_id, revision_id, 4, "table_row", "timeout | request timeout"),
            chunk_row(document_id, revision_id, 19, "table_row", "lateParam | out of window"),
        ];
        let anchors = vec![SourceContextAnchor { chunk_index: 0, score: 10.0, first_rank: 0 }];

        let selected = select_procedural_structured_sibling_rows(&rows, &anchors, 3);

        assert_eq!(selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>(), [1, 2, 4]);
    }

    #[test]
    fn procedural_structured_siblings_reserve_setup_rows_before_late_anchor_rows() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let rows = vec![
            chunk_row(document_id, revision_id, 0, "metadata_block", "[source_profile units=8]"),
            chunk_row(
                document_id,
                revision_id,
                1,
                "code_block",
                "install package alpha-connector\nconfig /opt/alpha/connector.conf",
            ),
            chunk_row(document_id, revision_id, 4, "table_row", "merchantId | partner id"),
            chunk_row(document_id, revision_id, 21, "table_row", "lateFlag | optional behavior"),
            chunk_row(document_id, revision_id, 22, "table_row", "lateMode | optional mode"),
        ];
        let anchors = vec![
            SourceContextAnchor { chunk_index: 0, score: 10.0, first_rank: 0 },
            SourceContextAnchor { chunk_index: 20, score: 9.0, first_rank: 1 },
        ];

        let selected = select_procedural_structured_sibling_rows(&rows, &anchors, 3);

        assert_eq!(selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>(), [1, 4, 21]);
    }

    #[test]
    fn procedural_structured_sibling_windows_expand_forward_only() {
        let anchors = vec![
            SourceContextAnchor { chunk_index: 0, score: 10.0, first_rank: 0 },
            SourceContextAnchor { chunk_index: 20, score: 9.0, first_rank: 1 },
        ];

        assert_eq!(
            procedural_structured_sibling_windows(&anchors),
            vec![
                (0, SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_FORWARD),
                (20, 20 + SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_FORWARD)
            ]
        );
    }

    #[test]
    fn source_context_focus_keywords_include_typed_query_ir_focus() {
        let mut query_ir = QueryIR {
            act: QueryAct::Describe,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: Vec::new(),
            target_entities: vec![crate::domains::query_ir::EntityMention {
                label: "deferred ticket".to_string(),
                role: crate::domains::query_ir::EntityRole::Object,
            }],
            literal_constraints: vec![crate::domains::query_ir::LiteralSpan {
                text: "code verification".to_string(),
                kind: crate::domains::query_ir::LiteralKind::Other,
            }],
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: Some(crate::domains::query_ir::DocumentHint {
                hint: "regulated product category".to_string(),
            }),
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence: 0.9,
        };

        let keywords = source_context_focus_keywords(
            "Which controlled product rule applies?",
            Some(&query_ir),
            &["controlled".to_string()],
        );

        assert!(keywords.contains(&"regulated product category".to_string()));
        assert!(keywords.contains(&"deferred ticket".to_string()));
        assert!(keywords.contains(&"code verification".to_string()));
        assert!(keywords.contains(&"controlled".to_string()));

        query_ir.document_focus = None;
        let keywords_without_focus =
            source_context_focus_keywords("Which rule applies?", Some(&query_ir), &[]);
        assert!(!keywords_without_focus.contains(&"regulated product category".to_string()));
    }

    #[test]
    fn source_slice_top_k_expands_and_clamps_context_budget() {
        let requested = source_slice_ir(SourceSliceDirection::Tail, Some(20));
        let defaulted = source_slice_ir(SourceSliceDirection::Head, None);
        let too_large = source_slice_ir(SourceSliceDirection::All, Some(500));

        assert_eq!(source_slice_context_top_k(&requested, 8), 21);
        assert_eq!(source_slice_context_top_k(&defaulted, 8), 13);
        assert_eq!(source_slice_context_top_k(&too_large, 8), 31);
        assert_eq!(structured_source_context_top_k(&requested, 8), 21);
    }

    #[test]
    fn latest_release_tail_slice_carries_typed_marker_filter() {
        let query_ir = latest_release_source_slice_ir(Some(10));
        let slice = query_ir.source_slice.as_ref().unwrap();

        assert_eq!(slice.filter, SourceSliceFilter::ReleaseMarker);
    }

    #[test]
    fn ordinary_tail_slice_keeps_unfiltered_tail_units() {
        let query_ir = source_slice_ir(SourceSliceDirection::Tail, Some(2));
        let slice = query_ir.source_slice.as_ref().unwrap();

        assert_eq!(slice.filter, SourceSliceFilter::None);
    }

    #[test]
    fn procedural_source_context_expands_default_top_k() {
        let query_ir = source_context_ir(QueryAct::ConfigureHow, QueryScope::SingleDocument);

        assert_eq!(
            structured_source_context_top_k(&query_ir, 5),
            procedural_source_context_chunk_floor()
        );
        assert_eq!(
            structured_source_context_top_k(
                &query_ir,
                procedural_source_context_chunk_floor().saturating_add(1)
            ),
            procedural_source_context_chunk_floor().saturating_add(1)
        );
    }

    #[test]
    fn descriptive_source_context_keeps_default_top_k() {
        let query_ir = source_context_ir(QueryAct::Describe, QueryScope::SingleDocument);

        assert_eq!(structured_source_context_top_k(&query_ir, 5), 5);
    }

    #[test]
    fn map_source_unit_block_preserves_record_ordinal_and_text() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let document_index = HashMap::from([(document_id, document_row(document_id, revision_id))]);
        let block = source_unit_block(
            document_id,
            revision_id,
            42,
            "[unit_id=u-42 occurred_at=2026-04-29T12:22:51Z] final record",
        );

        let mapped = map_source_unit_block(block, 3.0, &document_index).unwrap();

        assert_eq!(mapped.chunk_index, 42);
        assert_eq!(mapped.chunk_kind.as_deref(), Some(SOURCE_UNIT_CHUNK_KIND));
        assert!(is_source_unit_runtime_chunk(&mapped));
        assert!(mapped.source_text.contains("final record"));
    }

    #[test]
    fn collect_candidates_limits_documents_and_anchors() {
        let revision_id = Uuid::now_v7();
        let docs = [Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()];
        let chunks = docs
            .iter()
            .flat_map(|document_id| {
                [
                    runtime_chunk(*document_id, revision_id, 4),
                    runtime_chunk(*document_id, revision_id, 5),
                    runtime_chunk(*document_id, revision_id, 6),
                ]
            })
            .collect::<Vec<_>>();

        let candidates = collect_source_context_candidates(&chunks);

        assert_eq!(candidates.len(), SOURCE_CONTEXT_DOCUMENT_LIMIT);
        assert!(candidates.iter().all(|candidate| {
            candidate.anchors.len() == SOURCE_CONTEXT_ANCHOR_LIMIT_PER_DOCUMENT
        }));
        assert_eq!(candidates[0].document_id, docs[0]);
    }

    #[test]
    fn graph_evidence_documents_prepend_source_context_candidates() {
        let graph_document_id = Uuid::now_v7();
        let graph_revision_id = Uuid::now_v7();
        let generic_document_id = Uuid::now_v7();
        let generic_revision_id = Uuid::now_v7();
        let document_index = HashMap::from([
            (graph_document_id, document_row(graph_document_id, graph_revision_id)),
            (generic_document_id, document_row(generic_document_id, generic_revision_id)),
        ]);
        let generic_chunk = RuntimeMatchedChunk {
            score: Some(4.0),
            ..runtime_chunk(generic_document_id, generic_revision_id, 7)
        };
        let candidates = collect_source_context_candidates(std::slice::from_ref(&generic_chunk));

        let merged = merge_graph_evidence_source_context_candidates(
            candidates,
            &[graph_document_id],
            &document_index,
            &[generic_chunk],
        );

        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].document_id, graph_document_id);
        assert_eq!(merged[0].revision_id, graph_revision_id);
        assert!(merged[0].anchors.is_empty());
        assert_eq!(merged[1].document_id, generic_document_id);
    }

    #[test]
    fn graph_evidence_documents_promote_existing_candidate_without_losing_anchors() {
        let graph_document_id = Uuid::now_v7();
        let graph_revision_id = Uuid::now_v7();
        let other_document_id = Uuid::now_v7();
        let other_revision_id = Uuid::now_v7();
        let document_index = HashMap::from([
            (graph_document_id, document_row(graph_document_id, graph_revision_id)),
            (other_document_id, document_row(other_document_id, other_revision_id)),
        ]);
        let graph_chunk = RuntimeMatchedChunk {
            score: Some(1.0),
            ..runtime_chunk(graph_document_id, graph_revision_id, 3)
        };
        let other_chunk = RuntimeMatchedChunk {
            score: Some(5.0),
            ..runtime_chunk(other_document_id, other_revision_id, 4)
        };
        let candidates =
            collect_source_context_candidates(&[other_chunk.clone(), graph_chunk.clone()]);

        let merged = merge_graph_evidence_source_context_candidates(
            candidates,
            &[graph_document_id],
            &document_index,
            &[other_chunk, graph_chunk],
        );

        assert_eq!(merged[0].document_id, graph_document_id);
        assert_eq!(merged[0].anchors.len(), 1);
        assert_eq!(merged[0].anchors[0].chunk_index, 3);
        assert!(merged[0].best_score > 5.0);
    }

    #[test]
    fn procedural_head_anchor_reaches_initial_setup_chunk() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let late_detail_chunk =
            RuntimeMatchedChunk { score: Some(7.0), ..runtime_chunk(document_id, revision_id, 22) };
        let mut candidates =
            collect_source_context_candidates(std::slice::from_ref(&late_detail_chunk));
        seed_document_head_source_context_anchors(&mut candidates);
        let span = SourceContextNeighborSpan {
            backward: SOURCE_CONTEXT_PROCEDURAL_NEIGHBOR_BACKWARD,
            forward: SOURCE_CONTEXT_DEFAULT_NEIGHBOR_FORWARD,
        };

        assert_eq!(candidates.len(), 1);
        assert!(
            candidates[0]
                .anchors
                .iter()
                .any(|anchor| anchor.chunk_index == SOURCE_CONTEXT_DOCUMENT_HEAD_CHUNK_INDEX)
        );
        assert!(source_context_neighbor_windows(&candidates[0].anchors, span).contains(
            &source_anchor_window(
                SOURCE_CONTEXT_DOCUMENT_HEAD_CHUNK_INDEX,
                SOURCE_CONTEXT_PROCEDURAL_NEIGHBOR_BACKWARD,
                SOURCE_CONTEXT_DEFAULT_NEIGHBOR_FORWARD,
            )
        ));
        assert!(
            source_context_best_neighbor_score(&candidates[0].anchors, 1, span).is_some(),
            "procedural source context must keep the document's setup block reachable"
        );
    }

    #[test]
    fn collect_candidates_ranks_anchors_by_score_before_ordinal() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let low_ordinal_low_score = runtime_chunk(document_id, revision_id, 1);
        let high_ordinal_high_score =
            RuntimeMatchedChunk { score: Some(8.0), ..runtime_chunk(document_id, revision_id, 50) };
        let next_best =
            RuntimeMatchedChunk { score: Some(7.0), ..runtime_chunk(document_id, revision_id, 2) };

        let candidates = collect_source_context_candidates(&[
            low_ordinal_low_score,
            high_ordinal_high_score,
            next_best,
        ]);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].anchors[0].chunk_index, 50);
        assert_eq!(candidates[0].anchors[1].chunk_index, 2);
    }

    #[test]
    fn source_profile_does_not_drive_candidate_score_or_anchors() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let profile = companion_chunk(document_id, revision_id, 0, "source_profile", 100.0);
        let content =
            RuntimeMatchedChunk { score: Some(3.0), ..runtime_chunk(document_id, revision_id, 8) };

        let candidates = collect_source_context_candidates(&[profile, content]);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].best_score, 3.0);
        assert_eq!(candidates[0].anchors.len(), 1);
        assert_eq!(candidates[0].anchors[0].chunk_index, 8);
    }

    #[test]
    fn neighbor_score_stays_below_anchor_score() {
        let score = source_context_neighbor_score(42.0, 0.0, 10);

        assert!(score < 42.0);
    }

    #[test]
    fn focused_match_extends_neighbor_windows() {
        let span = source_context_neighbor_span(None);
        let mut anchors = vec![
            SourceContextAnchor { chunk_index: 24, score: 12.0, first_rank: 0 },
            SourceContextAnchor { chunk_index: 22, score: 11.0, first_rank: 1 },
        ];

        push_unique_source_context_anchor(
            &mut anchors,
            SourceContextAnchor { chunk_index: 20, score: 13.0, first_rank: usize::MAX },
        );

        assert_eq!(
            anchors.iter().map(|anchor| anchor.chunk_index).collect::<Vec<_>>(),
            [24, 22, 20]
        );
        assert!(source_context_neighbor_windows(&anchors, span).contains(&(19, 21)));
        assert!(
            source_context_best_neighbor_score(&anchors, 19, span).is_some(),
            "the source chunk immediately before a focused match must be eligible"
        );
    }

    #[test]
    fn configure_how_expands_preceding_setup_context() {
        let query_ir = source_context_ir(QueryAct::ConfigureHow, QueryScope::SingleDocument);
        let span = source_context_neighbor_span(Some(&query_ir));
        let anchors = vec![SourceContextAnchor { chunk_index: 22, score: 11.0, first_rank: 0 }];

        assert_eq!(
            span,
            SourceContextNeighborSpan {
                backward: SOURCE_CONTEXT_PROCEDURAL_NEIGHBOR_BACKWARD,
                forward: SOURCE_CONTEXT_DEFAULT_NEIGHBOR_FORWARD
            }
        );
        assert!(source_context_neighbor_windows(&anchors, span).contains(&(19, 23)));
        assert!(
            source_context_best_neighbor_score(&anchors, 19, span).is_some(),
            "procedural answers need the setup block that precedes the matching detail chunk"
        );
    }

    #[test]
    fn configuration_target_types_expand_source_context_without_configure_act() {
        let mut query_ir = source_context_ir(QueryAct::Describe, QueryScope::SingleDocument);
        query_ir.target_types = vec!["configuration_file".to_string(), "config_key".to_string()];
        query_ir.document_focus =
            Some(crate::domains::query_ir::DocumentHint { hint: "Provider Alpha".to_string() });
        let span = source_context_neighbor_span(Some(&query_ir));

        assert!(requests_expanded_source_context(&query_ir));
        assert_eq!(
            span,
            SourceContextNeighborSpan {
                backward: SOURCE_CONTEXT_PROCEDURAL_NEIGHBOR_BACKWARD,
                forward: SOURCE_CONTEXT_DEFAULT_NEIGHBOR_FORWARD
            }
        );
        assert!(
            structured_source_context_top_k(&query_ir, 3) > 3,
            "typed configuration answers need room for nearby key/value and code chunks"
        );
    }

    #[test]
    fn anchorless_configuration_describe_keeps_default_source_context() {
        let mut query_ir = source_context_ir(QueryAct::Describe, QueryScope::SingleDocument);
        query_ir.target_types = vec!["config_key".to_string()];

        assert!(!requests_expanded_source_context(&query_ir));
        assert_eq!(structured_source_context_top_k(&query_ir, 3), 3);
    }

    #[test]
    fn error_code_intent_expands_source_context_like_setup_questions() {
        let mut query_ir = source_context_ir(QueryAct::RetrieveValue, QueryScope::SingleDocument);
        query_ir.target_types = vec!["error_code".to_string()];
        let span = source_context_neighbor_span(Some(&query_ir));

        assert!(requests_expanded_source_context(&query_ir));
        assert_eq!(
            span,
            SourceContextNeighborSpan {
                backward: SOURCE_CONTEXT_PROCEDURAL_NEIGHBOR_BACKWARD,
                forward: SOURCE_CONTEXT_DEFAULT_NEIGHBOR_FORWARD
            }
        );
        assert!(
            structured_source_context_top_k(&query_ir, 3) > 3,
            "typed diagnostic lookups need enough room for graph-source companions"
        );
    }

    #[test]
    fn transport_inventory_intent_expands_source_context_like_setup_questions() {
        let mut query_ir = source_context_ir(QueryAct::RetrieveValue, QueryScope::SingleDocument);
        query_ir.target_types =
            vec!["port".to_string(), "protocol".to_string(), "connection".to_string()];
        let span = source_context_neighbor_span(Some(&query_ir));

        assert!(requests_expanded_source_context(&query_ir));
        assert_eq!(
            span,
            SourceContextNeighborSpan {
                backward: SOURCE_CONTEXT_PROCEDURAL_NEIGHBOR_BACKWARD,
                forward: SOURCE_CONTEXT_DEFAULT_NEIGHBOR_FORWARD
            }
        );
        assert!(
            structured_source_context_top_k(&query_ir, 3) > 3,
            "transport inventory lookups need room for URL and port-bearing companions"
        );
    }

    #[test]
    fn code_pattern_query_terms_keep_short_digit_anchors() {
        let terms = code_pattern_query_terms(
            &[
                "E101".to_string(),
                "8583".to_string(),
                "362".to_string(),
                "error".to_string(),
                "codes".to_string(),
                "card".to_string(),
            ],
            6,
        );

        assert_eq!(terms, vec!["e101", "8583", "362", "error", "codes", "card"]);
    }

    #[test]
    fn non_procedural_source_context_stays_narrow() {
        let query_ir = source_context_ir(QueryAct::Describe, QueryScope::SingleDocument);
        let span = source_context_neighbor_span(Some(&query_ir));
        let anchors = vec![SourceContextAnchor { chunk_index: 22, score: 11.0, first_rank: 0 }];

        assert_eq!(
            span,
            SourceContextNeighborSpan {
                backward: SOURCE_CONTEXT_DEFAULT_NEIGHBOR_BACKWARD,
                forward: SOURCE_CONTEXT_DEFAULT_NEIGHBOR_FORWARD
            }
        );
        assert!(source_context_neighbor_windows(&anchors, span).contains(&(21, 23)));
        assert!(
            source_context_best_neighbor_score(&anchors, 19, span).is_none(),
            "default descriptive context should not silently widen evidence windows"
        );
    }

    #[test]
    fn focused_match_anchor_dedupes_against_existing_anchor() {
        let mut anchors = vec![SourceContextAnchor { chunk_index: 7, score: 5.0, first_rank: 4 }];

        push_unique_source_context_anchor(
            &mut anchors,
            SourceContextAnchor { chunk_index: 7, score: 8.0, first_rank: usize::MAX },
        );

        assert_eq!(anchors.len(), 1);
        assert_eq!(anchors[0].chunk_index, 7);
        assert_eq!(anchors[0].score, 8.0);
    }

    #[test]
    fn neighbor_companion_does_not_sort_above_anchor() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mut chunks = vec![RuntimeMatchedChunk {
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(42.0),
            ..runtime_chunk(document_id, revision_id, 10)
        }];
        let neighbor = RuntimeMatchedChunk {
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(source_context_neighbor_score(42.0, 1.0, 11)),
            ..runtime_chunk(document_id, revision_id, 11)
        };

        apply_structured_source_companions(
            &mut chunks,
            vec![StructuredSourceCompanion {
                chunk: neighbor,
                kind: StructuredSourceCompanionKind::Neighbor,
            }],
        );

        assert_eq!(chunks[0].chunk_index, 10);
        assert_eq!(chunks[1].chunk_index, 11);
    }

    #[test]
    fn companions_promote_profile_and_dedupe_neighbor() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let existing = runtime_chunk(document_id, revision_id, 10);
        let duplicate_neighbor = RuntimeMatchedChunk {
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(2.0),
            excerpt: "expanded unit 10".to_string(),
            source_text: "expanded unit 10".to_string(),
            ..existing.clone()
        };
        let profile = companion_chunk(document_id, revision_id, 0, "source_profile", 3.0);
        let mut chunks = vec![existing.clone()];

        let diagnostics = apply_structured_source_companions(
            &mut chunks,
            vec![
                StructuredSourceCompanion {
                    chunk: profile.clone(),
                    kind: StructuredSourceCompanionKind::SourceProfile,
                },
                StructuredSourceCompanion {
                    chunk: duplicate_neighbor,
                    kind: StructuredSourceCompanionKind::Neighbor,
                },
            ],
        );

        assert_eq!(diagnostics.source_profile_count, 1);
        assert_eq!(diagnostics.neighbor_count, 1);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chunk_kind.as_deref(), Some("source_profile"));
        let expanded = chunks.iter().find(|chunk| chunk.chunk_id == existing.chunk_id).unwrap();
        assert_eq!(expanded.excerpt, "expanded unit 10");
    }

    #[test]
    fn companions_enrich_duplicate_chunks_with_missing_paths() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let existing = RuntimeMatchedChunk {
            score: Some(20.0),
            source_text: "install alpha-connector and configure the module".to_string(),
            excerpt: "install alpha-connector".to_string(),
            ..runtime_chunk(document_id, revision_id, 10)
        };
        let enriched = RuntimeMatchedChunk {
            score: Some(10.0),
            source_text:
                "install alpha-connector and configure /opt/alpha/modules/connector/connector.conf"
                    .to_string(),
            excerpt: "configure /opt/alpha/modules/connector/connector.conf".to_string(),
            ..existing.clone()
        };
        let mut chunks = vec![existing.clone()];

        apply_structured_source_companions(
            &mut chunks,
            vec![StructuredSourceCompanion {
                chunk: enriched,
                kind: StructuredSourceCompanionKind::Neighbor,
            }],
        );

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].score, Some(20.0));
        assert!(chunks[0].source_text.contains("/opt/alpha/modules/connector/connector.conf"));
        assert!(chunks[0].excerpt.contains("/opt/alpha/modules/connector/connector.conf"));
    }

    #[test]
    fn map_companion_chunk_drops_orphan_documents_without_heads() {
        // Contract update mirrors `map_chunk_hit`: companion chunks are
        // no longer dropped on plain revision-id mismatch — only when
        // the owning document has both heads null (orphan).
        let document_id = Uuid::now_v7();
        let stale_revision_id = Uuid::now_v7();
        let mut orphan = document_row(document_id, Uuid::now_v7());
        orphan.active_revision_id = None;
        orphan.readable_revision_id = None;
        let document_index = HashMap::from([(document_id, orphan)]);
        let row = chunk_row(
            document_id,
            stale_revision_id,
            0,
            "source_profile",
            "[source_profile unit_count=3]",
        );

        let mapped = map_companion_chunk(row, 1.0, &document_index, &[]);

        assert!(mapped.is_none());
    }
}
