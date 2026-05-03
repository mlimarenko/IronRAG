use std::collections::HashMap;

use anyhow::Context;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::query_ir::{QueryIR, QueryScope, SourceSliceDirection, SourceSliceSpec},
    infra::arangodb::document_store::{
        KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeStructuredBlockRow,
    },
};

use super::{
    RuntimeMatchedChunk,
    retrieve::{
        canonical_document_revision_id, excerpt_for, focused_excerpt_for, map_chunk_hit,
        score_value,
    },
    source_profile::{
        SOURCE_PROFILE_CHUNK_KIND, is_record_stream_source_profile_row,
        is_source_profile_chunk_row, is_source_profile_runtime_chunk,
    },
    technical_literals::{technical_chunk_selection_score, technical_literal_focus_keywords},
};

const SOURCE_CONTEXT_DOCUMENT_LIMIT: usize = 3;
const SOURCE_CONTEXT_ANCHOR_LIMIT_PER_DOCUMENT: usize = 2;
const SOURCE_CONTEXT_NEIGHBOR_RADIUS: i32 = 1;
const SOURCE_CONTEXT_FOCUSED_MATCH_LIMIT_PER_DOCUMENT: usize = 2;
const SOURCE_CONTEXT_FOCUSED_MATCH_CANDIDATE_LIMIT_PER_DOCUMENT: usize = 64;
const SOURCE_CONTEXT_FOCUSED_MATCH_SCORE_BONUS: f32 = 1.0;
pub(crate) const SOURCE_SLICE_DEFAULT_COUNT: usize = 12;
pub(crate) const SOURCE_SLICE_MAX_COUNT: usize = 30;
pub(crate) const SOURCE_UNIT_CHUNK_KIND: &str = "source_unit";
const SOURCE_SLICE_CONTEXT_CHARS_PER_UNIT: usize = 1_600;
const SOURCE_SLICE_CONTEXT_MAX_CHARS: usize = 64_000;
const SOURCE_CONTEXT_SELECTED_PROFILE_BONUS: f32 = 2.0;
const SOURCE_CONTEXT_LIBRARY_PROFILE_BONUS: f32 = 1.5;
const SOURCE_CONTEXT_NEIGHBOR_PENALTY: f32 = 0.01;
const SOURCE_CONTEXT_SLICE_PROFILE_BONUS: f32 = 4.0;
const SOURCE_CONTEXT_SLICE_BONUS: f32 = 3.0;
const SOURCE_CONTEXT_EXCERPT_CHARS: usize = 720;

pub(crate) fn source_anchor_window(anchor: i32, backward: i32, forward: i32) -> (i32, i32) {
    (anchor.saturating_sub(backward.max(0)), anchor.saturating_add(forward.max(0)))
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub(crate) struct StructuredSourceContextDiagnostics {
    pub(crate) eligible_document_count: usize,
    pub(crate) source_profile_count: usize,
    pub(crate) neighbor_count: usize,
    pub(crate) focused_match_count: usize,
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
enum StructuredSourceCompanionKind {
    SourceProfile,
    Neighbor,
    FocusedMatch,
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
    chunks: &mut Vec<RuntimeMatchedChunk>,
) -> anyhow::Result<StructuredSourceContextDiagnostics> {
    let mut companions = Vec::<StructuredSourceCompanion>::new();
    let candidates = collect_source_context_candidates(chunks);
    let focus_keywords = source_context_focus_keywords(question, query_ir, plan_keywords);
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
        for (rank, row) in select_query_focused_source_rows(
            &focused_rows,
            &focus_keywords,
            false,
            &candidate.anchors,
            SOURCE_CONTEXT_FOCUSED_MATCH_LIMIT_PER_DOCUMENT,
        )
        .into_iter()
        .enumerate()
        {
            let score = candidate.best_score + SOURCE_CONTEXT_FOCUSED_MATCH_SCORE_BONUS
                - rank as f32 * 0.01;
            if let Some(focused) = map_companion_chunk(row, score, document_index, &focus_keywords)
            {
                companions.push(StructuredSourceCompanion {
                    chunk: focused,
                    kind: StructuredSourceCompanionKind::FocusedMatch,
                });
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
        let Some(profile_row) = profile_rows.into_iter().find(is_source_profile_chunk_row) else {
            continue;
        };
        let profile_score = candidate.best_score + SOURCE_CONTEXT_SELECTED_PROFILE_BONUS;
        if let Some(profile) =
            map_companion_chunk(profile_row, profile_score, document_index, plan_keywords)
        {
            companions.push(StructuredSourceCompanion {
                chunk: profile,
                kind: StructuredSourceCompanionKind::SourceProfile,
            });
        }

        for anchor in candidate.anchors.iter().take(SOURCE_CONTEXT_ANCHOR_LIMIT_PER_DOCUMENT) {
            let (min_index, max_index) = source_anchor_window(
                anchor.chunk_index,
                SOURCE_CONTEXT_NEIGHBOR_RADIUS,
                SOURCE_CONTEXT_NEIGHBOR_RADIUS,
            );
            let rows = state
                .arango_document_store
                .list_chunks_by_revision_range(candidate.revision_id, min_index, max_index)
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
                let distance = row.chunk_index.abs_diff(anchor.chunk_index) as f32;
                let score = source_context_neighbor_score(anchor.score, distance, row.chunk_index);
                if let Some(neighbor) =
                    map_companion_chunk(row, score, document_index, plan_keywords)
                {
                    companions.push(StructuredSourceCompanion {
                        chunk: neighbor,
                        kind: StructuredSourceCompanionKind::Neighbor,
                    });
                }
            }
        }
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
        first_record_stream_candidate_profile(state, candidates, library_id, document_index).await?
    else {
        return Ok(None);
    };
    let count = source_slice_count(slice);
    let unit_blocks = match slice.direction {
        SourceSliceDirection::Head | SourceSliceDirection::All => state
            .arango_document_store
            .list_head_source_unit_blocks_by_revision(
                candidate.revision_id,
                count,
                temporal_start,
                temporal_end,
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
    let canonical_revision_ids = canonical_source_profile_revision_ids(
        document_index,
        SOURCE_CONTEXT_DOCUMENT_LIMIT * 4,
    );
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

fn source_context_anchor_is_better(
    score: f32,
    rank: usize,
    existing: &SourceContextAnchor,
) -> bool {
    score > existing.score || (score == existing.score && rank < existing.first_rank)
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
    let mut chunk = map_chunk_hit(row, score, document_index, plan_keywords)?;
    if is_source_profile {
        chunk.chunk_kind = Some(SOURCE_PROFILE_CHUNK_KIND.to_string());
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

    use crate::domains::query_ir::{QueryAct, QueryLanguage};

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
            source_slice: Some(SourceSliceSpec { direction, count }),
            confidence: 0.8,
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
    fn source_context_focus_keywords_include_typed_query_ir_focus() {
        let mut query_ir = QueryIR {
            act: QueryAct::Describe,
            scope: crate::domains::query_ir::QueryScope::SingleDocument,
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
