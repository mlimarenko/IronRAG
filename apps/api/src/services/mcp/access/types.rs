use std::collections::HashMap;

use uuid::Uuid;

use crate::{
    domains::content::ContentSourceAccess,
    infra::repositories::catalog_repository::CatalogLibraryRow,
    mcp_types::{
        McpChunkReference, McpEntityReference, McpEvidenceReference, McpLibraryDescriptor,
        McpReadabilityState, McpRelationReference, McpTechnicalFactReference,
    },
    services::{
        mcp::{
            access::fusion::{LaneRankFusion, SearchLane},
            search_excerpt::preview_hit,
        },
        query::vector_dimensions::EmbeddingProfileInventoryVersion,
    },
};

#[derive(Debug, Clone)]
pub(crate) struct VisibleLibraryContext {
    pub(crate) library: CatalogLibraryRow,
    pub(crate) descriptor: McpLibraryDescriptor,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedDocumentState {
    pub(crate) snapshot:
        crate::infra::repositories::content_document_head_repository::ContentDocumentReadSnapshot,
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
    pub(crate) embedding_profile_key: String,
    pub(crate) inventory_version: EmbeddingProfileInventoryVersion,
    pub(crate) dimensions: u64,
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
    pub(crate) readable_revision_id: Option<Uuid>,
    pub(crate) document_title: String,
    /// Per-lane RRF rank accumulator. The exposed document score is its
    /// [`LaneRankFusion::fused_score`], replacing the previous raw-`max`
    /// combine that compared incommensurate per-lane score scales.
    pub(crate) fusion: LaneRankFusion,
    pub(crate) excerpt: Option<String>,
    excerpt_score: f64,
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
        row: &crate::infra::repositories::content_metadata_search_repository::ContentDocumentMetadataSearchRow,
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
            excerpt_score: f64::MIN,
            excerpt_start_offset: None,
            excerpt_end_offset: None,
            suggested_start_offset: row.readable_revision_id.map(|_| 0),
            // Metadata has no body span. Zero is a safe fallback, but it must
            // never outrank a real structured/chunk anchor whose score lives
            // on a different retrieval scale.
            suggested_start_offset_score: f64::MIN,
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
            readable_revision_id: Some(revision.revision_id),
            document_title,
            fusion: LaneRankFusion::default(),
            excerpt: None,
            excerpt_score: f64::MIN,
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

    pub(crate) fn from_structured_block(
        row: &crate::infra::repositories::content_structured_block_search_repository::ContentStructuredBlockSearchRow,
    ) -> Self {
        let document_title = row
            .document_title
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| row.external_key.clone());
        Self {
            document_id: row.document_id,
            library_id: row.library_id,
            workspace_id: row.workspace_id,
            readable_revision_id: Some(row.readable_revision_id),
            document_title,
            fusion: LaneRankFusion::default(),
            excerpt: None,
            excerpt_score: f64::MIN,
            excerpt_start_offset: None,
            excerpt_end_offset: None,
            suggested_start_offset: None,
            suggested_start_offset_score: f64::MIN,
            content_char_count: None,
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
        if let Some((excerpt, start, end, score)) = preview_hit(text, query)
            && score > self.excerpt_score
        {
            self.excerpt = Some(excerpt);
            self.excerpt_score = score;
            self.excerpt_start_offset = Some(byte_offset_to_char_offset(text, start));
            self.excerpt_end_offset = Some(byte_offset_to_char_offset(text, end));
        }
    }

    /// Builds a bounded, match-centered preview from the chosen block's
    /// original source text. The returned offsets are block-local, so this path
    /// deliberately leaves document excerpt offsets unset. The persisted block
    /// span remains a trustworthy character anchor into the full revision.
    pub(crate) fn merge_structured_block_match(
        &mut self,
        row: &crate::infra::repositories::content_structured_block_search_repository::ContentStructuredBlockSearchRow,
        query: &str,
        read_window_chars: usize,
    ) {
        if let Some((excerpt, _, _, score)) = preview_hit(&row.source_text, query)
            && score > self.excerpt_score
        {
            self.excerpt = Some(excerpt);
            self.excerpt_score = score;
            self.excerpt_start_offset = None;
            self.excerpt_end_offset = None;
        }
        self.merge_chunk_span_anchor(row.span_start, row.text_score, read_window_chars);
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

fn byte_offset_to_char_offset(text: &str, byte_offset: usize) -> usize {
    let mut boundary = byte_offset.min(text.len());
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    text[..boundary].chars().count()
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
    use super::{McpDocumentAccumulator, byte_offset_to_char_offset, window_safe_start_offset};
    use crate::infra::repositories::{
        content_metadata_search_repository::ContentDocumentMetadataSearchRow,
        content_structured_block_search_repository::ContentStructuredBlockSearchRow,
    };
    use uuid::Uuid;

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

    #[test]
    fn structured_block_match_keeps_headline_offsets_unset_and_uses_block_span_anchor() {
        let row = ContentStructuredBlockSearchRow {
            document_id: Uuid::from_u128(1),
            workspace_id: Uuid::from_u128(2),
            library_id: Uuid::from_u128(3),
            external_key: "synthetic-document".to_string(),
            readable_revision_id: Uuid::from_u128(4),
            document_title: None,
            source_text: "context search-anchor suffix".to_string(),
            span_start: Some(240),
            text_score: 0.5,
        };
        let mut accumulator = McpDocumentAccumulator::from_structured_block(&row);

        accumulator.merge_structured_block_match(&row, "search-anchor", 120);

        assert!(
            accumulator.excerpt.as_deref().is_some_and(|value| value.contains("search-anchor"))
        );
        assert_eq!(accumulator.excerpt_start_offset, None);
        assert_eq!(accumulator.excerpt_end_offset, None);
        assert_eq!(accumulator.suggested_start_offset, Some(240));
    }

    #[test]
    fn populated_chunk_excerpt_uses_character_offsets_after_diacritic_fold() {
        let normalized_text = format!("{}Résumé suffix", "α".repeat(100));
        let row = ContentStructuredBlockSearchRow {
            document_id: Uuid::from_u128(1),
            workspace_id: Uuid::from_u128(2),
            library_id: Uuid::from_u128(3),
            external_key: "synthetic-document".to_string(),
            readable_revision_id: Uuid::from_u128(4),
            document_title: None,
            source_text: String::new(),
            span_start: None,
            text_score: 0.5,
        };
        let mut accumulator = McpDocumentAccumulator::from_structured_block(&row);

        accumulator.populate_excerpt_from_text(&normalized_text, "resume");

        assert!(accumulator.excerpt.as_deref().is_some_and(|value| value.contains("Résumé")));
        assert_eq!(accumulator.excerpt_start_offset, Some(60));
    }

    #[test]
    fn stronger_body_match_replaces_earlier_metadata_token_excerpt() {
        let row = ContentDocumentMetadataSearchRow {
            document_id: Uuid::from_u128(1),
            workspace_id: Uuid::from_u128(2),
            library_id: Uuid::from_u128(3),
            external_key: "synthetic-document".to_string(),
            readable_revision_id: Some(Uuid::from_u128(4)),
            revision_title: Some("Upload Proof".to_string()),
            metadata_score: 150.0,
            matched_text: "Upload Proof".to_string(),
        };
        let mut accumulator = McpDocumentAccumulator::from_metadata(&row);

        accumulator.populate_excerpt_from_text(&row.matched_text, "shared operation proof");
        assert_eq!(accumulator.excerpt.as_deref(), Some("Upload Proof"));

        accumulator.populate_excerpt_from_text(
            "Shared operation proof from the authoritative document body.",
            "shared operation proof",
        );
        accumulator.merge_chunk_span_anchor(Some(600), 0.1, 120);

        assert!(
            accumulator
                .excerpt
                .as_deref()
                .is_some_and(|excerpt| excerpt.contains("Shared operation proof"))
        );
        assert_eq!(accumulator.excerpt_start_offset, Some(0));
        assert_eq!(accumulator.suggested_start_offset, Some(600));
    }

    #[test]
    fn byte_offsets_are_converted_to_character_offsets_without_splitting_utf8() {
        assert_eq!(byte_offset_to_char_offset("αβgamma", 4), 2);
        assert_eq!(byte_offset_to_char_offset("αβgamma", 3), 1);
    }
}
