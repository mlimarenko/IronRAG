use super::*;

impl StagedVectorRebuildPromotion<'_> {
    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.active_embedding_profile_key != self.staging_embedding_model_key,
            "active and staging embedding profiles must be distinct"
        );
        validate_vector_rebuild_staging_profile_key(self.staging_embedding_model_key)?;
        anyhow::ensure!(
            self.chunk_count.is_some() || self.entity_count.is_some(),
            "staged vector promotion requires at least one lane"
        );
        Ok(())
    }
}

impl PgSearchStore {
    pub(super) async fn purge_empty_library_vector_plane_impl(
        &self,
        library_id: Uuid,
        expected_source_truth_version: i64,
    ) -> anyhow::Result<u64> {
        let mut transaction = self.pool.begin().await.context("begin empty vector-plane purge")?;
        // Lock order for cross-process vector-plane mutations is always:
        // per-library data serializer -> AI-config serializer -> library row.
        // Ordinary fenced writes use the shared form of this first lock.
        lock_library_vector_plane_data(&mut transaction, library_id, true).await?;
        sqlx::query(
            "select pg_advisory_xact_lock(
                hashtextextended('ironrag:ai-config-generation', 0)
             )",
        )
        .execute(&mut *transaction)
        .await
        .context("serialize empty vector purge with AI configuration changes")?;
        let observed_source_truth_version = sqlx::query_scalar::<_, i64>(
            "select source_truth_version
             from catalog_library
             where id = $1
             for update",
        )
        .bind(library_id)
        .fetch_optional(&mut *transaction)
        .await
        .context("lock library source fence before empty vector purge")?
        .ok_or_else(|| anyhow::anyhow!("library disappeared during empty vector purge"))?;
        anyhow::ensure!(
            observed_source_truth_version == expected_source_truth_version,
            "library source truth changed before empty vector purge"
        );
        ensure_no_authoritative_vector_candidates(&mut transaction, library_id).await?;
        let active_chunk_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_chunk c
             join knowledge_document d
               on d.document_id = c.document_id
              and d.library_id = c.library_id
              and d.readable_revision_id = c.revision_id
              and d.document_state = 'active'
              and d.deleted_at is null
             where c.library_id = $1
               and c.chunk_state = 'ready'",
        )
        .bind(library_id)
        .fetch_one(&mut *transaction)
        .await
        .context("recheck active chunks before empty vector purge")?;
        anyhow::ensure!(active_chunk_count == 0, "library gained chunks before empty vector purge");

        let mut deleted = 0_u64;
        for relation_prefix in [CHUNK_VECTOR_RELATION_PREFIX, ENTITY_VECTOR_RELATION_PREFIX] {
            for relation_name in
                list_physical_vector_relations_in_transaction(&mut transaction, relation_prefix)
                    .await?
            {
                let relation = quote_identifier(&relation_name)?;
                let result = sqlx::query(sqlx::AssertSqlSafe(format!(
                    "delete from {relation} where library_id = $1"
                )))
                .bind(library_id)
                .execute(&mut *transaction)
                .await
                .with_context(|| {
                    format!("delete empty-library vector rows from {relation_name}")
                })?;
                deleted = deleted
                    .checked_add(result.rows_affected())
                    .context("empty-library vector purge count overflowed u64")?;
            }
        }
        let deleted_manifests =
            sqlx::query("delete from knowledge_vector_relation_manifest where library_id = $1")
                .bind(library_id)
                .execute(&mut *transaction)
                .await
                .context("delete empty-library vector manifests")?;
        if deleted > 0 || deleted_manifests.rows_affected() > 0 {
            let advanced = sqlx::query_scalar::<_, i64>(
                "update catalog_library
                 set source_truth_version = greatest(
                        coalesce(source_truth_version, 0) + 1,
                        (extract(epoch from clock_timestamp()) * 1000000)::bigint
                     ),
                     updated_at = now()
                 where id = $1 and source_truth_version = $2
                 returning source_truth_version",
            )
            .bind(library_id)
            .bind(expected_source_truth_version)
            .fetch_optional(&mut *transaction)
            .await
            .context("advance source fence with empty vector purge")?;
            anyhow::ensure!(
                advanced.is_some(),
                "library source fence changed before empty vector purge"
            );
        }
        transaction.commit().await.context("commit empty vector-plane purge")?;
        Ok(deleted)
    }

    pub(super) async fn promote_staged_vector_rebuild_impl(
        &self,
        input: StagedVectorRebuildPromotion<'_>,
    ) -> anyhow::Result<()> {
        input.validate()?;
        let StagedVectorRebuildPromotion {
            library_id,
            dimensions,
            active_embedding_profile_key,
            staging_embedding_model_key,
            expected_source_truth_version,
            chunk_count,
            entity_count,
        } = input;
        let mut transaction = self.pool.begin().await.context("begin staged vector promotion")?;
        lock_library_vector_plane_data(&mut transaction, library_id, true).await?;
        // AI-binding trigger transactions acquire this serializer before
        // taking child and catalog-library locks. Promotion must enter the
        // same global order before comparing the source fence; otherwise a
        // binding change can wait behind the library row and commit an
        // obsolete vector profile immediately after promotion.
        sqlx::query(
            "select pg_advisory_xact_lock(
                hashtextextended('ironrag:ai-config-generation', 0)
             )",
        )
        .execute(&mut *transaction)
        .await
        .context("serialize staged promotion with AI configuration changes")?;
        let observed_source_truth_version = sqlx::query_scalar::<_, i64>(
            "select source_truth_version
             from catalog_library
             where id = $1
             for update",
        )
        .bind(library_id)
        .fetch_optional(&mut *transaction)
        .await
        .context("lock library source fence before staged vector promotion")?
        .ok_or_else(|| anyhow::anyhow!("library disappeared during staged vector promotion"))?;
        anyhow::ensure!(
            observed_source_truth_version == expected_source_truth_version,
            "library source truth changed during staged vector rebuild"
        );
        ensure_no_authoritative_vector_candidates(&mut transaction, library_id).await?;

        if let Some(expected_count) = chunk_count {
            promote_staged_vector_kind(
                &mut transaction,
                library_id,
                dimensions,
                &StagedVectorLane {
                    vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND,
                    relation_prefix: CHUNK_VECTOR_RELATION_PREFIX,
                    active_embedding_profile_key,
                    staging_embedding_model_key,
                },
                expected_count,
            )
            .await?;
        }
        if let Some(expected_count) = entity_count {
            promote_staged_vector_kind(
                &mut transaction,
                library_id,
                dimensions,
                &StagedVectorLane {
                    vector_kind: KNOWLEDGE_ENTITY_VECTOR_KIND,
                    relation_prefix: ENTITY_VECTOR_RELATION_PREFIX,
                    active_embedding_profile_key,
                    staging_embedding_model_key,
                },
                expected_count,
            )
            .await?;
        }
        let advanced = sqlx::query_scalar::<_, i64>(
            "update catalog_library
             set source_truth_version = greatest(
                    coalesce(source_truth_version, 0) + 1,
                    (extract(epoch from clock_timestamp()) * 1000000)::bigint
                 ),
                 updated_at = now()
             where id = $1 and source_truth_version = $2
             returning source_truth_version",
        )
        .bind(library_id)
        .bind(expected_source_truth_version)
        .fetch_optional(&mut *transaction)
        .await
        .context("advance library source fence with staged vector promotion")?;
        anyhow::ensure!(advanced.is_some(), "library source fence changed before vector promotion");
        transaction.commit().await.context("commit staged vector promotion")?;
        Ok(())
    }

    pub(super) async fn discard_staged_vector_rebuild_impl(
        &self,
        library_id: Uuid,
        dimensions: u64,
        staging_embedding_model_key: &str,
    ) -> anyhow::Result<()> {
        let mut transaction = self.pool.begin().await.context("begin staged vector cleanup")?;
        // Cleanup refreshes active manifest counts for touched physical
        // relations, so it participates in the same short exclusive data
        // boundary as purge/promotion in addition to the rebuild session lock.
        lock_library_vector_plane_data(&mut transaction, library_id, true).await?;
        let touched_relations = discard_staged_vector_rebuild_in_transaction(
            &mut transaction,
            library_id,
            dimensions,
            staging_embedding_model_key,
        )
        .await?;
        delete_source_less_vectors_in_touched_relations(
            &mut transaction,
            library_id,
            &touched_relations,
        )
        .await?;
        transaction.commit().await.context("commit staged vector cleanup")?;
        Ok(())
    }

    pub(super) async fn discard_abandoned_staged_vector_rebuilds_impl(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<u64> {
        let limit = i64::try_from(MAX_ABANDONED_VECTOR_REBUILD_MANIFEST_ROWS)
            .context("abandoned staging scan limit overflowed i64")?;
        let mut discarded_profiles = 0_u64;
        loop {
            let mut transaction =
                self.pool.begin().await.context("begin abandoned staging cleanup batch")?;
            lock_library_vector_plane_data(&mut transaction, library_id, true).await?;
            let rows = sqlx::query_as::<_, (i32, String)>(
                "select dim, embedding_model_key
                 from knowledge_vector_relation_manifest
                 where library_id = $1
                   and promoted = false
                   and embedding_model_key like $2
                 order by dim, embedding_model_key, vector_kind
                 limit $3
                 for update",
            )
            .bind(library_id)
            .bind(format!("{VECTOR_REBUILD_STAGING_PROFILE_PREFIX}%"))
            .bind(limit)
            .fetch_all(&mut *transaction)
            .await
            .context("scan abandoned vector rebuild manifest batch")?;
            let mut staging_profiles = rows.into_iter().collect::<BTreeSet<(i32, String)>>();
            for relation_prefix in [CHUNK_VECTOR_RELATION_PREFIX, ENTITY_VECTOR_RELATION_PREFIX] {
                for relation_name in
                    list_physical_vector_relations_in_transaction(&mut transaction, relation_prefix)
                        .await?
                {
                    let relation = quote_identifier(&relation_name)?;
                    let physical_profiles =
                        sqlx::query_as::<_, (i32, String)>(sqlx::AssertSqlSafe(format!(
                            "select distinct dimensions, embedding_model_key
                             from {relation}
                             where library_id = $1
                               and embedding_model_key like $2
                             order by dimensions, embedding_model_key
                             limit $3"
                        )))
                        .bind(library_id)
                        .bind(format!("{VECTOR_REBUILD_STAGING_PROFILE_PREFIX}%"))
                        .bind(limit)
                        .fetch_all(&mut *transaction)
                        .await
                        .with_context(|| {
                            format!("scan abandoned physical staging profiles in {relation_name}")
                        })?;
                    staging_profiles.extend(physical_profiles);
                }
            }
            if staging_profiles.is_empty() {
                transaction.commit().await.context("commit empty abandoned staging scan")?;
                break;
            }
            let staging_profiles = staging_profiles
                .into_iter()
                .take(MAX_ABANDONED_VECTOR_REBUILD_MANIFEST_ROWS)
                .map(|(dimensions, profile_key)| {
                    validate_vector_rebuild_staging_profile_key(&profile_key)?;
                    Ok((
                        u64::try_from(dimensions)
                            .context("abandoned staging dimension was negative")?,
                        profile_key,
                    ))
                })
                .collect::<anyhow::Result<BTreeSet<_>>>()?;
            let mut touched_relations = BTreeSet::new();
            for (dimensions, profile_key) in &staging_profiles {
                touched_relations.extend(
                    discard_staged_vector_rebuild_in_transaction(
                        &mut transaction,
                        library_id,
                        *dimensions,
                        profile_key,
                    )
                    .await?,
                );
            }
            delete_source_less_vectors_in_touched_relations(
                &mut transaction,
                library_id,
                &touched_relations,
            )
            .await?;
            transaction.commit().await.context("commit abandoned staging cleanup batch")?;
            let batch_profiles = u64::try_from(staging_profiles.len())
                .context("abandoned staging batch count overflowed u64")?;
            discarded_profiles = discarded_profiles
                .checked_add(batch_profiles)
                .context("abandoned staging cleanup count overflowed u64")?;
        }
        Ok(discarded_profiles)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn promotion(staging_key: &str) -> StagedVectorRebuildPromotion<'_> {
        StagedVectorRebuildPromotion {
            library_id: Uuid::from_u128(11),
            dimensions: 7,
            active_embedding_profile_key: "active-profile",
            staging_embedding_model_key: staging_key,
            expected_source_truth_version: 19,
            chunk_count: None,
            entity_count: None,
        }
    }

    #[test]
    fn promotion_requires_a_lane_but_accepts_zero_expected_rows() {
        let staging =
            format!("{VECTOR_REBUILD_STAGING_PROFILE_PREFIX}{}", "0123456789abcdef".repeat(4));
        let mut input = promotion(&staging);
        assert_eq!(
            input.validate().unwrap_err().to_string(),
            "staged vector promotion requires at least one lane"
        );
        for (chunk_count, entity_count) in [(Some(0), None), (None, Some(0)), (Some(3), Some(5))] {
            input.chunk_count = chunk_count;
            input.entity_count = entity_count;
            assert!(input.validate().is_ok());
        }
    }

    #[test]
    fn promotion_checks_profile_identity_before_staging_format_or_lanes() {
        let mut input = promotion("not-a-staging-profile");
        input.active_embedding_profile_key = input.staging_embedding_model_key;
        assert_eq!(
            input.validate().unwrap_err().to_string(),
            "active and staging embedding profiles must be distinct"
        );
        input.active_embedding_profile_key = "active-profile";
        let expected =
            validate_vector_rebuild_staging_profile_key(input.staging_embedding_model_key)
                .unwrap_err();
        assert_eq!(input.validate().unwrap_err().to_string(), expected.to_string());
    }
}
