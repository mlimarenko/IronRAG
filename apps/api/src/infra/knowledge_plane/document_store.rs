use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::infra::knowledge_rows::{
    KnowledgeChunkRow, KnowledgeChunkSupportReferenceRow, KnowledgeDocumentRow,
    KnowledgeRevisionRow, KnowledgeStructuredBlockRow, KnowledgeStructuredRevisionRow,
    KnowledgeTechnicalFactRow, LibraryGenerationSignals, StructuredRevisionCounts,
};

use super::write_authority::{
    IngestAttemptMutationOutcome, IngestRevisionExtractArtifact, IngestRevisionWriteAuthority,
};

/// Shared handle for a [`DocumentStore`] adapter.
pub type DocumentStoreRef = Arc<dyn DocumentStore>;

/// Mutable document pointer fields applied by
/// [`DocumentStore::update_document_pointers`].
pub struct DocumentPointersUpdate<'a> {
    pub document_id: Uuid,
    pub document_state: &'a str,
    pub active_revision_id: Option<Uuid>,
    pub readable_revision_id: Option<Uuid>,
    pub latest_revision_no: Option<i64>,
    pub title: Option<&'a str>,
    pub deleted_at: Option<DateTime<Utc>>,
}

/// Revision readiness transitions applied by
/// [`DocumentStore::update_revision_readiness`].
pub struct RevisionReadinessUpdate<'a> {
    pub revision_id: Uuid,
    pub text_state: &'a str,
    pub vector_state: &'a str,
    pub graph_state: &'a str,
    pub text_readable_at: Option<DateTime<Utc>>,
    pub vector_ready_at: Option<DateTime<Utc>>,
    pub graph_ready_at: Option<DateTime<Utc>>,
    pub superseded_by_revision_id: Option<Uuid>,
}

/// Document revision tree, structured prep, chunks, and technical facts.
///
/// Owns the `knowledge_document` / `knowledge_revision` /
/// `knowledge_structured_revision` / `knowledge_structured_block` /
/// `knowledge_chunk` / `knowledge_technical_fact` collections.
#[async_trait]
pub trait DocumentStore: Send + Sync {
    // --- documents ---
    async fn upsert_document(
        &self,
        row: &KnowledgeDocumentRow,
    ) -> anyhow::Result<KnowledgeDocumentRow>;
    async fn get_document(&self, document_id: Uuid)
    -> anyhow::Result<Option<KnowledgeDocumentRow>>;
    async fn get_document_by_external_key(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
        external_key: &str,
    ) -> anyhow::Result<Option<KnowledgeDocumentRow>>;
    async fn list_documents_by_library(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
        include_deleted: bool,
    ) -> anyhow::Result<Vec<KnowledgeDocumentRow>>;
    async fn list_documents_by_ids(
        &self,
        document_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeDocumentRow>>;
    /// Repairs at most `limit` exact-scoped document projection names from
    /// authoritative content identity. Existing non-null names are preserved.
    async fn repair_missing_document_file_names(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
        limit: u32,
    ) -> anyhow::Result<u64>;
    async fn update_document_pointers(
        &self,
        update: DocumentPointersUpdate<'_>,
    ) -> anyhow::Result<Option<KnowledgeDocumentRow>>;

    // --- revisions ---
    async fn upsert_revision(
        &self,
        row: &KnowledgeRevisionRow,
    ) -> anyhow::Result<KnowledgeRevisionRow>;
    /// Attempt-fenced repair of missing knowledge projections. Existing
    /// lifecycle/head/readiness data is never updated; the document repair may
    /// fill a missing display name from exact-scoped authoritative content
    /// identity without overwriting an existing name.
    async fn ensure_ingest_revision_projection_fenced(
        &self,
        authority: IngestRevisionWriteAuthority,
        document: &KnowledgeDocumentRow,
        revision: &KnowledgeRevisionRow,
    ) -> anyhow::Result<IngestAttemptMutationOutcome<KnowledgeRevisionRow>>;
    async fn update_revision_document_hint(
        &self,
        revision_id: Uuid,
        document_hint: Option<&str>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>>;
    async fn get_revision(&self, revision_id: Uuid)
    -> anyhow::Result<Option<KnowledgeRevisionRow>>;
    async fn list_revisions_by_ids(
        &self,
        revision_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeRevisionRow>>;
    async fn list_revisions_by_document(
        &self,
        document_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRevisionRow>>;
    async fn aggregate_library_generation_signals(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<LibraryGenerationSignals>;
    /// Documents whose vector index is missing: a revision marked vector-ready
    /// with chunks but no chunk vectors. Both the dashboard notice and the
    /// documents-list filter read this one list, so the count an operator sees
    /// and the set they are sent to are the same fact.
    async fn list_documents_missing_chunk_vectors(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<Uuid>>;
    async fn update_revision_readiness(
        &self,
        update: RevisionReadinessUpdate<'_>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>>;
    async fn update_revision_text_content(
        &self,
        revision_id: Uuid,
        normalized_text: Option<&str>,
        text_checksum: Option<&str>,
        text_state: &str,
        text_readable_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>>;
    /// Atomically applies the explicit extract-artifact replacement semantics
    /// only while the supplied attempt remains the latest leased owner.
    async fn update_revision_extract_artifact_fenced(
        &self,
        authority: IngestRevisionWriteAuthority,
        text_state: &str,
        artifact: IngestRevisionExtractArtifact<'_>,
    ) -> anyhow::Result<IngestAttemptMutationOutcome<KnowledgeRevisionRow>>;
    async fn update_revision_image_checksum(
        &self,
        revision_id: Uuid,
        image_checksum: Option<&str>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>>;
    async fn update_revision_storage_ref(
        &self,
        revision_id: Uuid,
        storage_ref: Option<&str>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>>;

    // --- chunks ---
    async fn upsert_chunk(&self, row: &KnowledgeChunkRow) -> anyhow::Result<KnowledgeChunkRow>;
    async fn insert_chunks(
        &self,
        rows: &[KnowledgeChunkRow],
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>>;
    /// Read one keyset page of ready, primary chunks on active documents'
    /// readable revisions. Maintenance rebuilds must page through this
    /// inventory instead of applying a silent global row cap.
    async fn list_active_chunks_by_library_page(
        &self,
        library_id: Uuid,
        after: Option<(i32, Uuid)>,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>>;
    /// Count ready, primary chunks on active documents' readable revisions.
    /// This is the expected inventory for a complete library vector profile.
    async fn count_active_chunks_by_library(&self, library_id: Uuid) -> anyhow::Result<u64>;
    /// Source-profile chunks for the given revisions, ordered by the **input
    /// rank** of `revision_ids` then `chunk_index` (`unnest(...) WITH
    /// ORDINALITY`). The adapter MUST preserve input-rank
    /// ordering, not storage order.
    async fn list_source_profile_chunks_by_revisions(
        &self,
        library_id: Uuid,
        revision_ids: &[Uuid],
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>>;
    async fn list_chunks_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>>;
    async fn list_head_chunks_by_revision(
        &self,
        revision_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>>;
    async fn count_chunks_by_revision(&self, revision_id: Uuid) -> anyhow::Result<i64>;
    async fn list_chunks_by_revision_matching_terms(
        &self,
        revision_id: Uuid,
        terms: &[String],
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>>;
    async fn list_chunks_by_revisions_matching_terms(
        &self,
        revision_ids: &[Uuid],
        terms: &[String],
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>>;
    async fn list_chunks_by_revision_range(
        &self,
        revision_id: Uuid,
        min_chunk_index: i32,
        max_chunk_index: i32,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>>;
    async fn list_chunks_by_revision_windows(
        &self,
        revision_id: Uuid,
        windows: &[(i32, i32)],
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>>;
    async fn list_chunks_by_revisions_windows(
        &self,
        windows: &[(Uuid, i32, i32)],
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>>;
    async fn list_tail_chunks_by_revision(
        &self,
        revision_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>>;
    async fn list_head_source_unit_blocks_by_revision(
        &self,
        revision_id: Uuid,
        limit: usize,
        temporal_start: Option<DateTime<Utc>>,
        temporal_end: Option<DateTime<Utc>>,
        release_marker_required: bool,
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockRow>>;
    async fn list_tail_source_unit_blocks_by_revision(
        &self,
        revision_id: Uuid,
        limit: usize,
        temporal_start: Option<DateTime<Utc>>,
        temporal_end: Option<DateTime<Utc>>,
        release_marker_required: bool,
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockRow>>;
    async fn get_chunk(&self, chunk_id: Uuid) -> anyhow::Result<Option<KnowledgeChunkRow>>;
    /// Membership read ordered by `chunk_id ASC`.
    async fn list_chunks_by_ids(
        &self,
        chunk_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>>;
    /// Searches only the bounded candidate set selected by primary retrieval.
    /// An empty candidate set MUST return no rows and MUST NOT fall back to a
    /// library-wide scan.
    async fn search_code_pattern_chunks_by_terms(
        &self,
        library_id: Uuid,
        candidate_document_ids: &[Uuid],
        terms: &[String],
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>>;
    /// Searches only the bounded candidate set selected by primary retrieval.
    /// An empty candidate set MUST return no rows and MUST NOT fall back to a
    /// library-wide scan.
    async fn search_transport_pattern_chunks_by_terms(
        &self,
        library_id: Uuid,
        candidate_document_ids: &[Uuid],
        terms: &[String],
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>>;
    async fn delete_chunks_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>>;

    // --- structured revisions / blocks ---
    /// Unconditional operator/test primitive that atomically replaces the
    /// structured metadata and complete block set for one revision.
    /// Implementations must reject metadata or blocks outside the authoritative
    /// revision scope and must not expose a mixed artifact. Production ingest
    /// must use [`Self::replace_structured_revision_artifact_fenced`].
    async fn replace_structured_revision_artifact(
        &self,
        row: &KnowledgeStructuredRevisionRow,
        blocks: &[KnowledgeStructuredBlockRow],
    ) -> anyhow::Result<KnowledgeStructuredRevisionRow>;
    /// Ingest-owned variant of [`Self::replace_structured_revision_artifact`].
    /// Metadata and blocks share the same attempt-fenced transaction.
    async fn replace_structured_revision_artifact_fenced(
        &self,
        authority: IngestRevisionWriteAuthority,
        row: &KnowledgeStructuredRevisionRow,
        blocks: &[KnowledgeStructuredBlockRow],
    ) -> anyhow::Result<IngestAttemptMutationOutcome<KnowledgeStructuredRevisionRow>>;
    /// Promotes a fully persisted `preparing` artifact to `prepared` only
    /// after verifying the exact extracted-text checksum and the actual block,
    /// dual-chunk, and fact counts in the same authority transaction.
    async fn finalize_structured_revision_artifact_fenced(
        &self,
        authority: IngestRevisionWriteAuthority,
        expected_text_checksum: &str,
    ) -> anyhow::Result<IngestAttemptMutationOutcome<KnowledgeStructuredRevisionRow>>;
    async fn get_structured_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeStructuredRevisionRow>>;
    async fn get_structured_revision_counts(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Option<StructuredRevisionCounts>>;
    async fn list_structured_revisions_by_revision_ids(
        &self,
        revision_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeStructuredRevisionRow>>;
    async fn list_structured_revisions_by_document(
        &self,
        document_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeStructuredRevisionRow>>;
    async fn list_structured_blocks_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockRow>>;
    async fn list_setup_structured_blocks_by_revision(
        &self,
        revision_id: Uuid,
        sample_limit: usize,
        structured_limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockRow>>;
    async fn list_structured_blocks_page_by_revision(
        &self,
        revision_id: Uuid,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<(Vec<KnowledgeStructuredBlockRow>, usize)>;
    async fn list_chunk_support_references_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeChunkSupportReferenceRow>>;
    async fn list_structured_blocks_by_ids(
        &self,
        block_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockRow>>;
    async fn delete_structured_blocks_by_revision(&self, revision_id: Uuid) -> anyhow::Result<()>;

    // --- technical facts ---
    /// Unconditional operator/test primitive. Production ingest must use
    /// [`Self::replace_technical_facts_fenced`].
    async fn replace_technical_facts(
        &self,
        revision_id: Uuid,
        rows: &[KnowledgeTechnicalFactRow],
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>>;
    /// Ingest-owned transactional technical-fact replacement.
    async fn replace_technical_facts_fenced(
        &self,
        authority: IngestRevisionWriteAuthority,
        rows: &[KnowledgeTechnicalFactRow],
    ) -> anyhow::Result<IngestAttemptMutationOutcome<Vec<KnowledgeTechnicalFactRow>>>;
    async fn list_technical_facts_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>>;
    async fn count_technical_facts_by_revision(&self, revision_id: Uuid) -> anyhow::Result<i64>;
    async fn list_technical_facts_by_ids(
        &self,
        fact_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>>;
    async fn list_technical_facts_by_chunk_ids(
        &self,
        chunk_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>>;
    async fn list_technical_facts_by_document(
        &self,
        document_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>>;
    async fn delete_technical_facts_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>>;
}
