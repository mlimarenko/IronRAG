use std::collections::HashMap;

use serde::Deserialize;
use uuid::Uuid;

use crate::{
    domains::content::ContentSourceAccess,
    infra::repositories::catalog_repository::CatalogLibraryRow,
    mcp_types::{
        McpChunkReference, McpEntityReference, McpEvidenceReference, McpLibraryDescriptor,
        McpReadabilityState, McpRelationReference, McpTechnicalFactReference,
    },
    services::mcp::support::preview_hit,
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

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ArangoChunkMentionReferenceRow {
    pub(crate) entity_id: Uuid,
    pub(crate) rank: i32,
    pub(crate) score: f64,
    pub(crate) inclusion_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ArangoRelationSupportReferenceRow {
    pub(crate) relation_id: Uuid,
    pub(crate) rank: i32,
    pub(crate) score: f64,
    pub(crate) inclusion_reason: Option<String>,
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
    pub(crate) score: f64,
    pub(crate) excerpt: Option<String>,
    pub(crate) excerpt_start_offset: Option<usize>,
    pub(crate) excerpt_end_offset: Option<usize>,
    /// Character offset of the top-scoring chunk inside the full
    /// normalized revision text. Populated from `chunk.span_start`
    /// when `merge_chunk_span_anchor` is called with a higher score
    /// than we've previously recorded — so the document-level hit
    /// ends up pointing at the best chunk's start offset.
    pub(crate) suggested_start_offset: Option<usize>,
    pub(crate) suggested_start_offset_score: f64,
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
            score: row.metadata_score,
            excerpt: None,
            excerpt_start_offset: None,
            excerpt_end_offset: None,
            suggested_start_offset: None,
            suggested_start_offset_score: f64::MIN,
            chunk_references: HashMap::new(),
        }
    }

    pub(crate) fn from_knowledge(
        document: &crate::infra::arangodb::document_store::KnowledgeDocumentRow,
        revision: &crate::infra::arangodb::document_store::KnowledgeRevisionRow,
        hit: &crate::infra::arangodb::search_store::KnowledgeChunkSearchRow,
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
            score: hit.score,
            excerpt: None,
            excerpt_start_offset: None,
            excerpt_end_offset: None,
            suggested_start_offset: None,
            suggested_start_offset_score: f64::MIN,
            chunk_references: HashMap::new(),
        }
    }

    /// Record the start offset of a candidate chunk. Keeps the offset
    /// whose score is the highest we've seen so far — the idea is
    /// that the top chunk's `span_start` points right at the paragraph
    /// the user is actually asking about, so handing it to the agent
    /// as `suggestedStartOffset` skips the PDF table of contents.
    pub(crate) fn merge_chunk_span_anchor(&mut self, span_start: Option<i32>, score: f64) {
        let Some(start) = span_start else {
            return;
        };
        if start < 0 {
            return;
        }
        if score <= self.suggested_start_offset_score {
            return;
        }
        self.suggested_start_offset = Some(start as usize);
        self.suggested_start_offset_score = score;
    }

    pub(crate) fn bump_score(&mut self, score: f64) {
        self.score = self.score.max(score);
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
        let query_lower = query.to_ascii_lowercase();
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
