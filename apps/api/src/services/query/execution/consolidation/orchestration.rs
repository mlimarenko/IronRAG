use super::{
    AppState, ConsolidationDiagnostics, DocumentAggregate, FOCUSED_WINNER_EXCERPT_CHARS,
    FocusReason, QueryIR, RetrievalBundle, RuntimeMatchedChunk, decide_focus, select_winner_rows,
    winner_anchor_windows,
};

/// Materialize `fetched_rows` into winner-chunk `RuntimeMatchedChunk`s,
/// combine with the existing tangential chunks in `bundle`, and write
/// the result back. Pure (no I/O) so it can be unit-tested directly
/// against hand-built `KnowledgeChunkRow` fixtures.
///
/// Returns the consolidation diagnostics. If the fetched set contains
/// no rows for the winner revision (or zero rows total), returns
/// `ConsolidationDiagnostics::noop()` and leaves the bundle untouched.
pub(crate) fn apply_winner_chunks(
    bundle: &mut RetrievalBundle,
    winner: &DocumentAggregate,
    focus_reason: FocusReason,
    budget: usize,
    top_k: usize,
    fetched_rows: Vec<crate::infra::knowledge_rows::KnowledgeChunkRow>,
) -> ConsolidationDiagnostics {
    let (winner_label, winner_score_seed) = bundle
        .chunks
        .iter()
        .find(|chunk| chunk.document_id == winner.document_id)
        .map(|chunk| (chunk.document_label.clone(), chunk.score.unwrap_or(0.0)))
        .unwrap_or_else(|| (String::new(), 0.0));

    let winner_chunks: Vec<RuntimeMatchedChunk> = select_winner_rows(winner, budget, fetched_rows)
        .into_iter()
        .enumerate()
        .map(|(selection_rank, row)| {
            let source_text = chunk_source_text(&row);
            let excerpt = super::retrieve::focused_excerpt_for(
                &source_text,
                &[],
                FOCUSED_WINNER_EXCERPT_CHARS,
            );
            // Bias winner scores above any tangential so the downstream
            // global sort cannot re-interleave the pack back into the
            // original fragmentation. Selection rank, not source
            // ordinal, owns the bias so sparse late anchors survive
            // later top_k truncation.
            let biased = winner_score_seed.max(0.0) + 10_000.0 - (selection_rank as f32 * 0.001);
            RuntimeMatchedChunk {
                chunk_id: row.chunk_id,
                document_id: row.document_id,
                revision_id: row.revision_id,
                chunk_index: row.chunk_index,
                chunk_kind: row.chunk_kind.clone(),
                document_label: winner_label.clone(),
                excerpt,
                score_kind:
                    crate::services::query::execution::RuntimeChunkScoreKind::FocusedDocument,
                score: Some(biased),
                source_text,
            }
        })
        .collect();

    let winner_chunk_count = winner_chunks.len();
    if winner_chunk_count == 0 {
        return ConsolidationDiagnostics::noop();
    }

    let mut tangential: Vec<RuntimeMatchedChunk> = std::mem::take(&mut bundle.chunks)
        .into_iter()
        .filter(|chunk| chunk.document_id != winner.document_id)
        .collect();
    tangential.sort_by(|a, b| {
        b.score
            .unwrap_or(0.0)
            .partial_cmp(&a.score.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // Tangentials only fill the leftover slots *below* top_k — they are
    // a fallback for when the winner revision doesn't have enough
    // chunks to fill its share, not a bonus pad on top of an
    // already-packed winner. When `explicit_focus_budget` lets the
    // winner pack past top_k, `top_k.saturating_sub(winner_chunk_count)`
    // collapses to 0 and the bundle is all winner chunks — which is
    // exactly the "the compiler pinned ONE document, pack it" invariant.
    let tangential_slots = top_k.saturating_sub(winner_chunk_count);
    tangential.truncate(tangential_slots);
    let tangential_chunk_count = tangential.len();

    bundle.chunks = winner_chunks;
    bundle.chunks.extend(tangential);

    ConsolidationDiagnostics {
        focused_document_id: Some(winner.document_id),
        focus_reason,
        winner_chunk_count,
        tangential_chunk_count,
    }
}

/// Run the consolidation stage in-place on a reranked bundle.
///
/// Mutation contract:
/// - On `FocusReason::None` the bundle is left untouched.
/// - On any other focus reason, `bundle.chunks` is rewritten to
///   `[winner_chunks_by_anchor_priority..., tangential_chunks...]`
///   truncated to the allocated winner budget + remaining slots
///   for top-scored tangentials.
/// - Winner chunks are materialized via `list_chunks_by_revision_windows`; if
///   that call fails, the bundle
///   is left untouched and `FocusReason::None` is returned (we'd
///   rather ship the original than panic the whole answer).
pub(crate) async fn focused_document_consolidation(
    state: &AppState,
    bundle: &mut RetrievalBundle,
    query_ir: &QueryIR,
    question: &str,
    top_k: usize,
) -> ConsolidationDiagnostics {
    let Some((winner, focus_reason, budget)) = decide_focus(bundle, query_ir, question, top_k)
    else {
        return ConsolidationDiagnostics::noop();
    };

    let windows = winner_anchor_windows(&winner, budget);
    if windows.is_empty() {
        return ConsolidationDiagnostics::noop();
    }

    let Some(fetched) = fetch_winner_rows(state, &winner, &windows).await else {
        return ConsolidationDiagnostics::noop();
    };

    let diagnostics = apply_winner_chunks(bundle, &winner, focus_reason, budget, top_k, fetched);
    log_consolidation(&diagnostics, &winner, top_k, budget);
    diagnostics
}

/// Fetch the winner revision's bounded window rows. A store failure is
/// reported and downgraded to a skipped consolidation rather than a panic.
async fn fetch_winner_rows(
    state: &AppState,
    winner: &DocumentAggregate,
    windows: &[(i32, i32)],
) -> Option<Vec<crate::infra::knowledge_rows::KnowledgeChunkRow>> {
    match state.document_store.list_chunks_by_revision_windows(winner.revision_id, windows).await {
        Ok(rows) => Some(rows),
        Err(error) => {
            tracing::warn!(
                stage = "answer.consolidation",
                %error,
                document_id = %winner.document_id,
                revision_id = %winner.revision_id,
                window_count = windows.len(),
                "window fetch failed — consolidation skipped"
            );
            None
        }
    }
}

fn log_consolidation(
    diagnostics: &ConsolidationDiagnostics,
    winner: &DocumentAggregate,
    top_k: usize,
    budget: usize,
) {
    if diagnostics.focused_document_id.is_some() {
        tracing::info!(
            stage = "answer.consolidation",
            focus_reason = diagnostics.focus_reason.as_str(),
            document_id = ?diagnostics.focused_document_id,
            revision_id = %winner.revision_id,
            winner_chunk_count = diagnostics.winner_chunk_count,
            tangential_chunk_count = diagnostics.tangential_chunk_count,
            top_k,
            budget,
            "consolidation reshaped retrieval bundle"
        );
    }
}

fn chunk_source_text(chunk: &crate::infra::knowledge_rows::KnowledgeChunkRow) -> String {
    use crate::shared::extraction::text_render::repair_technical_layout_noise;
    if chunk.chunk_kind.as_deref() == Some("table_row") {
        return repair_technical_layout_noise(&chunk.normalized_text);
    }
    if chunk.content_text.trim().is_empty() && !chunk.normalized_text.trim().is_empty() {
        return repair_technical_layout_noise(&chunk.normalized_text);
    }
    repair_technical_layout_noise(&chunk.content_text)
}
