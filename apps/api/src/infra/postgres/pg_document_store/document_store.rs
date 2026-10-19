use super::*;

mod chunks;
mod documents;
mod revisions;
mod structured_content;

#[async_trait]
impl DocumentStore for PgDocumentStore {
    async fn upsert_document(
        &self,
        row: &KnowledgeDocumentRow,
    ) -> anyhow::Result<KnowledgeDocumentRow> {
        self.upsert_document_impl(row).await
    }

    async fn get_document(
        &self,
        document_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeDocumentRow>> {
        self.get_document_impl(document_id).await
    }

    async fn get_document_by_external_key(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
        external_key: &str,
    ) -> anyhow::Result<Option<KnowledgeDocumentRow>> {
        self.get_document_by_external_key_impl(workspace_id, library_id, external_key).await
    }

    async fn list_documents_by_library(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
        include_deleted: bool,
    ) -> anyhow::Result<Vec<KnowledgeDocumentRow>> {
        self.list_documents_by_library_impl(workspace_id, library_id, include_deleted).await
    }

    async fn list_documents_by_ids(
        &self,
        document_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeDocumentRow>> {
        self.list_documents_by_ids_impl(document_ids).await
    }

    async fn repair_missing_document_file_names(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
        limit: u32,
    ) -> anyhow::Result<u64> {
        if limit == 0 {
            return Ok(0);
        }
        let repaired = sqlx::query(
            "WITH candidates AS MATERIALIZED (
                SELECT knowledge.document_id, content.source_file_name
                FROM knowledge_document AS knowledge
                JOIN content_document AS content
                  ON content.id = knowledge.document_id
                 AND content.workspace_id = knowledge.workspace_id
                 AND content.library_id = knowledge.library_id
                WHERE knowledge.workspace_id = $1
                  AND knowledge.library_id = $2
                  AND knowledge.file_name IS NULL
                  AND content.source_file_name IS NOT NULL
                ORDER BY knowledge.document_id
                LIMIT $3
                FOR UPDATE OF knowledge SKIP LOCKED
             )
             UPDATE knowledge_document AS knowledge
             SET file_name = candidates.source_file_name,
                 updated_at = now()
             FROM candidates
             WHERE knowledge.document_id = candidates.document_id
               AND knowledge.workspace_id = $1
               AND knowledge.library_id = $2
               AND knowledge.file_name IS NULL",
        )
        .bind(workspace_id)
        .bind(library_id)
        .bind(i64::from(limit))
        .execute(&self.pool)
        .await
        .context("failed to repair missing knowledge document file names")?;
        Ok(repaired.rows_affected())
    }

    async fn update_document_pointers(
        &self,
        update: DocumentPointersUpdate<'_>,
    ) -> anyhow::Result<Option<KnowledgeDocumentRow>> {
        self.update_document_pointers_impl(update).await
    }

    async fn upsert_revision(
        &self,
        row: &KnowledgeRevisionRow,
    ) -> anyhow::Result<KnowledgeRevisionRow> {
        self.upsert_revision_impl(row).await
    }

    async fn ensure_ingest_revision_projection_fenced(
        &self,
        authority: IngestRevisionWriteAuthority,
        document: &KnowledgeDocumentRow,
        revision: &KnowledgeRevisionRow,
    ) -> anyhow::Result<IngestAttemptMutationOutcome<KnowledgeRevisionRow>> {
        anyhow::ensure!(
            document.document_id == revision.document_id
                && document.workspace_id == authority.workspace_id
                && document.library_id == authority.library_id
                && revision.revision_id == authority.revision_id
                && revision.workspace_id == authority.workspace_id
                && revision.library_id == authority.library_id,
            "knowledge self-heal projection does not match ingest authority scope"
        );
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("failed to start fenced knowledge projection self-heal")?;
        if !lock_ingest_revision_write_authority(&mut transaction, authority).await? {
            transaction
                .commit()
                .await
                .context("failed to finish preserved stale knowledge projection self-heal")?;
            return Ok(IngestAttemptMutationOutcome::AuthorityLost);
        }
        sqlx::query(
            "INSERT INTO knowledge_document (
                document_id, workspace_id, library_id, external_key, file_name, title,
                document_state, active_revision_id, readable_revision_id, latest_revision_no,
                parent_document_id, document_role, created_at, updated_at, deleted_at
             ) VALUES (
                $1,$2,$3,$4,
                COALESCE(
                    (
                        SELECT source_file_name
                        FROM content_document
                        WHERE id = $1
                          AND workspace_id = $2
                          AND library_id = $3
                    ),
                    $5
                ),
                $6,$7,$8,$9,$10,$11,$12,$13,$14,$15
             )
             ON CONFLICT (document_id) DO UPDATE
             SET file_name = COALESCE(
                    (
                        SELECT source_file_name
                        FROM content_document
                        WHERE id = EXCLUDED.document_id
                          AND workspace_id = EXCLUDED.workspace_id
                          AND library_id = EXCLUDED.library_id
                    ),
                    EXCLUDED.file_name
                 ),
                 updated_at = GREATEST(knowledge_document.updated_at, now())
             WHERE knowledge_document.workspace_id = EXCLUDED.workspace_id
               AND knowledge_document.library_id = EXCLUDED.library_id
               AND knowledge_document.file_name IS NULL
               AND COALESCE(
                    (
                        SELECT source_file_name
                        FROM content_document
                        WHERE id = EXCLUDED.document_id
                          AND workspace_id = EXCLUDED.workspace_id
                          AND library_id = EXCLUDED.library_id
                    ),
                    EXCLUDED.file_name
               ) IS NOT NULL",
        )
        .bind(document.document_id)
        .bind(document.workspace_id)
        .bind(document.library_id)
        .bind(&document.external_key)
        .bind(&document.file_name)
        .bind(&document.title)
        .bind(&document.document_state)
        .bind(document.active_revision_id)
        .bind(document.readable_revision_id)
        .bind(document.latest_revision_no)
        .bind(document.parent_document_id)
        .bind(&document.document_role)
        .bind(document.created_at)
        .bind(document.updated_at)
        .bind(document.deleted_at)
        .execute(&mut *transaction)
        .await
        .context("failed to insert missing knowledge document projection")?;
        let persisted_document_scope = sqlx::query_as::<_, (Uuid, Uuid)>(
            "SELECT workspace_id, library_id
             FROM knowledge_document
             WHERE document_id = $1
             FOR UPDATE",
        )
        .bind(document.document_id)
        .fetch_optional(&mut *transaction)
        .await
        .context("failed to lock ensured knowledge document projection")?
        .context("ensured knowledge document projection disappeared")?;
        anyhow::ensure!(
            persisted_document_scope == (authority.workspace_id, authority.library_id),
            "existing knowledge document projection has incompatible authority scope"
        );

        sqlx::query(
            "INSERT INTO knowledge_revision (
                revision_id, workspace_id, library_id, document_id, revision_number,
                revision_state, revision_kind, storage_ref, source_uri, document_hint, mime_type,
                checksum, title, byte_size, normalized_text, text_checksum, image_checksum,
                text_state, vector_state, graph_state, text_readable_at, vector_ready_at,
                graph_ready_at, superseded_by_revision_id, created_at
             ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25)
             ON CONFLICT (revision_id) DO NOTHING",
        )
        .bind(revision.revision_id)
        .bind(revision.workspace_id)
        .bind(revision.library_id)
        .bind(revision.document_id)
        .bind(revision.revision_number)
        .bind(&revision.revision_state)
        .bind(&revision.revision_kind)
        .bind(&revision.storage_ref)
        .bind(&revision.source_uri)
        .bind(&revision.document_hint)
        .bind(&revision.mime_type)
        .bind(&revision.checksum)
        .bind(&revision.title)
        .bind(revision.byte_size)
        .bind(&revision.normalized_text)
        .bind(&revision.text_checksum)
        .bind(&revision.image_checksum)
        .bind(&revision.text_state)
        .bind(&revision.vector_state)
        .bind(&revision.graph_state)
        .bind(revision.text_readable_at)
        .bind(revision.vector_ready_at)
        .bind(revision.graph_ready_at)
        .bind(revision.superseded_by_revision_id)
        .bind(revision.created_at)
        .execute(&mut *transaction)
        .await
        .context("failed to insert missing knowledge revision projection")?;
        let persisted_revision =
            sqlx::query_as::<_, KnowledgeRevisionRow>(sqlx::AssertSqlSafe(format!(
                "SELECT {REVISION_COLUMNS}
                 FROM knowledge_revision
                 WHERE revision_id = $1
                   AND workspace_id = $2
                   AND library_id = $3
                   AND document_id = $4
                 FOR UPDATE"
            )))
            .bind(authority.revision_id)
            .bind(authority.workspace_id)
            .bind(authority.library_id)
            .bind(document.document_id)
            .fetch_optional(&mut *transaction)
            .await
            .context("failed to lock ensured knowledge revision projection")?
            .context("existing knowledge revision projection has incompatible authority scope")?;
        transaction
            .commit()
            .await
            .context("failed to commit fenced knowledge projection self-heal")?;
        Ok(IngestAttemptMutationOutcome::Applied(persisted_revision))
    }

    async fn update_revision_document_hint(
        &self,
        revision_id: Uuid,
        document_hint: Option<&str>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>> {
        self.update_revision_document_hint_impl(revision_id, document_hint).await
    }

    async fn get_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>> {
        self.get_revision_impl(revision_id).await
    }

    async fn list_revisions_by_ids(
        &self,
        revision_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeRevisionRow>> {
        self.list_revisions_by_ids_impl(revision_ids).await
    }

    async fn list_revisions_by_document(
        &self,
        document_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRevisionRow>> {
        self.list_revisions_by_document_impl(document_id).await
    }

    async fn aggregate_library_generation_signals(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<LibraryGenerationSignals> {
        self.aggregate_library_generation_signals_impl(library_id).await
    }

    async fn list_documents_missing_chunk_vectors(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<Uuid>> {
        self.list_documents_missing_chunk_vectors_impl(library_id).await
    }

    async fn update_revision_readiness(
        &self,
        update: RevisionReadinessUpdate<'_>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>> {
        self.update_revision_readiness_impl(update).await
    }

    async fn update_revision_text_content(
        &self,
        revision_id: Uuid,
        normalized_text: Option<&str>,
        text_checksum: Option<&str>,
        text_state: &str,
        text_readable_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>> {
        self.update_revision_text_content_impl(
            revision_id,
            normalized_text,
            text_checksum,
            text_state,
            text_readable_at,
        )
        .await
    }

    async fn update_revision_extract_artifact_fenced(
        &self,
        authority: IngestRevisionWriteAuthority,
        text_state: &str,
        artifact: IngestRevisionExtractArtifact<'_>,
    ) -> anyhow::Result<IngestAttemptMutationOutcome<KnowledgeRevisionRow>> {
        let mut transaction =
            self.pool.begin().await.context("failed to start fenced revision extract update")?;
        if !lock_ingest_revision_write_authority(&mut transaction, authority).await? {
            transaction
                .commit()
                .await
                .context("failed to finish preserved stale revision extract update")?;
            return Ok(IngestAttemptMutationOutcome::AuthorityLost);
        }
        let (replace_artifact, normalized_text, text_checksum, text_readable_at, image_checksum) =
            match artifact {
                IngestRevisionExtractArtifact::StateOnly => (false, None, None, None, None),
                IngestRevisionExtractArtifact::Replace {
                    normalized_text,
                    text_checksum,
                    text_readable_at,
                    image_checksum,
                } => (
                    true,
                    Some(normalized_text),
                    Some(text_checksum),
                    text_readable_at,
                    image_checksum,
                ),
            };
        let row = sqlx::query_as::<_, KnowledgeRevisionRow>(sqlx::AssertSqlSafe(format!(
            "UPDATE knowledge_revision SET
                normalized_text = CASE WHEN $2 THEN $3 ELSE normalized_text END,
                text_checksum = CASE WHEN $2 THEN $4 ELSE text_checksum END,
                text_state = $5,
                text_readable_at = CASE WHEN $2 THEN $6 ELSE text_readable_at END,
                image_checksum = CASE WHEN $2 THEN $7 ELSE image_checksum END
             WHERE revision_id = $1
               AND workspace_id = $8
               AND library_id = $9
             RETURNING {REVISION_COLUMNS}"
        )))
        .bind(authority.revision_id)
        .bind(replace_artifact)
        .bind(normalized_text)
        .bind(text_checksum)
        .bind(text_state)
        .bind(text_readable_at)
        .bind(image_checksum)
        .bind(authority.workspace_id)
        .bind(authority.library_id)
        .fetch_optional(&mut *transaction)
        .await
        .context("failed to update fenced knowledge revision extract artifact")?
        .context("fenced knowledge revision extract artifact does not exist in authority scope")?;
        transaction.commit().await.context("failed to commit fenced revision extract update")?;
        Ok(IngestAttemptMutationOutcome::Applied(row))
    }

    async fn update_revision_image_checksum(
        &self,
        revision_id: Uuid,
        image_checksum: Option<&str>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>> {
        self.update_revision_image_checksum_impl(revision_id, image_checksum).await
    }

    async fn update_revision_storage_ref(
        &self,
        revision_id: Uuid,
        storage_ref: Option<&str>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>> {
        self.update_revision_storage_ref_impl(revision_id, storage_ref).await
    }

    async fn upsert_chunk(&self, row: &KnowledgeChunkRow) -> anyhow::Result<KnowledgeChunkRow> {
        self.upsert_chunk_impl(row).await
    }

    async fn insert_chunks(
        &self,
        rows: &[KnowledgeChunkRow],
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.insert_chunks_impl(rows).await
    }

    async fn list_active_chunks_by_library_page(
        &self,
        library_id: Uuid,
        after: Option<(i32, Uuid)>,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.list_active_chunks_by_library_page_impl(library_id, after, limit).await
    }

    async fn count_active_chunks_by_library(&self, library_id: Uuid) -> anyhow::Result<u64> {
        self.count_active_chunks_by_library_impl(library_id).await
    }

    async fn list_source_profile_chunks_by_revisions(
        &self,
        library_id: Uuid,
        revision_ids: &[Uuid],
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.list_source_profile_chunks_by_revisions_impl(library_id, revision_ids, limit).await
    }

    async fn list_chunks_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.list_chunks_by_revision_impl(revision_id).await
    }

    async fn list_head_chunks_by_revision(
        &self,
        revision_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.list_head_chunks_by_revision_impl(revision_id, limit).await
    }

    async fn count_chunks_by_revision(&self, revision_id: Uuid) -> anyhow::Result<i64> {
        self.count_chunks_by_revision_impl(revision_id).await
    }

    async fn list_chunks_by_revision_matching_terms(
        &self,
        revision_id: Uuid,
        terms: &[String],
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.list_chunks_by_revision_matching_terms_impl(revision_id, terms, limit).await
    }

    async fn list_chunks_by_revisions_matching_terms(
        &self,
        revision_ids: &[Uuid],
        terms: &[String],
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.list_chunks_by_revisions_matching_terms_impl(revision_ids, terms, limit).await
    }

    async fn list_chunks_by_revision_range(
        &self,
        revision_id: Uuid,
        min_chunk_index: i32,
        max_chunk_index: i32,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.list_chunks_by_revision_range_impl(revision_id, min_chunk_index, max_chunk_index).await
    }

    async fn list_chunks_by_revision_windows(
        &self,
        revision_id: Uuid,
        windows: &[(i32, i32)],
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.list_chunks_by_revision_windows_impl(revision_id, windows).await
    }

    async fn list_chunks_by_revisions_windows(
        &self,
        windows: &[(Uuid, i32, i32)],
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.list_chunks_by_revisions_windows_impl(windows).await
    }

    async fn list_tail_chunks_by_revision(
        &self,
        revision_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.list_tail_chunks_by_revision_impl(revision_id, limit).await
    }

    async fn list_head_source_unit_blocks_by_revision(
        &self,
        revision_id: Uuid,
        limit: usize,
        temporal_start: Option<DateTime<Utc>>,
        temporal_end: Option<DateTime<Utc>>,
        release_marker_required: bool,
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockRow>> {
        self.list_head_source_unit_blocks_by_revision_impl(
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
        temporal_start: Option<DateTime<Utc>>,
        temporal_end: Option<DateTime<Utc>>,
        release_marker_required: bool,
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockRow>> {
        self.list_tail_source_unit_blocks_by_revision_impl(
            revision_id,
            limit,
            temporal_start,
            temporal_end,
            release_marker_required,
        )
        .await
    }

    async fn get_chunk(&self, chunk_id: Uuid) -> anyhow::Result<Option<KnowledgeChunkRow>> {
        self.get_chunk_impl(chunk_id).await
    }

    async fn list_chunks_by_ids(
        &self,
        chunk_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.list_chunks_by_ids_impl(chunk_ids).await
    }

    async fn search_code_pattern_chunks_by_terms(
        &self,
        library_id: Uuid,
        candidate_document_ids: &[Uuid],
        terms: &[String],
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.search_code_pattern_chunks_by_terms_impl(
            library_id,
            candidate_document_ids,
            terms,
            limit,
        )
        .await
    }

    async fn search_transport_pattern_chunks_by_terms(
        &self,
        library_id: Uuid,
        candidate_document_ids: &[Uuid],
        terms: &[String],
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.search_transport_pattern_chunks_by_terms_impl(
            library_id,
            candidate_document_ids,
            terms,
            limit,
        )
        .await
    }

    async fn delete_chunks_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        self.delete_chunks_by_revision_impl(revision_id).await
    }

    async fn replace_structured_revision_artifact(
        &self,
        row: &KnowledgeStructuredRevisionRow,
        blocks: &[KnowledgeStructuredBlockRow],
    ) -> anyhow::Result<KnowledgeStructuredRevisionRow> {
        self.replace_structured_revision_artifact_impl(row, blocks).await
    }

    async fn replace_structured_revision_artifact_fenced(
        &self,
        authority: IngestRevisionWriteAuthority,
        row: &KnowledgeStructuredRevisionRow,
        blocks: &[KnowledgeStructuredBlockRow],
    ) -> anyhow::Result<IngestAttemptMutationOutcome<KnowledgeStructuredRevisionRow>> {
        self.replace_structured_revision_artifact_fenced_impl(authority, row, blocks).await
    }

    async fn finalize_structured_revision_artifact_fenced(
        &self,
        authority: IngestRevisionWriteAuthority,
        expected_text_checksum: &str,
    ) -> anyhow::Result<IngestAttemptMutationOutcome<KnowledgeStructuredRevisionRow>> {
        self.finalize_structured_revision_artifact_fenced_impl(authority, expected_text_checksum)
            .await
    }

    async fn get_structured_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeStructuredRevisionRow>> {
        self.get_structured_revision_impl(revision_id).await
    }

    async fn get_structured_revision_counts(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Option<StructuredRevisionCounts>> {
        self.get_structured_revision_counts_impl(revision_id).await
    }

    async fn list_structured_revisions_by_revision_ids(
        &self,
        revision_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeStructuredRevisionRow>> {
        self.list_structured_revisions_by_revision_ids_impl(revision_ids).await
    }

    async fn list_structured_revisions_by_document(
        &self,
        document_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeStructuredRevisionRow>> {
        self.list_structured_revisions_by_document_impl(document_id).await
    }

    async fn list_structured_blocks_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockRow>> {
        self.list_structured_blocks_by_revision_impl(revision_id).await
    }

    async fn list_setup_structured_blocks_by_revision(
        &self,
        revision_id: Uuid,
        sample_limit: usize,
        structured_limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockRow>> {
        self.list_setup_structured_blocks_by_revision_impl(
            revision_id,
            sample_limit,
            structured_limit,
        )
        .await
    }

    async fn list_structured_blocks_page_by_revision(
        &self,
        revision_id: Uuid,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<(Vec<KnowledgeStructuredBlockRow>, usize)> {
        self.list_structured_blocks_page_by_revision_impl(revision_id, offset, limit).await
    }

    async fn list_chunk_support_references_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeChunkSupportReferenceRow>> {
        self.list_chunk_support_references_by_revision_impl(revision_id).await
    }

    async fn list_structured_blocks_by_ids(
        &self,
        block_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockRow>> {
        self.list_structured_blocks_by_ids_impl(block_ids).await
    }

    async fn delete_structured_blocks_by_revision(&self, revision_id: Uuid) -> anyhow::Result<()> {
        self.delete_structured_blocks_by_revision_impl(revision_id).await
    }

    async fn replace_technical_facts(
        &self,
        revision_id: Uuid,
        rows: &[KnowledgeTechnicalFactRow],
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        self.replace_technical_facts_impl(revision_id, rows).await
    }

    async fn replace_technical_facts_fenced(
        &self,
        authority: IngestRevisionWriteAuthority,
        rows: &[KnowledgeTechnicalFactRow],
    ) -> anyhow::Result<IngestAttemptMutationOutcome<Vec<KnowledgeTechnicalFactRow>>> {
        let mut transaction =
            self.pool.begin().await.context("failed to start fenced technical fact replacement")?;
        if !lock_ingest_revision_write_authority(&mut transaction, authority).await? {
            transaction
                .commit()
                .await
                .context("failed to finish preserved stale technical fact replacement")?;
            return Ok(IngestAttemptMutationOutcome::AuthorityLost);
        }
        let inserted =
            replace_technical_facts_in_transaction(&mut transaction, authority.revision_id, rows)
                .await?;
        transaction.commit().await.context("failed to commit fenced technical fact replacement")?;
        Ok(IngestAttemptMutationOutcome::Applied(inserted))
    }

    async fn list_technical_facts_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        self.list_technical_facts_by_revision_impl(revision_id).await
    }

    async fn count_technical_facts_by_revision(&self, revision_id: Uuid) -> anyhow::Result<i64> {
        self.count_technical_facts_by_revision_impl(revision_id).await
    }

    async fn list_technical_facts_by_ids(
        &self,
        fact_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        self.list_technical_facts_by_ids_impl(fact_ids).await
    }

    async fn list_technical_facts_by_chunk_ids(
        &self,
        chunk_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        self.list_technical_facts_by_chunk_ids_impl(chunk_ids).await
    }

    async fn list_technical_facts_by_document(
        &self,
        document_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        self.list_technical_facts_by_document_impl(document_id).await
    }

    async fn delete_technical_facts_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        self.delete_technical_facts_by_revision_impl(revision_id).await
    }
}
