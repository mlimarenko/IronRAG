use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::infra::knowledge_rows::{
    KnowledgeChunkSearchRow, KnowledgeChunkVectorRow, KnowledgeChunkVectorSearchRow,
    KnowledgeEntitySearchRow, KnowledgeEntityVectorRow, KnowledgeEntityVectorSearchRow,
    KnowledgeRelationSearchRow, KnowledgeStructuredBlockSearchRow, KnowledgeTechnicalFactSearchRow,
};

use super::write_authority::{
    ActiveEmbeddingProfileSource, ChunkVectorProfileInventory, IngestAttemptMutationOutcome,
    IngestVectorWriteFence, LexicalQueryOrigin, VectorPlaneDeleteOutcome, VectorWriteFence,
};

/// Shared handle for a [`SearchStore`] adapter.
pub type SearchStoreRef = Arc<dyn SearchStore>;

/// Scope and optional caller-supplied Postgres text-search config for one
/// chunk lexical query. `text_search_config: None` selects the cached default
/// analyzer SQL; `Some(config)` renders the lane SQL for that config.
pub struct ChunkLexicalQuery<'a> {
    pub library_id: Uuid,
    pub query: &'a str,
    pub query_origin: LexicalQueryOrigin,
    pub limit: usize,
    pub temporal_start: Option<DateTime<Utc>>,
    pub temporal_end: Option<DateTime<Utc>>,
    pub text_search_config: Option<&'a str>,
}

/// Scope and vector shared by the chunk, document-diversity and entity lookups.
pub struct VectorSimilaritySearchInput<'a> {
    pub dim: u64,
    pub library_id: Uuid,
    pub embedding_model_key: &'a str,
    pub query_vector: &'a [f32],
    pub n_probe: Option<u64>,
}

/// Fenced promotion from one staged embedding profile to its active profile.
pub struct StagedVectorRebuildPromotion<'a> {
    pub library_id: Uuid,
    pub dimensions: u64,
    pub active_embedding_profile_key: &'a str,
    pub staging_embedding_model_key: &'a str,
    pub expected_source_truth_version: i64,
    pub chunk_count: Option<u64>,
    pub entity_count: Option<u64>,
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
    /// repair tooling. Product graph writes must use the fenced bulk method
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
    /// Atomically validates one authoritative source/profile fence, writes the
    /// manifest and vectors, and reconciles the manifest count. The database
    /// lock is acquired only for this short transaction, never for provider
    /// I/O. Returns the source version visible after the commit.
    async fn upsert_chunk_vectors_bulk_fenced(
        &self,
        rows: &[KnowledgeChunkVectorRow],
        fence: &VectorWriteFence,
    ) -> anyhow::Result<i64>;
    /// Revision-scoped ingest variant of
    /// [`Self::upsert_chunk_vectors_bulk_fenced`]. It validates the fence's
    /// exact embedding profile against the current library binding while the
    /// library row is locked, but permits unrelated source-generation advances.
    async fn upsert_ingest_chunk_vectors_bulk_fenced(
        &self,
        rows: &[KnowledgeChunkVectorRow],
        fence: &VectorWriteFence,
        active_profile_source: &dyn ActiveEmbeddingProfileSource,
    ) -> anyhow::Result<i64>;
    /// Prepare the exact grounded-answer lane once before a chunk-vector rebuild.
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
        ingest_attempt: IngestVectorWriteFence,
    ) -> anyhow::Result<Option<VectorPlaneDeleteOutcome>>;
    /// Revision-wide ingest cleanup. `AuthorityLost` preserves every vector
    /// committed or adopted by the replacement attempt.
    async fn delete_attempt_owned_chunk_vectors_by_revision_fenced(
        &self,
        library_id: Uuid,
        expected_source_truth_version: i64,
        ingest_attempt: IngestVectorWriteFence,
    ) -> anyhow::Result<IngestAttemptMutationOutcome<VectorPlaneDeleteOutcome>>;
    /// Revision-wide destructive operation for explicit readiness/content
    /// mutations. Delete, manifest reconciliation, and source-fence advance
    /// commit atomically behind the exclusive per-library data lock.
    async fn delete_chunk_vectors_by_revision_fenced(
        &self,
        library_id: Uuid,
        revision_id: Uuid,
        expected_source_truth_version: i64,
    ) -> anyhow::Result<VectorPlaneDeleteOutcome>;
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
    /// Inspect visible active chunk vectors for an exact embedding
    /// execution profile in one adapter call. Dimensions are ordered by the
    /// implementation's strongest evidence first.
    async fn inspect_chunk_vector_profile(
        &self,
        library_id: Uuid,
        embedding_model_key: &str,
        vector_kind: &str,
    ) -> anyhow::Result<ChunkVectorProfileInventory>;
    /// Unconditional fixture/repair write. Product graph writes use
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
        fence: &VectorWriteFence,
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
    /// library only if its source fence still matches and its corresponding active
    /// chunk inventory is empty. No dimension is required or invented.
    async fn purge_empty_library_vector_plane(
        &self,
        library_id: Uuid,
        expected_source_truth_version: i64,
    ) -> anyhow::Result<u64>;
    /// Atomically replaces the selected active vector lanes with rows that
    /// were built under an opaque staging profile. Implementations must verify
    /// the exact staged row counts, switch chunk/entity lanes in one
    /// transaction, and advance the library source fence in that transaction.
    async fn promote_staged_vector_rebuild(
        &self,
        promotion: StagedVectorRebuildPromotion<'_>,
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
    /// scan must be bounded and active profiles must remain untouched.
    async fn discard_abandoned_staged_vector_rebuilds(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<u64>;
    async fn list_entity_vectors_by_entity(
        &self,
        entity_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityVectorRow>>;
    async fn search_chunks(
        &self,
        query: ChunkLexicalQuery<'_>,
    ) -> anyhow::Result<Vec<KnowledgeChunkSearchRow>>;
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
        input: VectorSimilaritySearchInput<'_>,
        limit: usize,
        temporal_start: Option<DateTime<Utc>>,
        temporal_end: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<KnowledgeChunkVectorSearchRow>>;
    /// Returns at most one best vector candidate per document.
    ///
    /// This is intentionally separate from chunk-level similarity search:
    /// document retrieval must not let many near-identical chunks from one
    /// document exhaust the bounded ANN window and hide every other document.
    async fn search_chunk_vectors_by_document_similarity(
        &self,
        input: VectorSimilaritySearchInput<'_>,
        document_limit: usize,
        temporal_start: Option<DateTime<Utc>>,
        temporal_end: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<KnowledgeChunkVectorSearchRow>>;
    async fn search_entity_vectors_by_similarity(
        &self,
        input: VectorSimilaritySearchInput<'_>,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeEntityVectorSearchRow>>;
}
