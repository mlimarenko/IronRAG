//! Knowledge-plane storage ports (trait surfaces).
//!
//! These traits are the boundary between query/ingest services and the concrete
//! `PostgreSQL` knowledge-plane adapter. The traits stay as domain ports so the
//! services do not depend on table layout details, but runtime backend selection
//! has been removed: `PostgreSQL` is the single implementation.
//!
//! ## Surface split
//!
//! Storage responsibilities are split by domain: [`DocumentStore`],
//! [`SearchStore`], [`GraphStore`], and [`ContextStore`]. Infrastructure
//! construction details stay outside these traits; callers receive trait objects
//! and use only domain-level operations.
//!
//! ## Leaky contracts (design §14.4)
//!
//! Three observable behaviors are part of the contract and are pinned in the
//! relevant method doc-comments below:
//!
//! 1. **Input-rank ordering.** Membership reads that take an ordered id slice and
//!    rank their output by the *input* position (`unnest(...) WITH ORDINALITY`)
//!    must return rows ordered by the rank of
//!    the input id, not by storage order. See
//!    [`DocumentStore::list_source_profile_chunks_by_revisions`].
//! 2. **Write-count returns.** Methods returning a mutation count return the
//!    number of rows actually written/removed (`cmd_tuples`/`RETURNING`), not a request count. See the `delete_*`/`u64`
//!    methods on [`SearchStore`], [`GraphStore`], and [`ContextStore`].
//! 3. **Vector write-routing is hidden.** Callers never name a physical
//!    dimension relation. [`SearchStore::upsert_chunk_vectors_bulk`] (and its
//!    singular sibling) own routing into a shared typed-by-dimension table;
//!    library, profile, and vector kind identify logical lanes inside it.
//!
//! ## Canonical edge direction
//!
//! `evidence_source_edge` is written EVIDENCE→REVISION by
//! [`GraphStore::upsert_evidence_source_edge`]. The canonical direction is
//! **EVIDENCE→REVISION** and the `PostgreSQL` adapter enforces it with an FK.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::infra::knowledge_rows::{
    GraphViewData, GraphViewEdgeWrite, GraphViewNodeWrite, GraphViewWriteError,
    KnowledgeBundleChunkEdgeRow, KnowledgeBundleEntityEdgeRow, KnowledgeBundleEvidenceEdgeRow,
    KnowledgeBundleRelationEdgeRow, KnowledgeChunkRow, KnowledgeChunkSearchRow,
    KnowledgeChunkSupportReferenceRow, KnowledgeChunkVectorRow, KnowledgeChunkVectorSearchRow,
    KnowledgeContextBundleReferenceSetRow, KnowledgeContextBundleRow, KnowledgeDocumentRow,
    KnowledgeEntityCandidateRow, KnowledgeEntityRow, KnowledgeEntitySearchRow,
    KnowledgeEntityVectorRow, KnowledgeEntityVectorSearchRow, KnowledgeEvidenceRow,
    KnowledgeGraphTraversalRow, KnowledgeRelationCandidateRow, KnowledgeRelationEvidenceLookupRow,
    KnowledgeRelationRow, KnowledgeRelationSearchRow, KnowledgeRelationTopologyRow,
    KnowledgeRetrievalTraceRow, KnowledgeRevisionRow, KnowledgeStructuredBlockRow,
    KnowledgeStructuredBlockSearchRow, KnowledgeStructuredRevisionRow, KnowledgeTechnicalFactRow,
    KnowledgeTechnicalFactSearchRow, LibraryGenerationSignals, NewKnowledgeEntity,
    NewKnowledgeEntityCandidate, NewKnowledgeEvidence, NewKnowledgeRelation,
    NewKnowledgeRelationCandidate, StructuredRevisionCounts,
};

/// Formal identifier protocol for an unpromoted vector rebuild lane. The
/// suffix is a SHA-256 digest, not an operator/model alias.
pub const VECTOR_REBUILD_STAGING_PROFILE_PREFIX: &str = "embedding-rebuild:v1:";
pub const VECTOR_REBUILD_STAGING_PROFILE_DIGEST_LEN: usize = 64;
/// Cross-process serializer namespace for canonical vector-plane writes.
///
/// Ordinary writes take a transaction-scoped shared lock; destructive purge
/// and promotion take the exclusive form of the same per-library lock.
pub const VECTOR_PLANE_DATA_ADVISORY_LOCK_PREFIX: &str = "query.vector_plane.data.library";

/// Source/profile fence for a canonical vector write.
///
/// AI binding, account, model, and provider changes advance the library source
/// generation in the same database transaction. Capturing this version before
/// resolving the embedding binding therefore lets the storage transaction
/// reject both source drift and profile drift without reconstructing provider
/// configuration inside the storage adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalVectorWriteFence {
    pub expected_source_truth_version: i64,
    pub embedding_profile_key: String,
    /// Ordinary ingest writes are additionally authorized by the latest
    /// leased attempt for the exact revision. Direct operator writes do not
    /// have an ingest lifecycle and leave this unset.
    pub ingest_attempt: Option<CanonicalIngestVectorWriteFence>,
    /// Direct operator/API writes advance the cross-replica cache fence.
    /// Ingest batches preserve it because one ingest attempt writes several
    /// batches under the same pre-provider source/profile snapshot.
    pub advance_source_truth_version: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalIngestVectorWriteFence {
    pub attempt_id: Uuid,
    pub revision_id: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VectorPlaneDeleteOutcome {
    pub deleted: u64,
    pub source_truth_version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkVectorProfileInventory {
    pub dimensions: Vec<u64>,
    pub active_vector_count: u64,
}

/// Shared handle for a [`DocumentStore`] adapter.
pub type DocumentStoreRef = Arc<dyn DocumentStore>;
/// Shared handle for a [`SearchStore`] adapter.
pub type SearchStoreRef = Arc<dyn SearchStore>;
/// Shared handle for a [`GraphStore`] adapter.
pub type GraphStoreRef = Arc<dyn GraphStore>;
/// Shared handle for a [`ContextStore`] adapter.
pub type ContextStoreRef = Arc<dyn ContextStore>;

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
    async fn update_document_pointers(
        &self,
        document_id: Uuid,
        document_state: &str,
        active_revision_id: Option<Uuid>,
        readable_revision_id: Option<Uuid>,
        latest_revision_no: Option<i64>,
        title: Option<&str>,
        deleted_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Option<KnowledgeDocumentRow>>;

    // --- revisions ---
    async fn upsert_revision(
        &self,
        row: &KnowledgeRevisionRow,
    ) -> anyhow::Result<KnowledgeRevisionRow>;
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
    async fn count_vector_ready_revisions_missing_chunk_vectors(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<i64>;
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
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>>;
    async fn update_revision_text_content(
        &self,
        revision_id: Uuid,
        normalized_text: Option<&str>,
        text_checksum: Option<&str>,
        text_state: &str,
        text_readable_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>>;
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
    // Canonical chunk read/count/search methods MUST exclude rows with a
    // non-null `raptor_level`. The retired summary format has no revision-safe
    // leaf lineage and is not valid answer, preview, MCP, or embedding input.
    // Write/delete methods still accept those rows so maintenance can remove
    // legacy data without a separate unsafe read surface.
    async fn upsert_chunk(&self, row: &KnowledgeChunkRow) -> anyhow::Result<KnowledgeChunkRow>;
    async fn insert_chunks(
        &self,
        rows: &[KnowledgeChunkRow],
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>>;
    /// Read one keyset page of ready, canonical chunks on active documents'
    /// readable revisions. Maintenance rebuilds must page through this
    /// inventory instead of applying a silent global row cap.
    async fn list_active_chunks_by_library_page(
        &self,
        library_id: Uuid,
        after: Option<(i32, Uuid)>,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>>;
    /// Count ready, canonical chunks on active documents' readable revisions.
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
    async fn upsert_structured_revision(
        &self,
        row: &KnowledgeStructuredRevisionRow,
    ) -> anyhow::Result<KnowledgeStructuredRevisionRow>;
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
    async fn replace_structured_blocks(
        &self,
        revision_id: Uuid,
        rows: &[KnowledgeStructuredBlockRow],
    ) -> anyhow::Result<()>;
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
    async fn replace_technical_facts(
        &self,
        revision_id: Uuid,
        rows: &[KnowledgeTechnicalFactRow],
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>>;
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

/// Lexical (FTS/trigram) search lanes and the vector ANN lane.
///
/// Owns the `knowledge_search_view` reads and shared per-dimension chunk/entity
/// vector relations. Libraries and embedding profiles are filtered logical
/// lanes, not physically isolated shards.
#[async_trait]
pub trait SearchStore: Send + Sync {
    async fn ensure_chunk_vector_shard(&self, dim: u64) -> anyhow::Result<()>;
    async fn ensure_chunk_vector_lane_for_library(
        &self,
        dim: u64,
        library_id: Uuid,
    ) -> anyhow::Result<()>;
    async fn ensure_entity_vector_shard(&self, dim: u64) -> anyhow::Result<()>;
    /// Unconditional adapter-level upsert used by storage fixtures and scoped
    /// repair tooling. Product canonical writes must use the fenced bulk method
    /// below. Vector write-routing remains hidden (leaky contract §14.4(c)):
    /// callers never name a relation/table.
    async fn upsert_chunk_vector(
        &self,
        row: &KnowledgeChunkVectorRow,
    ) -> anyhow::Result<KnowledgeChunkVectorRow>;
    /// Unconditional bulk variant of [`SearchStore::upsert_chunk_vector`]; same
    /// fixture/repair boundary and hidden-routing contract (§14.4(c)).
    async fn upsert_chunk_vectors_bulk(
        &self,
        rows: &[KnowledgeChunkVectorRow],
    ) -> anyhow::Result<()>;
    /// Atomically validates one canonical source/profile fence, writes the
    /// manifest and vectors, and reconciles the manifest count. The database
    /// lock is acquired only for this short transaction, never for provider
    /// I/O. Returns the source version visible after the commit.
    async fn upsert_chunk_vectors_bulk_fenced(
        &self,
        rows: &[KnowledgeChunkVectorRow],
        fence: &CanonicalVectorWriteFence,
    ) -> anyhow::Result<i64>;
    /// Prepare the exact canonical lane once before a chunk-vector rebuild.
    /// This is deliberately separate from deferred bulk writes so relation /
    /// index setup (including any sizing scan) cannot repeat per provider
    /// batch.
    async fn prepare_chunk_vector_rebuild_lane(
        &self,
        library_id: Uuid,
        dimensions: u64,
        embedding_model_key: &str,
    ) -> anyhow::Result<()>;
    /// Rebuild-only bulk write into a lane prepared by
    /// [`Self::prepare_chunk_vector_rebuild_lane`]. The adapter deliberately
    /// defers its expensive exact `COUNT(*)`. The caller must invoke
    /// [`Self::reconcile_chunk_vector_manifest_count`] exactly once on both
    /// success and partial failure before releasing its rebuild fence.
    async fn upsert_chunk_vectors_bulk_deferred_manifest(
        &self,
        rows: &[KnowledgeChunkVectorRow],
    ) -> anyhow::Result<()>;
    async fn reconcile_chunk_vector_manifest_count(
        &self,
        library_id: Uuid,
        dimensions: u64,
        embedding_model_key: &str,
    ) -> anyhow::Result<()>;
    async fn delete_chunk_vector(
        &self,
        chunk_id: Uuid,
        embedding_model_key: &str,
        freshness_generation: i64,
    ) -> anyhow::Result<Option<KnowledgeChunkVectorRow>>;
    /// Low-level fixture/repair primitive. Production revision cleanup must use
    /// [`Self::delete_chunk_vectors_by_revision_fenced`].
    /// Returns the number of vector rows removed.
    async fn delete_chunk_vectors_by_revision(&self, revision_id: Uuid) -> anyhow::Result<u64>;
    /// Low-level exact-identity fixture/repair primitive. Ordinary ingest
    /// failure cleanup must also prove that its attempt still owns the lease
    /// through [`Self::delete_attempt_owned_chunk_vectors_by_ids_fenced`].
    async fn delete_chunk_vectors_by_ids_fenced(
        &self,
        library_id: Uuid,
        vector_ids: &[Uuid],
        expected_source_truth_version: i64,
    ) -> anyhow::Result<VectorPlaneDeleteOutcome>;
    /// Delete vector rows created by a failed ingest attempt only while that
    /// exact attempt remains the latest leased authority for the revision.
    /// `None` means authority moved to a retry, so every listed row was
    /// deliberately preserved.
    async fn delete_attempt_owned_chunk_vectors_by_ids_fenced(
        &self,
        library_id: Uuid,
        vector_ids: &[Uuid],
        expected_source_truth_version: i64,
        ingest_attempt: CanonicalIngestVectorWriteFence,
    ) -> anyhow::Result<Option<VectorPlaneDeleteOutcome>>;
    /// Revision-wide destructive operation for explicit readiness/content
    /// mutations. Delete, manifest reconciliation, and source-fence advance
    /// commit atomically behind the exclusive per-library data lock.
    async fn delete_chunk_vectors_by_revision_fenced(
        &self,
        library_id: Uuid,
        revision_id: Uuid,
        expected_source_truth_version: i64,
    ) -> anyhow::Result<VectorPlaneDeleteOutcome>;
    /// Returns the number of vector rows removed (§14.4(b)).
    async fn delete_chunk_vectors_by_library(&self, library_id: Uuid) -> anyhow::Result<u64>;
    /// Delete this library's chunk and entity vector rows from every physical
    /// vector relation/shard whose manifest or collection dimension is not
    /// `keep_dim`. Returns the number of vector rows removed (§14.4(b)).
    async fn delete_library_vectors_except_dim(
        &self,
        library_id: Uuid,
        keep_dim: u64,
    ) -> anyhow::Result<u64>;
    /// Returns the number of vector rows removed (§14.4(b)).
    async fn delete_all_chunk_vectors(&self) -> anyhow::Result<u64>;
    async fn list_chunk_vectors_by_chunk(
        &self,
        chunk_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeChunkVectorRow>>;
    async fn list_chunk_vectors_by_chunks(
        &self,
        chunk_ids: &[Uuid],
        embedding_model_key: &str,
        vector_kind: &str,
    ) -> anyhow::Result<Vec<KnowledgeChunkVectorRow>>;
    async fn count_chunk_vectors_by_revision(
        &self,
        revision_id: Uuid,
        embedding_model_key: &str,
        vector_kind: &str,
        freshness_generation: i64,
    ) -> anyhow::Result<usize>;
    /// Read the durable dimension claim for one exact execution profile from
    /// vector-relation metadata only. The claim remains observable when its
    /// row count is zero; implementations must not infer it by scanning vector
    /// rows or by falling back to another profile/model identifier.
    async fn read_vector_profile_dimension_claim(
        &self,
        library_id: Uuid,
        embedding_model_key: &str,
        vector_kind: &str,
    ) -> anyhow::Result<Option<u64>>;
    /// Inspect visible canonical chunk vectors for an exact embedding
    /// execution profile in one adapter call. Dimensions are ordered by the
    /// implementation's strongest evidence first.
    async fn inspect_chunk_vector_profile(
        &self,
        library_id: Uuid,
        embedding_model_key: &str,
        vector_kind: &str,
    ) -> anyhow::Result<ChunkVectorProfileInventory>;
    /// Unconditional fixture/repair write. Product canonical writes use
    /// [`Self::upsert_entity_vectors_bulk_fenced`].
    async fn upsert_entity_vector(
        &self,
        row: &KnowledgeEntityVectorRow,
    ) -> anyhow::Result<KnowledgeEntityVectorRow>;
    /// Entity-vector counterpart of
    /// [`Self::upsert_chunk_vectors_bulk_fenced`].
    async fn upsert_entity_vectors_bulk_fenced(
        &self,
        rows: &[KnowledgeEntityVectorRow],
        fence: &CanonicalVectorWriteFence,
    ) -> anyhow::Result<i64>;
    /// Entity-lane counterpart of
    /// [`Self::prepare_chunk_vector_rebuild_lane`].
    async fn prepare_entity_vector_rebuild_lane(
        &self,
        library_id: Uuid,
        dimensions: u64,
        embedding_model_key: &str,
    ) -> anyhow::Result<()>;
    /// Entity-lane counterpart of
    /// [`Self::upsert_chunk_vectors_bulk_deferred_manifest`].
    async fn upsert_entity_vectors_bulk_deferred_manifest(
        &self,
        rows: &[KnowledgeEntityVectorRow],
    ) -> anyhow::Result<()>;
    async fn reconcile_entity_vector_manifest_count(
        &self,
        library_id: Uuid,
        dimensions: u64,
        embedding_model_key: &str,
    ) -> anyhow::Result<()>;
    /// Atomically purge every chunk/entity vector row and manifest for a
    /// library only if its source fence still matches and its canonical active
    /// chunk inventory is empty. No dimension is required or invented.
    async fn purge_empty_library_vector_plane(
        &self,
        library_id: Uuid,
        expected_source_truth_version: i64,
    ) -> anyhow::Result<u64>;
    /// Atomically replaces the selected canonical vector lanes with rows that
    /// were built under an opaque staging profile. Implementations must verify
    /// the exact staged row counts, switch chunk/entity lanes in one
    /// transaction, and advance the library source fence in that transaction.
    async fn promote_staged_vector_rebuild(
        &self,
        library_id: Uuid,
        dimensions: u64,
        canonical_embedding_model_key: &str,
        staging_embedding_model_key: &str,
        expected_source_truth_version: i64,
        expected_chunk_count: Option<u64>,
        expected_entity_count: Option<u64>,
    ) -> anyhow::Result<()>;
    /// Removes only rows/manifests owned by an unpromoted staging profile.
    async fn discard_staged_vector_rebuild(
        &self,
        library_id: Uuid,
        dimensions: u64,
        staging_embedding_model_key: &str,
    ) -> anyhow::Result<()>;
    /// Recovers durable staging residue left by a cancelled process. Only
    /// validated, unpromoted rebuild-profile manifests may be removed; the
    /// scan must be bounded and canonical profiles must remain untouched.
    async fn discard_abandoned_staged_vector_rebuilds(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<u64>;
    async fn delete_entity_vector(
        &self,
        entity_id: Uuid,
        embedding_model_key: &str,
        freshness_generation: i64,
    ) -> anyhow::Result<Option<KnowledgeEntityVectorRow>>;
    /// Low-level fixture/repair primitive. Production library cleanup must use
    /// [`Self::delete_entity_vectors_by_library_fenced`].
    async fn delete_entity_vectors_by_library(&self, library_id: Uuid) -> anyhow::Result<()>;
    async fn delete_entity_vectors_by_library_fenced(
        &self,
        library_id: Uuid,
        expected_source_truth_version: i64,
    ) -> anyhow::Result<VectorPlaneDeleteOutcome>;
    /// Returns the number of vector rows removed (§14.4(b)).
    async fn delete_all_entity_vectors(&self) -> anyhow::Result<u64>;
    async fn list_entity_vectors_by_entity(
        &self,
        entity_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityVectorRow>>;
    async fn search_chunks(
        &self,
        library_id: Uuid,
        query: &str,
        limit: usize,
        temporal_start: Option<DateTime<Utc>>,
        temporal_end: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<KnowledgeChunkSearchRow>>;
    /// Config-aware variant of [`Self::search_chunks`] that allows the caller to
    /// supply the Postgres FTS text-search config name sourced from the
    /// library's [`RetrievalConfig`].  The default implementation delegates
    /// to `search_chunks` so that test doubles satisfy the trait for free.
    /// `PgSearchStore` overrides this to use `text_search_config` in the
    /// lexical SQL instead of the hardcoded `'simple'` default.
    ///
    /// [`RetrievalConfig`]: crate::domains::retrieval::RetrievalConfig
    async fn search_chunks_with_config(
        &self,
        library_id: Uuid,
        query: &str,
        limit: usize,
        temporal_start: Option<DateTime<Utc>>,
        temporal_end: Option<DateTime<Utc>>,
        text_search_config: &str,
    ) -> anyhow::Result<Vec<KnowledgeChunkSearchRow>> {
        let _ = text_search_config; // ignored by non-Postgres backends
        self.search_chunks(library_id, query, limit, temporal_start, temporal_end).await
    }
    async fn search_structured_blocks(
        &self,
        library_id: Uuid,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockSearchRow>>;
    async fn search_technical_facts(
        &self,
        library_id: Uuid,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactSearchRow>>;
    async fn search_entities(
        &self,
        library_id: Uuid,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeEntitySearchRow>>;
    async fn search_relations(
        &self,
        library_id: Uuid,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeRelationSearchRow>>;
    async fn search_chunk_vectors_by_similarity(
        &self,
        dim: u64,
        library_id: Uuid,
        embedding_model_key: &str,
        query_vector: &[f32],
        limit: usize,
        n_probe: Option<u64>,
        temporal_start: Option<DateTime<Utc>>,
        temporal_end: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<KnowledgeChunkVectorSearchRow>>;
    async fn search_entity_vectors_by_similarity(
        &self,
        dim: u64,
        library_id: Uuid,
        embedding_model_key: &str,
        query_vector: &[f32],
        limit: usize,
        n_probe: Option<u64>,
    ) -> anyhow::Result<Vec<KnowledgeEntityVectorSearchRow>>;
}

/// Canonical materialized graph: entities, relations, evidence, their
/// candidates, the structural edges, and the depth-capped traversal reads.
#[async_trait]
pub trait GraphStore: Send + Sync {
    async fn ping(&self) -> anyhow::Result<()>;

    // --- candidates ---
    async fn upsert_entity_candidate(
        &self,
        input: &NewKnowledgeEntityCandidate,
    ) -> anyhow::Result<KnowledgeEntityCandidateRow>;
    async fn upsert_entity_candidates(
        &self,
        inputs: &[NewKnowledgeEntityCandidate],
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>>;
    async fn list_entity_candidates_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>>;
    async fn list_entity_candidates_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>>;
    async fn delete_entity_candidates_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>>;
    async fn delete_entity_candidates_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>>;
    async fn upsert_relation_candidate(
        &self,
        input: &NewKnowledgeRelationCandidate,
    ) -> anyhow::Result<KnowledgeRelationCandidateRow>;
    async fn upsert_relation_candidates(
        &self,
        inputs: &[NewKnowledgeRelationCandidate],
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>>;
    async fn list_relation_candidates_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>>;
    async fn list_relation_candidates_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>>;
    async fn delete_relation_candidates_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>>;
    async fn delete_relation_candidates_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>>;

    // --- structural edges / projection ---
    async fn upsert_document_revision_edge(
        &self,
        document_id: Uuid,
        revision_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()>;
    async fn upsert_revision_chunk_edge(
        &self,
        revision_id: Uuid,
        chunk_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()>;
    async fn insert_revision_chunk_edges(
        &self,
        revision_id: Uuid,
        chunk_ids: &[Uuid],
        library_id: Uuid,
    ) -> anyhow::Result<()>;
    /// Returns the number of edges removed (§14.4(b)).
    async fn delete_revision_chunk_edges(&self, revision_id: Uuid) -> anyhow::Result<u64>;
    async fn upsert_chunk_mentions_entity_edge(
        &self,
        chunk_id: Uuid,
        entity_id: Uuid,
        rank: Option<i32>,
        score: Option<f64>,
        inclusion_reason: Option<String>,
        library_id: Uuid,
    ) -> anyhow::Result<()>;
    async fn upsert_relation_subject_edge(
        &self,
        relation_id: Uuid,
        subject_entity_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()>;
    async fn upsert_relation_object_edge(
        &self,
        relation_id: Uuid,
        object_entity_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()>;
    /// Upserts the evidence→source structural edge. Canonical direction is
    /// **EVIDENCE→REVISION** (`_from`=evidence, `_to`=revision); the stale
    /// bootstrap EVIDENCE→CHUNK declaration does not match what is written. The
    /// PG adapter enforces EVIDENCE→REVISION with an FK. (Doc pin only — no
    /// runtime change in this refactor.)
    async fn upsert_evidence_source_edge(
        &self,
        evidence_id: Uuid,
        revision_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()>;
    async fn upsert_evidence_supports_entity_edge(
        &self,
        evidence_id: Uuid,
        entity_id: Uuid,
        rank: Option<i32>,
        score: Option<f64>,
        inclusion_reason: Option<String>,
        library_id: Uuid,
    ) -> anyhow::Result<()>;
    async fn upsert_evidence_supports_relation_edge(
        &self,
        evidence_id: Uuid,
        relation_id: Uuid,
        rank: Option<i32>,
        score: Option<f64>,
        inclusion_reason: Option<String>,
        library_id: Uuid,
    ) -> anyhow::Result<()>;
    async fn upsert_fact_supports_evidence_edge(
        &self,
        fact_id: Uuid,
        evidence_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()>;
    async fn replace_library_projection(
        &self,
        library_id: Uuid,
        projection_version: i64,
        nodes: &[GraphViewNodeWrite],
        edges: &[GraphViewEdgeWrite],
    ) -> Result<(), GraphViewWriteError>;
    async fn refresh_library_projection_targets(
        &self,
        library_id: Uuid,
        projection_version: i64,
        remove_node_ids: &[Uuid],
        remove_edge_ids: &[Uuid],
        nodes: &[GraphViewNodeWrite],
        edges: &[GraphViewEdgeWrite],
    ) -> Result<(), GraphViewWriteError>;
    async fn load_library_projection(
        &self,
        library_id: Uuid,
        projection_version: i64,
    ) -> anyhow::Result<GraphViewData>;

    // --- materialized entities / relations / evidence ---
    async fn upsert_evidence_with_edges(
        &self,
        input: &NewKnowledgeEvidence,
        source_revision_id: Option<Uuid>,
        supporting_entity_id: Option<Uuid>,
        supporting_relation_id: Option<Uuid>,
        supporting_fact_id: Option<Uuid>,
        library_id: Uuid,
    ) -> anyhow::Result<KnowledgeEvidenceRow>;
    async fn reset_library_materialized_graph(&self, library_id: Uuid) -> anyhow::Result<()>;
    async fn upsert_entity(&self, input: &NewKnowledgeEntity)
    -> anyhow::Result<KnowledgeEntityRow>;
    async fn upsert_entities(
        &self,
        inputs: &[NewKnowledgeEntity],
    ) -> anyhow::Result<Vec<KnowledgeEntityRow>>;
    async fn get_entity_by_id(&self, entity_id: Uuid)
    -> anyhow::Result<Option<KnowledgeEntityRow>>;
    async fn get_entity_by_library_and_label(
        &self,
        library_id: Uuid,
        canonical_label: &str,
    ) -> anyhow::Result<Option<KnowledgeEntityRow>>;
    async fn list_entities_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityRow>>;
    async fn upsert_relation(
        &self,
        input: &NewKnowledgeRelation,
    ) -> anyhow::Result<KnowledgeRelationRow>;
    async fn upsert_relation_with_endpoints(
        &self,
        input: &NewKnowledgeRelation,
        subject_entity_id: Option<Uuid>,
        object_entity_id: Option<Uuid>,
        library_id: Uuid,
    ) -> anyhow::Result<KnowledgeRelationRow>;
    async fn upsert_relations(
        &self,
        inputs: &[NewKnowledgeRelation],
    ) -> anyhow::Result<Vec<KnowledgeRelationRow>>;
    async fn get_relation_by_id(
        &self,
        relation_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeRelationRow>>;
    async fn get_relation_by_library_and_assertion(
        &self,
        library_id: Uuid,
        normalized_assertion: &str,
    ) -> anyhow::Result<Option<KnowledgeRelationRow>>;
    async fn list_relations_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationRow>>;
    /// Returns the number of entities removed (§14.4(b)).
    async fn delete_entities_by_canonical_keys(
        &self,
        library_id: Uuid,
        keys: &[String],
    ) -> anyhow::Result<u64>;
    /// Returns the number of relations removed (§14.4(b)).
    async fn delete_relations_by_canonical_keys(
        &self,
        library_id: Uuid,
        keys: &[String],
    ) -> anyhow::Result<u64>;
    async fn upsert_evidence(
        &self,
        input: &NewKnowledgeEvidence,
    ) -> anyhow::Result<KnowledgeEvidenceRow>;
    async fn get_evidence_by_id(
        &self,
        evidence_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeEvidenceRow>>;
    async fn list_evidence_by_ids(
        &self,
        evidence_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeEvidenceRow>>;
    async fn list_evidence_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEvidenceRow>>;
    async fn list_evidence_by_chunk(
        &self,
        chunk_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEvidenceRow>>;

    // --- traversal reads (depth-capped) ---
    async fn list_relation_topology_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationTopologyRow>>;
    async fn get_relation_topology_by_id(
        &self,
        relation_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeRelationTopologyRow>>;
    async fn list_entity_neighborhood(
        &self,
        entity_id: Uuid,
        library_id: Uuid,
        max_depth: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeGraphTraversalRow>>;
    async fn expand_relation_centric(
        &self,
        relation_id: Uuid,
        library_id: Uuid,
        max_depth: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeGraphTraversalRow>>;
    async fn list_relation_evidence_lookup(
        &self,
        relation_id: Uuid,
        library_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeRelationEvidenceLookupRow>>;
}

/// Context-bundle materialization: bundle header, four ranked child reference
/// sets, retrieval traces, and the materialized read-back join.
#[async_trait]
pub trait ContextStore: Send + Sync {
    async fn upsert_bundle(
        &self,
        row: &KnowledgeContextBundleRow,
    ) -> anyhow::Result<KnowledgeContextBundleRow>;
    async fn get_bundle(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeContextBundleRow>>;
    async fn get_bundle_by_query_execution(
        &self,
        query_execution_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeContextBundleRow>>;
    async fn list_bundles_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeContextBundleRow>>;
    async fn update_bundle_state(
        &self,
        bundle_id: Uuid,
        bundle_state: &str,
        selected_fact_ids: &[Uuid],
        verification_state: &str,
        verification_warnings: serde_json::Value,
        freshness_snapshot: serde_json::Value,
        candidate_summary: serde_json::Value,
        assembly_diagnostics: serde_json::Value,
    ) -> anyhow::Result<Option<KnowledgeContextBundleRow>>;
    async fn upsert_trace(
        &self,
        row: &KnowledgeRetrievalTraceRow,
    ) -> anyhow::Result<KnowledgeRetrievalTraceRow>;
    async fn get_trace(&self, trace_id: Uuid)
    -> anyhow::Result<Option<KnowledgeRetrievalTraceRow>>;
    async fn list_traces_by_bundle(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRetrievalTraceRow>>;
    async fn list_traces_by_query_execution(
        &self,
        query_execution_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRetrievalTraceRow>>;
    async fn update_trace_state(
        &self,
        trace_id: Uuid,
        trace_state: &str,
        diagnostics_json: serde_json::Value,
    ) -> anyhow::Result<Option<KnowledgeRetrievalTraceRow>>;
    async fn replace_bundle_chunk_edges(
        &self,
        bundle_id: Uuid,
        library_id: Uuid,
        edges: &[KnowledgeBundleChunkEdgeRow],
    ) -> anyhow::Result<Vec<KnowledgeBundleChunkEdgeRow>>;
    async fn replace_bundle_entity_edges(
        &self,
        bundle_id: Uuid,
        library_id: Uuid,
        edges: &[KnowledgeBundleEntityEdgeRow],
    ) -> anyhow::Result<Vec<KnowledgeBundleEntityEdgeRow>>;
    async fn replace_bundle_relation_edges(
        &self,
        bundle_id: Uuid,
        library_id: Uuid,
        edges: &[KnowledgeBundleRelationEdgeRow],
    ) -> anyhow::Result<Vec<KnowledgeBundleRelationEdgeRow>>;
    async fn replace_bundle_evidence_edges(
        &self,
        bundle_id: Uuid,
        library_id: Uuid,
        edges: &[KnowledgeBundleEvidenceEdgeRow],
    ) -> anyhow::Result<Vec<KnowledgeBundleEvidenceEdgeRow>>;
    async fn list_bundle_chunk_edges(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeBundleChunkEdgeRow>>;
    async fn list_bundle_entity_edges(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeBundleEntityEdgeRow>>;
    async fn list_bundle_relation_edges(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeBundleRelationEdgeRow>>;
    async fn list_bundle_evidence_edges(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeBundleEvidenceEdgeRow>>;
    /// Materialized read-back: bundle header + four ranked child reference sets
    /// in one round-trip. Child rows are ordered by `rank ASC, score DESC`. The
    /// PG adapter MUST keep this a single round-trip to hold the ≤30 s tool-call
    /// SLO.
    async fn get_bundle_reference_set(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeContextBundleReferenceSetRow>>;
    async fn get_bundle_reference_set_by_query_execution(
        &self,
        query_execution_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeContextBundleReferenceSetRow>>;
    async fn list_bundle_reference_sets_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeContextBundleReferenceSetRow>>;
    /// Returns the number of edges removed (§14.4(b)).
    async fn delete_bundle_chunk_edges(&self, bundle_id: Uuid) -> anyhow::Result<u64>;
    /// Returns the number of edges removed (§14.4(b)).
    async fn delete_bundle_entity_edges(&self, bundle_id: Uuid) -> anyhow::Result<u64>;
    /// Returns the number of edges removed (§14.4(b)).
    async fn delete_bundle_relation_edges(&self, bundle_id: Uuid) -> anyhow::Result<u64>;
    /// Returns the number of edges removed (§14.4(b)).
    async fn delete_bundle_evidence_edges(&self, bundle_id: Uuid) -> anyhow::Result<u64>;
}
