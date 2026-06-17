//! Per-lane score normalization for `search_documents` result merging.
//!
//! The three retrieval lanes (document metadata, lexical chunk, vector
//! chunk) produce scores on incommensurate scales: metadata and lexical
//! scores come from Postgres text ranking and are unbounded `>= 0`, while
//! vector scores are cosine similarities in `0..=1`. Combining them by a
//! raw `max` let a single lexical/metadata hit on a larger numeric scale
//! always bury a vector-strong document, so cross-lane ordering was wrong.
//!
//! This module fuses the lanes with Reciprocal Rank Fusion (RRF). RRF is
//! scale-free: it only consumes each lane's *rank order*, never the raw
//! score magnitude, so a single outlier lexical score can no longer
//! dominate. A document gets one RRF contribution per lane it appears in
//! (using its best — lowest — rank in that lane), and the contributions
//! are summed. A document present in multiple lanes therefore ranks above
//! an otherwise equally-ranked single-lane document, all else equal.
//!
//! The fusion is purely numeric over lane ranks; it contains no
//! natural-language or per-script branching.

/// RRF damping constant `k`. The fused contribution of a lane hit at
/// 1-based `rank` is `1 / (k + rank)`. `k = 60` is the long-standing
/// default from Cormack et al. (2009) "Reciprocal Rank Fusion outperforms
/// Condorcet and individual Rank Learning Methods"; it flattens the gap
/// between the very top ranks just enough that a strong hit in two lanes
/// can overtake a single-lane top hit without letting deep-tail ranks
/// contribute meaningful weight.
pub(crate) const RRF_K: f64 = 60.0;

/// Retrieval lanes that feed `search_documents`. Each lane contributes at
/// most once per document (using the document's best rank within it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SearchLane {
    /// Postgres document-metadata text match.
    Metadata,
    /// Postgres lexical chunk match.
    LexicalChunk,
    /// PostgreSQL pgvector (cosine) chunk match.
    VectorChunk,
}

/// RRF contribution of a single lane hit at the given 1-based `rank`.
///
/// `rank` is clamped to `>= 1` so a caller passing a 0-based index by
/// mistake cannot produce an outsized contribution.
pub(crate) fn rrf_contribution(rank: i32) -> f64 {
    let rank = rank.max(1) as f64;
    1.0 / (RRF_K + rank)
}

/// Tracks the best (lowest) 1-based rank a document achieved in each lane
/// and exposes the fused RRF score over those lanes.
///
/// Best-rank-per-lane (rather than accumulating every chunk hit) avoids a
/// length/fragmentation bias where a large document would win simply by
/// contributing many mediocre chunks to the same lane.
#[derive(Debug, Clone, Default)]
pub(crate) struct LaneRankFusion {
    metadata: Option<i32>,
    lexical_chunk: Option<i32>,
    vector_chunk: Option<i32>,
}

impl LaneRankFusion {
    /// Record a hit for `lane` at the given 1-based `rank`, keeping the
    /// best (lowest) rank seen so far for that lane.
    pub(crate) fn observe(&mut self, lane: SearchLane, rank: i32) {
        let slot = match lane {
            SearchLane::Metadata => &mut self.metadata,
            SearchLane::LexicalChunk => &mut self.lexical_chunk,
            SearchLane::VectorChunk => &mut self.vector_chunk,
        };
        *slot = Some(match *slot {
            Some(existing) => existing.min(rank),
            None => rank,
        });
    }

    /// Fused RRF score: the sum of `rrf_contribution(best_rank)` over every
    /// lane in which the document appeared. A document absent from all
    /// lanes scores `0.0`.
    pub(crate) fn fused_score(&self) -> f64 {
        [self.metadata, self.lexical_chunk, self.vector_chunk]
            .into_iter()
            .flatten()
            .map(rrf_contribution)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A vector-strong document (top of the vector lane, absent from the
    /// lexical lane) outranks a weak lexical-only hit — the exact inversion
    /// the old raw-`max` combine produced when the lexical scale was
    /// numerically larger than cosine similarity.
    #[test]
    fn vector_strong_document_outranks_weak_lexical_only_hit() {
        let mut vector_strong = LaneRankFusion::default();
        vector_strong.observe(SearchLane::VectorChunk, 1);

        let mut weak_lexical = LaneRankFusion::default();
        weak_lexical.observe(SearchLane::LexicalChunk, 5);

        assert!(
            vector_strong.fused_score() > weak_lexical.fused_score(),
            "top vector hit must beat a rank-5 lexical-only hit",
        );
    }

    /// A document present in two lanes outranks a single-lane document that
    /// holds the same rank in its one lane.
    #[test]
    fn multi_lane_document_outranks_equal_single_lane_document() {
        let mut multi = LaneRankFusion::default();
        multi.observe(SearchLane::LexicalChunk, 1);
        multi.observe(SearchLane::VectorChunk, 1);

        let mut single = LaneRankFusion::default();
        single.observe(SearchLane::LexicalChunk, 1);

        assert!(
            multi.fused_score() > single.fused_score(),
            "a doc in two lanes must outrank an equally-ranked single-lane doc",
        );
    }

    /// Identical lane observations yield identical fused scores regardless
    /// of the order in which hits were observed, enabling a deterministic
    /// final ordering (the caller adds a stable doc-id tie-break).
    #[test]
    fn fusion_is_deterministic_and_order_independent() {
        let mut forward = LaneRankFusion::default();
        forward.observe(SearchLane::Metadata, 3);
        forward.observe(SearchLane::LexicalChunk, 2);
        forward.observe(SearchLane::VectorChunk, 1);

        let mut reverse = LaneRankFusion::default();
        reverse.observe(SearchLane::VectorChunk, 1);
        reverse.observe(SearchLane::LexicalChunk, 2);
        reverse.observe(SearchLane::Metadata, 3);

        assert_eq!(forward.fused_score(), reverse.fused_score());
    }

    /// Single-lane-only input degrades gracefully: fused scores preserve
    /// that lane's own rank order (lower rank => higher score).
    #[test]
    fn single_lane_only_preserves_rank_order() {
        let mut rank_one = LaneRankFusion::default();
        rank_one.observe(SearchLane::VectorChunk, 1);
        let mut rank_two = LaneRankFusion::default();
        rank_two.observe(SearchLane::VectorChunk, 2);
        let mut rank_three = LaneRankFusion::default();
        rank_three.observe(SearchLane::VectorChunk, 3);

        assert!(rank_one.fused_score() > rank_two.fused_score());
        assert!(rank_two.fused_score() > rank_three.fused_score());
    }

    /// Best (lowest) rank per lane wins; repeated worse-ranked chunk hits
    /// in the same lane do not inflate the score (no fragmentation bias).
    #[test]
    fn best_rank_per_lane_is_kept_and_repeats_do_not_accumulate() {
        let mut acc = LaneRankFusion::default();
        acc.observe(SearchLane::LexicalChunk, 5);
        acc.observe(SearchLane::LexicalChunk, 2);
        acc.observe(SearchLane::LexicalChunk, 9);

        let mut best_only = LaneRankFusion::default();
        best_only.observe(SearchLane::LexicalChunk, 2);

        assert_eq!(acc.fused_score(), best_only.fused_score());
    }

    /// An empty fusion (document absent from every lane) scores zero.
    #[test]
    fn empty_fusion_scores_zero() {
        assert_eq!(LaneRankFusion::default().fused_score(), 0.0);
    }

    /// A 0-based index passed by mistake is clamped to rank 1 rather than
    /// producing an inflated `1 / (k + 0)` contribution.
    #[test]
    fn rank_is_clamped_to_one() {
        assert_eq!(rrf_contribution(0), rrf_contribution(1));
        assert_eq!(rrf_contribution(-4), rrf_contribution(1));
    }
}
