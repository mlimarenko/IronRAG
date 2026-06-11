//! Knowledge-plane [`DocumentStore`] port implementation for the Arango
//! adapter. Each method forwards verbatim to the inherent
//! `ArangoDocumentStore` method (same AQL, same logic) — the trait is the
//! swappable boundary; the bodies stay on the concrete struct.
//!
//! See [`crate::infra::knowledge_plane::DocumentStore`] for the leaky-contract
//! doc-comments (input-rank ordering, write-count returns, vector routing).
#![allow(clippy::too_many_arguments)]

use super::*;

#[async_trait::async_trait]
impl crate::infra::knowledge_plane::DocumentStore for ArangoDocumentStore {
    async fn upsert_document(
        &self,
        row: &KnowledgeDocumentRow,
    ) -> anyhow::Result<KnowledgeDocumentRow> {
        self.upsert_document(row).await
    }
    async fn get_document(
        &self,
        document_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeDocumentRow>> {
        self.get_document(document_id).await
    }
    async fn get_document_by_external_key(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
        external_key: &str,
    ) -> anyhow::Result<Option<KnowledgeDocumentRow>> {
        self.get_document_by_external_key(workspace_id, library_id, external_key).await
    }
    async fn list_documents_by_library(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
        include_deleted: bool,
    ) -> anyhow::Result<Vec<KnowledgeDocumentRow>> {
        self.list_documents_by_library(workspace_id, library_id, include_deleted).await
    }
    async fn list_documents_by_ids(
        &self,
        document_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeDocumentRow>> {
        self.list_documents_by_ids(document_ids).await
    }
    async fn update_document_pointers(
        &self,
        document_id: Uuid,
        document_state: &str,
        active_revision_id: Option<Uuid>,
        readable_revision_id: Option<Uuid>,
        latest_revision_no: Option<i64>,
        title: Option<&str>,
        deleted_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Option<KnowledgeDocumentRow>> {
        self.update_document_pointers(
            document_id,
            document_state,
            active_revision_id,
            readable_revision_id,
            latest_revision_no,
            title,
            deleted_at,
        )
        .await
    }
    async fn upsert_revision(
        &self,
        row: &KnowledgeRevisionRow,
    ) -> anyhow::Result<KnowledgeRevisionRow> {
        self.upsert_revision(row).await
    }
    async fn update_revision_document_hint(
        &self,
        revision_id: Uuid,
        document_hint: Option<&str>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>> {
        self.update_revision_document_hint(revision_id, document_hint).await
    }
    async fn get_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>> {
        self.get_revision(revision_id).await
    }
    async fn list_revisions_by_ids(
        &self,
        revision_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeRevisionRow>> {
        self.list_revisions_by_ids(revision_ids).await
    }
    async fn list_revisions_by_document(
        &self,
        document_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRevisionRow>> {
        self.list_revisions_by_document(document_id).await
    }
    async fn aggregate_library_generation_signals(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<LibraryGenerationSignals> {
        self.aggregate_library_generation_signals(library_id).await
    }
    async fn count_vector_ready_revisions_missing_chunk_vectors(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<i64> {
        self.count_vector_ready_revisions_missing_chunk_vectors(library_id).await
    }
    async fn update_revision_readiness(
        &self,
        revision_id: Uuid,
        text_state: &str,
        vector_state: &str,
        graph_state: &str,
        text_readable_at: Option<DateTime<Utc>>,
        vector_ready_at: Option<DateTime<Utc>>,
        graph_ready_at: Option<DateTime<Utc>>,
        superseded_by_revision_id: Option<Uuid>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>> {
        self.update_revision_readiness(
            revision_id,
            text_state,
            vector_state,
            graph_state,
            text_readable_at,
            vector_ready_at,
            graph_ready_at,
            superseded_by_revision_id,
        )
        .await
    }
    async fn update_revision_text_content(
        &self,
        revision_id: Uuid,
        normalized_text: Option<&str>,
        text_checksum: Option<&str>,
        text_state: &str,
        text_readable_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>> {
        self.update_revision_text_content(
            revision_id,
            normalized_text,
            text_checksum,
            text_state,
            text_readable_at,
        )
        .await
    }
    async fn update_revision_image_checksum(
        &self,
        revision_id: Uuid,
        image_checksum: Option<&str>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>> {
        self.update_revision_image_checksum(revision_id, image_checksum).await
    }
    async fn update_revision_storage_ref(
        &self,
        revision_id: Uuid,
        storage_ref: Option<&str>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>> {
        self.update_revision_storage_ref(revision_id, storage_ref).await
    }
    async fn upsert_chunk(&self, row: &KnowledgeChunkRow) -> anyhow::Result<KnowledgeChunkRow> {
        self.upsert_chunk(row).await
    }
    async fn insert_chunks(
        &self,
        rows: &[KnowledgeChunkRow],
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.insert_chunks(rows).await
    }
    async fn list_chunks_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.list_chunks_by_library(library_id).await
    }
    async fn list_source_profile_chunks_by_revisions(
        &self,
        library_id: Uuid,
        revision_ids: &[Uuid],
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.list_source_profile_chunks_by_revisions(library_id, revision_ids, limit).await
    }
    async fn list_chunks_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.list_chunks_by_revision(revision_id).await
    }
    async fn list_head_chunks_by_revision(
        &self,
        revision_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.list_head_chunks_by_revision(revision_id, limit).await
    }
    async fn count_chunks_by_revision(&self, revision_id: Uuid) -> anyhow::Result<i64> {
        self.count_chunks_by_revision(revision_id).await
    }
    async fn list_chunks_by_revision_matching_terms(
        &self,
        revision_id: Uuid,
        terms: &[String],
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.list_chunks_by_revision_matching_terms(revision_id, terms, limit).await
    }
    async fn list_chunks_by_revisions_matching_terms(
        &self,
        revision_ids: &[Uuid],
        terms: &[String],
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.list_chunks_by_revisions_matching_terms(revision_ids, terms, limit).await
    }
    async fn list_chunks_by_revision_range(
        &self,
        revision_id: Uuid,
        min_chunk_index: i32,
        max_chunk_index: i32,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.list_chunks_by_revision_range(revision_id, min_chunk_index, max_chunk_index).await
    }
    async fn list_chunks_by_revision_windows(
        &self,
        revision_id: Uuid,
        windows: &[(i32, i32)],
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.list_chunks_by_revision_windows(revision_id, windows).await
    }
    async fn list_chunks_by_revisions_windows(
        &self,
        windows: &[(Uuid, i32, i32)],
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.list_chunks_by_revisions_windows(windows).await
    }
    async fn list_tail_chunks_by_revision(
        &self,
        revision_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.list_tail_chunks_by_revision(revision_id, limit).await
    }
    async fn list_head_source_unit_blocks_by_revision(
        &self,
        revision_id: Uuid,
        limit: usize,
        temporal_start: Option<chrono::DateTime<chrono::Utc>>,
        temporal_end: Option<chrono::DateTime<chrono::Utc>>,
        release_marker_required: bool,
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockRow>> {
        self.list_head_source_unit_blocks_by_revision(
            revision_id,
            limit,
            temporal_start,
            temporal_end,
            release_marker_required,
        )
        .await
    }
    async fn list_tail_source_unit_blocks_by_revision(
        &self,
        revision_id: Uuid,
        limit: usize,
        temporal_start: Option<chrono::DateTime<chrono::Utc>>,
        temporal_end: Option<chrono::DateTime<chrono::Utc>>,
        release_marker_required: bool,
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockRow>> {
        self.list_tail_source_unit_blocks_by_revision(
            revision_id,
            limit,
            temporal_start,
            temporal_end,
            release_marker_required,
        )
        .await
    }
    async fn get_chunk(&self, chunk_id: Uuid) -> anyhow::Result<Option<KnowledgeChunkRow>> {
        self.get_chunk(chunk_id).await
    }
    async fn list_chunks_by_ids(
        &self,
        chunk_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.list_chunks_by_ids(chunk_ids).await
    }
    async fn search_code_pattern_chunks_by_terms(
        &self,
        library_id: Uuid,
        terms: &[String],
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.search_code_pattern_chunks_by_terms(library_id, terms, limit).await
    }
    async fn search_transport_pattern_chunks_by_terms(
        &self,
        library_id: Uuid,
        terms: &[String],
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.search_transport_pattern_chunks_by_terms(library_id, terms, limit).await
    }
    async fn delete_chunks_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.delete_chunks_by_revision(revision_id).await
    }
    async fn upsert_structured_revision(
        &self,
        row: &KnowledgeStructuredRevisionRow,
    ) -> anyhow::Result<KnowledgeStructuredRevisionRow> {
        self.upsert_structured_revision(row).await
    }
    async fn get_structured_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeStructuredRevisionRow>> {
        self.get_structured_revision(revision_id).await
    }
    async fn get_structured_revision_counts(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Option<StructuredRevisionCounts>> {
        self.get_structured_revision_counts(revision_id).await
    }
    async fn list_structured_revisions_by_revision_ids(
        &self,
        revision_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeStructuredRevisionRow>> {
        self.list_structured_revisions_by_revision_ids(revision_ids).await
    }
    async fn list_structured_revisions_by_document(
        &self,
        document_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeStructuredRevisionRow>> {
        self.list_structured_revisions_by_document(document_id).await
    }
    async fn replace_structured_blocks(
        &self,
        revision_id: Uuid,
        rows: &[KnowledgeStructuredBlockRow],
    ) -> anyhow::Result<()> {
        self.replace_structured_blocks(revision_id, rows).await
    }
    async fn list_structured_blocks_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockRow>> {
        self.list_structured_blocks_by_revision(revision_id).await
    }
    async fn list_structured_blocks_page_by_revision(
        &self,
        revision_id: Uuid,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<(Vec<KnowledgeStructuredBlockRow>, usize)> {
        self.list_structured_blocks_page_by_revision(revision_id, offset, limit).await
    }
    async fn list_chunk_support_references_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeChunkSupportReferenceRow>> {
        self.list_chunk_support_references_by_revision(revision_id).await
    }
    async fn list_structured_blocks_by_ids(
        &self,
        block_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockRow>> {
        self.list_structured_blocks_by_ids(block_ids).await
    }
    async fn delete_structured_blocks_by_revision(&self, revision_id: Uuid) -> anyhow::Result<()> {
        self.delete_structured_blocks_by_revision(revision_id).await
    }
    async fn replace_technical_facts(
        &self,
        revision_id: Uuid,
        rows: &[KnowledgeTechnicalFactRow],
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        self.replace_technical_facts(revision_id, rows).await
    }
    async fn list_technical_facts_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        self.list_technical_facts_by_revision(revision_id).await
    }
    async fn count_technical_facts_by_revision(&self, revision_id: Uuid) -> anyhow::Result<i64> {
        self.count_technical_facts_by_revision(revision_id).await
    }
    async fn list_technical_facts_by_ids(
        &self,
        fact_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        self.list_technical_facts_by_ids(fact_ids).await
    }
    async fn list_technical_facts_by_chunk_ids(
        &self,
        chunk_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        self.list_technical_facts_by_chunk_ids(chunk_ids).await
    }
    async fn list_technical_facts_by_document(
        &self,
        document_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        self.list_technical_facts_by_document(document_id).await
    }
    async fn delete_technical_facts_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        self.delete_technical_facts_by_revision(revision_id).await
    }
}
