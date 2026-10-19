use super::*;

impl PgDocumentStore {
    pub(super) async fn upsert_chunk_impl(
        &self,
        row: &KnowledgeChunkRow,
    ) -> anyhow::Result<KnowledgeChunkRow> {
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(format!(
            "INSERT INTO knowledge_chunk (
                chunk_id, workspace_id, library_id, document_id, revision_id, chunk_index,
                chunk_kind, content_text, normalized_text, span_start, span_end, token_count,
                support_block_ids, section_path, heading_trail, literal_digest, chunk_state,
                text_generation, vector_generation, quality_score, window_text,
                occurred_at, occurred_until
             ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23)
             ON CONFLICT (chunk_id) DO UPDATE SET
                chunk_kind = EXCLUDED.chunk_kind,
                content_text = EXCLUDED.content_text,
                normalized_text = EXCLUDED.normalized_text,
                span_start = EXCLUDED.span_start,
                span_end = EXCLUDED.span_end,
                token_count = EXCLUDED.token_count,
                support_block_ids = EXCLUDED.support_block_ids,
                section_path = EXCLUDED.section_path,
                heading_trail = EXCLUDED.heading_trail,
                literal_digest = EXCLUDED.literal_digest,
                chunk_state = EXCLUDED.chunk_state,
                text_generation = EXCLUDED.text_generation,
                vector_generation = EXCLUDED.vector_generation,
                quality_score = EXCLUDED.quality_score,
                window_text = EXCLUDED.window_text,
                occurred_at = EXCLUDED.occurred_at,
                occurred_until = EXCLUDED.occurred_until
             RETURNING {CHUNK_COLUMNS}"
        )))
        .bind(row.chunk_id)
        .bind(row.workspace_id)
        .bind(row.library_id)
        .bind(row.document_id)
        .bind(row.revision_id)
        .bind(row.chunk_index)
        .bind(&row.chunk_kind)
        .bind(&row.content_text)
        .bind(&row.normalized_text)
        .bind(row.span_start)
        .bind(row.span_end)
        .bind(row.token_count)
        .bind(&row.support_block_ids)
        .bind(&row.section_path)
        .bind(&row.heading_trail)
        .bind(&row.literal_digest)
        .bind(&row.chunk_state)
        .bind(row.text_generation)
        .bind(row.vector_generation)
        .bind(row.quality_score)
        .bind(&row.window_text)
        .bind(row.occurred_at)
        .bind(row.occurred_until)
        .fetch_one(&self.pool)
        .await
        .context("failed to upsert knowledge chunk")
    }

    pub(super) async fn insert_chunks_impl(
        &self,
        rows: &[KnowledgeChunkRow],
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let mut inserted = Vec::with_capacity(rows.len());
        for chunk in rows.chunks(KNOWLEDGE_CHUNK_INSERT_BATCH_ROWS) {
            for row in chunk {
                inserted.push(self.upsert_chunk(row).await?);
            }
        }
        Ok(inserted)
    }

    pub(super) async fn list_active_chunks_by_library_page_impl(
        &self,
        library_id: Uuid,
        after: Option<(i32, Uuid)>,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).context("active chunk page limit overflowed i64")?;
        let (after_chunk_index, after_chunk_id) = after.unzip();
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(
            active_chunks_by_library_page_sql(),
        ))
        .bind(library_id)
        .bind(after_chunk_index)
        .bind(after_chunk_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("failed to list current primary chunk page by library")
    }

    pub(super) async fn count_active_chunks_by_library_impl(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<u64> {
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::bigint
             FROM knowledge_chunk c
             JOIN knowledge_document d
               ON d.document_id = c.document_id
              AND d.library_id = c.library_id
              AND d.readable_revision_id = c.revision_id
              AND d.document_state = 'active'
              AND d.deleted_at IS NULL
             JOIN knowledge_structured_revision r
               ON r.revision_id = c.revision_id
              AND r.workspace_id = c.workspace_id
              AND r.library_id = c.library_id
              AND r.document_id = c.document_id
              AND r.preparation_state = 'prepared'
             WHERE c.library_id = $1
               AND c.chunk_state = 'ready'",
        )
        .bind(library_id)
        .fetch_one(&self.pool)
        .await
        .context("failed to count active primary chunks by library")?;
        u64::try_from(count).context("current primary chunk count overflowed u64")
    }

    pub(super) async fn list_source_profile_chunks_by_revisions_impl(
        &self,
        library_id: Uuid,
        revision_ids: &[Uuid],
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        if limit == 0 || revision_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(format!(
            "WITH ranked_revisions AS (
               SELECT revision_id, min(input_rank) AS input_rank
               FROM unnest($2::uuid[]) WITH ORDINALITY AS r(revision_id, input_rank)
               GROUP BY revision_id
             )
             SELECT {CHUNK_COLUMNS}
             FROM (
               SELECT c.*, r.input_rank
               FROM knowledge_chunk c
               JOIN ranked_revisions r
                 ON r.revision_id = c.revision_id
               JOIN knowledge_structured_revision structured
                 ON structured.revision_id = c.revision_id
                AND structured.workspace_id = c.workspace_id
                AND structured.library_id = c.library_id
                AND structured.document_id = c.document_id
                AND structured.preparation_state = 'prepared'
               WHERE c.library_id = $1
                 AND c.chunk_state = 'ready'
                 AND (
                   c.chunk_kind = 'source_profile'
                   OR starts_with(c.normalized_text, '[source_profile ')
                   OR starts_with(c.content_text, '[source_profile ')
                 )
             ) c
             ORDER BY c.input_rank ASC, c.chunk_index ASC, c.chunk_id ASC
             LIMIT $3"
        )))
        .bind(library_id)
        .bind(revision_ids)
        .bind(limit_i64(limit))
        .fetch_all(&self.pool)
        .await
        .context("failed to list source profile chunks by revisions")
    }

    pub(super) async fn list_chunks_by_revision_impl(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {CHUNK_COLUMNS}
             FROM knowledge_chunk c
             WHERE c.revision_id = $1
               AND EXISTS (
                   SELECT 1
                   FROM knowledge_structured_revision r
                   WHERE r.revision_id = c.revision_id
                     AND r.workspace_id = c.workspace_id
                     AND r.library_id = c.library_id
                     AND r.document_id = c.document_id
                     AND r.preparation_state = 'prepared'
               )
             ORDER BY chunk_index ASC, chunk_id ASC"
        )))
        .bind(revision_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list knowledge chunks by revision")
    }

    pub(super) async fn list_head_chunks_by_revision_impl(
        &self,
        revision_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(
            list_head_chunks_by_revision_sql(),
        ))
        .bind(revision_id)
        .bind(limit_i64(limit))
        .fetch_all(&self.pool)
        .await
        .context("failed to list head knowledge chunks by revision")
    }

    pub(super) async fn count_chunks_by_revision_impl(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<i64> {
        sqlx::query_as::<_, (i64,)>(
            "SELECT COUNT(*)
             FROM knowledge_chunk
             WHERE revision_id = $1
               AND chunk_state = 'ready'
               AND EXISTS (
                   SELECT 1
                   FROM knowledge_structured_revision r
                   WHERE r.revision_id = knowledge_chunk.revision_id
                     AND r.workspace_id = knowledge_chunk.workspace_id
                     AND r.library_id = knowledge_chunk.library_id
                     AND r.document_id = knowledge_chunk.document_id
                     AND r.preparation_state = 'prepared'
               )",
        )
        .bind(revision_id)
        .fetch_one(&self.pool)
        .await
        .map(|(count,)| count)
        .context("failed to count knowledge chunks by revision")
    }

    pub(super) async fn list_chunks_by_revision_matching_terms_impl(
        &self,
        revision_id: Uuid,
        terms: &[String],
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        if limit == 0 || terms.is_empty() {
            return Ok(Vec::new());
        }
        let terms = normalized_revision_chunk_terms(terms);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        self.list_chunks_by_revisions_matching_terms_impl(&[revision_id], &terms, limit).await
    }

    pub(super) async fn list_chunks_by_revisions_matching_terms_impl(
        &self,
        revision_ids: &[Uuid],
        terms: &[String],
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        if revision_ids.is_empty() || limit == 0 || terms.is_empty() {
            return Ok(Vec::new());
        }
        let terms = normalized_revision_chunk_terms(terms);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let candidate_cap = multi_revision_matching_terms_candidate_cap(revision_ids.len(), limit);
        let like_patterns = matching_terms_like_patterns(&terms);
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("failed to start bounded multi-revision term search")?;
        sqlx::query("select set_config('statement_timeout', $1, true)")
            .bind(format!("{KNOWLEDGE_CHUNK_MULTI_REVISION_STATEMENT_TIMEOUT_MS}ms"))
            .execute(&mut *transaction)
            .await
            .context("failed to set bounded multi-revision term search timeout")?;
        sqlx::query("select set_config('work_mem', $1, true)")
            .bind("64MB")
            .execute(&mut *transaction)
            .await
            .context("failed to set bounded multi-revision term search work_mem")?;
        sqlx::query("select set_config('plan_cache_mode', $1, true)")
            .bind("force_custom_plan")
            .execute(&mut *transaction)
            .await
            .context("failed to force a custom matching-terms plan")?;
        let rows = sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(
            multi_revision_matching_terms_sql(),
        ))
        .bind(revision_ids)
        .bind(&terms)
        .bind(limit_i64(limit))
        .bind(limit_i64(candidate_cap))
        .bind(&like_patterns)
        .fetch_all(&mut *transaction)
        .await
        .context("failed to list knowledge chunks by revision terms")?;
        transaction
            .commit()
            .await
            .context("failed to finish bounded multi-revision term search")?;
        Ok(rows)
    }

    pub(super) async fn list_chunks_by_revision_range_impl(
        &self,
        revision_id: Uuid,
        min_chunk_index: i32,
        max_chunk_index: i32,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        if max_chunk_index < min_chunk_index {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(
            list_chunks_by_revision_range_sql(),
        ))
        .bind(revision_id)
        .bind(min_chunk_index)
        .bind(max_chunk_index)
        .bind(limit_i64(KNOWLEDGE_CHUNK_WINDOW_FETCH_LIMIT))
        .fetch_all(&self.pool)
        .await
        .context("failed to list knowledge chunks by revision range")
    }

    pub(super) async fn list_chunks_by_revision_windows_impl(
        &self,
        revision_id: Uuid,
        windows: &[(i32, i32)],
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        let normalized_windows = normalized_windows(windows);
        if normalized_windows.is_empty() {
            return Ok(Vec::new());
        }
        let min_indexes = normalized_windows.iter().map(|(min, _)| *min).collect::<Vec<_>>();
        let max_indexes = normalized_windows.iter().map(|(_, max)| *max).collect::<Vec<_>>();
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(
            list_chunks_by_revision_windows_sql(),
        ))
        .bind(revision_id)
        .bind(&min_indexes)
        .bind(&max_indexes)
        .bind(limit_i64(KNOWLEDGE_CHUNK_WINDOW_FETCH_LIMIT))
        .fetch_all(&self.pool)
        .await
        .context("failed to list knowledge chunks by revision windows")
    }

    pub(super) async fn list_chunks_by_revisions_windows_impl(
        &self,
        windows: &[(Uuid, i32, i32)],
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        let windows = normalized_revision_windows(windows);
        if windows.is_empty() {
            return Ok(Vec::new());
        }
        let revision_ids =
            windows.iter().map(|(revision_id, _, _)| *revision_id).collect::<Vec<_>>();
        let min_indexes = windows.iter().map(|(_, min, _)| *min).collect::<Vec<_>>();
        let max_indexes = windows.iter().map(|(_, _, max)| *max).collect::<Vec<_>>();
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(
            list_chunks_by_revisions_windows_sql(),
        ))
        .bind(&revision_ids)
        .bind(&min_indexes)
        .bind(&max_indexes)
        .bind(limit_i64(KNOWLEDGE_CHUNK_WINDOW_FETCH_LIMIT))
        .fetch_all(&self.pool)
        .await
        .context("failed to list knowledge chunks by revision windows")
    }

    pub(super) async fn list_tail_chunks_by_revision_impl(
        &self,
        revision_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(
            list_tail_chunks_by_revision_sql(),
        ))
        .bind(revision_id)
        .bind(limit_i64(limit))
        .fetch_all(&self.pool)
        .await
        .context("failed to list tail knowledge chunks by revision")
    }

    pub(super) async fn list_head_source_unit_blocks_by_revision_impl(
        &self,
        revision_id: Uuid,
        limit: usize,
        temporal_start: Option<DateTime<Utc>>,
        temporal_end: Option<DateTime<Utc>>,
        release_marker_required: bool,
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let temporal_start_iso = temporal_start.map(|value| value.to_rfc3339());
        let temporal_end_iso = temporal_end.map(|value| value.to_rfc3339());
        sqlx::query_as::<_, KnowledgeStructuredBlockRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {STRUCTURED_BLOCK_COLUMNS}
             FROM knowledge_structured_block
             CROSS JOIN LATERAL (
               SELECT CASE
                 WHEN $3::text IS NULL AND $4::text IS NULL THEN NULL
                 ELSE substring(normalized_text FROM $7)
               END AS occurred_iso
             ) temporal
             WHERE revision_id = $1
               AND block_kind = 'source_unit'
               AND EXISTS (
                 SELECT 1
                 FROM knowledge_structured_revision r
                 WHERE r.revision_id = knowledge_structured_block.revision_id
                   AND r.workspace_id = knowledge_structured_block.workspace_id
                   AND r.library_id = knowledge_structured_block.library_id
                   AND r.document_id = knowledge_structured_block.document_id
                   AND r.preparation_state = 'prepared'
               )
               AND (NOT $5 OR normalized_text ~ $6)
               AND (
                 ($3::text IS NULL AND $4::text IS NULL)
                 OR (
                   temporal.occurred_iso IS NOT NULL
                   AND ($3::text IS NULL OR temporal.occurred_iso >= $3)
                   AND ($4::text IS NULL OR temporal.occurred_iso <= $4)
                 )
               )
             ORDER BY ordinal ASC, block_id ASC
             LIMIT $2"
        )))
        .bind(revision_id)
        .bind(limit_i64(limit))
        .bind(temporal_start_iso)
        .bind(temporal_end_iso)
        .bind(release_marker_required)
        .bind(SOURCE_UNIT_RELEASE_MARKER_REGEX)
        .bind(SOURCE_UNIT_OCCURRED_AT_REGEX)
        .fetch_all(&self.pool)
        .await
        .context("failed to list head source-unit structured blocks by revision")
    }

    pub(super) async fn list_tail_source_unit_blocks_by_revision_impl(
        &self,
        revision_id: Uuid,
        limit: usize,
        temporal_start: Option<DateTime<Utc>>,
        temporal_end: Option<DateTime<Utc>>,
        release_marker_required: bool,
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let temporal_start_iso = temporal_start.map(|value| value.to_rfc3339());
        let temporal_end_iso = temporal_end.map(|value| value.to_rfc3339());
        sqlx::query_as::<_, KnowledgeStructuredBlockRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {STRUCTURED_BLOCK_COLUMNS}
             FROM (
               SELECT *
               FROM knowledge_structured_block
               CROSS JOIN LATERAL (
                 SELECT CASE
                   WHEN $3::text IS NULL AND $4::text IS NULL THEN NULL
                   ELSE substring(normalized_text FROM $7)
                 END AS occurred_iso
               ) temporal
               WHERE revision_id = $1
                 AND block_kind = 'source_unit'
                 AND EXISTS (
                   SELECT 1
                   FROM knowledge_structured_revision r
                   WHERE r.revision_id = knowledge_structured_block.revision_id
                     AND r.workspace_id = knowledge_structured_block.workspace_id
                     AND r.library_id = knowledge_structured_block.library_id
                     AND r.document_id = knowledge_structured_block.document_id
                     AND r.preparation_state = 'prepared'
                 )
                 AND (NOT $5 OR normalized_text ~ $6)
                 AND (
                   ($3::text IS NULL AND $4::text IS NULL)
                   OR (
                     temporal.occurred_iso IS NOT NULL
                     AND ($3::text IS NULL OR temporal.occurred_iso >= $3)
                     AND ($4::text IS NULL OR temporal.occurred_iso <= $4)
                   )
                 )
               ORDER BY ordinal DESC, block_id DESC
               LIMIT $2
             ) block
             ORDER BY ordinal ASC, block_id ASC"
        )))
        .bind(revision_id)
        .bind(limit_i64(limit))
        .bind(temporal_start_iso)
        .bind(temporal_end_iso)
        .bind(release_marker_required)
        .bind(SOURCE_UNIT_RELEASE_MARKER_REGEX)
        .bind(SOURCE_UNIT_OCCURRED_AT_REGEX)
        .fetch_all(&self.pool)
        .await
        .context("failed to list tail source-unit structured blocks by revision")
    }

    pub(super) async fn get_chunk_impl(
        &self,
        chunk_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeChunkRow>> {
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {CHUNK_COLUMNS}
             FROM knowledge_chunk
             WHERE chunk_id = $1
               AND EXISTS (
                   SELECT 1
                   FROM knowledge_structured_revision r
                   WHERE r.revision_id = knowledge_chunk.revision_id
                     AND r.workspace_id = knowledge_chunk.workspace_id
                     AND r.library_id = knowledge_chunk.library_id
                     AND r.document_id = knowledge_chunk.document_id
                     AND r.preparation_state = 'prepared'
               )
             LIMIT 1"
        )))
        .bind(chunk_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get knowledge chunk by id")
    }

    pub(super) async fn list_chunks_by_ids_impl(
        &self,
        chunk_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        if chunk_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {CHUNK_COLUMNS}
             FROM knowledge_chunk
             WHERE chunk_id = ANY($1)
               AND EXISTS (
                   SELECT 1
                   FROM knowledge_structured_revision r
                   WHERE r.revision_id = knowledge_chunk.revision_id
                     AND r.workspace_id = knowledge_chunk.workspace_id
                     AND r.library_id = knowledge_chunk.library_id
                     AND r.document_id = knowledge_chunk.document_id
                     AND r.preparation_state = 'prepared'
               )
             ORDER BY chunk_id ASC"
        )))
        .bind(chunk_ids)
        .fetch_all(&self.pool)
        .await
        .context("failed to list knowledge chunks by ids")
    }

    pub(super) async fn search_code_pattern_chunks_by_terms_impl(
        &self,
        library_id: Uuid,
        candidate_document_ids: &[Uuid],
        terms: &[String],
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        let candidate_document_ids = candidate_document_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let terms = normalized_search_terms(terms);
        if candidate_document_ids.is_empty() || terms.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let min_terms = i32::try_from(terms.len().clamp(1, 3)).unwrap_or(i32::MAX);
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(code_pattern_chunk_search_sql()))
            .bind(library_id)
            .bind(&candidate_document_ids)
            .bind(&terms)
            .bind(min_terms)
            .bind(CODE_PATTERN_ASSIGNMENT_REGEX)
            .bind(CODE_PATTERN_NUMERIC_MAPPING_REGEX)
            .bind(CODE_PATTERN_SECTION_REGEX)
            .bind(limit_i64(limit))
            .fetch_all(&self.pool)
            .await
            .context("failed to search code-pattern knowledge chunks")
    }

    pub(super) async fn search_transport_pattern_chunks_by_terms_impl(
        &self,
        library_id: Uuid,
        candidate_document_ids: &[Uuid],
        terms: &[String],
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        let candidate_document_ids = candidate_document_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let terms = normalized_search_terms(terms);
        if candidate_document_ids.is_empty() || terms.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(
            transport_pattern_chunk_search_sql(),
        ))
        .bind(library_id)
        .bind(&candidate_document_ids)
        .bind(&terms)
        .bind(limit_i64(limit))
        .fetch_all(&self.pool)
        .await
        .context("failed to search transport-pattern knowledge chunks")
    }

    pub(super) async fn delete_chunks_by_revision_impl(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(format!(
            "DELETE FROM knowledge_chunk
             WHERE revision_id = $1
             RETURNING {CHUNK_COLUMNS}"
        )))
        .bind(revision_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to delete knowledge chunks by revision")
    }
}
