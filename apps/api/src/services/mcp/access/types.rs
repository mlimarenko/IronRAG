use std::collections::HashMap;

use uuid::Uuid;

use crate::{
    domains::content::ContentSourceAccess,
    infra::repositories::catalog_repository::CatalogLibraryRow,
    mcp_types::{
        McpChunkReference, McpEntityReference, McpEvidenceReference, McpLibraryDescriptor,
        McpReadabilityState, McpRelationReference, McpTechnicalFactReference,
    },
    services::mcp::{
        access::fusion::{LaneRankFusion, SearchLane},
        support::preview_hit,
    },
};

#[derive(Debug, Clone)]
pub(crate) struct VisibleLibraryContext {
    pub(crate) library: CatalogLibraryRow,
    pub(crate) descriptor: McpLibraryDescriptor,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedDocumentState {
    pub(crate) document_id: Uuid,
    pub(crate) document_title: String,
    pub(crate) library: CatalogLibraryRow,
    pub(crate) latest_revision_id: Option<Uuid>,
    pub(crate) readability_state: McpReadabilityState,
    pub(crate) readiness_kind: String,
    pub(crate) graph_coverage_kind: String,
    pub(crate) status_reason: Option<String>,
    pub(crate) mime_type: Option<String>,
    pub(crate) source_uri: Option<String>,
    pub(crate) source_access: Option<ContentSourceAccess>,
    pub(crate) storage_ref: Option<String>,
    pub(crate) content: Option<String>,
    pub(crate) chunk_references: Vec<McpChunkReference>,
    pub(crate) technical_fact_references: Vec<McpTechnicalFactReference>,
    pub(crate) entity_references: Vec<McpEntityReference>,
    pub(crate) relation_references: Vec<McpRelationReference>,
    pub(crate) evidence_references: Vec<McpEvidenceReference>,
}

#[derive(Debug, Clone)]
pub(crate) struct McpSearchEmbeddingContext {
    pub(crate) model_catalog_id: Uuid,
    pub(crate) query_vector: Vec<f32>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct McpRevisionGroundingReferences {
    pub(crate) technical_fact_references: Vec<McpTechnicalFactReference>,
    pub(crate) entity_references: Vec<McpEntityReference>,
    pub(crate) relation_references: Vec<McpRelationReference>,
    pub(crate) evidence_references: Vec<McpEvidenceReference>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RankedSearchReference {
    pub(crate) rank: i32,
    pub(crate) score: f64,
    pub(crate) inclusion_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct McpDocumentAccumulator {
    pub(crate) document_id: Uuid,
    pub(crate) library_id: Uuid,
    pub(crate) workspace_id: Uuid,
    pub(crate) readable_revision_id: Uuid,
    pub(crate) document_title: String,
    /// Per-lane RRF rank accumulator. The exposed document score is its
    /// [`LaneRankFusion::fused_score`], replacing the previous raw-`max`
    /// combine that compared incommensurate per-lane score scales.
    pub(crate) fusion: LaneRankFusion,
    pub(crate) excerpt: Option<String>,
    pub(crate) excerpt_start_offset: Option<usize>,
    pub(crate) excerpt_end_offset: Option<usize>,
    /// Character offset of the top-scoring chunk inside the full
    /// normalized revision text. Populated from `chunk.span_start`,
    /// bounded so the first `read_document` window contains content
    /// even when chunk spans drift to the final character boundary.
    pub(crate) suggested_start_offset: Option<usize>,
    pub(crate) suggested_start_offset_score: f64,
    pub(crate) content_char_count: Option<usize>,
    pub(crate) chunk_references: HashMap<Uuid, RankedSearchReference>,
}

impl McpDocumentAccumulator {
    pub(crate) fn from_metadata(
        row: &crate::infra::repositories::content_repository::ContentDocumentMetadataSearchRow,
    ) -> Self {
        let document_title = row
            .revision_title
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| row.external_key.clone());
        Self {
            document_id: row.document_id,
            library_id: row.library_id,
            workspace_id: row.workspace_id,
            readable_revision_id: row.readable_revision_id,
            document_title,
            fusion: LaneRankFusion::default(),
            excerpt: None,
            excerpt_start_offset: None,
            excerpt_end_offset: None,
            suggested_start_offset: Some(0),
            suggested_start_offset_score: row.metadata_score,
            content_char_count: None,
            chunk_references: HashMap::new(),
        }
    }

    pub(crate) fn from_knowledge(
        document: &crate::infra::knowledge_rows::KnowledgeDocumentRow,
        revision: &crate::infra::knowledge_rows::KnowledgeRevisionRow,
    ) -> Self {
        let document_title = revision
            .title
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| document.external_key.clone());
        Self {
            document_id: document.document_id,
            library_id: document.library_id,
            workspace_id: document.workspace_id,
            readable_revision_id: revision.revision_id,
            document_title,
            fusion: LaneRankFusion::default(),
            excerpt: None,
            excerpt_start_offset: None,
            excerpt_end_offset: None,
            suggested_start_offset: None,
            suggested_start_offset_score: f64::MIN,
            content_char_count: revision
                .normalized_text
                .as_ref()
                .map(|value| value.chars().count()),
            chunk_references: HashMap::new(),
        }
    }

    /// Record the start offset of a candidate chunk. Keeps the offset
    /// whose score is the highest we've seen so far. The exposed offset
    /// is a read-window anchor rather than an unchecked raw span: it
    /// starts at the chunk when possible, but backs up near document
    /// tails so a first `read_document` call returns useful content.
    pub(crate) fn merge_chunk_span_anchor(
        &mut self,
        span_start: Option<i32>,
        score: f64,
        read_window_chars: usize,
    ) {
        let Some(start) = span_start else {
            return;
        };
        if start < 0 {
            return;
        }
        if score <= self.suggested_start_offset_score {
            return;
        }
        self.suggested_start_offset = Some(window_safe_start_offset(
            start as usize,
            self.content_char_count,
            read_window_chars,
        ));
        self.suggested_start_offset_score = score;
    }

    /// Record that this document appeared in `lane` at the given 1-based
    /// `rank`, keeping the best (lowest) rank per lane. The document's
    /// exposed score is then the RRF fusion over its observed lanes
    /// ([`Self::fused_score`]), so cross-lane ordering no longer depends on
    /// raw, incommensurate per-lane score magnitudes.
    pub(crate) fn observe_lane(&mut self, lane: SearchLane, rank: i32) {
        self.fusion.observe(lane, rank);
    }

    /// Reciprocal-rank-fusion score across the lanes this document hit.
    pub(crate) fn fused_score(&self) -> f64 {
        self.fusion.fused_score()
    }

    pub(crate) fn merge_chunk_reference(
        &mut self,
        chunk_id: Uuid,
        rank: i32,
        score: f64,
        inclusion_reason: Option<String>,
    ) {
        let entry = self.chunk_references.entry(chunk_id).or_insert_with(|| {
            RankedSearchReference { rank, score, inclusion_reason: inclusion_reason.clone() }
        });
        entry.rank = entry.rank.min(rank);
        if score > entry.score {
            entry.score = score;
        }
        if entry.inclusion_reason.is_none() {
            entry.inclusion_reason = inclusion_reason;
        }
    }

    pub(crate) fn populate_excerpt_from_text(&mut self, text: &str, query: &str) {
        if self.excerpt.is_some() {
            return;
        }
        let query_lower = query.to_lowercase();
        if let Some((excerpt, start, end, _)) = preview_hit(text, &query_lower) {
            self.excerpt = Some(excerpt);
            self.excerpt_start_offset = Some(start);
            self.excerpt_end_offset = Some(end);
        }
    }

    pub(crate) fn chunk_reference_ids(&self) -> Vec<Uuid> {
        self.chunk_references.keys().copied().collect()
    }

    pub(crate) fn into_chunk_references(self) -> Vec<McpChunkReference> {
        let mut rows = self.chunk_references.into_iter().collect::<Vec<_>>();
        rows.sort_by(|(left_id, left), (right_id, right)| {
            left.rank
                .cmp(&right.rank)
                .then_with(|| right.score.total_cmp(&left.score))
                .then_with(|| left_id.cmp(right_id))
        });
        rows.into_iter()
            .map(|(chunk_id, reference)| McpChunkReference {
                chunk_id,
                rank: reference.rank,
                score: reference.score,
                inclusion_reason: reference.inclusion_reason,
            })
            .collect()
    }
}

pub(crate) fn window_safe_start_offset(
    requested_start: usize,
    total_content_chars: Option<usize>,
    read_window_chars: usize,
) -> usize {
    let Some(total_content_chars) = total_content_chars else {
        return requested_start;
    };
    if total_content_chars == 0 {
        return 0;
    }
    let window_chars = read_window_chars.max(1).min(total_content_chars);
    let latest_useful_start = total_content_chars.saturating_sub(window_chars);
    requested_start.min(latest_useful_start)
}

#[cfg(test)]
mod tests {
    use super::window_safe_start_offset;

    #[test]
    fn window_safe_start_keeps_middle_chunk_anchor() {
        assert_eq!(window_safe_start_offset(200, Some(1_000), 300), 200);
    }

    #[test]
    fn window_safe_start_backs_up_for_tail_chunk_anchor() {
        assert_eq!(window_safe_start_offset(900, Some(1_000), 300), 700);
    }

    #[test]
    fn window_safe_start_returns_zero_when_document_fits_window() {
        assert_eq!(window_safe_start_offset(900, Some(1_000), 2_000), 0);
    }

    #[test]
    fn window_safe_start_tolerates_unknown_or_empty_content_length() {
        assert_eq!(window_safe_start_offset(900, None, 300), 900);
        assert_eq!(window_safe_start_offset(900, Some(0), 300), 0);
    }
}
