use super::*;

impl PgSearchStore {
    pub(super) async fn ensure_entity_vector_shard_impl(&self, dim: u64) -> anyhow::Result<()> {
        self.ensure_entity_vector_relation(dim).await?;
        Ok(())
    }

    pub(super) async fn upsert_entity_vector_impl(
        &self,
        row: &KnowledgeEntityVectorRow,
    ) -> anyhow::Result<KnowledgeEntityVectorRow> {
        self.upsert_entity_vectors_bulk_immediate(std::slice::from_ref(row)).await?;
        Ok(row.clone())
    }

    pub(super) async fn upsert_entity_vectors_bulk_fenced_impl(
        &self,
        rows: &[KnowledgeEntityVectorRow],
        fence: &VectorWriteFence,
    ) -> anyhow::Result<i64> {
        self.upsert_entity_vectors_bulk_fenced_immediate(rows, fence).await
    }

    pub(super) async fn prepare_entity_vector_rebuild_lane_impl(
        &self,
        library_id: Uuid,
        dimensions: u64,
        embedding_model_key: &str,
    ) -> anyhow::Result<()> {
        validate_vector_rebuild_staging_profile_key(embedding_model_key)?;
        let relation_name = self.ensure_entity_vector_relation(dimensions).await?;
        let mut transaction =
            self.pool.begin().await.context("begin entity rebuild lane preparation")?;
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
            KNOWLEDGE_ENTITY_VECTOR_KIND,
            embedding_model_key,
            &relation_name,
        )
        .await?;
        transaction.commit().await.context("commit entity rebuild lane preparation")?;
        Ok(())
    }

    pub(super) async fn upsert_entity_vectors_bulk_deferred_manifest_impl(
        &self,
        rows: &[KnowledgeEntityVectorRow],
    ) -> anyhow::Result<()> {
        let Some(expected_relation_name) = deferred_entity_vector_relation(rows)? else {
            return Ok(());
        };
        let first = rows.first().context("deferred entity vector batch unexpectedly empty")?;
        let dimensions =
            u64::try_from(first.dimensions).context("entity vector dimension overflowed u64")?;
        let mut transaction =
            self.pool.begin().await.context("begin deferred entity vector batch")?;
        lock_library_vector_plane_data(&mut transaction, first.library_id, false).await?;
        let source_truth_version =
            lock_and_validate_deferred_entity_vector_sources(&mut transaction, rows).await?;
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
            ENTITY_VECTOR_RELATION_PREFIX,
        )
        .await?;
        anyhow::ensure!(
            relation_name == expected_relation_name,
            "prepared entity vector manifest relation changed before batch write"
        );
        let borrowed = rows.iter().collect::<Vec<_>>();
        Self::upsert_entity_vectors_in_relation_bulk_with_executor(
            &relation_name,
            borrowed.as_slice(),
            &mut *transaction,
        )
        .await?;
        transaction.commit().await.context("commit deferred entity vector batch")?;
        Ok(())
    }

    pub(super) async fn reconcile_entity_vector_manifest_count_impl(
        &self,
        library_id: Uuid,
        dimensions: u64,
        embedding_model_key: &str,
    ) -> anyhow::Result<()> {
        self.reconcile_manifest_count_for_lane(
            library_id,
            dimensions,
            KNOWLEDGE_ENTITY_VECTOR_KIND,
            embedding_model_key,
            ENTITY_VECTOR_RELATION_PREFIX,
        )
        .await
    }

    pub(super) async fn list_entity_vectors_by_entity_impl(
        &self,
        entity_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityVectorRow>> {
        let mut rows = Vec::new();
        for relation_name in self.list_vector_relations(ENTITY_VECTOR_RELATION_PREFIX).await? {
            let relation = quote_identifier(&relation_name)?;
            let pg_rows = sqlx::query_as::<_, PgEntityVectorRow>(sqlx::AssertSqlSafe(format!(
                "select key, vector_id, workspace_id, library_id, entity_id,
                    embedding_model_key, vector_kind, dimensions, embedding::text as vector_text,
                    freshness_generation, created_at
                 from {relation}
                 where entity_id = $1
                 order by freshness_generation desc, created_at desc
                 limit 1000"
            )))
            .bind(entity_id)
            .fetch_all(&self.pool)
            .await
            .with_context(|| format!("failed to list entity vectors from {relation_name}"))?;
            for row in pg_rows {
                rows.push(entity_vector_from_pg(row)?);
            }
        }
        rows.sort_by(|left, right| {
            right
                .freshness_generation
                .cmp(&left.freshness_generation)
                .then_with(|| right.created_at.cmp(&left.created_at))
        });
        rows.truncate(1000);
        Ok(rows)
    }
}
