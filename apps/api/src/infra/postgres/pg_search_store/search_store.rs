use super::*;

mod chunk_vectors;
mod entity_vectors;
mod lexical_search;
mod rebuild_lifecycle;
mod similarity_search;

#[async_trait]
impl SearchStore for PgSearchStore {
    async fn ensure_chunk_vector_shard(&self, dim: u64) -> anyhow::Result<()> {
        self.ensure_chunk_vector_shard_impl(dim).await
    }

    async fn ensure_chunk_vector_lane_for_library(
        &self,
        dim: u64,
        _library_id: Uuid,
    ) -> anyhow::Result<()> {
        self.ensure_chunk_vector_lane_for_library_impl(dim, _library_id).await
    }

    async fn ensure_entity_vector_shard(&self, dim: u64) -> anyhow::Result<()> {
        self.ensure_entity_vector_shard_impl(dim).await
    }

    async fn upsert_chunk_vector(
        &self,
        row: &KnowledgeChunkVectorRow,
    ) -> anyhow::Result<KnowledgeChunkVectorRow> {
        self.upsert_chunk_vector_impl(row).await
    }

    async fn upsert_chunk_vectors_bulk(
        &self,
        rows: &[KnowledgeChunkVectorRow],
    ) -> anyhow::Result<()> {
        self.upsert_chunk_vectors_bulk_impl(rows).await
    }

    async fn upsert_chunk_vectors_bulk_fenced(
        &self,
        rows: &[KnowledgeChunkVectorRow],
        fence: &VectorWriteFence,
    ) -> anyhow::Result<i64> {
        self.upsert_chunk_vectors_bulk_fenced_impl(rows, fence).await
    }

    async fn upsert_ingest_chunk_vectors_bulk_fenced(
        &self,
        rows: &[KnowledgeChunkVectorRow],
        fence: &VectorWriteFence,
        active_profile_source: &dyn ActiveEmbeddingProfileSource,
    ) -> anyhow::Result<i64> {
        self.upsert_ingest_chunk_vectors_bulk_fenced_impl(rows, fence, active_profile_source).await
    }

    async fn prepare_chunk_vector_rebuild_lane(
        &self,
        library_id: Uuid,
        dimensions: u64,
        embedding_model_key: &str,
    ) -> anyhow::Result<()> {
        self.prepare_chunk_vector_rebuild_lane_impl(library_id, dimensions, embedding_model_key)
            .await
    }

    async fn upsert_chunk_vectors_bulk_deferred_manifest(
        &self,
        rows: &[KnowledgeChunkVectorRow],
    ) -> anyhow::Result<()> {
        self.upsert_chunk_vectors_bulk_deferred_manifest_impl(rows).await
    }

    async fn reconcile_chunk_vector_manifest_count(
        &self,
        library_id: Uuid,
        dimensions: u64,
        embedding_model_key: &str,
    ) -> anyhow::Result<()> {
        self.reconcile_chunk_vector_manifest_count_impl(library_id, dimensions, embedding_model_key)
            .await
    }

    async fn delete_chunk_vectors_by_ids_fenced(
        &self,
        library_id: Uuid,
        vector_ids: &[Uuid],
        expected_source_truth_version: i64,
    ) -> anyhow::Result<VectorPlaneDeleteOutcome> {
        self.delete_chunk_vectors_by_ids_fenced_impl(
            library_id,
            vector_ids,
            expected_source_truth_version,
        )
        .await
    }

    async fn delete_attempt_owned_chunk_vectors_by_ids_fenced(
        &self,
        library_id: Uuid,
        vector_ids: &[Uuid],
        expected_source_truth_version: i64,
        ingest_attempt: IngestVectorWriteFence,
    ) -> anyhow::Result<Option<VectorPlaneDeleteOutcome>> {
        self.delete_attempt_owned_chunk_vectors_by_ids_fenced_impl(
            library_id,
            vector_ids,
            expected_source_truth_version,
            ingest_attempt,
        )
        .await
    }

    async fn delete_attempt_owned_chunk_vectors_by_revision_fenced(
        &self,
        library_id: Uuid,
        expected_source_truth_version: i64,
        ingest_attempt: IngestVectorWriteFence,
    ) -> anyhow::Result<IngestAttemptMutationOutcome<VectorPlaneDeleteOutcome>> {
        self.delete_attempt_owned_chunk_vectors_by_revision_fenced_impl(
            library_id,
            expected_source_truth_version,
            ingest_attempt,
        )
        .await
    }

    async fn delete_chunk_vectors_by_revision_fenced(
        &self,
        library_id: Uuid,
        revision_id: Uuid,
        expected_source_truth_version: i64,
    ) -> anyhow::Result<VectorPlaneDeleteOutcome> {
        self.delete_chunk_vectors_by_revision_fenced_impl(
            library_id,
            revision_id,
            expected_source_truth_version,
        )
        .await
    }

    async fn list_chunk_vectors_by_chunk(
        &self,
        chunk_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeChunkVectorRow>> {
        self.list_chunk_vectors_by_chunk_impl(chunk_id).await
    }

    async fn list_chunk_vectors_by_chunks(
        &self,
        chunk_ids: &[Uuid],
        embedding_model_key: &str,
        vector_kind: &str,
    ) -> anyhow::Result<Vec<KnowledgeChunkVectorRow>> {
        self.list_chunk_vectors_by_chunks_impl(chunk_ids, embedding_model_key, vector_kind).await
    }

    async fn count_chunk_vectors_by_revision(
        &self,
        revision_id: Uuid,
        embedding_model_key: &str,
        vector_kind: &str,
        freshness_generation: i64,
    ) -> anyhow::Result<usize> {
        self.count_chunk_vectors_by_revision_impl(
            revision_id,
            embedding_model_key,
            vector_kind,
            freshness_generation,
        )
        .await
    }

    async fn read_vector_profile_dimension_claim(
        &self,
        library_id: Uuid,
        embedding_model_key: &str,
        vector_kind: &str,
    ) -> anyhow::Result<Option<u64>> {
        self.read_vector_profile_dimension_claim_impl(library_id, embedding_model_key, vector_kind)
            .await
    }

    async fn inspect_chunk_vector_profile(
        &self,
        library_id: Uuid,
        embedding_model_key: &str,
        vector_kind: &str,
    ) -> anyhow::Result<ChunkVectorProfileInventory> {
        self.inspect_chunk_vector_profile_impl(library_id, embedding_model_key, vector_kind).await
    }

    async fn upsert_entity_vector(
        &self,
        row: &KnowledgeEntityVectorRow,
    ) -> anyhow::Result<KnowledgeEntityVectorRow> {
        self.upsert_entity_vector_impl(row).await
    }

    async fn upsert_entity_vectors_bulk_fenced(
        &self,
        rows: &[KnowledgeEntityVectorRow],
        fence: &VectorWriteFence,
    ) -> anyhow::Result<i64> {
        self.upsert_entity_vectors_bulk_fenced_impl(rows, fence).await
    }

    async fn prepare_entity_vector_rebuild_lane(
        &self,
        library_id: Uuid,
        dimensions: u64,
        embedding_model_key: &str,
    ) -> anyhow::Result<()> {
        self.prepare_entity_vector_rebuild_lane_impl(library_id, dimensions, embedding_model_key)
            .await
    }

    async fn upsert_entity_vectors_bulk_deferred_manifest(
        &self,
        rows: &[KnowledgeEntityVectorRow],
    ) -> anyhow::Result<()> {
        self.upsert_entity_vectors_bulk_deferred_manifest_impl(rows).await
    }

    async fn reconcile_entity_vector_manifest_count(
        &self,
        library_id: Uuid,
        dimensions: u64,
        embedding_model_key: &str,
    ) -> anyhow::Result<()> {
        self.reconcile_entity_vector_manifest_count_impl(
            library_id,
            dimensions,
            embedding_model_key,
        )
        .await
    }

    async fn purge_empty_library_vector_plane(
        &self,
        library_id: Uuid,
        expected_source_truth_version: i64,
    ) -> anyhow::Result<u64> {
        self.purge_empty_library_vector_plane_impl(library_id, expected_source_truth_version).await
    }

    async fn promote_staged_vector_rebuild(
        &self,
        promotion: StagedVectorRebuildPromotion<'_>,
    ) -> anyhow::Result<()> {
        self.promote_staged_vector_rebuild_impl(promotion).await
    }

    async fn discard_staged_vector_rebuild(
        &self,
        library_id: Uuid,
        dimensions: u64,
        staging_embedding_model_key: &str,
    ) -> anyhow::Result<()> {
        self.discard_staged_vector_rebuild_impl(library_id, dimensions, staging_embedding_model_key)
            .await
    }

    async fn discard_abandoned_staged_vector_rebuilds(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<u64> {
        self.discard_abandoned_staged_vector_rebuilds_impl(library_id).await
    }

    async fn list_entity_vectors_by_entity(
        &self,
        entity_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityVectorRow>> {
        self.list_entity_vectors_by_entity_impl(entity_id).await
    }

    async fn search_chunks(
        &self,
        query: ChunkLexicalQuery<'_>,
    ) -> anyhow::Result<Vec<KnowledgeChunkSearchRow>> {
        self.search_chunks_impl(&query).await
    }

    async fn search_structured_blocks(
        &self,
        library_id: Uuid,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockSearchRow>> {
        self.search_structured_blocks_impl(library_id, query, limit).await
    }

    async fn search_technical_facts(
        &self,
        library_id: Uuid,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactSearchRow>> {
        self.search_technical_facts_impl(library_id, query, limit).await
    }

    async fn search_entities(
        &self,
        library_id: Uuid,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeEntitySearchRow>> {
        self.search_entities_impl(library_id, query, limit).await
    }

    async fn search_relations(
        &self,
        library_id: Uuid,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeRelationSearchRow>> {
        self.search_relations_impl(library_id, query, limit).await
    }

    async fn search_chunk_vectors_by_similarity(
        &self,
        input: VectorSimilaritySearchInput<'_>,
        limit: usize,
        temporal_start: Option<DateTime<Utc>>,
        temporal_end: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<KnowledgeChunkVectorSearchRow>> {
        self.search_chunk_vectors_by_similarity_impl(input, limit, temporal_start, temporal_end)
            .await
    }

    async fn search_chunk_vectors_by_document_similarity(
        &self,
        input: VectorSimilaritySearchInput<'_>,
        document_limit: usize,
        temporal_start: Option<DateTime<Utc>>,
        temporal_end: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<KnowledgeChunkVectorSearchRow>> {
        self.search_chunk_vectors_by_document_similarity_impl(
            input,
            document_limit,
            temporal_start,
            temporal_end,
        )
        .await
    }

    async fn search_entity_vectors_by_similarity(
        &self,
        input: VectorSimilaritySearchInput<'_>,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeEntityVectorSearchRow>> {
        self.search_entity_vectors_by_similarity_impl(input, limit).await
    }
}
