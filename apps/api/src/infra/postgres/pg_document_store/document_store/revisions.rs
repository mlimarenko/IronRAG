use super::*;

impl PgDocumentStore {
    pub(super) async fn upsert_revision_impl(
        &self,
        row: &KnowledgeRevisionRow,
    ) -> anyhow::Result<KnowledgeRevisionRow> {
        sqlx::query_as::<_, KnowledgeRevisionRow>(sqlx::AssertSqlSafe(format!(
            "INSERT INTO knowledge_revision (
                revision_id, workspace_id, library_id, document_id, revision_number,
                revision_state, revision_kind, storage_ref, source_uri, document_hint, mime_type,
                checksum, title, byte_size, normalized_text, text_checksum, image_checksum,
                text_state, vector_state, graph_state, text_readable_at, vector_ready_at,
                graph_ready_at, superseded_by_revision_id, created_at
             ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25)
             ON CONFLICT (revision_id) DO UPDATE SET
                workspace_id = EXCLUDED.workspace_id,
                library_id = EXCLUDED.library_id,
                document_id = EXCLUDED.document_id,
                revision_number = EXCLUDED.revision_number,
                revision_state = EXCLUDED.revision_state,
                revision_kind = EXCLUDED.revision_kind,
                storage_ref = EXCLUDED.storage_ref,
                source_uri = EXCLUDED.source_uri,
                document_hint = EXCLUDED.document_hint,
                mime_type = EXCLUDED.mime_type,
                checksum = EXCLUDED.checksum,
                title = EXCLUDED.title,
                byte_size = EXCLUDED.byte_size,
                normalized_text = EXCLUDED.normalized_text,
                text_checksum = EXCLUDED.text_checksum,
                image_checksum = EXCLUDED.image_checksum,
                text_state = EXCLUDED.text_state,
                vector_state = EXCLUDED.vector_state,
                graph_state = EXCLUDED.graph_state,
                text_readable_at = EXCLUDED.text_readable_at,
                vector_ready_at = EXCLUDED.vector_ready_at,
                graph_ready_at = EXCLUDED.graph_ready_at,
                superseded_by_revision_id = EXCLUDED.superseded_by_revision_id
             RETURNING {REVISION_COLUMNS}"
        )))
        .bind(row.revision_id)
        .bind(row.workspace_id)
        .bind(row.library_id)
        .bind(row.document_id)
        .bind(row.revision_number)
        .bind(&row.revision_state)
        .bind(&row.revision_kind)
        .bind(&row.storage_ref)
        .bind(&row.source_uri)
        .bind(&row.document_hint)
        .bind(&row.mime_type)
        .bind(&row.checksum)
        .bind(&row.title)
        .bind(row.byte_size)
        .bind(&row.normalized_text)
        .bind(&row.text_checksum)
        .bind(&row.image_checksum)
        .bind(&row.text_state)
        .bind(&row.vector_state)
        .bind(&row.graph_state)
        .bind(row.text_readable_at)
        .bind(row.vector_ready_at)
        .bind(row.graph_ready_at)
        .bind(row.superseded_by_revision_id)
        .bind(row.created_at)
        .fetch_one(&self.pool)
        .await
        .context("failed to upsert knowledge revision")
    }

    pub(super) async fn update_revision_document_hint_impl(
        &self,
        revision_id: Uuid,
        document_hint: Option<&str>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>> {
        sqlx::query_as::<_, KnowledgeRevisionRow>(sqlx::AssertSqlSafe(format!(
            "UPDATE knowledge_revision SET document_hint = $2
             WHERE revision_id = $1
             RETURNING {REVISION_COLUMNS}"
        )))
        .bind(revision_id)
        .bind(document_hint)
        .fetch_optional(&self.pool)
        .await
        .context("failed to update knowledge revision document hint")
    }

    pub(super) async fn get_revision_impl(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>> {
        sqlx::query_as::<_, KnowledgeRevisionRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {REVISION_COLUMNS} FROM knowledge_revision WHERE revision_id = $1 LIMIT 1"
        )))
        .bind(revision_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get knowledge revision")
    }

    pub(super) async fn list_revisions_by_ids_impl(
        &self,
        revision_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeRevisionRow>> {
        if revision_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, KnowledgeRevisionRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {REVISION_COLUMNS}
             FROM knowledge_revision
             WHERE revision_id = ANY($1)"
        )))
        .bind(revision_ids)
        .fetch_all(&self.pool)
        .await
        .context("failed to list knowledge revisions by ids")
    }

    pub(super) async fn list_revisions_by_document_impl(
        &self,
        document_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRevisionRow>> {
        sqlx::query_as::<_, KnowledgeRevisionRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {REVISION_COLUMNS}
             FROM knowledge_revision
             WHERE document_id = $1
             ORDER BY revision_number DESC, revision_id DESC"
        )))
        .bind(document_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list knowledge revisions by document")
    }

    pub(super) async fn aggregate_library_generation_signals_impl(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<LibraryGenerationSignals> {
        sqlx::query_as::<_, LibraryGenerationSignals>(
            "SELECT
                COALESCE(MAX(revision_number) FILTER (WHERE text_state = 'text_readable'), 0) AS active_text_generation,
                COALESCE(MAX(revision_number) FILTER (WHERE vector_state = 'ready'), 0) AS active_vector_generation,
                COALESCE(MAX(revision_number) FILTER (WHERE graph_state = 'ready'), 0) AS active_graph_generation,
                COALESCE(bool_or(text_state = 'text_readable'), false) AS has_ready_text,
                COALESCE(bool_or(vector_state = 'ready'), false) AS has_ready_vector,
                COALESCE(bool_or(graph_state = 'ready'), false) AS has_ready_graph,
                MAX(created_at) AS latest_created_at
             FROM knowledge_revision
             WHERE library_id = $1",
        )
        .bind(library_id)
        .fetch_one(&self.pool)
        .await
        .context("failed to aggregate library generation signals")
    }

    /// Documents whose current revision claims a ready vector index and has no
    /// chunk vector to show for it: `vector_state = 'ready'`, not superseded,
    /// chunks present, and no row in any chunk-vector relation of the library.
    ///
    /// Scoped to the canonical active-document / current-head population, the
    /// same one the dashboard counts. A revision the document no longer serves
    /// (`knowledge_document.readable_revision_id` moved on) or one belonging to a
    /// retired document can still read `vector_state = 'ready'` with
    /// `superseded_by_revision_id IS NULL` — a replaced document's predecessor,
    /// for instance — and would otherwise be reported as a live vector gap the
    /// operator has no document to resolve.
    ///
    /// The dashboard's `vector_index_gap` notice and the documents-list
    /// `indexing=vector_gap` filter are the same fact seen twice — the notice
    /// counts this list — so the number an operator is shown and the set they
    /// are sent to cannot disagree.
    pub(super) async fn list_documents_missing_chunk_vectors_impl(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<Uuid>> {
        let ready_revisions = sqlx::query_as::<_, (Uuid, Uuid)>(
            "SELECT r.revision_id, r.document_id
             FROM knowledge_revision r
             JOIN knowledge_document kd
               ON kd.document_id = r.document_id
              AND kd.workspace_id = r.workspace_id
              AND kd.library_id = r.library_id
             WHERE r.library_id = $1
               AND kd.document_state = 'active'
               AND kd.deleted_at IS NULL
               AND kd.readable_revision_id = r.revision_id
               AND r.vector_state = 'ready'
               AND r.superseded_by_revision_id IS NULL
               AND EXISTS (
                 SELECT 1
                 FROM knowledge_chunk c
                 WHERE c.revision_id = r.revision_id
                 LIMIT 1
               )",
        )
        .bind(library_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list vector-ready revisions with chunks")?;

        if ready_revisions.is_empty() {
            return Ok(Vec::new());
        }
        let ready_revision_ids =
            ready_revisions.iter().map(|(revision_id, _)| *revision_id).collect::<Vec<_>>();

        let relation_names = sqlx::query_as::<_, (String,)>(
            "SELECT DISTINCT relation_name
             FROM knowledge_vector_relation_manifest
             WHERE library_id = $1 AND vector_kind = $2",
        )
        .bind(library_id)
        .bind(KNOWLEDGE_CHUNK_VECTOR_KIND)
        .fetch_all(&self.pool)
        .await
        .context("failed to list chunk vector relations")?
        .into_iter()
        .map(|(relation_name,)| relation_name)
        .collect::<Vec<_>>();

        let mut vector_revision_ids = HashSet::new();
        for relation_name in relation_names {
            let sql = format!(
                "SELECT DISTINCT revision_id FROM {} WHERE library_id = $1 AND revision_id = ANY($2) AND vector_kind = $3",
                quote_relation_name(&relation_name)
            );
            let rows = sqlx::query_as::<_, (Uuid,)>(sqlx::AssertSqlSafe(&*sql))
                .bind(library_id)
                .bind(&ready_revision_ids)
                .bind(KNOWLEDGE_CHUNK_VECTOR_KIND)
                .fetch_all(&self.pool)
                .await
                .with_context(|| format!("failed to count vector inventory in {relation_name}"))?;
            vector_revision_ids.extend(rows.into_iter().map(|(revision_id,)| revision_id));
        }

        // A document can hold several unsuperseded vector-ready revisions; it is
        // one entry in the gap either way, because the list surface this feeds
        // is a set of documents.
        let mut documents = ready_revisions
            .into_iter()
            .filter(|(revision_id, _)| !vector_revision_ids.contains(revision_id))
            .map(|(_, document_id)| document_id)
            .collect::<Vec<_>>();
        documents.sort_unstable();
        documents.dedup();
        Ok(documents)
    }

    pub(super) async fn update_revision_readiness_impl(
        &self,
        update: RevisionReadinessUpdate<'_>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>> {
        let RevisionReadinessUpdate {
            revision_id,
            text_state,
            vector_state,
            graph_state,
            text_readable_at,
            vector_ready_at,
            graph_ready_at,
            superseded_by_revision_id,
        } = update;
        sqlx::query_as::<_, KnowledgeRevisionRow>(sqlx::AssertSqlSafe(format!(
            "UPDATE knowledge_revision SET
                text_state = $2,
                vector_state = $3,
                graph_state = $4,
                text_readable_at = $5,
                vector_ready_at = $6,
                graph_ready_at = $7,
                superseded_by_revision_id = $8
             WHERE revision_id = $1
             RETURNING {REVISION_COLUMNS}"
        )))
        .bind(revision_id)
        .bind(text_state)
        .bind(vector_state)
        .bind(graph_state)
        .bind(text_readable_at)
        .bind(vector_ready_at)
        .bind(graph_ready_at)
        .bind(superseded_by_revision_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to update knowledge revision readiness")
    }

    pub(super) async fn update_revision_text_content_impl(
        &self,
        revision_id: Uuid,
        normalized_text: Option<&str>,
        text_checksum: Option<&str>,
        text_state: &str,
        text_readable_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>> {
        sqlx::query_as::<_, KnowledgeRevisionRow>(sqlx::AssertSqlSafe(format!(
            "UPDATE knowledge_revision SET
                normalized_text = $2,
                text_checksum = $3,
                text_state = $4,
                text_readable_at = $5
             WHERE revision_id = $1
             RETURNING {REVISION_COLUMNS}"
        )))
        .bind(revision_id)
        .bind(normalized_text)
        .bind(text_checksum)
        .bind(text_state)
        .bind(text_readable_at)
        .fetch_optional(&self.pool)
        .await
        .context("failed to update knowledge revision text content")
    }

    pub(super) async fn update_revision_image_checksum_impl(
        &self,
        revision_id: Uuid,
        image_checksum: Option<&str>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>> {
        sqlx::query_as::<_, KnowledgeRevisionRow>(sqlx::AssertSqlSafe(format!(
            "UPDATE knowledge_revision SET image_checksum = $2
             WHERE revision_id = $1
             RETURNING {REVISION_COLUMNS}"
        )))
        .bind(revision_id)
        .bind(image_checksum)
        .fetch_optional(&self.pool)
        .await
        .context("failed to update knowledge revision image checksum")
    }

    pub(super) async fn update_revision_storage_ref_impl(
        &self,
        revision_id: Uuid,
        storage_ref: Option<&str>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>> {
        sqlx::query_as::<_, KnowledgeRevisionRow>(sqlx::AssertSqlSafe(format!(
            "UPDATE knowledge_revision SET storage_ref = $2
             WHERE revision_id = $1
             RETURNING {REVISION_COLUMNS}"
        )))
        .bind(revision_id)
        .bind(storage_ref)
        .fetch_optional(&self.pool)
        .await
        .context("failed to update knowledge revision storage ref")
    }
}
