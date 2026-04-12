use std::collections::BTreeSet;

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::arangodb::graph_store::{
        KnowledgeEntityRow, KnowledgeRelationRow, NewKnowledgeEntity, NewKnowledgeRelation,
    },
};

use super::{
    ArangoRevisionContext, GraphService, canonical_chunk_mentions_entity_edge_key,
    canonical_document_revision_edge_key, canonical_edge_relation_key, canonical_entity_id,
    canonical_revision_chunk_edge_key, placeholder_entity_parts_from_key,
};

impl GraphService {
    pub(super) async fn upsert_canonical_entity(
        &self,
        state: &AppState,
        library_id: Uuid,
        workspace_id: Uuid,
        normalization_key: &str,
        canonical_label: &str,
        entity_type: &str,
        aliases: BTreeSet<String>,
        confidence: Option<f64>,
        support_count: i64,
        freshness_generation: i64,
    ) -> Result<KnowledgeEntityRow> {
        let entity_id = canonical_entity_id(library_id, normalization_key);
        let existing = state
            .arango_graph_store
            .get_entity_by_id(entity_id)
            .await
            .context("failed to load canonical entity before upsert")?;
        let mut merged_aliases =
            existing.as_ref().map(|row| row.aliases.clone()).unwrap_or_default();
        for alias in aliases {
            if !merged_aliases.iter().any(|existing| existing == &alias) {
                merged_aliases.push(alias);
            }
        }
        if !merged_aliases.iter().any(|alias| alias == canonical_label) {
            merged_aliases.push(canonical_label.to_string());
        }
        let summary = existing.as_ref().and_then(|row| row.summary.clone());
        let confidence = match (existing.as_ref().and_then(|row| row.confidence), confidence) {
            (Some(existing_confidence), Some(candidate_confidence)) => {
                Some(existing_confidence.max(candidate_confidence))
            }
            (Some(existing_confidence), None) => Some(existing_confidence),
            (None, Some(candidate_confidence)) => Some(candidate_confidence),
            (None, None) => None,
        };
        let entity = NewKnowledgeEntity {
            entity_id,
            workspace_id,
            library_id,
            canonical_label: canonical_label.to_string(),
            aliases: merged_aliases,
            entity_type: entity_type.to_string(),
            entity_sub_type: None,
            summary,
            confidence,
            support_count,
            freshness_generation,
            entity_state: "active".to_string(),
            created_at: existing.as_ref().map(|row| row.created_at),
            updated_at: Some(Utc::now()),
        };
        let mut last_err = None;
        for attempt in 0..3 {
            match state.arango_graph_store.upsert_entity(&entity).await {
                Ok(row) => return Ok(row),
                Err(e) => {
                    let msg = format!("{e:#}");
                    if msg.contains("409") && msg.contains("write-write conflict") && attempt < 2 {
                        let backoff = std::time::Duration::from_millis(50 * (1 << attempt));
                        tokio::time::sleep(backoff).await;
                        last_err = Some(e);
                        continue;
                    }
                    return Err(e).context("failed to upsert canonical arango entity");
                }
            }
        }
        Err(last_err.ok_or_else(|| {
            anyhow::anyhow!("canonical arango entity upsert exhausted retries without an error")
        })?)
        .context("failed to upsert canonical arango entity after retries")
    }

    pub(super) async fn upsert_placeholder_entity_for_key(
        &self,
        state: &AppState,
        library_id: Uuid,
        workspace_id: Uuid,
        canonical_key: &str,
    ) -> Result<KnowledgeEntityRow> {
        let normalization_key = canonical_key.trim();
        let (node_type, canonical_label) = placeholder_entity_parts_from_key(normalization_key)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "invalid canonical entity key `{normalization_key}` while materializing relation endpoints"
                )
            })?;
        let aliases = {
            let mut set = BTreeSet::new();
            set.insert(canonical_label.clone());
            set
        };
        self.upsert_canonical_entity(
            state,
            library_id,
            workspace_id,
            normalization_key,
            &canonical_label,
            crate::services::graph::identity::runtime_node_type_slug(&node_type),
            aliases,
            None,
            1,
            0,
        )
        .await
    }

    pub(super) async fn upsert_canonical_relation(
        &self,
        state: &AppState,
        library_id: Uuid,
        workspace_id: Uuid,
        normalized_assertion: &str,
        predicate: &str,
        confidence: Option<f64>,
        support_count: i64,
        freshness_generation: i64,
    ) -> Result<KnowledgeRelationRow> {
        let relation_id = super::canonical_relation_id(library_id, normalized_assertion);
        let existing = state
            .arango_graph_store
            .get_relation_by_id(relation_id)
            .await
            .context("failed to load canonical relation before upsert")?;
        let confidence = match (existing.as_ref().and_then(|row| row.confidence), confidence) {
            (Some(existing_confidence), Some(candidate_confidence)) => {
                Some(existing_confidence.max(candidate_confidence))
            }
            (Some(existing_confidence), None) => Some(existing_confidence),
            (None, Some(candidate_confidence)) => Some(candidate_confidence),
            (None, None) => None,
        };
        let relation = NewKnowledgeRelation {
            relation_id,
            workspace_id,
            library_id,
            predicate: predicate.to_string(),
            normalized_assertion: normalized_assertion.to_string(),
            confidence,
            support_count,
            contradiction_state: existing
                .as_ref()
                .map(|row| row.contradiction_state.clone())
                .unwrap_or_else(|| "unknown".to_string()),
            freshness_generation,
            relation_state: "active".to_string(),
            created_at: existing.as_ref().map(|row| row.created_at),
            updated_at: Some(Utc::now()),
        };
        let mut last_err = None;
        for attempt in 0..3 {
            match state.arango_graph_store.upsert_relation(&relation).await {
                Ok(row) => return Ok(row),
                Err(e) => {
                    let msg = format!("{e:#}");
                    if msg.contains("409") && msg.contains("write-write conflict") && attempt < 2 {
                        let backoff = std::time::Duration::from_millis(50 * (1 << attempt));
                        tokio::time::sleep(backoff).await;
                        last_err = Some(e);
                        continue;
                    }
                    return Err(e).context("failed to upsert canonical arango relation");
                }
            }
        }
        Err(last_err.ok_or_else(|| {
            anyhow::anyhow!("canonical arango relation upsert exhausted retries without an error")
        })?)
        .context("failed to upsert canonical arango relation after retries")
    }

    pub(super) async fn upsert_relation_edges(
        &self,
        state: &AppState,
        relation: &KnowledgeRelationRow,
        subject: &KnowledgeEntityRow,
        object: &KnowledgeEntityRow,
    ) -> Result<()> {
        self.upsert_arango_edge(
            state,
            "knowledge_relation_subject_edge",
            canonical_edge_relation_key(relation.relation_id, subject.entity_id, "subject"),
            "knowledge_relation",
            relation.relation_id,
            "knowledge_entity",
            subject.entity_id,
            json!({}),
        )
        .await?;
        self.upsert_arango_edge(
            state,
            "knowledge_relation_object_edge",
            canonical_edge_relation_key(relation.relation_id, object.entity_id, "object"),
            "knowledge_relation",
            relation.relation_id,
            "knowledge_entity",
            object.entity_id,
            json!({}),
        )
        .await
    }

    pub(super) async fn upsert_revision_edges(
        &self,
        state: &AppState,
        revision: &ArangoRevisionContext,
    ) -> Result<()> {
        self.upsert_arango_edge(
            state,
            "knowledge_document_revision_edge",
            canonical_document_revision_edge_key(revision.document_id, revision.revision_id),
            "knowledge_document",
            revision.document_id,
            "knowledge_revision",
            revision.revision_id,
            json!({}),
        )
        .await
    }

    pub(super) async fn upsert_chunk_edge(
        &self,
        state: &AppState,
        revision: &ArangoRevisionContext,
        chunk_id: Uuid,
    ) -> Result<()> {
        self.upsert_arango_edge(
            state,
            "knowledge_revision_chunk_edge",
            canonical_revision_chunk_edge_key(revision.revision_id, chunk_id),
            "knowledge_revision",
            revision.revision_id,
            "knowledge_chunk",
            chunk_id,
            json!({}),
        )
        .await
    }

    pub(super) async fn upsert_chunk_mentions_entity_edge(
        &self,
        state: &AppState,
        chunk_id: Uuid,
        entity_id: Uuid,
        score: Option<f64>,
    ) -> Result<()> {
        self.upsert_arango_edge(
            state,
            "knowledge_chunk_mentions_entity_edge",
            canonical_chunk_mentions_entity_edge_key(chunk_id, entity_id),
            "knowledge_chunk",
            chunk_id,
            "knowledge_entity",
            entity_id,
            json!({
                "rank": 1,
                "score": score,
                "inclusionReason": "graph_extract_entity_candidate",
            }),
        )
        .await
    }

    async fn upsert_arango_edge(
        &self,
        state: &AppState,
        collection: &str,
        key: String,
        from_collection: &str,
        from_id: Uuid,
        to_collection: &str,
        to_id: Uuid,
        extra_fields: serde_json::Value,
    ) -> Result<()> {
        let client = state.arango_graph_store.client();
        let query = "UPSERT { _key: @key }
                     INSERT {
                        _key: @key,
                        _from: @from,
                        _to: @to,
                        created_at: @created_at,
                        updated_at: @updated_at,
                        payload: @payload
                     }
                     UPDATE {
                        _from: @from,
                        _to: @to,
                        updated_at: @updated_at,
                        payload: @payload
                     }
                     IN @@collection
                     RETURN NEW";
        let bind_vars = json!({
            "@collection": collection,
            "key": key,
            "from": format!("{from_collection}/{from_id}"),
            "to": format!("{to_collection}/{to_id}"),
            "created_at": Utc::now(),
            "updated_at": Utc::now(),
            "payload": extra_fields,
        });
        let mut last_err = None;
        for attempt in 0..3u32 {
            match client.query_json(query, bind_vars.clone()).await {
                Ok(_) => return Ok(()),
                Err(e) => {
                    let msg = format!("{e:#}");
                    if msg.contains("409") && msg.contains("write-write conflict") && attempt < 2 {
                        let backoff = std::time::Duration::from_millis(50 * (1 << attempt));
                        tokio::time::sleep(backoff).await;
                        last_err = Some(e);
                        continue;
                    }
                    return Err(e)
                        .with_context(|| format!("failed to upsert arango edge in {collection}"));
                }
            }
        }
        Err(last_err.ok_or_else(|| {
            anyhow::anyhow!("arango edge upsert exhausted retries without an error")
        })?)
        .with_context(|| format!("failed to upsert arango edge in {collection} after retries"))
    }
}
