use super::*;

impl PgDocumentStore {
    pub(super) async fn finalize_structured_revision_artifact_fenced_impl(
        &self,
        authority: IngestRevisionWriteAuthority,
        expected_text_checksum: &str,
    ) -> anyhow::Result<IngestAttemptMutationOutcome<KnowledgeStructuredRevisionRow>> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("failed to start fenced structured artifact finalization")?;
        if !lock_ingest_revision_write_authority(&mut transaction, authority).await? {
            transaction
                .commit()
                .await
                .context("failed to finish preserved stale structured artifact finalization")?;
            return Ok(IngestAttemptMutationOutcome::AuthorityLost);
        }
        let revision_text_checksum = sqlx::query_scalar::<_, Option<String>>(
            "SELECT text_checksum
             FROM knowledge_revision
             WHERE revision_id = $1
               AND workspace_id = $2
               AND library_id = $3
             FOR UPDATE",
        )
        .bind(authority.revision_id)
        .bind(authority.workspace_id)
        .bind(authority.library_id)
        .fetch_optional(&mut *transaction)
        .await
        .context("failed to lock extracted revision before preparation finalization")?
        .context("structured artifact revision does not exist in authority scope")?;
        anyhow::ensure!(
            revision_text_checksum.as_deref() == Some(expected_text_checksum),
            "extracted text changed before structured artifact finalization"
        );
        let artifact =
            sqlx::query_as::<_, KnowledgeStructuredRevisionRow>(sqlx::AssertSqlSafe(format!(
                "SELECT {STRUCTURED_REVISION_COLUMNS}
                 FROM knowledge_structured_revision
                 WHERE revision_id = $1
                   AND workspace_id = $2
                   AND library_id = $3
                 FOR UPDATE"
            )))
            .bind(authority.revision_id)
            .bind(authority.workspace_id)
            .bind(authority.library_id)
            .fetch_optional(&mut *transaction)
            .await
            .context("failed to lock structured artifact before finalization")?
            .context("structured artifact disappeared before finalization")?;
        anyhow::ensure!(
            artifact.preparation_state == "preparing",
            "structured artifact must be preparing before finalization"
        );
        let actual_counts = sqlx::query_as::<_, (i64, i64, i64, i64)>(
            "SELECT
                (SELECT count(*) FROM knowledge_structured_block WHERE revision_id = $1),
                (SELECT count(*) FROM content_chunk WHERE revision_id = $1),
                (SELECT count(*) FROM knowledge_chunk WHERE revision_id = $1),
                (SELECT count(*) FROM knowledge_technical_fact WHERE revision_id = $1)",
        )
        .bind(authority.revision_id)
        .fetch_one(&mut *transaction)
        .await
        .context("failed to count persisted structured artifact families")?;
        anyhow::ensure!(
            actual_counts.0 == i64::from(artifact.block_count)
                && actual_counts.1 == i64::from(artifact.chunk_count)
                && actual_counts.2 == i64::from(artifact.chunk_count)
                && actual_counts.3 == i64::from(artifact.typed_fact_count),
            "structured artifact family counts do not match final metadata"
        );
        let finalized =
            sqlx::query_as::<_, KnowledgeStructuredRevisionRow>(sqlx::AssertSqlSafe(format!(
                "UPDATE knowledge_structured_revision
                 SET preparation_state = 'prepared', updated_at = now()
                 WHERE revision_id = $1
                 RETURNING {STRUCTURED_REVISION_COLUMNS}"
            )))
            .bind(authority.revision_id)
            .fetch_one(&mut *transaction)
            .await
            .context("failed to finalize structured artifact")?;
        transaction
            .commit()
            .await
            .context("failed to commit fenced structured artifact finalization")?;
        Ok(IngestAttemptMutationOutcome::Applied(finalized))
    }
    pub(super) async fn replace_structured_revision_artifact_fenced_impl(
        &self,
        authority: IngestRevisionWriteAuthority,
        row: &KnowledgeStructuredRevisionRow,
        blocks: &[KnowledgeStructuredBlockRow],
    ) -> anyhow::Result<IngestAttemptMutationOutcome<KnowledgeStructuredRevisionRow>> {
        anyhow::ensure!(
            row.revision_id == authority.revision_id
                && row.workspace_id == authority.workspace_id
                && row.library_id == authority.library_id,
            "structured artifact does not match its ingest authority scope"
        );
        anyhow::ensure!(
            row.preparation_state == "preparing",
            "ingest structured replacement must enter the non-readable preparing state"
        );
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("failed to start fenced structured artifact replacement")?;
        if !lock_ingest_revision_write_authority(&mut transaction, authority).await? {
            transaction
                .commit()
                .await
                .context("failed to finish preserved stale structured artifact replacement")?;
            return Ok(IngestAttemptMutationOutcome::AuthorityLost);
        }
        let persisted_revision =
            replace_structured_revision_artifact_in_transaction(&mut transaction, row, blocks)
                .await?;
        transaction
            .commit()
            .await
            .context("failed to commit fenced structured artifact replacement")?;
        Ok(IngestAttemptMutationOutcome::Applied(persisted_revision))
    }
    pub(super) async fn replace_structured_revision_artifact_impl(
        &self,
        row: &KnowledgeStructuredRevisionRow,
        blocks: &[KnowledgeStructuredBlockRow],
    ) -> anyhow::Result<KnowledgeStructuredRevisionRow> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("failed to start structured revision artifact replacement")?;
        let persisted_revision =
            replace_structured_revision_artifact_in_transaction(&mut transaction, row, blocks)
                .await?;
        transaction
            .commit()
            .await
            .context("failed to commit structured revision artifact replacement")?;
        Ok(persisted_revision)
    }
    pub(super) async fn get_structured_revision_impl(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeStructuredRevisionRow>> {
        sqlx::query_as::<_, KnowledgeStructuredRevisionRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {STRUCTURED_REVISION_COLUMNS}
             FROM knowledge_structured_revision
             WHERE revision_id = $1
               AND preparation_state = 'prepared'
             LIMIT 1"
        )))
        .bind(revision_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get structured revision")
    }

    pub(super) async fn get_structured_revision_counts_impl(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Option<StructuredRevisionCounts>> {
        sqlx::query_as::<_, StructuredRevisionCounts>(
            "SELECT block_count::int4 AS block_count, typed_fact_count::int4 AS typed_fact_count
             FROM knowledge_structured_revision
             WHERE revision_id = $1
               AND preparation_state = 'prepared'
             LIMIT 1",
        )
        .bind(revision_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get structured revision counts")
    }

    pub(super) async fn list_structured_revisions_by_revision_ids_impl(
        &self,
        revision_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeStructuredRevisionRow>> {
        if revision_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, KnowledgeStructuredRevisionRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {STRUCTURED_REVISION_COLUMNS}
             FROM knowledge_structured_revision
             WHERE revision_id = ANY($1)
               AND preparation_state = 'prepared'"
        )))
        .bind(revision_ids)
        .fetch_all(&self.pool)
        .await
        .context("failed to list structured revisions by revision ids")
    }

    pub(super) async fn list_structured_revisions_by_document_impl(
        &self,
        document_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeStructuredRevisionRow>> {
        sqlx::query_as::<_, KnowledgeStructuredRevisionRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {STRUCTURED_REVISION_COLUMNS}
             FROM knowledge_structured_revision
             WHERE document_id = $1
               AND preparation_state = 'prepared'
             ORDER BY prepared_at DESC, revision_id DESC"
        )))
        .bind(document_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list structured revisions by document")
    }

    pub(super) async fn list_structured_blocks_by_revision_impl(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockRow>> {
        sqlx::query_as::<_, KnowledgeStructuredBlockRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {STRUCTURED_BLOCK_COLUMNS}
             FROM knowledge_structured_block
             WHERE revision_id = $1
               AND EXISTS (
                   SELECT 1
                   FROM knowledge_structured_revision
                   WHERE knowledge_structured_revision.revision_id = knowledge_structured_block.revision_id
                     AND knowledge_structured_revision.preparation_state = 'prepared'
               )
             ORDER BY ordinal ASC, block_id ASC"
        )))
        .bind(revision_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list structured blocks by revision")
    }

    pub(super) async fn list_setup_structured_blocks_by_revision_impl(
        &self,
        revision_id: Uuid,
        sample_limit: usize,
        structured_limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockRow>> {
        let sample_limit = sample_limit.min(SETUP_STRUCTURED_BLOCK_LANE_LIMIT);
        let structured_limit = structured_limit.min(SETUP_STRUCTURED_BLOCK_LANE_LIMIT);
        if sample_limit == 0 && structured_limit == 0 {
            return Ok(Vec::new());
        }
        let total_limit = sample_limit.saturating_add(structured_limit);
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("failed to start bounded setup structured block read")?;
        sqlx::query("select set_config('statement_timeout', $1, true)")
            .bind(format!("{SETUP_STRUCTURED_BLOCK_STATEMENT_TIMEOUT_MS}ms"))
            .execute(&mut *transaction)
            .await
            .context("failed to set setup structured block read timeout")?;
        let rows = sqlx::query_as::<_, KnowledgeStructuredBlockRow>(sqlx::AssertSqlSafe(
            setup_structured_blocks_by_revision_sql(),
        ))
        .bind(revision_id)
        .bind(limit_i64(sample_limit))
        .bind(limit_i64(structured_limit))
        .bind(limit_i64(total_limit))
        .fetch_all(&mut *transaction)
        .await
        .context("failed to list bounded setup structured blocks by revision")?;
        transaction
            .commit()
            .await
            .context("failed to finish bounded setup structured block read")?;
        Ok(rows)
    }

    pub(super) async fn list_structured_blocks_page_by_revision_impl(
        &self,
        revision_id: Uuid,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<(Vec<KnowledgeStructuredBlockRow>, usize)> {
        let (total,) = sqlx::query_as::<_, (i64,)>(
            "SELECT COUNT(*)
             FROM knowledge_structured_block
             WHERE revision_id = $1
               AND EXISTS (
                   SELECT 1
                   FROM knowledge_structured_revision
                   WHERE knowledge_structured_revision.revision_id = knowledge_structured_block.revision_id
                     AND knowledge_structured_revision.preparation_state = 'prepared'
               )",
        )
        .bind(revision_id)
        .fetch_one(&self.pool)
        .await
        .context("failed to count structured blocks by revision")?;
        let page = sqlx::query_as::<_, KnowledgeStructuredBlockRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {STRUCTURED_BLOCK_COLUMNS}
             FROM knowledge_structured_block
             WHERE revision_id = $1
               AND EXISTS (
                   SELECT 1
                   FROM knowledge_structured_revision
                   WHERE knowledge_structured_revision.revision_id = knowledge_structured_block.revision_id
                     AND knowledge_structured_revision.preparation_state = 'prepared'
               )
             ORDER BY ordinal ASC, block_id ASC
             LIMIT $2 OFFSET $3"
        )))
        .bind(revision_id)
        .bind(limit_i64(limit))
        .bind(limit_i64(offset))
        .fetch_all(&self.pool)
        .await
        .context("failed to list structured block page by revision")?;
        Ok((page, usize::try_from(total).unwrap_or(usize::MAX)))
    }

    pub(super) async fn list_chunk_support_references_by_revision_impl(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeChunkSupportReferenceRow>> {
        sqlx::query_as::<_, KnowledgeChunkSupportReferenceRow>(
            "SELECT chunk_id, support_block_ids
             FROM knowledge_chunk
             WHERE revision_id = $1
               AND EXISTS (
                   SELECT 1
                   FROM knowledge_structured_revision
                   WHERE knowledge_structured_revision.revision_id = knowledge_chunk.revision_id
                     AND knowledge_structured_revision.preparation_state = 'prepared'
               )
             ORDER BY chunk_index ASC, chunk_id ASC
             LIMIT 2000",
        )
        .bind(revision_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list chunk support references by revision")
    }

    pub(super) async fn list_structured_blocks_by_ids_impl(
        &self,
        block_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockRow>> {
        if block_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, KnowledgeStructuredBlockRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {STRUCTURED_BLOCK_COLUMNS}
             FROM knowledge_structured_block
             WHERE block_id = ANY($1)
               AND EXISTS (
                   SELECT 1
                   FROM knowledge_structured_revision
                   WHERE knowledge_structured_revision.revision_id = knowledge_structured_block.revision_id
                     AND knowledge_structured_revision.preparation_state = 'prepared'
               )
             ORDER BY ordinal ASC, block_id ASC"
        )))
        .bind(block_ids)
        .fetch_all(&self.pool)
        .await
        .context("failed to list structured blocks by ids")
    }

    pub(super) async fn delete_structured_blocks_by_revision_impl(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM knowledge_structured_block WHERE revision_id = $1")
            .bind(revision_id)
            .execute(&self.pool)
            .await
            .context("failed to delete structured blocks by revision")?;
        Ok(())
    }

    pub(super) async fn replace_technical_facts_impl(
        &self,
        revision_id: Uuid,
        rows: &[KnowledgeTechnicalFactRow],
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        let mut transaction =
            self.pool.begin().await.context("failed to start technical fact replacement")?;
        let inserted =
            replace_technical_facts_in_transaction(&mut transaction, revision_id, rows).await?;
        transaction.commit().await.context("failed to commit technical fact replacement")?;
        Ok(inserted)
    }

    pub(super) async fn list_technical_facts_by_revision_impl(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        sqlx::query_as::<_, KnowledgeTechnicalFactRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {TECHNICAL_FACT_COLUMNS}
             FROM knowledge_technical_fact
             WHERE revision_id = $1
               AND EXISTS (
                   SELECT 1
                   FROM knowledge_structured_revision
                   WHERE knowledge_structured_revision.revision_id = knowledge_technical_fact.revision_id
                     AND knowledge_structured_revision.preparation_state = 'prepared'
               )
             ORDER BY fact_kind ASC, fact_id ASC"
        )))
        .bind(revision_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list technical facts by revision")
    }

    pub(super) async fn count_technical_facts_by_revision_impl(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<i64> {
        sqlx::query_as::<_, (i64,)>(
            "SELECT COUNT(*) FROM knowledge_technical_fact WHERE revision_id = $1",
        )
        .bind(revision_id)
        .fetch_one(&self.pool)
        .await
        .map(|(count,)| count)
        .context("failed to count technical facts by revision")
    }

    pub(super) async fn list_technical_facts_by_ids_impl(
        &self,
        fact_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        if fact_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, KnowledgeTechnicalFactRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {TECHNICAL_FACT_COLUMNS}
             FROM knowledge_technical_fact
             WHERE fact_id = ANY($1)
               AND EXISTS (
                   SELECT 1
                   FROM knowledge_structured_revision
                   WHERE knowledge_structured_revision.revision_id = knowledge_technical_fact.revision_id
                     AND knowledge_structured_revision.preparation_state = 'prepared'
               )
             ORDER BY fact_kind ASC, fact_id ASC"
        )))
        .bind(fact_ids)
        .fetch_all(&self.pool)
        .await
        .context("failed to list technical facts by ids")
    }

    pub(super) async fn list_technical_facts_by_chunk_ids_impl(
        &self,
        chunk_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        if chunk_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, KnowledgeTechnicalFactRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {TECHNICAL_FACT_COLUMNS}
             FROM knowledge_technical_fact
             WHERE support_chunk_ids && $1::uuid[]
               AND EXISTS (
                   SELECT 1
                   FROM knowledge_structured_revision
                   WHERE knowledge_structured_revision.revision_id = knowledge_technical_fact.revision_id
                     AND knowledge_structured_revision.preparation_state = 'prepared'
               )
             ORDER BY fact_kind ASC, fact_id ASC"
        )))
        .bind(chunk_ids)
        .fetch_all(&self.pool)
        .await
        .context("failed to list technical facts by chunk ids")
    }

    pub(super) async fn list_technical_facts_by_document_impl(
        &self,
        document_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        sqlx::query_as::<_, KnowledgeTechnicalFactRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {TECHNICAL_FACT_COLUMNS}
             FROM knowledge_technical_fact
             WHERE document_id = $1
               AND EXISTS (
                   SELECT 1
                   FROM knowledge_structured_revision
                   WHERE knowledge_structured_revision.revision_id = knowledge_technical_fact.revision_id
                     AND knowledge_structured_revision.preparation_state = 'prepared'
               )
             ORDER BY revision_id DESC, fact_kind ASC, fact_id ASC"
        )))
        .bind(document_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list technical facts by document")
    }

    pub(super) async fn delete_technical_facts_by_revision_impl(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        sqlx::query_as::<_, KnowledgeTechnicalFactRow>(sqlx::AssertSqlSafe(format!(
            "DELETE FROM knowledge_technical_fact
             WHERE revision_id = $1
             RETURNING {TECHNICAL_FACT_COLUMNS}"
        )))
        .bind(revision_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to delete technical facts by revision")
    }
}
