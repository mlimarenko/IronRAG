//! Deterministic candidate-fusion policies.
//!
//! This module owns score normalization and ordering only. Retrieval lanes
//! produce typed candidates; query-specific context reservations remain in the
//! caller so fusion can be characterized and replaced independently.

use std::collections::HashMap;

use uuid::Uuid;

use super::{
    tuning::DOCUMENT_IDENTITY_SCORE_FLOOR,
    types::{RuntimeChunkScoreKind, RuntimeMatchedChunk},
};

const RRF_K: f32 = 60.0;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum RrfFusionLane {
    Default,
    EntityBio,
    GraphEvidence,
    QueryIrFocus,
    VersionedUpdateProcedure,
}

impl RrfFusionLane {
    const fn score_kind(self) -> RuntimeChunkScoreKind {
        match self {
            Self::Default => RuntimeChunkScoreKind::Relevance,
            Self::EntityBio => RuntimeChunkScoreKind::EntityBio,
            Self::GraphEvidence => RuntimeChunkScoreKind::GraphEvidence,
            Self::QueryIrFocus => RuntimeChunkScoreKind::QueryIrFocus,
            Self::VersionedUpdateProcedure => RuntimeChunkScoreKind::FocusedDocument,
        }
    }
}

pub(crate) const fn score_kind_priority(kind: RuntimeChunkScoreKind) -> u8 {
    match kind {
        RuntimeChunkScoreKind::Relevance => 0,
        RuntimeChunkScoreKind::EntityBio
        | RuntimeChunkScoreKind::GraphEvidence
        | RuntimeChunkScoreKind::SourceContext
        | RuntimeChunkScoreKind::FocusedDocument => 1,
        RuntimeChunkScoreKind::QueryIrFocus => 2,
        RuntimeChunkScoreKind::DocumentIdentity | RuntimeChunkScoreKind::LatestVersion => 3,
        RuntimeChunkScoreKind::ContentAnchor => 4,
    }
}

/// Fuse two ranked candidate lists with reciprocal-rank fusion.
///
/// Scores from ordinary relevance lanes are normalized to RRF. Absolute
/// evidence lanes retain their strongest source score and typed score kind so
/// a normalized rank cannot erase an explicit evidence reservation.
pub(crate) fn fuse_rrf_chunks(
    left_hits: Vec<RuntimeMatchedChunk>,
    right_hits: Vec<RuntimeMatchedChunk>,
    right_lane: RrfFusionLane,
) -> Vec<RuntimeMatchedChunk> {
    let mut rrf_scores = HashMap::<Uuid, f32>::new();
    let mut source_scores = HashMap::<Uuid, f32>::new();
    let mut score_kinds = HashMap::<Uuid, RuntimeChunkScoreKind>::new();
    let mut chunks_by_id = HashMap::<Uuid, RuntimeMatchedChunk>::new();

    let mut record_hit = |rank: usize, chunk: RuntimeMatchedChunk, lane: RrfFusionLane| {
        *rrf_scores.entry(chunk.chunk_id).or_default() += 1.0 / (RRF_K + rank as f32 + 1.0);
        let source_score = score_value(chunk.score);
        let score_kind = effective_score_kind(&chunk, lane, source_score);
        score_kinds
            .entry(chunk.chunk_id)
            .and_modify(|existing| {
                if score_kind_priority(score_kind) > score_kind_priority(*existing) {
                    *existing = score_kind;
                }
            })
            .or_insert(score_kind);
        if source_score.is_finite() {
            source_scores
                .entry(chunk.chunk_id)
                .and_modify(|existing| *existing = existing.max(source_score))
                .or_insert(source_score);
        }
        chunks_by_id.entry(chunk.chunk_id).or_insert(chunk);
    };

    for (rank, chunk) in left_hits.into_iter().enumerate() {
        record_hit(rank, chunk, RrfFusionLane::Default);
    }
    for (rank, chunk) in right_hits.into_iter().enumerate() {
        record_hit(rank, chunk, right_lane);
    }

    let mut fused = chunks_by_id
        .into_values()
        .map(|mut chunk| {
            let score_kind = score_kinds
                .get(&chunk.chunk_id)
                .copied()
                .unwrap_or(RuntimeChunkScoreKind::Relevance);
            chunk.score = if preserves_absolute_score(score_kind) {
                source_scores.get(&chunk.chunk_id).copied()
            } else {
                rrf_scores.get(&chunk.chunk_id).copied()
            };
            chunk.score_kind = score_kind;
            chunk
        })
        .collect::<Vec<_>>();

    fused.sort_by(|left, right| {
        score_kind_priority(right.score_kind)
            .cmp(&score_kind_priority(left.score_kind))
            .then_with(|| score_value(right.score).total_cmp(&score_value(left.score)))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
            .then_with(|| left.document_id.cmp(&right.document_id))
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
    });
    fused
}

const fn preserves_absolute_score(kind: RuntimeChunkScoreKind) -> bool {
    !matches!(kind, RuntimeChunkScoreKind::Relevance)
}

fn effective_score_kind(
    chunk: &RuntimeMatchedChunk,
    lane: RrfFusionLane,
    source_score: f32,
) -> RuntimeChunkScoreKind {
    if chunk.score_kind != RuntimeChunkScoreKind::Relevance {
        return chunk.score_kind;
    }
    if source_score >= DOCUMENT_IDENTITY_SCORE_FLOOR {
        return RuntimeChunkScoreKind::DocumentIdentity;
    }
    lane.score_kind()
}

fn score_value(score: Option<f32>) -> f32 {
    score.unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(id: Uuid, score: f32, score_kind: RuntimeChunkScoreKind) -> RuntimeMatchedChunk {
        RuntimeMatchedChunk {
            chunk_id: id,
            document_id: Uuid::nil(),
            revision_id: Uuid::nil(),
            chunk_index: 0,
            chunk_kind: None,
            document_label: "Synthetic document".to_string(),
            excerpt: "Synthetic evidence".to_string(),
            score_kind,
            score: Some(score),
            source_text: "Synthetic evidence".to_string(),
        }
    }

    #[test]
    fn reciprocal_rank_fusion_rewards_candidates_present_in_both_lanes() {
        let shared_id = Uuid::now_v7();
        let left_only_id = Uuid::now_v7();
        let fused = fuse_rrf_chunks(
            vec![
                chunk(left_only_id, 0.9, RuntimeChunkScoreKind::Relevance),
                chunk(shared_id, 0.8, RuntimeChunkScoreKind::Relevance),
            ],
            vec![chunk(shared_id, 0.1, RuntimeChunkScoreKind::Relevance)],
            RrfFusionLane::Default,
        );

        assert_eq!(fused.first().map(|candidate| candidate.chunk_id), Some(shared_id));
    }

    #[test]
    fn evidence_lane_keeps_absolute_source_score_and_kind() {
        let chunk_id = Uuid::now_v7();
        let fused = fuse_rrf_chunks(
            Vec::new(),
            vec![chunk(chunk_id, 5000.0, RuntimeChunkScoreKind::GraphEvidence)],
            RrfFusionLane::GraphEvidence,
        );

        assert_eq!(fused[0].score, Some(5000.0));
        assert_eq!(fused[0].score_kind, RuntimeChunkScoreKind::GraphEvidence);
    }
}
