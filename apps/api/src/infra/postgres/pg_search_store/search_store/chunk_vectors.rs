use super::*;

impl PgSearchStore {
    pub(super) async fn ensure_chunk_vector_shard_impl(&self, dim: u64) -> anyhow::Result<()> {
        self.ensure_chunk_vector_relation(dim).await?;
        Ok(())
    }

    pub(super) async fn ensure_chunk_vector_lane_for_library_impl(
        &self,
        dim: u64,
        _library_id: Uuid,
    ) -> anyhow::Result<()> {
        self.ensure_chunk_vector_relation(dim).await?;
        Ok(())
    }

    pub(super) async fn upsert_chunk_vector_impl(
        &self,
        row: &KnowledgeChunkVectorRow,
    ) -> anyhow::Result<KnowledgeChunkVectorRow> {
        self.upsert_chunk_vectors_bulk_immediate(std::slice::from_ref(row)).await?;
        Ok(row.clone())
    }

    pub(super) async fn upsert_chunk_vectors_bulk_impl(
        &self,
        rows: &[KnowledgeChunkVectorRow],
    ) -> anyhow::Result<()> {
        self.upsert_chunk_vectors_bulk_immediate(rows).await
    }

    pub(super) async fn upsert_chunk_vectors_bulk_fenced_impl(
        &self,
        rows: &[KnowledgeChunkVectorRow],
        fence: &VectorWriteFence,
    ) -> anyhow::Result<i64> {
        self.upsert_chunk_vectors_bulk_fenced_immediate(rows, fence, None).await
    }

    pub(super) async fn upsert_ingest_chunk_vectors_bulk_fenced_impl(
        &self,
        rows: &[KnowledgeChunkVectorRow],
        fence: &VectorWriteFence,
        active_profile_source: &dyn ActiveEmbeddingProfileSource,
    ) -> anyhow::Result<i64> {
        self.upsert_chunk_vectors_bulk_fenced_immediate(rows, fence, Some(active_profile_source))
            .await
    }

    pub(super) async fn prepare_chunk_vector_rebuild_lane_impl(
        &self,
        library_id: Uuid,
        dimensions: u64,
        embedding_model_key: &str,
    ) -> anyhow::Result<()> {
        validate_vector_rebuild_staging_profile_key(embedding_model_key)?;
        let relation_name = self.ensure_chunk_vector_relation(dimensions).await?;
        let mut transaction =
            self.pool.begin().await.context("begin chunk rebuild lane preparation")?;
        lock_library_vector_plane_data(&mut transaction, library_id, false).await?;
        let (_, source_truth_version) =
            lock_vector_source_library(&mut transaction, library_id).await?;
        validate_vector_rebuild_staging_source_truth_version(
            embedding_model_key,
            source_truth_version,
        )?;
        Self::upsert_manifest_in_transaction(
            &mut transaction,
            library_id,
            dimensions,
            KNOWLEDGE_CHUNK_VECTOR_KIND,
            embedding_model_key,
            &relation_name,
        )
        .await?;
        transaction.commit().await.context("commit chunk rebuild lane preparation")?;
        Ok(())
    }

    pub(super) async fn upsert_chunk_vectors_bulk_deferred_manifest_impl(
        &self,
        rows: &[KnowledgeChunkVectorRow],
    ) -> anyhow::Result<()> {
        let Some(expected_relation_name) = deferred_chunk_vector_relation(rows)? else {
            return Ok(());
        };
        let first = rows.first().context("deferred chunk vector batch unexpectedly empty")?;
        let dimensions =
            u64::try_from(first.dimensions).context("chunk vector dimension overflowed u64")?;
        let mut transaction =
            self.pool.begin().await.context("begin deferred chunk vector batch")?;
        lock_library_vector_plane_data(&mut transaction, first.library_id, false).await?;
        let source_truth_version =
            lock_and_validate_deferred_chunk_vector_sources(&mut transaction, rows).await?;
        validate_vector_rebuild_staging_source_truth_version(
            &first.embedding_model_key,
            source_truth_version,
        )?;
        let relation_name = lock_prepared_manifest_lane_for_share(
            &mut transaction,
            first.library_id,
            dimensions,
            &first.vector_kind,
            &first.embedding_model_key,
            CHUNK_VECTOR_RELATION_PREFIX,
        )
        .await?;
        anyhow::ensure!(
            relation_name == expected_relation_name,
            "prepared chunk vector manifest relation changed before batch write"
        );
        let borrowed = rows.iter().collect::<Vec<_>>();
        Self::upsert_chunk_vectors_in_relation_bulk_with_executor(
            &relation_name,
            borrowed.as_slice(),
            &mut *transaction,
        )
        .await?;
        transaction.commit().await.context("commit deferred chunk vector batch")?;
        Ok(())
    }

    pub(super) async fn reconcile_chunk_vector_manifest_count_impl(
        &self,
        library_id: Uuid,
        dimensions: u64,
        embedding_model_key: &str,
    ) -> anyhow::Result<()> {
        self.reconcile_manifest_count_for_lane(
            library_id,
            dimensions,
            KNOWLEDGE_CHUNK_VECTOR_KIND,
            embedding_model_key,
            CHUNK_VECTOR_RELATION_PREFIX,
        )
        .await
    }

    pub(super) async fn delete_chunk_vectors_by_ids_fenced_impl(
        &self,
        library_id: Uuid,
        vector_ids: &[Uuid],
        expected_source_truth_version: i64,
    ) -> anyhow::Result<VectorPlaneDeleteOutcome> {
        delete_chunk_vectors_fenced(
            &self.pool,
            library_id,
            FencedChunkVectorDelete::ExactVectorIds(vector_ids),
            expected_source_truth_version,
            None,
        )
        .await?
        .context("unconditional exact-ID vector delete unexpectedly lost attempt authority")
    }

    pub(super) async fn delete_attempt_owned_chunk_vectors_by_ids_fenced_impl(
        &self,
        library_id: Uuid,
        vector_ids: &[Uuid],
        expected_source_truth_version: i64,
        ingest_attempt: IngestVectorWriteFence,
    ) -> anyhow::Result<Option<VectorPlaneDeleteOutcome>> {
        delete_chunk_vectors_fenced(
            &self.pool,
            library_id,
            FencedChunkVectorDelete::ExactVectorIds(vector_ids),
            expected_source_truth_version,
            Some(ingest_attempt),
        )
        .await
    }

    pub(super) async fn delete_attempt_owned_chunk_vectors_by_revision_fenced_impl(
        &self,
        library_id: Uuid,
        expected_source_truth_version: i64,
        ingest_attempt: IngestVectorWriteFence,
    ) -> anyhow::Result<IngestAttemptMutationOutcome<VectorPlaneDeleteOutcome>> {
        let outcome = delete_chunk_vectors_fenced(
            &self.pool,
            library_id,
            FencedChunkVectorDelete::Revision(ingest_attempt.revision_id),
            expected_source_truth_version,
            Some(ingest_attempt),
        )
        .await?;
        Ok(outcome.map_or(
            IngestAttemptMutationOutcome::AuthorityLost,
            IngestAttemptMutationOutcome::Applied,
        ))
    }

    pub(super) async fn delete_chunk_vectors_by_revision_fenced_impl(
        &self,
        library_id: Uuid,
        revision_id: Uuid,
        expected_source_truth_version: i64,
    ) -> anyhow::Result<VectorPlaneDeleteOutcome> {
        // IRG-217 test seam: a DB test refuses this revision's vector delete to
        // prove the inline cleanup failure is a loud structured error, the
        // accepted delete still succeeds, and the GC sweep stays the backstop.
        // A no-op in production builds.
        #[cfg(feature = "test-support")]
        if crate::shared::cancellation::revision_chunk_vector_delete_fault::take(revision_id) {
            anyhow::bail!("injected destructive revision vector delete refusal");
        }
        delete_chunk_vectors_fenced(
            &self.pool,
            library_id,
            FencedChunkVectorDelete::Revision(revision_id),
            expected_source_truth_version,
            None,
        )
        .await?
        .context("revision-wide vector delete unexpectedly lost attempt authority")
    }

    pub(super) async fn list_chunk_vectors_by_chunk_impl(
        &self,
        chunk_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeChunkVectorRow>> {
        let mut rows = Vec::new();
        for relation_name in self.list_vector_relations(CHUNK_VECTOR_RELATION_PREFIX).await? {
            let relation = quote_identifier(&relation_name)?;
            let pg_rows = sqlx::query_as::<_, PgChunkVectorRow>(sqlx::AssertSqlSafe(format!(
                "select key, vector_id, workspace_id, library_id, chunk_id, revision_id,
                    embedding_model_key, vector_kind, dimensions, embedding::text as vector_text,
                    freshness_generation, created_at, occurred_at, occurred_until
                 from {relation}
                 where chunk_id = $1
                 order by freshness_generation desc, created_at desc"
            )))
            .bind(chunk_id)
            .fetch_all(&self.pool)
            .await
            .with_context(|| format!("failed to list chunk vectors from {relation_name}"))?;
            for row in pg_rows {
                rows.push(chunk_vector_from_pg(row)?);
            }
        }
        rows.sort_by(|left, right| {
            right
                .freshness_generation
                .cmp(&left.freshness_generation)
                .then_with(|| right.created_at.cmp(&left.created_at))
        });
        Ok(rows)
    }

    pub(super) async fn list_chunk_vectors_by_chunks_impl(
        &self,
        chunk_ids: &[Uuid],
        embedding_model_key: &str,
        vector_kind: &str,
    ) -> anyhow::Result<Vec<KnowledgeChunkVectorRow>> {
        if chunk_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut rows = Vec::new();
        for relation_name in self.list_vector_relations(CHUNK_VECTOR_RELATION_PREFIX).await? {
            let relation = quote_identifier(&relation_name)?;
            let pg_rows = sqlx::query_as::<_, PgChunkVectorRow>(sqlx::AssertSqlSafe(format!(
                "select key, vector_id, workspace_id, library_id, chunk_id, revision_id,
                    embedding_model_key, vector_kind, dimensions, embedding::text as vector_text,
                    freshness_generation, created_at, occurred_at, occurred_until
                 from {relation}
                 where chunk_id = any($1::uuid[])
                   and embedding_model_key = $2
                   and vector_kind = $3
                 order by chunk_id asc, freshness_generation desc, created_at desc"
            )))
            .bind(chunk_ids)
            .bind(embedding_model_key)
            .bind(vector_kind)
            .fetch_all(&self.pool)
            .await
            .with_context(|| format!("failed to list chunk vectors from {relation_name}"))?;
            for row in pg_rows {
                rows.push(chunk_vector_from_pg(row)?);
            }
        }
        rows.sort_by(|left, right| {
            left.chunk_id
                .cmp(&right.chunk_id)
                .then_with(|| right.freshness_generation.cmp(&left.freshness_generation))
                .then_with(|| right.created_at.cmp(&left.created_at))
        });
        Ok(rows)
    }

    pub(super) async fn count_chunk_vectors_by_revision_impl(
        &self,
        revision_id: Uuid,
        embedding_model_key: &str,
        vector_kind: &str,
        freshness_generation: i64,
    ) -> anyhow::Result<usize> {
        let mut total = 0_i64;
        for relation_name in self.list_vector_relations(CHUNK_VECTOR_RELATION_PREFIX).await? {
            let relation = quote_identifier(&relation_name)?;
            let count = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(
                active_chunk_vector_count_sql(&relation),
            ))
            .bind(revision_id)
            .bind(embedding_model_key)
            .bind(vector_kind)
            .bind(freshness_generation)
            .fetch_one(&self.pool)
            .await
            .with_context(|| format!("failed to count chunk vectors in {relation_name}"))?;
            total = total.checked_add(count).context("chunk vector count overflowed i64")?;
        }
        usize::try_from(total).context("chunk vector count overflowed usize")
    }

    pub(super) async fn read_vector_profile_dimension_claim_impl(
        &self,
        library_id: Uuid,
        embedding_model_key: &str,
        vector_kind: &str,
    ) -> anyhow::Result<Option<u64>> {
        validate_exact_embedding_profile_key(embedding_model_key)?;
        let relation_prefix = match vector_kind {
            KNOWLEDGE_CHUNK_VECTOR_KIND => CHUNK_VECTOR_RELATION_PREFIX,
            KNOWLEDGE_ENTITY_VECTOR_KIND => ENTITY_VECTOR_RELATION_PREFIX,
            _ => anyhow::bail!("unsupported vector kind for an exact-profile dimension claim"),
        };
        let claims = sqlx::query_as::<_, (i32, String)>(
            "select dim, relation_name
             from knowledge_vector_relation_manifest
             where library_id = $1
               and embedding_model_key = $2
               and vector_kind = $3
               and embedding_model_key like 'embedding-profile:v1:%'
             order by dim
             limit 2",
        )
        .bind(library_id)
        .bind(embedding_model_key)
        .bind(vector_kind)
        .fetch_all(&self.pool)
        .await
        .context("failed to read exact-profile vector dimension claim")?;

        validate_exact_profile_dimension_claim(relation_prefix, &claims)
    }

    pub(super) async fn inspect_chunk_vector_profile_impl(
        &self,
        library_id: Uuid,
        embedding_model_key: &str,
        vector_kind: &str,
    ) -> anyhow::Result<ChunkVectorProfileInventory> {
        let manifest_rows = sqlx::query_as::<_, (i32, String)>(
            "select dim, relation_name
             from knowledge_vector_relation_manifest
             where library_id = $1
               and embedding_model_key = $2
               and vector_kind = $3
               and relation_name like $4
             order by dim desc",
        )
        .bind(library_id)
        .bind(embedding_model_key)
        .bind(vector_kind)
        .bind(format!("{CHUNK_VECTOR_RELATION_PREFIX}%"))
        .fetch_all(&self.pool)
        .await
        .context("failed to list chunk vector dimensions from manifest")?;

        let Some(counts_sql) = active_chunk_vector_dimension_counts_sql(&manifest_rows)? else {
            return Ok(ChunkVectorProfileInventory {
                dimensions: Vec::new(),
                active_vector_count: 0,
            });
        };
        let active_counts = sqlx::query_as::<_, (i32, i64)>(sqlx::AssertSqlSafe(counts_sql))
            .bind(library_id)
            .bind(embedding_model_key)
            .bind(vector_kind)
            .fetch_all(&self.pool)
            .await
            .context("failed to count active chunk vectors by dimension")?;
        chunk_vector_profile_inventory(active_counts)
    }
}
