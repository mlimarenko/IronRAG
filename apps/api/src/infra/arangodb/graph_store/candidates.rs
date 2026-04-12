use super::*;

impl ArangoGraphStore {
    pub async fn upsert_entity_candidate(
        &self,
        input: &NewKnowledgeEntityCandidate,
    ) -> anyhow::Result<KnowledgeEntityCandidateRow> {
        let mut rows = self.upsert_entity_candidates(std::slice::from_ref(input)).await?;
        rows.pop().ok_or_else(|| anyhow!("ArangoDB query returned no entity candidate rows"))
    }

    pub async fn upsert_entity_candidates(
        &self,
        inputs: &[NewKnowledgeEntityCandidate],
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let rows = inputs
            .iter()
            .map(|input| {
                serde_json::json!({
                    "key": input.candidate_id,
                    "candidate_id": input.candidate_id,
                    "workspace_id": input.workspace_id,
                    "library_id": input.library_id,
                    "revision_id": input.revision_id,
                    "chunk_id": input.chunk_id,
                    "candidate_label": input.candidate_label,
                    "candidate_type": input.candidate_type,
                    "candidate_sub_type": input.candidate_sub_type,
                    "normalization_key": input.normalization_key,
                    "confidence": input.confidence,
                    "extraction_method": input.extraction_method,
                    "candidate_state": input.candidate_state,
                    "created_at": input.created_at.unwrap_or_else(Utc::now),
                    "updated_at": input.updated_at.unwrap_or_else(Utc::now),
                })
            })
            .collect::<Vec<_>>();
        let cursor = self
            .run_retryable_upsert_query(
                "FOR row IN @rows
                 UPSERT { _key: row.key }
                 INSERT {
                    _key: row.key,
                    candidate_id: row.candidate_id,
                    workspace_id: row.workspace_id,
                    library_id: row.library_id,
                    revision_id: row.revision_id,
                    chunk_id: row.chunk_id,
                    candidate_label: row.candidate_label,
                    candidate_type: row.candidate_type,
                    candidate_sub_type: row.candidate_sub_type,
                    normalization_key: row.normalization_key,
                    confidence: row.confidence,
                    extraction_method: row.extraction_method,
                    candidate_state: row.candidate_state,
                    created_at: row.created_at,
                    updated_at: row.updated_at
                 }
                 UPDATE {
                    workspace_id: row.workspace_id,
                    library_id: row.library_id,
                    revision_id: row.revision_id,
                    chunk_id: row.chunk_id,
                    candidate_label: row.candidate_label,
                    candidate_type: row.candidate_type,
                    candidate_sub_type: row.candidate_sub_type,
                    normalization_key: row.normalization_key,
                    confidence: row.confidence,
                    extraction_method: row.extraction_method,
                    candidate_state: row.candidate_state,
                    updated_at: row.updated_at
                 }
                 IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_ENTITY_CANDIDATE_COLLECTION,
                    "rows": rows,
                }),
                "failed to upsert knowledge entity candidates",
            )
            .await?;
        decode_many_results(cursor)
    }

    pub async fn list_entity_candidates_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR candidate IN @@collection
                 FILTER candidate.revision_id == @revision_id
                 SORT candidate.created_at ASC, candidate.candidate_id ASC
                 RETURN candidate",
                serde_json::json!({
                    "@collection": KNOWLEDGE_ENTITY_CANDIDATE_COLLECTION,
                    "revision_id": revision_id,
                }),
            )
            .await
            .context("failed to list knowledge entity candidates by revision")?;
        decode_many_results(cursor)
    }

    pub async fn list_entity_candidates_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR candidate IN @@collection
                 FILTER candidate.library_id == @library_id
                 SORT candidate.created_at ASC, candidate.candidate_id ASC
                 RETURN candidate",
                serde_json::json!({
                    "@collection": KNOWLEDGE_ENTITY_CANDIDATE_COLLECTION,
                    "library_id": library_id,
                }),
            )
            .await
            .context("failed to list knowledge entity candidates by library")?;
        decode_many_results(cursor)
    }

    pub async fn delete_entity_candidates_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR candidate IN @@collection
                 FILTER candidate.revision_id == @revision_id
                 REMOVE candidate IN @@collection
                 RETURN OLD",
                serde_json::json!({
                    "@collection": KNOWLEDGE_ENTITY_CANDIDATE_COLLECTION,
                    "revision_id": revision_id,
                }),
            )
            .await
            .context("failed to delete knowledge entity candidates by revision")?;
        decode_many_results(cursor)
    }

    pub async fn delete_entity_candidates_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR candidate IN @@collection
                 FILTER candidate.library_id == @library_id
                 REMOVE candidate IN @@collection
                 RETURN OLD",
                serde_json::json!({
                    "@collection": KNOWLEDGE_ENTITY_CANDIDATE_COLLECTION,
                    "library_id": library_id,
                }),
            )
            .await
            .context("failed to delete knowledge entity candidates by library")?;
        decode_many_results(cursor)
    }

    pub async fn upsert_relation_candidate(
        &self,
        input: &NewKnowledgeRelationCandidate,
    ) -> anyhow::Result<KnowledgeRelationCandidateRow> {
        let mut rows = self.upsert_relation_candidates(std::slice::from_ref(input)).await?;
        rows.pop().ok_or_else(|| anyhow!("ArangoDB query returned no relation candidate rows"))
    }

    pub async fn upsert_relation_candidates(
        &self,
        inputs: &[NewKnowledgeRelationCandidate],
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let rows = inputs
            .iter()
            .map(|input| {
                serde_json::json!({
                    "key": input.candidate_id,
                    "candidate_id": input.candidate_id,
                    "workspace_id": input.workspace_id,
                    "library_id": input.library_id,
                    "revision_id": input.revision_id,
                    "chunk_id": input.chunk_id,
                    "subject_label": input.subject_label,
                    "subject_candidate_key": input.subject_candidate_key,
                    "predicate": input.predicate,
                    "object_label": input.object_label,
                    "object_candidate_key": input.object_candidate_key,
                    "normalized_assertion": input.normalized_assertion,
                    "confidence": input.confidence,
                    "extraction_method": input.extraction_method,
                    "candidate_state": input.candidate_state,
                    "created_at": input.created_at.unwrap_or_else(Utc::now),
                    "updated_at": input.updated_at.unwrap_or_else(Utc::now),
                })
            })
            .collect::<Vec<_>>();
        let cursor = self
            .run_retryable_upsert_query(
                "FOR row IN @rows
                 UPSERT { _key: row.key }
                 INSERT {
                    _key: row.key,
                    candidate_id: row.candidate_id,
                    workspace_id: row.workspace_id,
                    library_id: row.library_id,
                    revision_id: row.revision_id,
                    chunk_id: row.chunk_id,
                    subject_label: row.subject_label,
                    subject_candidate_key: row.subject_candidate_key,
                    predicate: row.predicate,
                    object_label: row.object_label,
                    object_candidate_key: row.object_candidate_key,
                    normalized_assertion: row.normalized_assertion,
                    confidence: row.confidence,
                    extraction_method: row.extraction_method,
                    candidate_state: row.candidate_state,
                    created_at: row.created_at,
                    updated_at: row.updated_at
                 }
                 UPDATE {
                    workspace_id: row.workspace_id,
                    library_id: row.library_id,
                    revision_id: row.revision_id,
                    chunk_id: row.chunk_id,
                    subject_label: row.subject_label,
                    subject_candidate_key: row.subject_candidate_key,
                    predicate: row.predicate,
                    object_label: row.object_label,
                    object_candidate_key: row.object_candidate_key,
                    normalized_assertion: row.normalized_assertion,
                    confidence: row.confidence,
                    extraction_method: row.extraction_method,
                    candidate_state: row.candidate_state,
                    updated_at: row.updated_at
                 }
                 IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_RELATION_CANDIDATE_COLLECTION,
                    "rows": rows,
                }),
                "failed to upsert knowledge relation candidates",
            )
            .await?;
        decode_many_results(cursor)
    }

    pub async fn list_relation_candidates_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR candidate IN @@collection
                 FILTER candidate.revision_id == @revision_id
                 SORT candidate.created_at ASC, candidate.candidate_id ASC
                 RETURN candidate",
                serde_json::json!({
                    "@collection": KNOWLEDGE_RELATION_CANDIDATE_COLLECTION,
                    "revision_id": revision_id,
                }),
            )
            .await
            .context("failed to list knowledge relation candidates by revision")?;
        decode_many_results(cursor)
    }

    pub async fn list_relation_candidates_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR candidate IN @@collection
                 FILTER candidate.library_id == @library_id
                 SORT candidate.created_at ASC, candidate.candidate_id ASC
                 RETURN candidate",
                serde_json::json!({
                    "@collection": KNOWLEDGE_RELATION_CANDIDATE_COLLECTION,
                    "library_id": library_id,
                }),
            )
            .await
            .context("failed to list knowledge relation candidates by library")?;
        decode_many_results(cursor)
    }

    pub async fn delete_relation_candidates_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR candidate IN @@collection
                 FILTER candidate.revision_id == @revision_id
                 REMOVE candidate IN @@collection
                 RETURN OLD",
                serde_json::json!({
                    "@collection": KNOWLEDGE_RELATION_CANDIDATE_COLLECTION,
                    "revision_id": revision_id,
                }),
            )
            .await
            .context("failed to delete knowledge relation candidates by revision")?;
        decode_many_results(cursor)
    }

    pub async fn delete_relation_candidates_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR candidate IN @@collection
                 FILTER candidate.library_id == @library_id
                 REMOVE candidate IN @@collection
                 RETURN OLD",
                serde_json::json!({
                    "@collection": KNOWLEDGE_RELATION_CANDIDATE_COLLECTION,
                    "library_id": library_id,
                }),
            )
            .await
            .context("failed to delete knowledge relation candidates by library")?;
        decode_many_results(cursor)
    }
}
