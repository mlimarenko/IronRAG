use super::*;

impl PgGraphStore {
    pub(super) async fn upsert_entity_candidate_impl(
        &self,
        input: &NewKnowledgeEntityCandidate,
    ) -> anyhow::Result<KnowledgeEntityCandidateRow> {
        let sql = format!(
            "with upserted as (
                insert into knowledge_entity_candidate (
                    candidate_id, workspace_id, library_id, revision_id, chunk_id,
                    candidate_label, candidate_type, candidate_sub_type, normalization_key,
                    confidence, extraction_method, candidate_state, created_at, updated_at
                ) values (
                    $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                    coalesce($13, now()), coalesce($14, now())
                )
                on conflict (candidate_id) do update
                set workspace_id = excluded.workspace_id,
                    library_id = excluded.library_id,
                    revision_id = excluded.revision_id,
                    chunk_id = excluded.chunk_id,
                    candidate_label = excluded.candidate_label,
                    candidate_type = excluded.candidate_type,
                    candidate_sub_type = excluded.candidate_sub_type,
                    normalization_key = excluded.normalization_key,
                    confidence = excluded.confidence,
                    extraction_method = excluded.extraction_method,
                    candidate_state = excluded.candidate_state,
                    updated_at = excluded.updated_at
                returning *
            )
            select to_jsonb(row) from ({ENTITY_CANDIDATE_ROW_SQL} from upserted) row"
        );
        let row = sqlx::query_scalar::<_, serde_json::Value>(sqlx::AssertSqlSafe(&*sql))
            .bind(input.candidate_id)
            .bind(input.workspace_id)
            .bind(input.library_id)
            .bind(input.revision_id)
            .bind(input.chunk_id)
            .bind(&input.candidate_label)
            .bind(&input.candidate_type)
            .bind(input.candidate_sub_type.as_deref())
            .bind(&input.normalization_key)
            .bind(input.confidence)
            .bind(&input.extraction_method)
            .bind(&input.candidate_state)
            .bind(input.created_at.as_ref())
            .bind(input.updated_at.as_ref())
            .fetch_one(&self.pool)
            .await
            .context("failed to upsert knowledge entity candidate")?;
        decode_json(row, "failed to decode knowledge entity candidate row")
    }

    pub(super) async fn upsert_entity_candidates_impl(
        &self,
        inputs: &[NewKnowledgeEntityCandidate],
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>> {
        let mut rows = Vec::with_capacity(inputs.len());
        for input in inputs {
            rows.push(self.upsert_entity_candidate(input).await?);
        }
        Ok(rows)
    }

    pub(super) async fn list_entity_candidates_by_revision_impl(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>> {
        let sql = format!(
            "select to_jsonb(row)
             from ({ENTITY_CANDIDATE_ROW_SQL}
                   from knowledge_entity_candidate
                   where revision_id = $1
                   order by created_at asc, candidate_id asc) row"
        );
        let rows = sqlx::query_scalar::<_, serde_json::Value>(sqlx::AssertSqlSafe(&*sql))
            .bind(revision_id)
            .fetch_all(&self.pool)
            .await
            .context("failed to list knowledge entity candidates by revision")?;
        decode_json_rows(rows, "failed to decode knowledge entity candidate rows")
    }

    pub(super) async fn list_entity_candidates_by_library_impl(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>> {
        let sql = format!(
            "select to_jsonb(row)
             from ({ENTITY_CANDIDATE_ROW_SQL}
                   from knowledge_entity_candidate
                   where library_id = $1
                   order by created_at asc, candidate_id asc) row"
        );
        let rows = sqlx::query_scalar::<_, serde_json::Value>(sqlx::AssertSqlSafe(&*sql))
            .bind(library_id)
            .fetch_all(&self.pool)
            .await
            .context("failed to list knowledge entity candidates by library")?;
        decode_json_rows(rows, "failed to decode knowledge entity candidate rows")
    }

    pub(super) async fn delete_entity_candidates_by_revision_impl(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>> {
        let sql = format!(
            "with deleted as (
                delete from knowledge_entity_candidate
                where revision_id = $1
                returning *
            )
            select to_jsonb(row)
            from ({ENTITY_CANDIDATE_ROW_SQL}
                  from deleted
                  order by created_at asc, candidate_id asc) row"
        );
        let rows = sqlx::query_scalar::<_, serde_json::Value>(sqlx::AssertSqlSafe(&*sql))
            .bind(revision_id)
            .fetch_all(&self.pool)
            .await
            .context("failed to delete knowledge entity candidates by revision")?;
        decode_json_rows(rows, "failed to decode deleted knowledge entity candidates")
    }

    pub(super) async fn delete_entity_candidates_by_library_impl(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>> {
        let sql = format!(
            "with deleted as (
                delete from knowledge_entity_candidate
                where library_id = $1
                returning *
            )
            select to_jsonb(row)
            from ({ENTITY_CANDIDATE_ROW_SQL}
                  from deleted
                  order by created_at asc, candidate_id asc) row"
        );
        let rows = sqlx::query_scalar::<_, serde_json::Value>(sqlx::AssertSqlSafe(&*sql))
            .bind(library_id)
            .fetch_all(&self.pool)
            .await
            .context("failed to delete knowledge entity candidates by library")?;
        decode_json_rows(rows, "failed to decode deleted knowledge entity candidates")
    }

    pub(super) async fn upsert_relation_candidate_impl(
        &self,
        input: &NewKnowledgeRelationCandidate,
    ) -> anyhow::Result<KnowledgeRelationCandidateRow> {
        let sql = format!(
            "with upserted as (
                insert into knowledge_relation_candidate (
                    candidate_id, workspace_id, library_id, revision_id, chunk_id,
                    subject_label, subject_candidate_key, predicate, object_label,
                    object_candidate_key, normalized_assertion, confidence, extraction_method,
                    candidate_state, created_at, updated_at
                ) values (
                    $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14,
                    coalesce($15, now()), coalesce($16, now())
                )
                on conflict (candidate_id) do update
                set workspace_id = excluded.workspace_id,
                    library_id = excluded.library_id,
                    revision_id = excluded.revision_id,
                    chunk_id = excluded.chunk_id,
                    subject_label = excluded.subject_label,
                    subject_candidate_key = excluded.subject_candidate_key,
                    predicate = excluded.predicate,
                    object_label = excluded.object_label,
                    object_candidate_key = excluded.object_candidate_key,
                    normalized_assertion = excluded.normalized_assertion,
                    confidence = excluded.confidence,
                    extraction_method = excluded.extraction_method,
                    candidate_state = excluded.candidate_state,
                    updated_at = excluded.updated_at
                returning *
            )
            select to_jsonb(row) from ({RELATION_CANDIDATE_ROW_SQL} from upserted) row"
        );
        let row = sqlx::query_scalar::<_, serde_json::Value>(sqlx::AssertSqlSafe(&*sql))
            .bind(input.candidate_id)
            .bind(input.workspace_id)
            .bind(input.library_id)
            .bind(input.revision_id)
            .bind(input.chunk_id)
            .bind(&input.subject_label)
            .bind(&input.subject_candidate_key)
            .bind(&input.predicate)
            .bind(&input.object_label)
            .bind(&input.object_candidate_key)
            .bind(&input.normalized_assertion)
            .bind(input.confidence)
            .bind(&input.extraction_method)
            .bind(&input.candidate_state)
            .bind(input.created_at.as_ref())
            .bind(input.updated_at.as_ref())
            .fetch_one(&self.pool)
            .await
            .context("failed to upsert knowledge relation candidate")?;
        decode_json(row, "failed to decode knowledge relation candidate row")
    }

    pub(super) async fn upsert_relation_candidates_impl(
        &self,
        inputs: &[NewKnowledgeRelationCandidate],
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>> {
        let mut rows = Vec::with_capacity(inputs.len());
        for input in inputs {
            rows.push(self.upsert_relation_candidate(input).await?);
        }
        Ok(rows)
    }

    pub(super) async fn list_relation_candidates_by_revision_impl(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>> {
        let sql = format!(
            "select to_jsonb(row)
             from ({RELATION_CANDIDATE_ROW_SQL}
                   from knowledge_relation_candidate
                   where revision_id = $1
                   order by created_at asc, candidate_id asc) row"
        );
        let rows = sqlx::query_scalar::<_, serde_json::Value>(sqlx::AssertSqlSafe(&*sql))
            .bind(revision_id)
            .fetch_all(&self.pool)
            .await
            .context("failed to list knowledge relation candidates by revision")?;
        decode_json_rows(rows, "failed to decode knowledge relation candidate rows")
    }

    pub(super) async fn list_relation_candidates_by_library_impl(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>> {
        let sql = format!(
            "select to_jsonb(row)
             from ({RELATION_CANDIDATE_ROW_SQL}
                   from knowledge_relation_candidate
                   where library_id = $1
                   order by created_at asc, candidate_id asc) row"
        );
        let rows = sqlx::query_scalar::<_, serde_json::Value>(sqlx::AssertSqlSafe(&*sql))
            .bind(library_id)
            .fetch_all(&self.pool)
            .await
            .context("failed to list knowledge relation candidates by library")?;
        decode_json_rows(rows, "failed to decode knowledge relation candidate rows")
    }

    pub(super) async fn delete_relation_candidates_by_revision_impl(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>> {
        let sql = format!(
            "with deleted as (
                delete from knowledge_relation_candidate
                where revision_id = $1
                returning *
            )
            select to_jsonb(row)
            from ({RELATION_CANDIDATE_ROW_SQL}
                  from deleted
                  order by created_at asc, candidate_id asc) row"
        );
        let rows = sqlx::query_scalar::<_, serde_json::Value>(sqlx::AssertSqlSafe(&*sql))
            .bind(revision_id)
            .fetch_all(&self.pool)
            .await
            .context("failed to delete knowledge relation candidates by revision")?;
        decode_json_rows(rows, "failed to decode deleted knowledge relation candidates")
    }

    pub(super) async fn delete_relation_candidates_by_library_impl(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>> {
        let sql = format!(
            "with deleted as (
                delete from knowledge_relation_candidate
                where library_id = $1
                returning *
            )
            select to_jsonb(row)
            from ({RELATION_CANDIDATE_ROW_SQL}
                  from deleted
                  order by created_at asc, candidate_id asc) row"
        );
        let rows = sqlx::query_scalar::<_, serde_json::Value>(sqlx::AssertSqlSafe(&*sql))
            .bind(library_id)
            .fetch_all(&self.pool)
            .await
            .context("failed to delete knowledge relation candidates by library")?;
        decode_json_rows(rows, "failed to decode deleted knowledge relation candidates")
    }
}
