use super::*;

impl PgDocumentStore {
    pub(super) async fn upsert_document_impl(
        &self,
        row: &KnowledgeDocumentRow,
    ) -> anyhow::Result<KnowledgeDocumentRow> {
        sqlx::query_as::<_, KnowledgeDocumentRow>(sqlx::AssertSqlSafe(format!(
            "INSERT INTO knowledge_document (
                document_id, workspace_id, library_id, external_key, file_name, title,
                document_state, active_revision_id, readable_revision_id, latest_revision_no,
                parent_document_id, document_role,
                created_at, updated_at, deleted_at
             ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)
             ON CONFLICT (document_id) DO UPDATE SET
                workspace_id = EXCLUDED.workspace_id,
                library_id = EXCLUDED.library_id,
                external_key = EXCLUDED.external_key,
                file_name = EXCLUDED.file_name,
                title = EXCLUDED.title,
                document_state = EXCLUDED.document_state,
                active_revision_id = EXCLUDED.active_revision_id,
                readable_revision_id = EXCLUDED.readable_revision_id,
                latest_revision_no = EXCLUDED.latest_revision_no,
                parent_document_id = EXCLUDED.parent_document_id,
                document_role = EXCLUDED.document_role,
                updated_at = EXCLUDED.updated_at,
                deleted_at = EXCLUDED.deleted_at
             RETURNING {DOCUMENT_COLUMNS}"
        )))
        .bind(row.document_id)
        .bind(row.workspace_id)
        .bind(row.library_id)
        .bind(&row.external_key)
        .bind(&row.file_name)
        .bind(&row.title)
        .bind(&row.document_state)
        .bind(row.active_revision_id)
        .bind(row.readable_revision_id)
        .bind(row.latest_revision_no)
        .bind(row.parent_document_id)
        .bind(&row.document_role)
        .bind(row.created_at)
        .bind(row.updated_at)
        .bind(row.deleted_at)
        .fetch_one(&self.pool)
        .await
        .context("failed to upsert knowledge document")
    }

    pub(super) async fn get_document_impl(
        &self,
        document_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeDocumentRow>> {
        sqlx::query_as::<_, KnowledgeDocumentRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {DOCUMENT_COLUMNS} FROM knowledge_document WHERE document_id = $1 LIMIT 1"
        )))
        .bind(document_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get knowledge document")
    }

    pub(super) async fn get_document_by_external_key_impl(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
        external_key: &str,
    ) -> anyhow::Result<Option<KnowledgeDocumentRow>> {
        sqlx::query_as::<_, KnowledgeDocumentRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {DOCUMENT_COLUMNS}
             FROM knowledge_document
             WHERE workspace_id = $1 AND library_id = $2 AND external_key = $3
             LIMIT 1"
        )))
        .bind(workspace_id)
        .bind(library_id)
        .bind(external_key)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get knowledge document by external key")
    }

    pub(super) async fn list_documents_by_library_impl(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
        include_deleted: bool,
    ) -> anyhow::Result<Vec<KnowledgeDocumentRow>> {
        sqlx::query_as::<_, KnowledgeDocumentRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {DOCUMENT_COLUMNS}
             FROM knowledge_document
             WHERE workspace_id = $1
               AND library_id = $2
               AND ($3 OR (document_state <> 'deleted' AND deleted_at IS NULL))
             ORDER BY updated_at DESC, document_id DESC"
        )))
        .bind(workspace_id)
        .bind(library_id)
        .bind(include_deleted)
        .fetch_all(&self.pool)
        .await
        .context("failed to list knowledge documents by library")
    }

    pub(super) async fn list_documents_by_ids_impl(
        &self,
        document_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeDocumentRow>> {
        if document_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, KnowledgeDocumentRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {DOCUMENT_COLUMNS}
             FROM knowledge_document
             WHERE document_id = ANY($1)
               AND document_state <> 'deleted'
               AND deleted_at IS NULL
             ORDER BY updated_at DESC, document_id DESC"
        )))
        .bind(document_ids)
        .fetch_all(&self.pool)
        .await
        .context("failed to list knowledge documents by ids")
    }

    pub(super) async fn update_document_pointers_impl(
        &self,
        update: DocumentPointersUpdate<'_>,
    ) -> anyhow::Result<Option<KnowledgeDocumentRow>> {
        let DocumentPointersUpdate {
            document_id,
            document_state,
            active_revision_id,
            readable_revision_id,
            latest_revision_no,
            title,
            deleted_at,
        } = update;
        sqlx::query_as::<_, KnowledgeDocumentRow>(sqlx::AssertSqlSafe(format!(
            "UPDATE knowledge_document SET
                document_state = $2,
                active_revision_id = $3,
                readable_revision_id = $4,
                latest_revision_no = $5,
                title = $6,
                updated_at = now(),
                deleted_at = $7
             WHERE document_id = $1
             RETURNING {DOCUMENT_COLUMNS}"
        )))
        .bind(document_id)
        .bind(document_state)
        .bind(active_revision_id)
        .bind(readable_revision_id)
        .bind(latest_revision_no)
        .bind(title)
        .bind(deleted_at)
        .fetch_optional(&self.pool)
        .await
        .context("failed to update knowledge document pointers")
    }
}
