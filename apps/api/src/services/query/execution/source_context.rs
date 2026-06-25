use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::Context;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::query_ir::{
        QueryAct, QueryIR, QueryScope, SourceSliceDirection, SourceSliceFilter, SourceSliceSpec,
    },
    infra::knowledge_rows::{KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeStructuredBlockRow},
    shared::extraction::{
        record_jsonl::focused_record_unit_excerpt, text_render::repair_technical_layout_noise,
    },
};

use super::{
    RuntimeMatchedChunk, question_asks_table_aggregation,
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
        detect_technical_literal_intent_from_query_ir, extract_config_assignment_literals,
        extract_config_section_literals, extract_explicit_path_literals,
        extract_package_command_literals, extract_parameter_literals,
        technical_chunk_selection_score, technical_literal_focus_keywords,
    },
};

const SOURCE_CONTEXT_DOCUMENT_LIMIT: usize = 3;
const SOURCE_CONTEXT_FALLBACK_DOCUMENT_LIMIT: usize = SOURCE_CONTEXT_DOCUMENT_LIMIT * 8;
const SOURCE_CONTEXT_PROFILE_DOCUMENT_LIMIT: usize = SOURCE_CONTEXT_DOCUMENT_LIMIT * 2;
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
const SOURCE_CONTEXT_TABLE_STRUCTURED_FORWARD: i32 = 64;
const SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_LIMIT_PER_DOCUMENT: usize = 20;
const SOURCE_CONTEXT_PROCEDURAL_SETUP_LIMIT_PER_DOCUMENT: usize = 6;
const SOURCE_CONTEXT_TABLE_STRUCTURED_LIMIT_PER_DOCUMENT: usize = 32;
const SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_SCORE_BONUS: f32 = 0.6;
const SOURCE_CONTEXT_CODE_PATTERN_TERM_LIMIT: usize = 10;
const SOURCE_CONTEXT_CODE_PATTERN_HIT_LIMIT: usize = 8;
const SOURCE_CONTEXT_CODE_PATTERN_SCORE_BONUS: f32 = 3.0;
const SOURCE_CONTEXT_TRANSPORT_PATTERN_TERM_LIMIT: usize = 10;
const SOURCE_CONTEXT_TRANSPORT_PATTERN_HIT_LIMIT: usize = 16;
const SOURCE_CONTEXT_TRANSPORT_PATTERN_SCORE_BONUS: f32 = 2.5;
const SOURCE_CONTEXT_FALLBACK_STRUCTURED_SEARCH_TERM_LIMIT: usize = 8;
const SOURCE_CONTEXT_FALLBACK_STRUCTURED_SEARCH_HIT_LIMIT_PER_TERM: usize = 48;
const SOURCE_CONTEXT_FALLBACK_STRUCTURED_SEARCH_COMPANION_LIMIT: usize = 12;
const SOURCE_CONTEXT_FALLBACK_STRUCTURED_SEARCH_SCORE_BONUS: f32 = 2.25;
const SOURCE_CONTEXT_INVENTORY_PROFILE_TERM_LIMIT: usize = 8;
const SOURCE_CONTEXT_INVENTORY_PROFILE_HIT_LIMIT_PER_TERM: usize = 32;
const SOURCE_CONTEXT_INVENTORY_PROFILE_LIMIT: usize = 6;
const SOURCE_CONTEXT_FALLBACK_STRUCTURED_TOP_K_FLOOR: usize = 32;
pub(crate) const SOURCE_SLICE_DEFAULT_COUNT: usize = 12;
pub(crate) const SOURCE_SLICE_MAX_COUNT: usize = 30;
pub(crate) const SOURCE_UNIT_CHUNK_KIND: &str = "source_unit";
const HEADING_CHUNK_KIND: &str = "heading";
const TABLE_ROW_CHUNK_KIND: &str = "table_row";
const CODE_BLOCK_CHUNK_KIND: &str = "code_block";
const KEY_VALUE_BLOCK_CHUNK_KIND: &str = "key_value_block";
const METADATA_BLOCK_CHUNK_KIND: &str = "metadata_block";
const SOURCE_SLICE_CONTEXT_CHARS_PER_UNIT: usize = 1_600;
const SOURCE_SLICE_CONTEXT_MAX_CHARS: usize = 64_000;
const SOURCE_CONTEXT_SELECTED_PROFILE_BONUS: f32 = 2.0;
const SOURCE_CONTEXT_LIBRARY_PROFILE_BONUS: f32 = 1.5;
const SOURCE_CONTEXT_STRUCTURED_PROFILE_SCORE_BONUS: f32 = 16.0;
const SOURCE_CONTEXT_PROFILE_ANCHOR_SCORE_CAP: f32 = 8.0;
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

#[derive(Debug, Clone)]
struct ScoredSourceContextRow {
    row: KnowledgeChunkRow,
    score: f32,
}

#[derive(Debug, Clone)]
struct PreparedSourceContextCandidate {
    candidate: SourceContextCandidate,
    neighbor_anchors: Vec<SourceContextAnchor>,
    focused_rows: Vec<ScoredSourceContextRow>,
    path_rows: Vec<ScoredSourceContextRow>,
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
    let fallback_structured_context_requested =
        requests_fallback_structured_source_context(query_ir, chunks);
    let structured_source_unit_inventory_context_requested =
        query_ir.is_some_and(requests_structured_source_unit_inventory_context);
    let fallback_structured_candidate_expansion_requested =
        requests_fallback_structured_candidate_expansion(question, query_ir, chunks);
    let table_row_context_requested = query_ir.is_some_and(requests_table_row_source_context);
    let promote_source_profiles =
        query_ir.is_some_and(requests_structured_inventory_profile_context);
    let requests_expanded_source_context = query_ir.is_some_and(requests_expanded_source_context);
    let source_profile_candidate_limit = if requests_expanded_source_context
        || structured_source_unit_inventory_context_requested
        || promote_source_profiles
        || query_ir.is_some_and(|ir| ir.source_slice.is_some())
    {
        SOURCE_CONTEXT_PROFILE_DOCUMENT_LIMIT
    } else {
        0
    };
    let candidate_document_limit = source_context_candidate_document_limit_for_request(
        fallback_structured_candidate_expansion_requested,
        table_row_context_requested,
    );
    let mut candidates = collect_source_context_candidates_with_limit(
        chunks,
        candidate_document_limit,
        source_profile_candidate_limit,
    );
    let focus_keywords = source_context_focus_keywords(question, query_ir, plan_keywords);
    let inventory_profile_companions = if promote_source_profiles {
        load_structured_inventory_profile_source_context(
            state,
            library_id,
            document_index,
            &focus_keywords,
            chunks,
            &companions,
        )
        .await?
    } else {
        Vec::new()
    };
    if source_profile_candidate_limit > 0 && !inventory_profile_companions.is_empty() {
        let profile_chunks = inventory_profile_companions
            .iter()
            .map(|companion| companion.chunk.clone())
            .collect::<Vec<_>>();
        let profile_candidates = collect_source_profile_context_candidates(
            &profile_chunks,
            &candidates,
            source_profile_candidate_limit,
        );
        merge_source_profile_candidates(
            &mut candidates,
            profile_candidates,
            source_profile_candidate_limit,
        );
    }
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
    // storage substring filter on `occurred_at=ISO` headers, so we no longer
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
    let candidate_revision_ids = unique_candidate_revision_ids(&candidates);
    let focused_rows_by_revision = chunks_by_revision(
        state
            .document_store
            .list_chunks_by_revisions_matching_terms(
                &candidate_revision_ids,
                &focus_keywords,
                SOURCE_CONTEXT_FOCUSED_MATCH_CANDIDATE_LIMIT_PER_DOCUMENT,
            )
            .await
            .context(
                "failed to load query-focused source context chunks for candidate revisions",
            )?,
    );
    let path_context_requested = query_ir.is_some_and(requests_path_source_context);
    let configuration_path_context =
        query_ir.is_some_and(requests_configuration_file_path_source_context);
    let path_terms = ["/".to_string()];
    let path_rows_by_revision = if path_context_requested {
        chunks_by_revision(
            state
                .document_store
                .list_chunks_by_revisions_matching_terms(
                    &candidate_revision_ids,
                    &path_terms,
                    SOURCE_CONTEXT_PATH_MATCH_CANDIDATE_LIMIT_PER_DOCUMENT,
                )
                .await
                .context(
                    "failed to load path-bearing source context chunks for candidate revisions",
                )?,
        )
    } else {
        HashMap::new()
    };
    let path_head_rows_by_revision = if path_context_requested && configuration_path_context {
        let windows = candidate_revision_ids
            .iter()
            .map(|revision_id| {
                (
                    *revision_id,
                    SOURCE_CONTEXT_DOCUMENT_HEAD_CHUNK_INDEX,
                    SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_FORWARD,
                )
            })
            .collect::<Vec<_>>();
        chunks_by_revision(
            state.document_store.list_chunks_by_revisions_windows(&windows).await.context(
                "failed to load setup path source context chunks for candidate revisions",
            )?,
        )
    } else {
        HashMap::new()
    };

    let mut prepared_candidates = Vec::with_capacity(candidates.len());
    for candidate in &candidates {
        let focused_candidate_rows =
            focused_rows_by_revision.get(&candidate.revision_id).cloned().unwrap_or_default();
        let focused_rows = select_query_focused_source_rows(
            &focused_candidate_rows,
            &focus_keywords,
            false,
            &candidate.anchors,
            SOURCE_CONTEXT_FOCUSED_MATCH_LIMIT_PER_DOCUMENT,
        );
        let mut neighbor_anchors = candidate.anchors.clone();
        let mut scored_focused_rows = Vec::with_capacity(focused_rows.len());
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
            scored_focused_rows.push(ScoredSourceContextRow { row, score });
        }

        let mut scored_path_rows = Vec::new();
        if path_context_requested {
            let mut path_rows =
                path_rows_by_revision.get(&candidate.revision_id).cloned().unwrap_or_default();
            if configuration_path_context {
                if let Some(head_path_rows) = path_head_rows_by_revision.get(&candidate.revision_id)
                {
                    path_rows.extend(head_path_rows.iter().cloned());
                }
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
                scored_path_rows.push(ScoredSourceContextRow { row, score });
            }
        }

        prepared_candidates.push(PreparedSourceContextCandidate {
            candidate: candidate.clone(),
            neighbor_anchors,
            focused_rows: scored_focused_rows,
            path_rows: scored_path_rows,
        });
    }

    let procedural_context_requested = query_ir.is_some_and(requests_procedural_source_context)
        || fallback_structured_context_requested
        || structured_source_unit_inventory_context_requested
        || table_row_context_requested;
    let procedural_windows = if procedural_context_requested {
        let structured_forward = if table_row_context_requested {
            SOURCE_CONTEXT_TABLE_STRUCTURED_FORWARD
        } else {
            SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_FORWARD
        };
        prepared_candidates
            .iter()
            .flat_map(|prepared| {
                procedural_structured_sibling_windows(
                    &prepared.neighbor_anchors,
                    structured_forward,
                )
                .into_iter()
                .map(|(min_index, max_index)| {
                    (prepared.candidate.revision_id, min_index, max_index)
                })
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let procedural_rows_by_revision = if procedural_context_requested {
        chunks_by_revision(
            state
                .document_store
                .list_chunks_by_revisions_windows(&procedural_windows)
                .await
                .context(
                    "failed to load procedural structured source siblings for candidate revisions",
                )?,
        )
    } else {
        HashMap::new()
    };

    let neighbor_span = source_context_neighbor_span_for_request(
        query_ir,
        fallback_structured_context_requested || structured_source_unit_inventory_context_requested,
    );
    let neighbor_windows = prepared_candidates
        .iter()
        .flat_map(|prepared| {
            source_context_neighbor_windows(&prepared.neighbor_anchors, neighbor_span)
                .into_iter()
                .map(|(min_index, max_index)| {
                    (prepared.candidate.revision_id, min_index, max_index)
                })
        })
        .collect::<Vec<_>>();
    let neighbor_rows_by_revision = chunks_by_revision(
        state
            .document_store
            .list_chunks_by_revisions_windows(&neighbor_windows)
            .await
            .context("failed to load structured source neighbor chunks for candidate revisions")?,
    );

    for prepared in prepared_candidates {
        let candidate = prepared.candidate;
        for scored in prepared.focused_rows {
            if let Some(focused) =
                map_companion_chunk(scored.row, scored.score, document_index, &focus_keywords)
            {
                companions.push(StructuredSourceCompanion {
                    chunk: focused,
                    kind: StructuredSourceCompanionKind::FocusedMatch,
                });
            }
        }
        for scored in prepared.path_rows {
            if let Some(path_match) =
                map_companion_chunk(scored.row, scored.score, document_index, &focus_keywords)
            {
                companions.push(StructuredSourceCompanion {
                    chunk: path_match,
                    kind: StructuredSourceCompanionKind::FocusedMatch,
                });
            }
        }

        if procedural_context_requested {
            let rows = procedural_rows_by_revision
                .get(&candidate.revision_id)
                .cloned()
                .unwrap_or_default();
            let rows = if table_row_context_requested {
                select_table_structured_sibling_rows(
                    &rows,
                    &prepared.neighbor_anchors,
                    SOURCE_CONTEXT_TABLE_STRUCTURED_LIMIT_PER_DOCUMENT,
                )
            } else {
                select_procedural_structured_sibling_rows(
                    &rows,
                    &prepared.neighbor_anchors,
                    SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_LIMIT_PER_DOCUMENT,
                )
            };
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
            .document_store
            .list_chunks_by_revision_range(candidate.revision_id, 0, 0)
            .await
            .with_context(|| {
                format!(
                    "failed to load source profile chunk for revision {}",
                    candidate.revision_id
                )
            })?;
        if let Some(profile_row) = profile_rows.into_iter().find(is_source_profile_chunk_row) {
            if promote_source_profiles
                && !source_profile_matches_focus(&profile_row, &focus_keywords)
            {
                continue;
            }
            let profile_score = if promote_source_profiles {
                structured_inventory_source_profile_score(candidate.best_score, 0)
            } else {
                candidate.best_score + SOURCE_CONTEXT_SELECTED_PROFILE_BONUS
            };
            if let Some(profile) =
                map_companion_chunk(profile_row, profile_score, document_index, plan_keywords)
            {
                companions.push(StructuredSourceCompanion {
                    chunk: profile,
                    kind: StructuredSourceCompanionKind::SourceProfile,
                });
            }
        }

        let rows =
            neighbor_rows_by_revision.get(&candidate.revision_id).cloned().unwrap_or_default();
        for row in rows {
            if is_source_profile_chunk_row(&row) {
                continue;
            }
            let Some(score) = source_context_best_neighbor_score(
                &prepared.neighbor_anchors,
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

    if promote_source_profiles {
        companions.extend(inventory_profile_companions);
    }

    let fallback_structured_search_requested = fallback_structured_candidate_expansion_requested
        || query_ir.is_some_and(requests_fallback_structured_search_source_context);
    if fallback_structured_search_requested {
        let fallback_structured_companions = load_fallback_structured_search_source_context(
            state,
            library_id,
            document_index,
            &focus_keywords,
            chunks,
            &companions,
            slice_temporal_start,
            slice_temporal_end,
            !fallback_structured_candidate_expansion_requested,
        )
        .await?;
        companions.extend(fallback_structured_companions);
    }

    if query_ir.is_some_and(requests_library_source_profile_context) {
        let global_best_score =
            chunks.iter().map(|chunk| score_value(chunk.score)).fold(0.0, f32::max);
        let revision_ids = canonical_source_profile_revision_ids(
            document_index,
            SOURCE_CONTEXT_DOCUMENT_LIMIT * 4,
        );
        let rows = state
            .document_store
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
pub(crate) fn structured_source_context_top_k_for_chunks(
    query_ir: &QueryIR,
    base_top_k: usize,
    chunks: &[RuntimeMatchedChunk],
) -> usize {
    let top_k = structured_source_context_top_k(query_ir, base_top_k);
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
            .document_store
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
            .document_store
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
    let source_unit_support_chunks_by_block =
        source_unit_support_chunks_by_block(state, candidate.revision_id, &unit_blocks).await?;

    let mut selected = Vec::with_capacity(count.saturating_add(1));
    let profile_score = candidate.best_score + SOURCE_CONTEXT_SLICE_PROFILE_BONUS;
    if let Some(profile) =
        map_companion_chunk(profile_row, profile_score, document_index, plan_keywords)
    {
        selected.push(profile);
    }
    let slice_score = candidate.best_score + SOURCE_CONTEXT_SLICE_BONUS;
    for block in unit_blocks.into_iter().take(count) {
        let support_chunk = source_unit_support_chunks_by_block.get(&block.block_id);
        if let Some(unit) =
            map_source_unit_block(block, support_chunk, slice_score, document_index, plan_keywords)
        {
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

async fn source_unit_support_chunks_by_block(
    state: &AppState,
    revision_id: Uuid,
    unit_blocks: &[KnowledgeStructuredBlockRow],
) -> anyhow::Result<HashMap<Uuid, KnowledgeChunkRow>> {
    if unit_blocks.is_empty() {
        return Ok(HashMap::new());
    }
    let unit_block_ids = unit_blocks.iter().map(|block| block.block_id).collect::<HashSet<_>>();
    let support_refs = state
        .document_store
        .list_chunk_support_references_by_revision(revision_id)
        .await
        .with_context(|| {
            format!(
                "failed to load chunk support references for source-unit revision {revision_id}"
            )
        })?;
    let mut chunk_ids_by_block = HashMap::<Uuid, Uuid>::new();
    for chunk in support_refs {
        for block_id in chunk.support_block_ids {
            if unit_block_ids.contains(&block_id) {
                chunk_ids_by_block.entry(block_id).or_insert(chunk.chunk_id);
            }
        }
    }
    if chunk_ids_by_block.is_empty() {
        return Ok(HashMap::new());
    }

    let chunk_ids = chunk_ids_by_block.values().copied().collect::<Vec<_>>();
    let support_chunks = state
        .document_store
        .list_chunks_by_ids(&chunk_ids)
        .await
        .with_context(|| {
            format!("failed to load source-unit support chunks for revision {revision_id}")
        })?
        .into_iter()
        .map(|chunk| (chunk.chunk_id, chunk))
        .collect::<HashMap<_, _>>();

    Ok(chunk_ids_by_block
        .into_iter()
        .filter_map(|(block_id, chunk_id)| {
            support_chunks.get(&chunk_id).cloned().map(|chunk| (block_id, chunk))
        })
        .collect())
}

async fn first_record_stream_candidate_profile(
    state: &AppState,
    candidates: &[SourceContextCandidate],
    library_id: Uuid,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> anyhow::Result<Option<(SourceContextCandidate, KnowledgeChunkRow)>> {
    for candidate in candidates {
        let profile_rows = state
            .document_store
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
        .document_store
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

#[cfg(test)]
fn collect_source_context_candidates(
    chunks: &[RuntimeMatchedChunk],
) -> Vec<SourceContextCandidate> {
    collect_source_context_candidates_with_limit(chunks, SOURCE_CONTEXT_DOCUMENT_LIMIT, 0)
}

fn collect_source_context_candidates_with_limit(
    chunks: &[RuntimeMatchedChunk],
    document_limit: usize,
    source_profile_document_limit: usize,
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
    selected.truncate(document_limit);
    let source_profile_candidates =
        collect_source_profile_context_candidates(chunks, &selected, source_profile_document_limit);
    merge_source_profile_candidates(
        &mut selected,
        source_profile_candidates,
        source_profile_document_limit,
    );
    selected
}

fn collect_source_profile_context_candidates(
    chunks: &[RuntimeMatchedChunk],
    existing_candidates: &[SourceContextCandidate],
    limit: usize,
) -> Vec<SourceContextCandidate> {
    if limit == 0 {
        return Vec::new();
    }
    let mut existing_document_ids =
        existing_candidates.iter().map(|candidate| candidate.document_id).collect::<HashSet<_>>();
    let mut by_document = HashMap::<Uuid, SourceContextCandidate>::new();
    let mut ordered_document_ids = Vec::<Uuid>::new();
    for (rank, chunk) in chunks.iter().enumerate() {
        if !is_source_profile_runtime_chunk(chunk)
            || existing_document_ids.contains(&chunk.document_id)
        {
            continue;
        }
        let score = score_value(chunk.score);
        let entry = by_document.entry(chunk.document_id).or_insert_with(|| {
            ordered_document_ids.push(chunk.document_id);
            SourceContextCandidate {
                document_id: chunk.document_id,
                revision_id: chunk.revision_id,
                first_rank: rank,
                best_score: score,
                anchors: Vec::new(),
            }
        });
        if source_context_anchor_is_better(
            score,
            rank,
            &SourceContextAnchor {
                chunk_index: entry
                    .anchors
                    .first()
                    .map(|anchor| anchor.chunk_index)
                    .unwrap_or(i32::MAX),
                score: entry.best_score,
                first_rank: entry.first_rank,
            },
        ) {
            entry.revision_id = chunk.revision_id;
            entry.first_rank = rank.min(entry.first_rank);
            entry.best_score = score;
            entry.anchors = vec![SourceContextAnchor {
                chunk_index: chunk.chunk_index,
                score,
                first_rank: rank,
            }];
        } else if entry.anchors.is_empty() {
            entry.anchors.push(SourceContextAnchor {
                chunk_index: chunk.chunk_index,
                score,
                first_rank: rank,
            });
        }
    }
    let mut selected = Vec::new();
    for document_id in ordered_document_ids {
        if selected.len() >= limit {
            break;
        }
        let Some(candidate) = by_document.remove(&document_id) else {
            continue;
        };
        if candidate.anchors.is_empty() || !existing_document_ids.insert(candidate.document_id) {
            continue;
        }
        selected.push(candidate);
    }
    selected
}

fn merge_source_profile_candidates(
    candidates: &mut Vec<SourceContextCandidate>,
    source_profile_candidates: Vec<SourceContextCandidate>,
    limit: usize,
) {
    if limit == 0 || source_profile_candidates.is_empty() {
        return;
    }
    let mut existing_document_ids =
        candidates.iter().map(|candidate| candidate.document_id).collect::<HashSet<_>>();
    for candidate in source_profile_candidates.into_iter().take(limit) {
        if existing_document_ids.insert(candidate.document_id) {
            candidates.push(candidate);
        }
    }
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

fn unique_candidate_revision_ids(candidates: &[SourceContextCandidate]) -> Vec<Uuid> {
    let mut seen = HashSet::new();
    candidates
        .iter()
        .filter_map(|candidate| seen.insert(candidate.revision_id).then_some(candidate.revision_id))
        .collect()
}

fn chunks_by_revision(rows: Vec<KnowledgeChunkRow>) -> HashMap<Uuid, Vec<KnowledgeChunkRow>> {
    let mut grouped = HashMap::<Uuid, Vec<KnowledgeChunkRow>>::new();
    for row in rows {
        grouped.entry(row.revision_id).or_default().push(row);
    }
    grouped
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

#[cfg(test)]
fn source_context_neighbor_span(query_ir: Option<&QueryIR>) -> SourceContextNeighborSpan {
    source_context_neighbor_span_for_request(query_ir, false)
}

fn source_context_neighbor_span_for_request(
    query_ir: Option<&QueryIR>,
    fallback_structured_context_requested: bool,
) -> SourceContextNeighborSpan {
    let mut span = SourceContextNeighborSpan {
        backward: SOURCE_CONTEXT_DEFAULT_NEIGHBOR_BACKWARD,
        forward: SOURCE_CONTEXT_DEFAULT_NEIGHBOR_FORWARD,
    };
    if fallback_structured_context_requested
        || query_ir.is_some_and(requests_expanded_source_context)
    {
        span.backward = SOURCE_CONTEXT_PROCEDURAL_NEIGHBOR_BACKWARD;
    }
    span
}

fn requests_expanded_source_context(query_ir: &QueryIR) -> bool {
    requests_procedural_source_context(query_ir)
        || requests_error_code_source_context(query_ir)
        || requests_transport_source_context(query_ir)
        || requests_table_row_source_context(query_ir)
}

fn requests_procedural_source_context(query_ir: &QueryIR) -> bool {
    matches!(query_ir.scope, QueryScope::SingleDocument)
        && query_ir.source_slice.is_none()
        && (matches!(query_ir.act, QueryAct::ConfigureHow)
            || query_ir_requests_focused_configuration_source_context(query_ir))
}

fn requests_fallback_structured_source_context(
    query_ir: Option<&QueryIR>,
    chunks: &[RuntimeMatchedChunk],
) -> bool {
    let Some(query_ir) = query_ir else {
        return false;
    };
    query_ir.confidence <= 0.3
        && matches!(query_ir.scope, QueryScope::SingleDocument)
        && matches!(query_ir.act, QueryAct::Describe | QueryAct::ConfigureHow)
        && query_ir.source_slice.is_none()
        && query_ir.document_focus.is_none()
        && query_ir.target_types.is_empty()
        && query_ir.literal_constraints.is_empty()
        && chunks
            .iter()
            .filter(|chunk| !is_source_profile_runtime_chunk(chunk))
            .any(runtime_chunk_has_explicit_structured_literal_surface)
}

fn requests_fallback_structured_candidate_expansion(
    question: &str,
    query_ir: Option<&QueryIR>,
    chunks: &[RuntimeMatchedChunk],
) -> bool {
    requests_fallback_structured_source_context(query_ir, chunks)
        && technical_literal_focus_keywords(question, query_ir)
            .iter()
            .any(|keyword| keyword.chars().count() < 4)
}

fn requests_fallback_structured_search_source_context(query_ir: &QueryIR) -> bool {
    query_ir.source_slice.is_none()
        && query_ir.literal_constraints.is_empty()
        && (requests_expanded_source_context(query_ir)
            || requests_structured_inventory_profile_context(query_ir))
}

fn requests_structured_source_unit_inventory_context(query_ir: &QueryIR) -> bool {
    query_ir.source_slice.is_none()
        && matches!(query_ir.scope, QueryScope::SingleDocument)
        && matches!(
            query_ir.act,
            QueryAct::Compare | QueryAct::Describe | QueryAct::Enumerate | QueryAct::RetrieveValue
        )
        && !question_asks_table_aggregation("", Some(query_ir))
}

fn source_context_candidate_document_limit(fallback_structured_candidate_expansion: bool) -> usize {
    if fallback_structured_candidate_expansion {
        SOURCE_CONTEXT_FALLBACK_DOCUMENT_LIMIT
    } else {
        SOURCE_CONTEXT_DOCUMENT_LIMIT
    }
}

fn source_context_candidate_document_limit_for_request(
    fallback_structured_candidate_expansion: bool,
    table_row_context_requested: bool,
) -> usize {
    if table_row_context_requested {
        SOURCE_CONTEXT_FALLBACK_DOCUMENT_LIMIT
    } else {
        source_context_candidate_document_limit(fallback_structured_candidate_expansion)
    }
}

async fn load_fallback_structured_search_source_context(
    state: &AppState,
    library_id: Uuid,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    focus_keywords: &[String],
    chunks: &[RuntimeMatchedChunk],
    companions: &[StructuredSourceCompanion],
    temporal_start: Option<chrono::DateTime<chrono::Utc>>,
    temporal_end: Option<chrono::DateTime<chrono::Utc>>,
    include_long_focus_terms: bool,
) -> anyhow::Result<Vec<StructuredSourceCompanion>> {
    let search_terms = fallback_structured_search_terms(focus_keywords, include_long_focus_terms);
    if search_terms.is_empty() {
        return Ok(Vec::new());
    }

    let mut score_by_chunk = HashMap::<Uuid, f32>::new();
    for term in search_terms {
        let rows = state
            .search_store
            .search_chunks(
                library_id,
                &term,
                SOURCE_CONTEXT_FALLBACK_STRUCTURED_SEARCH_HIT_LIMIT_PER_TERM,
                temporal_start,
                temporal_end,
            )
            .await
            .with_context(|| {
                format!("failed to run fallback structured source context search for {term}")
            })?;
        for row in rows {
            score_by_chunk
                .entry(row.chunk_id)
                .and_modify(|existing| {
                    *existing = existing.max(row.score as f32);
                })
                .or_insert(row.score as f32);
        }
    }
    if score_by_chunk.is_empty() {
        return Ok(Vec::new());
    }

    let existing_chunk_ids = chunks
        .iter()
        .map(|chunk| chunk.chunk_id)
        .chain(companions.iter().map(|companion| companion.chunk.chunk_id))
        .collect::<HashSet<_>>();
    let chunk_ids = score_by_chunk.keys().copied().collect::<Vec<_>>();
    let rows = state
        .document_store
        .list_chunks_by_ids(&chunk_ids)
        .await
        .context("failed to hydrate fallback structured source context chunks")?;
    let candidates = rows
        .into_iter()
        .filter_map(|row| {
            let search_score = score_by_chunk.get(&row.chunk_id).copied()?;
            Some((row, search_score))
        })
        .collect::<Vec<_>>();
    let selected = select_fallback_structured_search_rows(
        candidates,
        focus_keywords,
        &existing_chunk_ids,
        SOURCE_CONTEXT_FALLBACK_STRUCTURED_SEARCH_COMPANION_LIMIT,
    );
    if selected.is_empty() {
        return Ok(Vec::new());
    }

    let global_best_score = chunks.iter().map(|chunk| score_value(chunk.score)).fold(0.0, f32::max);
    let selected_rows = selected.iter().map(|(row, _)| row.clone()).collect::<Vec<_>>();
    let mut companion_document_index = document_index.clone();
    hydrate_missing_companion_documents(state, &selected_rows, &mut companion_document_index)
        .await?;
    let mut companions = Vec::with_capacity(selected.len());
    for (rank, (row, structural_score)) in selected.into_iter().enumerate() {
        let score = global_best_score
            + SOURCE_CONTEXT_FALLBACK_STRUCTURED_SEARCH_SCORE_BONUS
            + structural_score as f32 * 0.001
            - rank as f32 * 0.01;
        if let Some(chunk) =
            map_companion_chunk(row, score, &companion_document_index, focus_keywords)
        {
            companions.push(StructuredSourceCompanion {
                chunk,
                kind: StructuredSourceCompanionKind::FocusedMatch,
            });
        }
    }
    Ok(companions)
}

fn fallback_structured_search_terms(
    focus_keywords: &[String],
    include_long_focus_terms: bool,
) -> Vec<String> {
    let mut seen = HashSet::new();
    focus_keywords
        .iter()
        .map(|keyword| keyword.trim().to_lowercase())
        .filter(|keyword| {
            let char_count = keyword.chars().count();
            if include_long_focus_terms { char_count >= 3 } else { (2..4).contains(&char_count) }
        })
        .filter(|keyword| {
            keyword.chars().any(|ch| ch.is_alphabetic() || ch.is_ascii_digit())
                && seen.insert(keyword.clone())
        })
        .take(SOURCE_CONTEXT_FALLBACK_STRUCTURED_SEARCH_TERM_LIMIT)
        .collect()
}

async fn load_structured_inventory_profile_source_context(
    state: &AppState,
    library_id: Uuid,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    focus_keywords: &[String],
    chunks: &[RuntimeMatchedChunk],
    companions: &[StructuredSourceCompanion],
) -> anyhow::Result<Vec<StructuredSourceCompanion>> {
    let search_terms = focus_keywords
        .iter()
        .filter(|keyword| keyword.chars().count() >= 3)
        .take(SOURCE_CONTEXT_INVENTORY_PROFILE_TERM_LIMIT)
        .cloned()
        .collect::<Vec<_>>();
    if search_terms.is_empty() {
        return Ok(Vec::new());
    }

    let mut revision_scores = HashMap::<Uuid, f32>::new();
    for term in search_terms {
        let rows = state
            .search_store
            .search_chunks(
                library_id,
                &term,
                SOURCE_CONTEXT_INVENTORY_PROFILE_HIT_LIMIT_PER_TERM,
                None,
                None,
            )
            .await
            .with_context(|| {
                format!("failed to search source profiles for structured inventory term {term}")
            })?;
        for row in rows {
            revision_scores
                .entry(row.revision_id)
                .and_modify(|score| *score = score.max(row.score as f32))
                .or_insert(row.score as f32);
        }
    }
    if revision_scores.is_empty() {
        return Ok(Vec::new());
    }

    let mut revision_ids = revision_scores.keys().copied().collect::<Vec<_>>();
    revision_ids.sort_by(|left, right| {
        revision_scores
            .get(right)
            .copied()
            .unwrap_or_default()
            .total_cmp(&revision_scores.get(left).copied().unwrap_or_default())
            .then_with(|| left.cmp(right))
    });
    revision_ids.truncate(SOURCE_CONTEXT_INVENTORY_PROFILE_LIMIT * 2);

    let existing_chunk_ids = chunks
        .iter()
        .map(|chunk| chunk.chunk_id)
        .chain(companions.iter().map(|companion| companion.chunk.chunk_id))
        .collect::<HashSet<_>>();
    let rows = state
        .document_store
        .list_source_profile_chunks_by_revisions(
            library_id,
            &revision_ids,
            SOURCE_CONTEXT_INVENTORY_PROFILE_LIMIT,
        )
        .await
        .context("failed to load structured inventory source profile chunks")?;
    let global_best_score = chunks.iter().map(|chunk| score_value(chunk.score)).fold(0.0, f32::max);
    let mut selected = rows
        .into_iter()
        .filter(|row| !existing_chunk_ids.contains(&row.chunk_id))
        .filter(|row| source_profile_matches_focus(row, focus_keywords))
        .map(|row| {
            let search_score = revision_scores.get(&row.revision_id).copied().unwrap_or_default();
            (row, search_score)
        })
        .collect::<Vec<_>>();
    selected.sort_by(|(left_row, left_score), (right_row, right_score)| {
        right_score
            .total_cmp(left_score)
            .then_with(|| left_row.document_id.cmp(&right_row.document_id))
            .then_with(|| left_row.chunk_id.cmp(&right_row.chunk_id))
    });
    selected.truncate(SOURCE_CONTEXT_INVENTORY_PROFILE_LIMIT);

    let mut companions = Vec::with_capacity(selected.len());
    for (rank, (row, search_score)) in selected.into_iter().enumerate() {
        let score = structured_inventory_source_profile_score(global_best_score, rank)
            + search_score * 0.001;
        if let Some(profile) = map_companion_chunk(row, score, document_index, focus_keywords) {
            companions.push(StructuredSourceCompanion {
                chunk: profile,
                kind: StructuredSourceCompanionKind::SourceProfile,
            });
        }
    }
    Ok(companions)
}

fn source_profile_matches_focus(row: &KnowledgeChunkRow, focus_keywords: &[String]) -> bool {
    if !is_source_profile_chunk_row(row) {
        return false;
    }
    let text = format!("{}\n{}", row.content_text, row.window_text.as_deref().unwrap_or_default())
        .to_lowercase();
    focus_keywords
        .iter()
        .filter(|keyword| keyword.chars().count() >= 3)
        .any(|keyword| text.contains(&keyword.to_lowercase()))
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

fn requests_table_row_source_context(query_ir: &QueryIR) -> bool {
    if query_ir.source_slice.is_some()
        || !matches!(query_ir.scope, QueryScope::SingleDocument)
        || !matches!(
            query_ir.act,
            QueryAct::Describe | QueryAct::Enumerate | QueryAct::RetrieveValue
        )
        || question_asks_table_aggregation("", Some(query_ir))
    {
        return false;
    }
    let target_types = query_ir
        .target_types
        .iter()
        .map(|target_type| canonical_target_type_tag(target_type))
        .collect::<HashSet<_>>();
    target_types.contains("table_row") && target_types.contains("table_summary")
}

fn requests_structured_inventory_profile_context(query_ir: &QueryIR) -> bool {
    if query_ir.source_slice.is_some() {
        return false;
    }
    if requests_transport_source_context(query_ir)
        || requests_configuration_file_path_source_context(query_ir)
    {
        return true;
    }
    let intents = classify_query_ir_intents(query_ir);
    has_question_intent(&intents, QuestionIntent::ConfigKey)
        || has_question_intent(&intents, QuestionIntent::EnvVar)
        || query_ir.target_types.iter().map(|value| canonical_target_type_tag(value)).any(|tag| {
            matches!(
                tag.as_str(),
                "connection" | "network" | "service" | "configuration_file" | "config_key"
            )
        })
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
        .document_store
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
        .document_store
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
        .document_store
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

fn fallback_structured_source_context_chunk_floor() -> usize {
    procedural_source_context_chunk_floor().max(SOURCE_CONTEXT_FALLBACK_STRUCTURED_TOP_K_FLOOR)
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

fn procedural_structured_sibling_windows(
    anchors: &[SourceContextAnchor],
    forward: i32,
) -> Vec<(i32, i32)> {
    anchors.iter().map(|anchor| source_anchor_window(anchor.chunk_index, 0, forward)).collect()
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

fn select_fallback_structured_search_rows(
    rows: Vec<(KnowledgeChunkRow, f32)>,
    focus_keywords: &[String],
    existing_chunk_ids: &HashSet<Uuid>,
    limit: usize,
) -> Vec<(KnowledgeChunkRow, isize)> {
    if limit == 0 || rows.is_empty() || focus_keywords.is_empty() {
        return Vec::new();
    }
    let mut candidates = rows
        .into_iter()
        .filter(|(row, _)| !existing_chunk_ids.contains(&row.chunk_id))
        .filter(|(row, _)| !is_source_profile_chunk_row(row))
        .filter_map(|(row, search_score)| {
            let structural_score = fallback_structured_search_score(&row, focus_keywords)?;
            Some((structural_score, search_score, row))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(
        |(left_structural, left_search, left), (right_structural, right_search, right)| {
            right_structural
                .cmp(left_structural)
                .then_with(|| right_search.total_cmp(left_search))
                .then_with(|| left.chunk_index.cmp(&right.chunk_index))
                .then_with(|| left.chunk_id.cmp(&right.chunk_id))
        },
    );
    candidates
        .into_iter()
        .take(limit)
        .map(|(structural_score, _, row)| (row, structural_score))
        .collect()
}

fn fallback_structured_search_score(
    row: &KnowledgeChunkRow,
    focus_keywords: &[String],
) -> Option<isize> {
    let text = format!("{}\n{}", row.content_text, row.window_text.as_deref().unwrap_or_default());
    if !text_has_structured_literal_surface(&text) {
        return None;
    }
    let focus_score = technical_chunk_selection_score(&text, focus_keywords, false);
    if focus_score == 0 {
        return None;
    }
    let literal_score = extract_parameter_literals(&text, 8).len().saturating_mul(6)
        + extract_config_section_literals(&text, 4).len().saturating_mul(4)
        + extract_explicit_path_literals(&text, 4).len().saturating_mul(3)
        + extract_package_command_literals(&text, 2).len().saturating_mul(2);
    let literal_score = isize::try_from(literal_score).unwrap_or(isize::MAX);
    Some(focus_score.saturating_add(literal_score))
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

    let non_head_anchors = anchors
        .iter()
        .copied()
        .filter(|anchor| anchor.chunk_index != SOURCE_CONTEXT_DOCUMENT_HEAD_CHUNK_INDEX)
        .collect::<Vec<_>>();
    let distance_anchors =
        if non_head_anchors.is_empty() { anchors } else { non_head_anchors.as_slice() };

    let mut candidates = eligible
        .iter()
        .copied()
        .filter(|row| !selected_ids.contains(&row.chunk_id))
        .filter_map(|row| {
            let distance = distance_anchors
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

fn select_table_structured_sibling_rows(
    rows: &[KnowledgeChunkRow],
    anchors: &[SourceContextAnchor],
    limit: usize,
) -> Vec<KnowledgeChunkRow> {
    if limit == 0 || rows.is_empty() || anchors.is_empty() {
        return Vec::new();
    }
    let anchor_indexes = anchors.iter().map(|anchor| anchor.chunk_index).collect::<HashSet<_>>();
    let anchor_section_paths = rows
        .iter()
        .filter(|row| anchor_indexes.contains(&row.chunk_index))
        .map(|row| row.section_path.clone())
        .filter(|section_path| !section_path.is_empty())
        .collect::<HashSet<_>>();
    let mut eligible = rows
        .iter()
        .filter(|row| !is_source_profile_chunk_row(row))
        .filter(|row| {
            row.chunk_kind.as_deref() == Some(TABLE_ROW_CHUNK_KIND)
                || (!anchor_section_paths.is_empty()
                    && row.chunk_kind.as_deref() == Some(HEADING_CHUNK_KIND))
        })
        .filter(|row| chunk_row_has_visible_text(row))
        .filter(|row| {
            anchor_section_paths.is_empty() || anchor_section_paths.contains(&row.section_path)
        })
        .collect::<Vec<_>>();
    eligible.sort_by(|left, right| {
        left.chunk_index.cmp(&right.chunk_index).then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    eligible.into_iter().take(limit).cloned().collect()
}

fn chunk_row_has_visible_text(row: &KnowledgeChunkRow) -> bool {
    !row.content_text.trim().is_empty()
        || row.window_text.as_deref().is_some_and(|text| !text.trim().is_empty())
}

fn is_procedural_structured_sibling_row(row: &KnowledgeChunkRow) -> bool {
    let text = format!("{}\n{}", row.content_text, row.window_text.as_deref().unwrap_or_default());
    if row.chunk_kind.as_deref() == Some(METADATA_BLOCK_CHUNK_KIND) {
        return text_has_structured_literal_surface(&text);
    }
    row_chunk_kind_is_structured(row.chunk_kind.as_deref())
        || text_has_structured_literal_surface(&text)
}

fn runtime_chunk_has_explicit_structured_literal_surface(chunk: &RuntimeMatchedChunk) -> bool {
    text_has_setup_literal_surface(&format!("{}\n{}", chunk.excerpt, chunk.source_text))
}

fn row_chunk_kind_is_structured(kind: Option<&str>) -> bool {
    matches!(
        kind,
        Some(
            TABLE_ROW_CHUNK_KIND
                | CODE_BLOCK_CHUNK_KIND
                | KEY_VALUE_BLOCK_CHUNK_KIND
                | METADATA_BLOCK_CHUNK_KIND
        )
    )
}

fn text_has_structured_literal_surface(text: &str) -> bool {
    !extract_parameter_literals(text, 2).is_empty()
        || !extract_config_assignment_literals(text, 2).is_empty()
        || !extract_config_section_literals(text, 2).is_empty()
        || !extract_explicit_path_literals(text, 2).is_empty()
        || !extract_package_command_literals(text, 1).is_empty()
}

fn text_has_setup_literal_surface(text: &str) -> bool {
    !extract_config_section_literals(text, 2).is_empty()
        || !extract_explicit_path_literals(text, 2).is_empty()
        || !extract_package_command_literals(text, 1).is_empty()
        || text.lines().any(line_has_key_value_literal_surface)
}

fn line_has_key_value_literal_surface(line: &str) -> bool {
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

pub(crate) fn structured_literal_excerpt_for(
    content: &str,
    keywords: &[String],
    max_chars: usize,
) -> Option<String> {
    if max_chars == 0 {
        return None;
    }
    let repaired = repair_technical_layout_noise(content);
    let trimmed = repaired.trim();
    if trimmed.is_empty() {
        return None;
    }

    let lines = trimmed.lines().map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>();
    if lines.is_empty() {
        return None;
    }

    let normalized_keywords = keywords
        .iter()
        .map(|keyword| keyword.trim())
        .filter(|keyword| keyword.chars().count() >= 3)
        .map(|keyword| keyword.to_lowercase())
        .collect::<Vec<_>>();
    let mut scored = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let literal_score = structured_literal_line_score(line);
            if literal_score == 0 {
                return None;
            }
            let lowered = line.to_lowercase();
            let keyword_score = normalized_keywords
                .iter()
                .filter(|keyword| lowered.contains(keyword.as_str()))
                .map(|keyword| keyword.chars().count().min(24))
                .sum::<usize>();
            Some((literal_score.saturating_mul(100).saturating_add(keyword_score), index))
        })
        .collect::<Vec<_>>();
    if scored.is_empty() {
        return None;
    }

    scored.sort_by(|(left_score, left_index), (right_score, right_index)| {
        right_score.cmp(left_score).then_with(|| left_index.cmp(right_index))
    });

    let mut selected = BTreeSet::new();
    let mut selected_chars = 0usize;
    for (_, index) in scored.into_iter().take(24) {
        if selected.contains(&index) {
            continue;
        }
        let line_chars = lines[index].chars().count().saturating_add(1);
        if selected_chars.saturating_add(line_chars) > max_chars && !selected.is_empty() {
            continue;
        }
        selected.insert(index);
        selected_chars = selected_chars.saturating_add(line_chars);

        for neighbor in
            [index.checked_sub(1), index.checked_add(1).filter(|idx| *idx < lines.len())]
                .into_iter()
                .flatten()
        {
            if selected.contains(&neighbor) {
                continue;
            }
            if structured_literal_line_score(lines[neighbor]) == 0
                && !line_is_short_context_label(lines[neighbor], &normalized_keywords)
            {
                continue;
            }
            let neighbor_chars = lines[neighbor].chars().count().saturating_add(1);
            if selected_chars.saturating_add(neighbor_chars) > max_chars && !selected.is_empty() {
                continue;
            }
            selected.insert(neighbor);
            selected_chars = selected_chars.saturating_add(neighbor_chars);
        }
    }

    if selected.is_empty() {
        return None;
    }

    let mut excerpt_lines = Vec::new();
    let mut previous = None;
    for index in selected {
        if previous.is_some_and(|previous| previous + 1 < index) {
            excerpt_lines.push("...");
        }
        excerpt_lines.push(lines[index]);
        previous = Some(index);
    }
    let excerpt = excerpt_for(&excerpt_lines.join("\n"), max_chars);
    (!excerpt.trim().is_empty()).then_some(excerpt)
}

pub(crate) fn salient_source_excerpt_for(
    content: &str,
    keywords: &[String],
    max_chars: usize,
) -> Option<String> {
    if max_chars == 0 {
        return None;
    }
    let repaired = repair_technical_layout_noise(content);
    let trimmed = repaired.trim();
    if trimmed.is_empty() {
        return None;
    }

    let lines = trimmed.lines().map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>();
    if lines.is_empty() {
        return None;
    }
    let normalized_keywords = normalized_excerpt_keywords(keywords);
    let mut scored = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let score = source_local_evidence_line_score(line, &normalized_keywords);
            (score > 0).then_some((score, index))
        })
        .collect::<Vec<_>>();
    if scored.is_empty() {
        return None;
    }

    scored.sort_by(|(left_score, left_index), (right_score, right_index)| {
        right_score.cmp(left_score).then_with(|| left_index.cmp(right_index))
    });

    let mut selected = BTreeSet::new();
    let mut selected_chars = 0usize;
    for (_, index) in scored.into_iter().take(32) {
        if selected.contains(&index) {
            continue;
        }
        let line_chars = lines[index].chars().count().saturating_add(1);
        if selected_chars.saturating_add(line_chars) > max_chars && !selected.is_empty() {
            continue;
        }
        selected.insert(index);
        selected_chars = selected_chars.saturating_add(line_chars);

        for neighbor in
            [index.checked_sub(1), index.checked_add(1).filter(|idx| *idx < lines.len())]
                .into_iter()
                .flatten()
        {
            if selected.contains(&neighbor) {
                continue;
            }
            if source_local_evidence_line_score(lines[neighbor], &normalized_keywords) == 0
                && !line_is_short_context_label(lines[neighbor], &normalized_keywords)
            {
                continue;
            }
            let neighbor_chars = lines[neighbor].chars().count().saturating_add(1);
            if selected_chars.saturating_add(neighbor_chars) > max_chars && !selected.is_empty() {
                continue;
            }
            selected.insert(neighbor);
            selected_chars = selected_chars.saturating_add(neighbor_chars);
        }
    }

    if selected.is_empty() {
        return None;
    }

    let mut excerpt_lines = Vec::new();
    let mut previous = None;
    for index in selected {
        if previous.is_some_and(|previous| previous + 1 < index) {
            excerpt_lines.push("...");
        }
        excerpt_lines.push(lines[index]);
        previous = Some(index);
    }
    let excerpt = excerpt_for(&excerpt_lines.join("\n"), max_chars);
    (!excerpt.trim().is_empty()).then_some(excerpt)
}

pub(crate) fn source_local_evidence_line_score(line: &str, keywords: &[String]) -> usize {
    let line = line.trim();
    if line.is_empty() {
        return 0;
    }
    let lowered = line.to_lowercase();
    let keyword_score = keywords
        .iter()
        .map(|keyword| keyword.trim())
        .filter(|keyword| keyword.chars().count() >= 3)
        .filter(|keyword| lowered.contains(*keyword))
        .map(|keyword| keyword.chars().count().min(24))
        .sum::<usize>();
    let structural_score = structured_literal_line_score(line);
    let surface_score = source_local_salience_score(line);
    if structural_score == 0 && surface_score == 0 && keyword_score == 0 {
        return 0;
    }
    structural_score
        .saturating_mul(100)
        .saturating_add(surface_score.saturating_mul(10))
        .saturating_add(keyword_score)
}

fn normalized_excerpt_keywords(keywords: &[String]) -> Vec<String> {
    keywords
        .iter()
        .map(|keyword| keyword.trim())
        .filter(|keyword| keyword.chars().count() >= 3)
        .map(|keyword| keyword.to_lowercase())
        .collect()
}

fn structured_literal_line_score(line: &str) -> usize {
    extract_config_assignment_literals(line, 4).len().saturating_mul(8)
        + extract_config_section_literals(line, 4).len().saturating_mul(6)
        + extract_explicit_path_literals(line, 4).len().saturating_mul(5)
        + extract_package_command_literals(line, 2).len().saturating_mul(5)
        + extract_parameter_literals(line, 8).len().saturating_mul(3)
        + usize::from(line_has_key_value_literal_surface(line)).saturating_mul(3)
        + usize::from(line_has_table_like_literal_surface(line)).saturating_mul(2)
}

fn line_has_table_like_literal_surface(line: &str) -> bool {
    let alphanumeric_count = line.chars().filter(|ch| ch.is_alphanumeric()).count();
    alphanumeric_count >= 3
        && (line.matches('|').count() >= 2
            || line.split('\t').filter(|cell| !cell.trim().is_empty()).count() >= 3)
}

fn source_local_salience_score(line: &str) -> usize {
    let line = line.trim();
    let alphanumeric_count = line.chars().filter(|ch| ch.is_alphanumeric()).count();
    if alphanumeric_count < 3 {
        return 0;
    }
    usize::from(line_has_list_marker_surface(line)).saturating_mul(4)
        + usize::from(line_has_label_value_surface(line)).saturating_mul(4)
        + usize::from(line_has_identifier_token_surface(line)).saturating_mul(3)
        + usize::from(line_has_bracket_or_quote_surface(line)).saturating_mul(2)
        + usize::from(line_has_numeric_marker_surface(line)).saturating_mul(2)
        + usize::from(line_has_compact_fact_surface(line)).saturating_mul(1)
}

fn line_has_list_marker_surface(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
        return true;
    }
    let mut chars = trimmed.chars().peekable();
    let mut digits = 0usize;
    while chars.peek().is_some_and(|ch| ch.is_ascii_digit()) {
        digits = digits.saturating_add(1);
        chars.next();
    }
    digits > 0 && chars.next().is_some_and(|ch| matches!(ch, '.' | ')' | ':'))
}

fn line_has_label_value_surface(line: &str) -> bool {
    let Some((left, right)) =
        line.split_once(':').or_else(|| line.split_once(" - ")).or_else(|| line.split_once(" | "))
    else {
        return false;
    };
    let left = left.trim().trim_start_matches(['-', '*', '+']).trim();
    let right = right.trim();
    !left.is_empty()
        && !right.is_empty()
        && left.chars().count() <= 120
        && left.chars().any(|ch| ch.is_alphanumeric())
        && right.chars().any(|ch| ch.is_alphanumeric())
}

fn line_has_identifier_token_surface(line: &str) -> bool {
    line.split_whitespace().any(token_has_identifier_surface)
}

fn token_has_identifier_surface(token: &str) -> bool {
    let token = token.trim_matches(|ch: char| {
        ch.is_whitespace()
            || matches!(ch, ',' | ';' | '"' | '\'' | '`' | '(' | ')' | '{' | '}' | '<' | '>')
    });
    let mut alphanumeric_count = 0usize;
    let mut has_alpha = false;
    let mut has_digit = false;
    let mut has_connector = false;
    for ch in token.chars() {
        if ch.is_alphanumeric() {
            alphanumeric_count = alphanumeric_count.saturating_add(1);
            has_alpha |= ch.is_alphabetic();
            has_digit |= ch.is_ascii_digit();
        } else if matches!(ch, '_' | '-' | '.' | '/' | ':' | '#' | '@' | '[' | ']') {
            has_connector = true;
        }
    }
    alphanumeric_count >= 3 && (has_connector || (has_alpha && has_digit))
}

fn line_has_bracket_or_quote_surface(line: &str) -> bool {
    (line.contains('[') && line.contains(']'))
        || (line.contains('(') && line.contains(')'))
        || (line.contains('"') && line.matches('"').count() >= 2)
        || (line.contains('`') && line.matches('`').count() >= 2)
}

fn line_has_numeric_marker_surface(line: &str) -> bool {
    let has_digit = line.chars().any(|ch| ch.is_ascii_digit());
    has_digit && line.chars().any(|ch| matches!(ch, ':' | '.' | '-' | '/' | '#'))
}

fn line_has_compact_fact_surface(line: &str) -> bool {
    let char_count = line.chars().count();
    if !(24..=240).contains(&char_count) {
        return false;
    }
    let alphanumeric_count = line.chars().filter(|ch| ch.is_alphanumeric()).count();
    alphanumeric_count >= 12
        && line.chars().any(|ch| matches!(ch, ':' | ';' | '.' | '!' | '?' | '|' | ')' | ']'))
}

fn line_is_short_context_label(line: &str, normalized_keywords: &[String]) -> bool {
    let char_count = line.chars().count();
    char_count <= 160
        && (line.ends_with(':')
            || line.starts_with('#')
            || normalized_keywords
                .iter()
                .any(|keyword| line.to_lowercase().contains(keyword.as_str())))
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
    let has_command_object_literal = !extract_package_command_literals(&text, 1).is_empty();
    let has_configuration_path = extract_explicit_path_literals(&text, 8).into_iter().any(|path| {
        let lowered = path.to_ascii_lowercase();
        lowered.ends_with(".conf") || lowered.ends_with(".ini")
    });
    if has_command_object_literal && has_configuration_path {
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
    let companion_visible_text =
        (!is_source_profile).then(|| source_context_companion_visible_text(&row));
    let mut chunk = map_chunk_hit(row, score, document_index, plan_keywords)?;
    if is_source_profile {
        chunk.chunk_kind = Some(SOURCE_PROFILE_CHUNK_KIND.to_string());
    }
    if let Some(companion_visible_text) = companion_visible_text
        && source_context_companion_text_should_replace(&companion_visible_text, &chunk.source_text)
    {
        chunk.source_text = companion_visible_text;
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
        structured_literal_excerpt_for(
            &chunk.source_text,
            plan_keywords,
            SOURCE_CONTEXT_EXCERPT_CHARS,
        )
        .unwrap_or_else(|| {
            let excerpt = focused_excerpt_for(
                &chunk.source_text,
                plan_keywords,
                SOURCE_CONTEXT_EXCERPT_CHARS,
            );
            if excerpt.trim().is_empty() {
                excerpt_for(&chunk.source_text, SOURCE_CONTEXT_EXCERPT_CHARS)
            } else {
                excerpt
            }
        })
    };
    Some(chunk)
}

fn source_context_content_preserves_missing_paths(content_text: &str, source_text: &str) -> bool {
    let paths = extract_explicit_path_literals(content_text, 8);
    !paths.is_empty() && paths.iter().any(|path| !source_text.contains(path))
}

fn source_context_companion_visible_text(row: &KnowledgeChunkRow) -> String {
    const MAX_VISIBLE_TEXT_CHARS: usize = 16_000;

    let mut parts = Vec::new();
    let mut seen = HashSet::new();
    for value in [
        row.window_text.as_deref(),
        Some(row.content_text.as_str()),
        Some(row.normalized_text.as_str()),
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
    let joined = parts.join("\n");
    if joined.chars().count() <= MAX_VISIBLE_TEXT_CHARS {
        return joined;
    }
    structured_literal_excerpt_for(&joined, &[], MAX_VISIBLE_TEXT_CHARS)
        .unwrap_or_else(|| excerpt_for(&joined, MAX_VISIBLE_TEXT_CHARS))
}

fn source_context_companion_text_should_replace(candidate: &str, current: &str) -> bool {
    let candidate_score = structured_literal_text_score(candidate);
    if candidate_score == 0 {
        return false;
    }
    let current_score = structured_literal_text_score(current);
    candidate_score > current_score
        || source_context_text_preserves_missing_structured_literals(candidate, current)
}

fn source_context_text_preserves_missing_structured_literals(
    candidate: &str,
    current: &str,
) -> bool {
    source_context_structured_literals(candidate, 32)
        .iter()
        .any(|literal| !current.contains(literal))
}

fn structured_literal_text_score(text: &str) -> usize {
    text.lines().map(str::trim).map(structured_literal_line_score).sum()
}

fn source_context_structured_literals(text: &str, limit: usize) -> Vec<String> {
    let mut values = Vec::new();
    let mut seen = HashSet::new();
    for value in extract_config_assignment_literals(text, limit) {
        push_unique_literal(&mut values, &mut seen, value, limit);
    }
    for value in extract_config_section_literals(text, limit) {
        push_unique_literal(&mut values, &mut seen, value, limit);
    }
    for value in extract_explicit_path_literals(text, limit) {
        push_unique_literal(&mut values, &mut seen, value, limit);
    }
    for value in extract_package_command_literals(text, limit) {
        push_unique_literal(&mut values, &mut seen, value, limit);
    }
    for value in extract_parameter_literals(text, limit) {
        push_unique_literal(&mut values, &mut seen, value, limit);
    }
    values
}

fn push_unique_literal(
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

fn map_source_unit_block(
    block: KnowledgeStructuredBlockRow,
    support_chunk: Option<&KnowledgeChunkRow>,
    score: f32,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    focus_keywords: &[String],
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
    let unit_text = if block.text.trim().is_empty() {
        block.normalized_text.clone()
    } else {
        block.text.clone()
    };
    let source_text = source_unit_visible_text(&unit_text, support_chunk);
    let excerpt =
        focused_record_unit_excerpt(&source_text, focus_keywords, SOURCE_CONTEXT_EXCERPT_CHARS)
            .unwrap_or_else(|| {
                let focused =
                    focused_excerpt_for(&source_text, focus_keywords, SOURCE_CONTEXT_EXCERPT_CHARS);
                if focused.trim().is_empty() {
                    excerpt_for(&source_text, SOURCE_CONTEXT_EXCERPT_CHARS)
                } else {
                    focused
                }
            });
    Some(RuntimeMatchedChunk {
        chunk_id: support_chunk.map(|chunk| chunk.chunk_id).unwrap_or(block.block_id),
        document_id: block.document_id,
        revision_id: block.revision_id,
        chunk_index: block.ordinal,
        chunk_kind: Some(SOURCE_UNIT_CHUNK_KIND.to_string()),
        document_label,
        excerpt,
        score_kind: crate::services::query::execution::RuntimeChunkScoreKind::SourceContext,
        score: Some(score),
        source_text,
    })
}

fn source_unit_visible_text(unit_text: &str, support_chunk: Option<&KnowledgeChunkRow>) -> String {
    let unit_text = repair_technical_layout_noise(unit_text);
    let Some(support_chunk) = support_chunk else {
        return unit_text;
    };
    let support_text = source_context_companion_visible_text(support_chunk);
    if !source_unit_support_text_should_extend(&support_text, &unit_text) {
        return unit_text;
    }
    format!("{}\n{}", unit_text.trim_end(), support_text.trim_start())
}

fn source_unit_support_text_should_extend(candidate: &str, current: &str) -> bool {
    let candidate = candidate.trim();
    let current = current.trim();
    if candidate.is_empty() {
        return false;
    }
    let normalized_candidate = candidate.split_whitespace().collect::<Vec<_>>().join(" ");
    let normalized_current = current.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized_candidate.is_empty()
        || normalized_candidate == normalized_current
        || normalized_current.contains(&normalized_candidate)
    {
        return false;
    }
    normalized_candidate.contains(&normalized_current)
        || source_context_companion_text_should_replace(candidate, current)
        || source_unit_support_text_has_new_salient_lines(candidate, current)
        || candidate.chars().count() > current.chars().count().saturating_add(64)
}

fn source_unit_support_text_has_new_salient_lines(candidate: &str, current: &str) -> bool {
    let normalized_current =
        current.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    candidate.lines().map(str::trim).any(|line| {
        if line.is_empty() || source_local_evidence_line_score(line, &[]) == 0 {
            return false;
        }
        let normalized_line = line.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        !normalized_line.is_empty() && !normalized_current.contains(&normalized_line)
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

fn structured_inventory_source_profile_score(anchor_score: f32, rank: usize) -> f32 {
    anchor_score.clamp(0.0, SOURCE_CONTEXT_PROFILE_ANCHOR_SCORE_CAP)
        + SOURCE_CONTEXT_STRUCTURED_PROFILE_SCORE_BONUS
        - rank as f32 * 0.01
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

    #[test]
    fn candidate_revision_ids_keep_first_seen_order_and_drop_duplicates() {
        let revision_a = Uuid::from_u128(1);
        let revision_b = Uuid::from_u128(2);
        let document_a = Uuid::from_u128(10);
        let document_b = Uuid::from_u128(11);
        let document_c = Uuid::from_u128(12);
        let candidates = vec![
            SourceContextCandidate {
                document_id: document_a,
                revision_id: revision_a,
                first_rank: 0,
                best_score: 1.0,
                anchors: Vec::new(),
            },
            SourceContextCandidate {
                document_id: document_b,
                revision_id: revision_b,
                first_rank: 1,
                best_score: 0.9,
                anchors: Vec::new(),
            },
            SourceContextCandidate {
                document_id: document_c,
                revision_id: revision_a,
                first_rank: 2,
                best_score: 0.8,
                anchors: Vec::new(),
            },
        ];

        assert_eq!(unique_candidate_revision_ids(&candidates), vec![revision_a, revision_b]);
    }

    #[test]
    fn profile_only_candidates_are_appended_under_separate_limit() {
        let profile_document_id = Uuid::from_u128(99);
        let profile_revision_id = Uuid::from_u128(199);
        let mut chunks = (0..4)
            .map(|index| {
                runtime_chunk(
                    Uuid::from_u128(10 + u128::from(index as u32)),
                    Uuid::from_u128(110 + u128::from(index as u32)),
                    index,
                )
            })
            .collect::<Vec<_>>();
        chunks.push(companion_chunk(
            profile_document_id,
            profile_revision_id,
            0,
            "source_profile",
            12.0,
        ));

        let narrow = collect_source_context_candidates_with_limit(&chunks, 3, 0);
        assert_eq!(narrow.len(), 3);
        assert!(
            narrow.iter().all(|candidate| candidate.document_id != profile_document_id),
            "profile-only document must not appear when profile anchors are disabled"
        );

        let expanded = collect_source_context_candidates_with_limit(&chunks, 3, 2);
        assert_eq!(expanded.len(), 4);
        let profile_candidate = expanded
            .iter()
            .find(|candidate| candidate.document_id == profile_document_id)
            .expect("profile-only document appended");
        assert_eq!(profile_candidate.revision_id, profile_revision_id);
        assert_eq!(profile_candidate.anchors.len(), 1);
        assert_eq!(profile_candidate.anchors[0].chunk_index, 0);
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
            document_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            external_key: "event-stream.jsonl".to_string(),
            file_name: Some("event-stream.jsonl".to_string()),
            title: Some("event-stream.jsonl".to_string()),
            source_uri: None,
            document_hint: None,
            document_state: "active".to_string(),
            active_revision_id: Some(revision_id),
            readable_revision_id: Some(revision_id),
            latest_revision_no: Some(1),
            parent_document_id: None,
            document_role: crate::domains::content::DOCUMENT_ROLE_PRIMARY.to_string(),
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
            "sample-install alpha-connector\n\
             sample-configure alpha-connector\n\
             module configuration: /opt/alpha/modules/connector/connector.conf",
        );
        row.window_text = Some(
            "sample-install alpha-connector\n\
             sample-configure alpha-connector\n\
             module configuration:"
                .to_string(),
        );

        let mapped = map_companion_chunk(row, 1.0, &document_index, &[]).unwrap();

        assert!(mapped.source_text.contains("/opt/alpha/modules/connector/connector.conf"));
        assert!(mapped.excerpt.contains("/opt/alpha/modules/connector/connector.conf"));
    }

    #[test]
    fn map_companion_chunk_merges_structured_literals_from_window_and_content() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let document_index = HashMap::from([(document_id, document_row(document_id, revision_id))]);
        let mut row = chunk_row(
            document_id,
            revision_id,
            2,
            "paragraph",
            "[Main]\nalphaMode = strict\nconfiguration path: /etc/sample/service.conf",
        );
        row.window_text = Some(
            "Run the reload command after editing the configuration:\n\
             samplectl reload alpha-plugin --config /etc/sample/service.conf"
                .to_string(),
        );

        let mapped = map_companion_chunk(row, 1.0, &document_index, &[]).unwrap();

        assert!(mapped.source_text.contains("[Main]"));
        assert!(mapped.source_text.contains("alphaMode = strict"));
        assert!(mapped.source_text.contains("samplectl reload alpha-plugin"));
        assert!(mapped.excerpt.contains("[Main]"));
        assert!(mapped.excerpt.contains("alphaMode = strict"));
        assert!(mapped.excerpt.contains("samplectl reload alpha-plugin"));
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
                "Install the module:\nsample-install alpha-connector\n\nConfigure it:\nsample-configure alpha-connector\n\nSettings are stored in /opt/alpha/modules/connector/connector.conf.",
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
                "Install the module:\nsample-install alpha-connector\n\nConfigure it:\nsample-configure alpha-connector\n\nSettings are stored in /opt/alpha/modules/connector/connector.conf.",
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
    fn procedural_structured_siblings_include_literal_paragraphs_after_anchors() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let rows = vec![
            chunk_row(document_id, revision_id, 0, "metadata_block", "[source_profile units=5]"),
            chunk_row(
                document_id,
                revision_id,
                1,
                "paragraph",
                "[UI.Alpha.Form]\nalphaFlag = true",
            ),
            chunk_row(document_id, revision_id, 2, "paragraph", "betaMode = 1"),
            chunk_row(document_id, revision_id, 3, "paragraph", "alpha beta gamma"),
        ];
        let anchors = vec![SourceContextAnchor { chunk_index: 0, score: 10.0, first_rank: 0 }];

        let selected = select_procedural_structured_sibling_rows(&rows, &anchors, 3);

        assert_eq!(selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>(), [1, 2]);
    }

    #[test]
    fn procedural_structured_siblings_skip_empty_metadata_blocks() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let rows = vec![
            chunk_row(document_id, revision_id, 1, "metadata_block", ""),
            chunk_row(document_id, revision_id, 2, "table_row", "alphaFlag | boolean | true"),
            chunk_row(document_id, revision_id, 3, "code_block", "[Main]\nalphaFlag = true"),
        ];
        let anchors = vec![SourceContextAnchor { chunk_index: 0, score: 10.0, first_rank: 0 }];

        let selected = select_procedural_structured_sibling_rows(&rows, &anchors, 3);

        assert_eq!(selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>(), [2, 3]);
    }

    #[test]
    fn low_confidence_fallback_expands_only_when_retrieved_chunks_have_structured_literals() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mut query_ir = source_context_ir(QueryAct::ConfigureHow, QueryScope::SingleDocument);
        query_ir.confidence = 0.25;
        let mut structured = runtime_chunk(document_id, revision_id, 7);
        structured.chunk_kind = Some("paragraph".to_string());
        structured.excerpt = "[UI.Alpha.Form]\nalphaFlag = true".to_string();
        structured.source_text = structured.excerpt.clone();
        let mut plain = runtime_chunk(document_id, revision_id, 8);
        plain.chunk_kind = Some("paragraph".to_string());
        plain.excerpt = "alpha beta gamma".to_string();
        plain.source_text = plain.excerpt.clone();

        assert!(requests_fallback_structured_source_context(Some(&query_ir), &[structured]));
        assert!(!requests_fallback_structured_source_context(Some(&query_ir), &[plain]));

        query_ir.confidence = 0.9;
        assert!(!requests_fallback_structured_source_context(Some(&query_ir), &[]));
    }

    #[test]
    fn low_confidence_fallback_ignores_structured_chunk_kind_without_literal_surface() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mut query_ir = source_context_ir(QueryAct::ConfigureHow, QueryScope::SingleDocument);
        query_ir.confidence = 0.25;
        let mut code = runtime_chunk(document_id, revision_id, 7);
        code.chunk_kind = Some("code_block".to_string());
        code.excerpt = "sample alpha beta gamma".to_string();
        code.source_text = code.excerpt.clone();
        let mut table = runtime_chunk(document_id, revision_id, 8);
        table.chunk_kind = Some("table_row".to_string());
        table.excerpt = "Alpha Beta Gamma".to_string();
        table.source_text = table.excerpt.clone();

        assert!(!requests_fallback_structured_source_context(Some(&query_ir), &[code]));
        assert!(!requests_fallback_structured_source_context(Some(&query_ir), &[table]));
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
    fn procedural_structured_siblings_do_not_starve_late_example_blocks() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mut rows = vec![chunk_row(
            document_id,
            revision_id,
            0,
            "metadata_block",
            "[source_profile units=32]",
        )];
        rows.extend((1..=10).map(|index| {
            chunk_row(
                document_id,
                revision_id,
                index,
                "table_row",
                &format!("headParam{index} | value"),
            )
        }));
        rows.extend([
            chunk_row(document_id, revision_id, 20, "heading", "Detailed option group"),
            chunk_row(document_id, revision_id, 21, "table_row", "lateFlag | boolean | true"),
            chunk_row(document_id, revision_id, 22, "code_block", "[Main]\nlateFlag = true"),
            chunk_row(document_id, revision_id, 23, "table_row", "printSlip | boolean | false"),
            chunk_row(document_id, revision_id, 24, "code_block", "[Check]\nprintSlip = false"),
            chunk_row(document_id, revision_id, 25, "table_row", "visible | boolean | true"),
            chunk_row(document_id, revision_id, 26, "code_block", "[UI.Component]\nvisible = true"),
        ]);
        let anchors = vec![
            SourceContextAnchor { chunk_index: 0, score: 10.0, first_rank: 0 },
            SourceContextAnchor { chunk_index: 20, score: 9.0, first_rank: 1 },
        ];

        let selected = select_procedural_structured_sibling_rows(&rows, &anchors, 12);
        let selected_indexes = selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>();

        assert!(selected_indexes.contains(&24), "{selected_indexes:?}");
        assert!(selected_indexes.contains(&26), "{selected_indexes:?}");
    }

    #[test]
    fn table_structured_siblings_keep_head_table_rows_without_setup_cap() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mut head =
            chunk_row(document_id, revision_id, 0, "metadata_block", "[source_profile units=12]");
        head.section_path = vec!["schema".to_string(), "accounts".to_string()];
        let mut rows = vec![head];
        rows.extend((1..=8).map(|index| {
            let mut row = chunk_row(
                document_id,
                revision_id,
                index,
                "table_row",
                &format!(
                    "Sheet: Schema | Table: 1. Table: accounts | Row {index} | Column: c{index}"
                ),
            );
            row.section_path = vec!["schema".to_string(), "accounts".to_string()];
            row
        }));
        let mut other_table_row = chunk_row(
            document_id,
            revision_id,
            9,
            "table_row",
            "Sheet: Schema | Table: 2. Table: orders | Row 1 | Column: order_id",
        );
        other_table_row.section_path = vec!["schema".to_string(), "orders".to_string()];
        rows.push(other_table_row);
        let anchors = vec![SourceContextAnchor { chunk_index: 0, score: 10.0, first_rank: 0 }];

        let selected = select_table_structured_sibling_rows(&rows, &anchors, 16);

        assert_eq!(
            selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5, 6, 7, 8]
        );
    }

    #[test]
    fn table_structured_siblings_include_section_heading_for_table_rows() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mut heading = chunk_row(document_id, revision_id, 0, "heading", "## Table: records");
        heading.section_path = vec!["schema".to_string(), "records".to_string()];
        let mut rows = vec![heading];
        rows.extend((1..=4).map(|index| {
            let mut row = chunk_row(
                document_id,
                revision_id,
                index,
                "table_row",
                &format!("| field_{index} | text | description |"),
            );
            row.section_path = vec!["schema".to_string(), "records".to_string()];
            row
        }));
        let mut other_heading =
            chunk_row(document_id, revision_id, 10, "heading", "## Table: events");
        other_heading.section_path = vec!["schema".to_string(), "events".to_string()];
        let mut other_row = chunk_row(
            document_id,
            revision_id,
            11,
            "table_row",
            "| event_id | text | description |",
        );
        other_row.section_path = vec!["schema".to_string(), "events".to_string()];
        rows.extend([other_heading, other_row]);
        let anchors = vec![SourceContextAnchor { chunk_index: 2, score: 10.0, first_rank: 0 }];

        let selected = select_table_structured_sibling_rows(&rows, &anchors, 16);

        assert_eq!(
            selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4]
        );
    }

    #[test]
    fn table_row_summary_describe_ir_requests_expanded_source_context() {
        let mut query_ir = source_context_ir(QueryAct::Describe, QueryScope::SingleDocument);
        query_ir.target_types = vec!["table_row".to_string(), "table_summary".to_string()];

        assert!(requests_table_row_source_context(&query_ir));
        assert_eq!(
            structured_source_context_top_k(&query_ir, 5),
            procedural_source_context_chunk_floor()
        );

        query_ir.target_types = vec!["table_average".to_string()];
        query_ir.act = QueryAct::RetrieveValue;

        assert!(!requests_table_row_source_context(&query_ir));
    }

    #[test]
    fn table_row_summary_context_expands_candidate_document_limit() {
        assert_eq!(
            source_context_candidate_document_limit_for_request(false, true),
            SOURCE_CONTEXT_FALLBACK_DOCUMENT_LIMIT
        );
        assert_eq!(
            source_context_candidate_document_limit_for_request(false, false),
            SOURCE_CONTEXT_DOCUMENT_LIMIT
        );
    }

    #[test]
    fn table_row_summary_retrieve_value_ir_requests_expanded_source_context() {
        let mut query_ir = source_context_ir(QueryAct::RetrieveValue, QueryScope::SingleDocument);
        query_ir.target_types = vec!["table_row".to_string(), "table_summary".to_string()];

        assert!(requests_table_row_source_context(&query_ir));
        assert_eq!(
            structured_source_context_top_k(&query_ir, 5),
            procedural_source_context_chunk_floor()
        );
    }

    #[test]
    fn procedural_structured_sibling_windows_expand_forward_only() {
        let anchors = vec![
            SourceContextAnchor { chunk_index: 0, score: 10.0, first_rank: 0 },
            SourceContextAnchor { chunk_index: 20, score: 9.0, first_rank: 1 },
        ];

        assert_eq!(
            procedural_structured_sibling_windows(
                &anchors,
                SOURCE_CONTEXT_PROCEDURAL_STRUCTURED_FORWARD
            ),
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
    fn fallback_structured_context_expands_top_k_when_structured_chunks_exist() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mut query_ir = source_context_ir(QueryAct::Describe, QueryScope::SingleDocument);
        query_ir.confidence = 0.25;
        let mut structured = runtime_chunk(document_id, revision_id, 12);
        structured.chunk_kind = Some("key_value_block".to_string());
        structured.source_text = "alphaKey = true".to_string();
        let mut plain = runtime_chunk(document_id, revision_id, 13);
        plain.chunk_kind = None;
        plain.source_text = "general overview".to_string();

        assert_eq!(
            structured_source_context_top_k_for_chunks(&query_ir, 24, &[structured]),
            fallback_structured_source_context_chunk_floor()
        );
        assert_eq!(
            structured_source_context_top_k_for_chunks(&query_ir, 24, &[plain]),
            fallback_structured_source_context_chunk_floor()
        );
    }

    #[test]
    fn structured_source_unit_inventory_expands_top_k_for_describe_questions() {
        let query_ir = source_context_ir(QueryAct::Describe, QueryScope::SingleDocument);
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let plain = runtime_chunk(document_id, revision_id, 12);

        assert!(requests_structured_source_unit_inventory_context(&query_ir));
        assert_eq!(
            structured_source_context_top_k_for_chunks(&query_ir, 24, &[plain]),
            fallback_structured_source_context_chunk_floor()
        );
        assert_eq!(
            source_context_neighbor_span_for_request(
                Some(&query_ir),
                requests_structured_source_unit_inventory_context(&query_ir)
            ),
            SourceContextNeighborSpan {
                backward: SOURCE_CONTEXT_PROCEDURAL_NEIGHBOR_BACKWARD,
                forward: SOURCE_CONTEXT_DEFAULT_NEIGHBOR_FORWARD
            }
        );
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
        let block_id = block.block_id;
        let support_chunk_id = Uuid::now_v7();
        let mut support_chunk =
            chunk_row(document_id, revision_id, 42, "paragraph", "supporting record payload");
        support_chunk.chunk_id = support_chunk_id;

        let mapped =
            map_source_unit_block(block, Some(&support_chunk), 3.0, &document_index, &[]).unwrap();

        assert_eq!(mapped.chunk_id, support_chunk_id);
        assert_ne!(mapped.chunk_id, block_id);
        assert_eq!(mapped.chunk_index, 42);
        assert_eq!(mapped.chunk_kind.as_deref(), Some(SOURCE_UNIT_CHUNK_KIND));
        assert!(is_source_unit_runtime_chunk(&mapped));
        assert!(mapped.source_text.contains("final record"));
    }

    #[test]
    fn map_source_unit_block_extends_text_with_support_payload() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let document_index = HashMap::from([(document_id, document_row(document_id, revision_id))]);
        let block = source_unit_block(
            document_id,
            revision_id,
            7,
            "[unit_id=u-7 occurred_at=2026-04-29T12:22:51Z] ![preview](asset.png)",
        );
        let support_chunk = chunk_row(
            document_id,
            revision_id,
            7,
            "paragraph",
            "Record title\n- Added neutral evidence line\n- Added second neutral evidence line",
        );

        let mapped = map_source_unit_block(block, Some(&support_chunk), 3.0, &document_index, &[])
            .expect("mapped source unit");

        assert!(mapped.source_text.starts_with("[unit_id=u-7"));
        assert!(mapped.source_text.contains("![preview](asset.png)"));
        assert!(mapped.source_text.contains("Record title"));
        assert!(mapped.source_text.contains("Added neutral evidence line"));
        assert_eq!(mapped.chunk_id, support_chunk.chunk_id);
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
    fn low_confidence_structured_candidate_expansion_reaches_lower_ranked_documents() {
        let docs = [
            (Uuid::now_v7(), Uuid::now_v7()),
            (Uuid::now_v7(), Uuid::now_v7()),
            (Uuid::now_v7(), Uuid::now_v7()),
            (Uuid::now_v7(), Uuid::now_v7()),
        ];
        let chunks = docs
            .iter()
            .enumerate()
            .map(|(rank, (document_id, revision_id))| {
                let mut chunk = runtime_chunk(*document_id, *revision_id, rank as i32 + 10);
                chunk.chunk_kind = Some("paragraph".to_string());
                chunk.source_text = if rank == 3 {
                    "[UI.Alpha.Form]\nalphaFlag = true".to_string()
                } else {
                    "unstructured filler".to_string()
                };
                chunk.excerpt = chunk.source_text.clone();
                chunk
            })
            .collect::<Vec<_>>();
        let mut query_ir = source_context_ir(QueryAct::Describe, QueryScope::SingleDocument);
        query_ir.confidence = 0.25;

        assert!(requests_fallback_structured_candidate_expansion(
            "Which QR setting applies?",
            Some(&query_ir),
            &chunks
        ));
        let default_candidates = collect_source_context_candidates(&chunks);
        assert!(!default_candidates.iter().any(|candidate| candidate.document_id == docs[3].0));

        let expanded_candidates = collect_source_context_candidates_with_limit(
            &chunks,
            source_context_candidate_document_limit(true),
            0,
        );
        assert!(expanded_candidates.iter().any(|candidate| candidate.document_id == docs[3].0));
    }

    #[test]
    fn fallback_structured_search_keeps_short_token_literal_rows() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let existing = chunk_row(
            document_id,
            revision_id,
            1,
            "paragraph",
            "[UI.Alpha]\nexistingFlag = true\nQ1",
        );
        let plain = chunk_row(document_id, revision_id, 2, "paragraph", "Q1 filler text only");
        let literal =
            chunk_row(document_id, revision_id, 3, "paragraph", "[UI.Alpha]\nalphaFlag = true\nQ1");
        let existing_chunk_ids = HashSet::from([existing.chunk_id]);

        let selected = select_fallback_structured_search_rows(
            vec![(existing, 20.0), (plain, 100.0), (literal, 10.0)],
            &["q1".to_string()],
            &existing_chunk_ids,
            4,
        );

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].0.chunk_index, 3);
        assert!(selected[0].1 > 0);
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
    fn fallback_structured_context_expands_preceding_setup_context() {
        let mut query_ir = source_context_ir(QueryAct::Describe, QueryScope::SingleDocument);
        query_ir.confidence = 0.25;
        let span = source_context_neighbor_span_for_request(Some(&query_ir), true);
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
            "fallback structured answers need preceding key/value blocks near the matching detail"
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
    fn inventory_profile_score_can_outrank_generic_context() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let unrelated_document_id = Uuid::now_v7();
        let mut chunks = vec![
            RuntimeMatchedChunk {
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(20.0),
                ..runtime_chunk(document_id, revision_id, 12)
            },
            RuntimeMatchedChunk {
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(22.0),
                ..runtime_chunk(unrelated_document_id, revision_id, 7)
            },
        ];
        let profile = companion_chunk(
            document_id,
            revision_id,
            0,
            "source_profile",
            structured_inventory_source_profile_score(20.0, 0),
        );

        apply_structured_source_companions(
            &mut chunks,
            vec![StructuredSourceCompanion {
                chunk: profile,
                kind: StructuredSourceCompanionKind::SourceProfile,
            }],
        );

        assert_eq!(chunks[0].chunk_kind.as_deref(), Some("source_profile"));
        assert_eq!(chunks[0].document_id, document_id);
    }

    #[test]
    fn inventory_profile_score_caps_artificial_anchor_scores() {
        let capped = structured_inventory_source_profile_score(1_000_000.0, 0);

        assert!(capped < 100.0);
        assert_eq!(
            capped,
            SOURCE_CONTEXT_PROFILE_ANCHOR_SCORE_CAP + SOURCE_CONTEXT_STRUCTURED_PROFILE_SCORE_BONUS
        );
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
