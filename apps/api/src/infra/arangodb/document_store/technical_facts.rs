use anyhow::Context;
use uuid::Uuid;

use super::{ArangoDocumentStore, KnowledgeTechnicalFactRow, decode::decode_many_results};
use crate::infra::arangodb::collections::KNOWLEDGE_TECHNICAL_FACT_COLLECTION;

impl ArangoDocumentStore {
    pub async fn replace_technical_facts(
        &self,
        revision_id: Uuid,
        rows: &[KnowledgeTechnicalFactRow],
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        self.delete_technical_facts_by_revision(revision_id).await?;
        if rows.is_empty() {
            return Ok(Vec::new());
        }

        let payload_rows = rows
            .iter()
            .map(|row| {
                serde_json::json!({
                    "_key": row.key,
                    "fact_id": row.fact_id,
                    "workspace_id": row.workspace_id,
                    "library_id": row.library_id,
                    "document_id": row.document_id,
                    "revision_id": row.revision_id,
                    "fact_kind": row.fact_kind,
                    "canonical_value_text": row.canonical_value_text,
                    "canonical_value_exact": row.canonical_value_exact,
                    "canonical_value_json": row.canonical_value_json,
                    "display_value": row.display_value,
                    "qualifiers_json": row.qualifiers_json,
                    "support_block_ids": row.support_block_ids,
                    "support_chunk_ids": row.support_chunk_ids,
                    "confidence": row.confidence,
                    "extraction_kind": row.extraction_kind,
                    "conflict_group_id": row.conflict_group_id,
                    "created_at": row.created_at,
                    "updated_at": row.updated_at,
                })
            })
            .collect::<Vec<_>>();

        let cursor = self
            .client
            .query_json(
                "FOR row IN @rows
                 INSERT row INTO @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_TECHNICAL_FACT_COLLECTION,
                    "rows": payload_rows,
                }),
            )
            .await
            .context("failed to replace technical facts")?;
        decode_many_results(cursor)
    }

    pub async fn list_technical_facts_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR fact IN @@collection
                 FILTER fact.revision_id == @revision_id
                 SORT fact.fact_kind ASC, fact.fact_id ASC
                 RETURN fact",
                serde_json::json!({
                    "@collection": KNOWLEDGE_TECHNICAL_FACT_COLLECTION,
                    "revision_id": revision_id,
                }),
            )
            .await
            .context("failed to list technical facts by revision")?;
        decode_many_results(cursor)
    }

    pub async fn list_technical_facts_by_ids(
        &self,
        fact_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        if fact_ids.is_empty() {
            return Ok(Vec::new());
        }
        let cursor = self
            .client
            .query_json(
                "FOR fact IN @@collection
                 FILTER fact.fact_id IN @fact_ids
                 SORT fact.fact_kind ASC, fact.fact_id ASC
                 RETURN fact",
                serde_json::json!({
                    "@collection": KNOWLEDGE_TECHNICAL_FACT_COLLECTION,
                    "fact_ids": fact_ids,
                }),
            )
            .await
            .context("failed to list technical facts by ids")?;
        decode_many_results(cursor)
    }

    pub async fn list_technical_facts_by_chunk_ids(
        &self,
        chunk_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        if chunk_ids.is_empty() {
            return Ok(Vec::new());
        }
        let cursor = self
            .client
            .query_json(
                "FOR fact IN @@collection
                 FILTER LENGTH(INTERSECTION(fact.support_chunk_ids, @chunk_ids)) > 0
                 SORT fact.fact_kind ASC, fact.fact_id ASC
                 RETURN fact",
                serde_json::json!({
                    "@collection": KNOWLEDGE_TECHNICAL_FACT_COLLECTION,
                    "chunk_ids": chunk_ids,
                }),
            )
            .await
            .context("failed to list technical facts by chunk ids")?;
        decode_many_results(cursor)
    }

    pub async fn list_technical_facts_by_document(
        &self,
        document_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR fact IN @@collection
                 FILTER fact.document_id == @document_id
                 SORT fact.revision_id DESC, fact.fact_kind ASC, fact.fact_id ASC
                 RETURN fact",
                serde_json::json!({
                    "@collection": KNOWLEDGE_TECHNICAL_FACT_COLLECTION,
                    "document_id": document_id,
                }),
            )
            .await
            .context("failed to list technical facts by document")?;
        decode_many_results(cursor)
    }

    pub async fn delete_technical_facts_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR fact IN @@collection
                 FILTER fact.revision_id == @revision_id
                 REMOVE fact IN @@collection
                 RETURN OLD",
                serde_json::json!({
                    "@collection": KNOWLEDGE_TECHNICAL_FACT_COLLECTION,
                    "revision_id": revision_id,
                }),
            )
            .await
            .context("failed to delete technical facts by revision")?;
        decode_many_results(cursor)
    }
}
