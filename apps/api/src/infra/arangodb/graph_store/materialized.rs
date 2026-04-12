use super::*;

impl ArangoGraphStore {
    pub async fn upsert_evidence_with_edges(
        &self,
        input: &NewKnowledgeEvidence,
        source_revision_id: Option<Uuid>,
        supporting_entity_id: Option<Uuid>,
        supporting_relation_id: Option<Uuid>,
        supporting_fact_id: Option<Uuid>,
    ) -> anyhow::Result<KnowledgeEvidenceRow> {
        let evidence = self.upsert_evidence(input).await?;
        if let Some(source_revision_id) = source_revision_id {
            self.upsert_evidence_source_edge(evidence.evidence_id, source_revision_id).await?;
        }
        if let Some(supporting_entity_id) = supporting_entity_id {
            self.upsert_evidence_supports_entity_edge(
                evidence.evidence_id,
                supporting_entity_id,
                None,
                None,
                None,
            )
            .await?;
        }
        if let Some(supporting_relation_id) = supporting_relation_id {
            self.upsert_evidence_supports_relation_edge(
                evidence.evidence_id,
                supporting_relation_id,
                None,
                None,
                None,
            )
            .await?;
        }
        if let Some(supporting_fact_id) = supporting_fact_id {
            self.upsert_fact_supports_evidence_edge(supporting_fact_id, evidence.evidence_id)
                .await?;
        }
        Ok(evidence)
    }

    pub async fn reset_library_materialized_graph(&self, library_id: Uuid) -> anyhow::Result<()> {
        self.delete_edges_by_library_reference(
            KNOWLEDGE_DOCUMENT_REVISION_EDGE,
            "_to",
            library_id,
            "failed to delete document-revision edges for library graph reset",
        )
        .await?;
        self.delete_edges_by_library_reference(
            KNOWLEDGE_REVISION_CHUNK_EDGE,
            "_from",
            library_id,
            "failed to delete revision-chunk edges for library graph reset",
        )
        .await?;
        self.delete_edges_by_library_reference(
            KNOWLEDGE_CHUNK_MENTIONS_ENTITY_EDGE,
            "_to",
            library_id,
            "failed to delete chunk mention edges for library graph reset",
        )
        .await?;
        self.delete_edges_by_library_reference(
            KNOWLEDGE_RELATION_SUBJECT_EDGE,
            "_from",
            library_id,
            "failed to delete relation subject edges for library graph reset",
        )
        .await?;
        self.delete_edges_by_library_reference(
            KNOWLEDGE_RELATION_OBJECT_EDGE,
            "_from",
            library_id,
            "failed to delete relation object edges for library graph reset",
        )
        .await?;
        self.delete_edges_by_library_reference(
            KNOWLEDGE_EVIDENCE_SOURCE_EDGE,
            "_from",
            library_id,
            "failed to delete evidence source edges for library graph reset",
        )
        .await?;
        self.delete_edges_by_library_reference(
            KNOWLEDGE_EVIDENCE_SUPPORTS_ENTITY_EDGE,
            "_from",
            library_id,
            "failed to delete evidence-entity edges for library graph reset",
        )
        .await?;
        self.delete_edges_by_library_reference(
            KNOWLEDGE_EVIDENCE_SUPPORTS_RELATION_EDGE,
            "_from",
            library_id,
            "failed to delete evidence-relation edges for library graph reset",
        )
        .await?;
        self.delete_edges_by_library_reference(
            KNOWLEDGE_FACT_EVIDENCE_EDGE,
            "_to",
            library_id,
            "failed to delete fact-evidence edges for library graph reset",
        )
        .await?;
        self.delete_edges_by_library_reference(
            KNOWLEDGE_BUNDLE_ENTITY_EDGE,
            "_to",
            library_id,
            "failed to delete bundle-entity edges for library graph reset",
        )
        .await?;
        self.delete_edges_by_library_reference(
            KNOWLEDGE_BUNDLE_RELATION_EDGE,
            "_to",
            library_id,
            "failed to delete bundle-relation edges for library graph reset",
        )
        .await?;
        self.delete_edges_by_library_reference(
            KNOWLEDGE_BUNDLE_EVIDENCE_EDGE,
            "_to",
            library_id,
            "failed to delete bundle-evidence edges for library graph reset",
        )
        .await?;
        self.delete_collection_documents_by_library(
            KNOWLEDGE_EVIDENCE_COLLECTION,
            library_id,
            "failed to delete evidence rows for library graph reset",
        )
        .await?;
        self.delete_collection_documents_by_library(
            KNOWLEDGE_RELATION_COLLECTION,
            library_id,
            "failed to delete relation rows for library graph reset",
        )
        .await?;
        self.delete_collection_documents_by_library(
            KNOWLEDGE_ENTITY_COLLECTION,
            library_id,
            "failed to delete entity rows for library graph reset",
        )
        .await?;
        Ok(())
    }

    pub async fn upsert_entity(
        &self,
        input: &NewKnowledgeEntity,
    ) -> anyhow::Result<KnowledgeEntityRow> {
        let mut rows = self.upsert_entities(std::slice::from_ref(input)).await?;
        rows.pop().ok_or_else(|| anyhow!("ArangoDB query returned no entity rows"))
    }

    pub async fn upsert_entities(
        &self,
        inputs: &[NewKnowledgeEntity],
    ) -> anyhow::Result<Vec<KnowledgeEntityRow>> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let rows = inputs
            .iter()
            .map(|input| {
                serde_json::json!({
                    "key": input.entity_id,
                    "entity_id": input.entity_id,
                    "workspace_id": input.workspace_id,
                    "library_id": input.library_id,
                    "canonical_label": input.canonical_label,
                    "aliases": input.aliases,
                    "entity_type": input.entity_type,
                    "entity_sub_type": input.entity_sub_type,
                    "summary": input.summary,
                    "confidence": input.confidence,
                    "support_count": input.support_count,
                    "freshness_generation": input.freshness_generation,
                    "entity_state": input.entity_state,
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
                    entity_id: row.entity_id,
                    workspace_id: row.workspace_id,
                    library_id: row.library_id,
                    canonical_label: row.canonical_label,
                    aliases: row.aliases,
                    entity_type: row.entity_type,
                    entity_sub_type: row.entity_sub_type,
                    summary: row.summary,
                    confidence: row.confidence,
                    support_count: row.support_count,
                    freshness_generation: row.freshness_generation,
                    entity_state: row.entity_state,
                    created_at: row.created_at,
                    updated_at: row.updated_at
                 }
                 UPDATE {
                    workspace_id: row.workspace_id,
                    library_id: row.library_id,
                    canonical_label: row.canonical_label,
                    aliases: UNION_DISTINCT((OLD.aliases == null ? [] : OLD.aliases), row.aliases),
                    entity_type: row.entity_type,
                    entity_sub_type: row.entity_sub_type,
                    summary: row.summary,
                    confidence: row.confidence,
                    support_count: row.support_count,
                    freshness_generation: row.freshness_generation,
                    entity_state: row.entity_state,
                    updated_at: row.updated_at
                 }
                 IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_ENTITY_COLLECTION,
                    "rows": rows,
                }),
                "failed to upsert knowledge entities",
            )
            .await?;
        decode_many_results(cursor)
    }

    pub async fn get_entity_by_id(
        &self,
        entity_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeEntityRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR entity IN @@collection
                 FILTER entity.entity_id == @entity_id
                 LIMIT 1
                 RETURN entity",
                serde_json::json!({
                    "@collection": KNOWLEDGE_ENTITY_COLLECTION,
                    "entity_id": entity_id,
                }),
            )
            .await
            .context("failed to get knowledge entity")?;
        decode_optional_single_result(cursor)
    }

    pub async fn get_entity_by_library_and_label(
        &self,
        library_id: Uuid,
        canonical_label: &str,
    ) -> anyhow::Result<Option<KnowledgeEntityRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR entity IN @@collection
                 FILTER entity.library_id == @library_id
                   AND entity.canonical_label == @canonical_label
                 LIMIT 1
                 RETURN entity",
                serde_json::json!({
                    "@collection": KNOWLEDGE_ENTITY_COLLECTION,
                    "library_id": library_id,
                    "canonical_label": canonical_label,
                }),
            )
            .await
            .context("failed to lookup knowledge entity by label")?;
        decode_optional_single_result(cursor)
    }

    pub async fn list_entities_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR entity IN @@collection
                 FILTER entity.library_id == @library_id
                 SORT entity.support_count DESC, entity.updated_at DESC, entity.entity_id DESC
                 LIMIT 5000
                 RETURN entity",
                serde_json::json!({
                    "@collection": KNOWLEDGE_ENTITY_COLLECTION,
                    "library_id": library_id,
                }),
            )
            .await
            .context("failed to list knowledge entities by library")?;
        decode_many_results(cursor)
    }

    pub async fn upsert_relation(
        &self,
        input: &NewKnowledgeRelation,
    ) -> anyhow::Result<KnowledgeRelationRow> {
        let mut rows = self.upsert_relations(std::slice::from_ref(input)).await?;
        rows.pop().ok_or_else(|| anyhow!("ArangoDB query returned no relation rows"))
    }

    pub async fn upsert_relation_with_endpoints(
        &self,
        input: &NewKnowledgeRelation,
        subject_entity_id: Option<Uuid>,
        object_entity_id: Option<Uuid>,
    ) -> anyhow::Result<KnowledgeRelationRow> {
        let relation = self.upsert_relation(input).await?;
        if let Some(subject_entity_id) = subject_entity_id {
            self.upsert_relation_subject_edge(relation.relation_id, subject_entity_id).await?;
        }
        if let Some(object_entity_id) = object_entity_id {
            self.upsert_relation_object_edge(relation.relation_id, object_entity_id).await?;
        }
        Ok(relation)
    }

    pub async fn upsert_relations(
        &self,
        inputs: &[NewKnowledgeRelation],
    ) -> anyhow::Result<Vec<KnowledgeRelationRow>> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let rows = inputs
            .iter()
            .map(|input| {
                serde_json::json!({
                    "key": input.relation_id,
                    "relation_id": input.relation_id,
                    "workspace_id": input.workspace_id,
                    "library_id": input.library_id,
                    "predicate": input.predicate,
                    "normalized_assertion": input.normalized_assertion,
                    "confidence": input.confidence,
                    "support_count": input.support_count,
                    "contradiction_state": input.contradiction_state,
                    "freshness_generation": input.freshness_generation,
                    "relation_state": input.relation_state,
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
                    relation_id: row.relation_id,
                    workspace_id: row.workspace_id,
                    library_id: row.library_id,
                    predicate: row.predicate,
                    normalized_assertion: row.normalized_assertion,
                    confidence: row.confidence,
                    support_count: row.support_count,
                    contradiction_state: row.contradiction_state,
                    freshness_generation: row.freshness_generation,
                    relation_state: row.relation_state,
                    created_at: row.created_at,
                    updated_at: row.updated_at
                 }
                 UPDATE {
                    workspace_id: row.workspace_id,
                    library_id: row.library_id,
                    predicate: row.predicate,
                    normalized_assertion: row.normalized_assertion,
                    confidence: row.confidence,
                    support_count: row.support_count,
                    contradiction_state: row.contradiction_state,
                    freshness_generation: row.freshness_generation,
                    relation_state: row.relation_state,
                    updated_at: row.updated_at
                 }
                 IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_RELATION_COLLECTION,
                    "rows": rows,
                }),
                "failed to upsert knowledge relations",
            )
            .await?;
        decode_many_results(cursor)
    }

    pub async fn get_relation_by_id(
        &self,
        relation_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeRelationRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR relation IN @@collection
                 FILTER relation.relation_id == @relation_id
                 LIMIT 1
                 RETURN relation",
                serde_json::json!({
                    "@collection": KNOWLEDGE_RELATION_COLLECTION,
                    "relation_id": relation_id,
                }),
            )
            .await
            .context("failed to get knowledge relation")?;
        decode_optional_single_result(cursor)
    }

    pub async fn get_relation_by_library_and_assertion(
        &self,
        library_id: Uuid,
        normalized_assertion: &str,
    ) -> anyhow::Result<Option<KnowledgeRelationRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR relation IN @@collection
                 FILTER relation.library_id == @library_id
                   AND relation.normalized_assertion == @normalized_assertion
                 LIMIT 1
                 RETURN relation",
                serde_json::json!({
                    "@collection": KNOWLEDGE_RELATION_COLLECTION,
                    "library_id": library_id,
                    "normalized_assertion": normalized_assertion,
                }),
            )
            .await
            .context("failed to lookup knowledge relation by assertion")?;
        decode_optional_single_result(cursor)
    }

    pub async fn list_relations_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR relation IN @@collection
                 FILTER relation.library_id == @library_id
                 SORT relation.support_count DESC, relation.updated_at DESC, relation.relation_id DESC
                 RETURN relation",
                serde_json::json!({
                    "@collection": KNOWLEDGE_RELATION_COLLECTION,
                    "library_id": library_id,
                }),
            )
            .await
            .context("failed to list knowledge relations by library")?;
        decode_many_results(cursor)
    }

    pub async fn delete_entities_by_canonical_keys(
        &self,
        library_id: Uuid,
        keys: &[String],
    ) -> anyhow::Result<u64> {
        if keys.is_empty() {
            return Ok(0);
        }
        let cursor = self
            .client
            .query_json(
                "FOR doc IN @@collection
                 FILTER doc.library_id == @library_id AND doc.canonical_key IN @keys
                 REMOVE doc IN @@collection
                 RETURN OLD._key",
                serde_json::json!({
                    "@collection": KNOWLEDGE_ENTITY_COLLECTION,
                    "library_id": library_id,
                    "keys": keys,
                }),
            )
            .await
            .context("failed to delete ArangoDB entities by canonical keys")?;
        let deleted: Vec<String> = decode_many_results(cursor)?;
        Ok(deleted.len() as u64)
    }

    pub async fn delete_relations_by_canonical_keys(
        &self,
        library_id: Uuid,
        keys: &[String],
    ) -> anyhow::Result<u64> {
        if keys.is_empty() {
            return Ok(0);
        }
        let cursor = self
            .client
            .query_json(
                "FOR doc IN @@collection
                 FILTER doc.library_id == @library_id AND doc.canonical_key IN @keys
                 REMOVE doc IN @@collection
                 RETURN OLD._key",
                serde_json::json!({
                    "@collection": KNOWLEDGE_RELATION_COLLECTION,
                    "library_id": library_id,
                    "keys": keys,
                }),
            )
            .await
            .context("failed to delete ArangoDB relations by canonical keys")?;
        let deleted: Vec<String> = decode_many_results(cursor)?;
        Ok(deleted.len() as u64)
    }

    pub async fn upsert_evidence(
        &self,
        input: &NewKnowledgeEvidence,
    ) -> anyhow::Result<KnowledgeEvidenceRow> {
        let cursor = self
            .client
            .query_json(
                "UPSERT { _key: @key }
                 INSERT {
                    _key: @key,
                    evidence_id: @evidence_id,
                    workspace_id: @workspace_id,
                    library_id: @library_id,
                    document_id: @document_id,
                    revision_id: @revision_id,
                    chunk_id: @chunk_id,
                    block_id: @block_id,
                    fact_id: @fact_id,
                    span_start: @span_start,
                    span_end: @span_end,
                    quote_text: @quote_text,
                    literal_spans_json: @literal_spans_json,
                    evidence_kind: @evidence_kind,
                    extraction_method: @extraction_method,
                    confidence: @confidence,
                    evidence_state: @evidence_state,
                    freshness_generation: @freshness_generation,
                    created_at: @created_at,
                    updated_at: @updated_at
                 }
                 UPDATE {
                    workspace_id: @workspace_id,
                    library_id: @library_id,
                    document_id: @document_id,
                    revision_id: @revision_id,
                    chunk_id: @chunk_id,
                    block_id: @block_id,
                    fact_id: @fact_id,
                    span_start: @span_start,
                    span_end: @span_end,
                    quote_text: @quote_text,
                    literal_spans_json: @literal_spans_json,
                    evidence_kind: @evidence_kind,
                    extraction_method: @extraction_method,
                    confidence: @confidence,
                    evidence_state: @evidence_state,
                    freshness_generation: @freshness_generation,
                    updated_at: @updated_at
                 }
                 IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_EVIDENCE_COLLECTION,
                    "key": input.evidence_id,
                    "evidence_id": input.evidence_id,
                    "workspace_id": input.workspace_id,
                    "library_id": input.library_id,
                    "document_id": input.document_id,
                    "revision_id": input.revision_id,
                    "chunk_id": input.chunk_id,
                    "block_id": input.block_id,
                    "fact_id": input.fact_id,
                    "span_start": input.span_start,
                    "span_end": input.span_end,
                    "quote_text": input.quote_text,
                    "literal_spans_json": input.literal_spans_json,
                    "evidence_kind": input.evidence_kind,
                    "extraction_method": input.extraction_method,
                    "confidence": input.confidence,
                    "evidence_state": input.evidence_state,
                    "freshness_generation": input.freshness_generation,
                    "created_at": input.created_at.unwrap_or_else(Utc::now),
                    "updated_at": input.updated_at.unwrap_or_else(Utc::now),
                }),
            )
            .await
            .context("failed to upsert knowledge evidence")?;
        decode_single_result(cursor)
    }

    pub async fn get_evidence_by_id(
        &self,
        evidence_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeEvidenceRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR evidence IN @@collection
                 FILTER evidence.evidence_id == @evidence_id
                 LIMIT 1
                 RETURN evidence",
                serde_json::json!({
                    "@collection": KNOWLEDGE_EVIDENCE_COLLECTION,
                    "evidence_id": evidence_id,
                }),
            )
            .await
            .context("failed to get knowledge evidence")?;
        decode_optional_single_result(cursor)
    }

    pub async fn list_evidence_by_ids(
        &self,
        evidence_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeEvidenceRow>> {
        if evidence_ids.is_empty() {
            return Ok(Vec::new());
        }
        let cursor = self
            .client
            .query_json(
                "FOR evidence IN @@collection
                 FILTER evidence.evidence_id IN @evidence_ids
                 SORT evidence.created_at ASC, evidence.evidence_id ASC
                 RETURN evidence",
                serde_json::json!({
                    "@collection": KNOWLEDGE_EVIDENCE_COLLECTION,
                    "evidence_ids": evidence_ids,
                }),
            )
            .await
            .context("failed to list knowledge evidence by ids")?;
        decode_many_results(cursor)
    }

    pub async fn list_evidence_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEvidenceRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR evidence IN @@collection
                 FILTER evidence.revision_id == @revision_id
                 SORT evidence.created_at ASC, evidence.evidence_id ASC
                 RETURN evidence",
                serde_json::json!({
                    "@collection": KNOWLEDGE_EVIDENCE_COLLECTION,
                    "revision_id": revision_id,
                }),
            )
            .await
            .context("failed to list knowledge evidence by revision")?;
        decode_many_results(cursor)
    }

    pub async fn list_evidence_by_chunk(
        &self,
        chunk_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEvidenceRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR evidence IN @@collection
                 FILTER evidence.chunk_id == @chunk_id
                 SORT evidence.created_at ASC, evidence.evidence_id ASC
                 RETURN evidence",
                serde_json::json!({
                    "@collection": KNOWLEDGE_EVIDENCE_COLLECTION,
                    "chunk_id": chunk_id,
                }),
            )
            .await
            .context("failed to list knowledge evidence by chunk")?;
        decode_many_results(cursor)
    }
}
