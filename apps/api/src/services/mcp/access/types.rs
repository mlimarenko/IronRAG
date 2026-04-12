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
    pub(crate) freshness_generation: i64,
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
    pub(crate) chunk_references: HashMap<Uuid, RankedSearchReference>,
}

impl McpDocumentAccumulator {
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
            chunk_references: HashMap::new(),
        }
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
