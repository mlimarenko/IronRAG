use super::*;

impl ArangoGraphStore {
    pub async fn upsert_document_revision_edge(
        &self,
        document_id: Uuid,
        revision_id: Uuid,
    ) -> anyhow::Result<()> {
        self.insert_edge(
            KNOWLEDGE_DOCUMENT_REVISION_EDGE,
            KNOWLEDGE_DOCUMENT_COLLECTION,
            document_id,
            KNOWLEDGE_REVISION_COLLECTION,
            revision_id,
            serde_json::json!({}),
        )
        .await
    }

    pub async fn upsert_revision_chunk_edge(
        &self,
        revision_id: Uuid,
        chunk_id: Uuid,
    ) -> anyhow::Result<()> {
        self.insert_edge(
            KNOWLEDGE_REVISION_CHUNK_EDGE,
            KNOWLEDGE_REVISION_COLLECTION,
            revision_id,
            KNOWLEDGE_CHUNK_COLLECTION,
            chunk_id,
            serde_json::json!({}),
        )
        .await
    }

    pub async fn insert_revision_chunk_edges(
        &self,
        revision_id: Uuid,
        chunk_ids: &[Uuid],
    ) -> anyhow::Result<()> {
        for chunk_id in chunk_ids {
            self.upsert_revision_chunk_edge(revision_id, *chunk_id).await?;
        }
        Ok(())
    }

    pub async fn delete_revision_chunk_edges(&self, revision_id: Uuid) -> anyhow::Result<u64> {
        let cursor = self
            .client
            .query_json(
                "FOR edge IN @@collection
                 FILTER edge._from == @from_id
                 REMOVE edge IN @@collection
                 RETURN OLD",
                serde_json::json!({
                    "@collection": KNOWLEDGE_REVISION_CHUNK_EDGE,
                    "from_id": format!("{}/{}", KNOWLEDGE_REVISION_COLLECTION, revision_id),
                }),
            )
            .await
            .context("failed to delete revision chunk edges")?;
        let removed: Vec<serde_json::Value> = decode_many_results(cursor)?;
        Ok(u64::try_from(removed.len()).unwrap_or(u64::MAX))
    }

    pub async fn upsert_chunk_mentions_entity_edge(
        &self,
        chunk_id: Uuid,
        entity_id: Uuid,
        rank: Option<i32>,
        score: Option<f64>,
        inclusion_reason: Option<String>,
    ) -> anyhow::Result<()> {
        self.insert_edge(
            KNOWLEDGE_CHUNK_MENTIONS_ENTITY_EDGE,
            KNOWLEDGE_CHUNK_COLLECTION,
            chunk_id,
            KNOWLEDGE_ENTITY_COLLECTION,
            entity_id,
            serde_json::json!({
                "rank": rank,
                "score": score,
                "inclusionReason": inclusion_reason,
            }),
        )
        .await
    }

    pub async fn upsert_relation_subject_edge(
        &self,
        relation_id: Uuid,
        subject_entity_id: Uuid,
    ) -> anyhow::Result<()> {
        self.insert_edge(
            KNOWLEDGE_RELATION_SUBJECT_EDGE,
            KNOWLEDGE_RELATION_COLLECTION,
            relation_id,
            KNOWLEDGE_ENTITY_COLLECTION,
            subject_entity_id,
            serde_json::json!({}),
        )
        .await
    }

    pub async fn upsert_relation_object_edge(
        &self,
        relation_id: Uuid,
        object_entity_id: Uuid,
    ) -> anyhow::Result<()> {
        self.insert_edge(
            KNOWLEDGE_RELATION_OBJECT_EDGE,
            KNOWLEDGE_RELATION_COLLECTION,
            relation_id,
            KNOWLEDGE_ENTITY_COLLECTION,
            object_entity_id,
            serde_json::json!({}),
        )
        .await
    }

    pub async fn upsert_evidence_source_edge(
        &self,
        evidence_id: Uuid,
        revision_id: Uuid,
    ) -> anyhow::Result<()> {
        self.insert_edge(
            KNOWLEDGE_EVIDENCE_SOURCE_EDGE,
            KNOWLEDGE_EVIDENCE_COLLECTION,
            evidence_id,
            KNOWLEDGE_REVISION_COLLECTION,
            revision_id,
            serde_json::json!({}),
        )
        .await
    }

    pub async fn upsert_evidence_supports_entity_edge(
        &self,
        evidence_id: Uuid,
        entity_id: Uuid,
        rank: Option<i32>,
        score: Option<f64>,
        inclusion_reason: Option<String>,
    ) -> anyhow::Result<()> {
        self.insert_edge(
            KNOWLEDGE_EVIDENCE_SUPPORTS_ENTITY_EDGE,
            KNOWLEDGE_EVIDENCE_COLLECTION,
            evidence_id,
            KNOWLEDGE_ENTITY_COLLECTION,
            entity_id,
            serde_json::json!({
                "rank": rank,
                "score": score,
                "inclusionReason": inclusion_reason,
            }),
        )
        .await
    }

    pub async fn upsert_evidence_supports_relation_edge(
        &self,
        evidence_id: Uuid,
        relation_id: Uuid,
        rank: Option<i32>,
        score: Option<f64>,
        inclusion_reason: Option<String>,
    ) -> anyhow::Result<()> {
        self.insert_edge(
            KNOWLEDGE_EVIDENCE_SUPPORTS_RELATION_EDGE,
            KNOWLEDGE_EVIDENCE_COLLECTION,
            evidence_id,
            KNOWLEDGE_RELATION_COLLECTION,
            relation_id,
            serde_json::json!({
                "rank": rank,
                "score": score,
                "inclusionReason": inclusion_reason,
            }),
        )
        .await
    }

    pub async fn upsert_fact_supports_evidence_edge(
        &self,
        fact_id: Uuid,
        evidence_id: Uuid,
    ) -> anyhow::Result<()> {
        self.insert_edge(
            KNOWLEDGE_FACT_EVIDENCE_EDGE,
            KNOWLEDGE_TECHNICAL_FACT_COLLECTION,
            fact_id,
            KNOWLEDGE_EVIDENCE_COLLECTION,
            evidence_id,
            serde_json::json!({}),
        )
        .await
    }

    async fn insert_edge(
        &self,
        collection: &str,
        from_collection: &str,
        from_id: Uuid,
        to_collection: &str,
        to_id: Uuid,
        extra_fields: serde_json::Value,
    ) -> anyhow::Result<()> {
        let mut payload = serde_json::json!({
            "_key": canonical_edge_key(from_id, to_id),
            "@collection": collection,
            "_from": format!("{}/{}", from_collection, from_id),
            "_to": format!("{}/{}", to_collection, to_id),
            "created_at": Utc::now(),
            "updated_at": Utc::now(),
        });
        if let (Some(target), Some(source)) = (payload.as_object_mut(), extra_fields.as_object()) {
            for (key, value) in source {
                target.insert(key.clone(), value.clone());
            }
        } else {
            return Err(anyhow!("failed to build edge payload"));
        }

        self.client
            .query_json(
                "UPSERT { _key: @payload._key }
                 INSERT @payload
                 UPDATE MERGE(@payload, { created_at: OLD.created_at })
                 IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": collection,
                    "payload": payload,
                }),
            )
            .await
            .with_context(|| format!("failed to insert edge into {collection}"))?;
        Ok(())
    }

    pub(crate) async fn delete_collection_documents_by_library(
        &self,
        collection: &str,
        library_id: Uuid,
        error_context: &str,
    ) -> anyhow::Result<()> {
        loop {
            let cursor = self
                .client
                .query_json(
                    "FOR doc IN @@collection
                     FILTER doc.library_id == @library_id
                     LIMIT @limit
                     RETURN doc._key",
                    serde_json::json!({
                        "@collection": collection,
                        "library_id": library_id,
                        "limit": Self::LIBRARY_RESET_BATCH_SIZE,
                    }),
                )
                .await
                .with_context(|| error_context.to_string())?;
            let keys: Vec<String> = decode_many_results(cursor)?;
            if keys.is_empty() {
                break;
            }

            self.client
                .query_json(
                    "FOR key IN @keys
                     REMOVE { _key: key } IN @@collection
                     OPTIONS { ignoreErrors: true }",
                    serde_json::json!({
                        "@collection": collection,
                        "keys": keys,
                    }),
                )
                .await
                .with_context(|| error_context.to_string())?;
        }
        Ok(())
    }

    pub(crate) async fn delete_edges_by_library_reference(
        &self,
        collection: &str,
        vertex_field: &str,
        library_id: Uuid,
        error_context: &str,
    ) -> anyhow::Result<()> {
        loop {
            let query = format!(
                "FOR edge IN @@collection
                 LET vertex = DOCUMENT(edge.{vertex_field})
                 FILTER vertex != null
                   AND vertex.library_id == @library_id
                 LIMIT @limit
                 RETURN edge._key"
            );
            let cursor = self
                .client
                .query_json(
                    &query,
                    serde_json::json!({
                        "@collection": collection,
                        "library_id": library_id,
                        "limit": Self::LIBRARY_RESET_BATCH_SIZE,
                    }),
                )
                .await
                .with_context(|| error_context.to_string())?;
            let keys: Vec<String> = decode_many_results(cursor)?;
            if keys.is_empty() {
                break;
            }

            self.client
                .query_json(
                    "FOR key IN @keys
                     REMOVE { _key: key } IN @@collection
                     OPTIONS { ignoreErrors: true }",
                    serde_json::json!({
                        "@collection": collection,
                        "keys": keys,
                    }),
                )
                .await
                .with_context(|| error_context.to_string())?;
        }
        Ok(())
    }

    pub(crate) async fn run_retryable_upsert_query(
        &self,
        query: &str,
        bind_vars: serde_json::Value,
        context_message: &str,
    ) -> anyhow::Result<serde_json::Value> {
        let mut last_error = None;
        for attempt in 0..3 {
            match self.client.query_json(query, bind_vars.clone()).await {
                Ok(cursor) => return Ok(cursor),
                Err(error) => {
                    let message = format!("{error:#}");
                    if attempt < 2 && is_retryable_upsert_error(&message) {
                        sleep(Duration::from_millis(100 * (1 << attempt))).await;
                        last_error = Some(error);
                        continue;
                    }
                    return Err(error).context(context_message.to_string());
                }
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow!("retryable ArangoDB upsert failed")))
            .context(context_message.to_string())
    }

    #[allow(clippy::unused_async)]
    pub async fn replace_library_projection(
        &self,
        _library_id: Uuid,
        _projection_version: i64,
        _nodes: &[GraphViewNodeWrite],
        _edges: &[GraphViewEdgeWrite],
    ) -> Result<(), GraphViewWriteError> {
        Ok(())
    }

    #[allow(clippy::unused_async)]
    pub async fn refresh_library_projection_targets(
        &self,
        _library_id: Uuid,
        _projection_version: i64,
        _remove_node_ids: &[Uuid],
        _remove_edge_ids: &[Uuid],
        _nodes: &[GraphViewNodeWrite],
        _edges: &[GraphViewEdgeWrite],
    ) -> Result<(), GraphViewWriteError> {
        Ok(())
    }

    pub async fn load_library_projection(
        &self,
        library_id: Uuid,
        _projection_version: i64,
    ) -> anyhow::Result<GraphViewData> {
        let nodes = self
            .list_entities_by_library(library_id)
            .await?
            .into_iter()
            .map(|entity| GraphViewNodeWrite {
                node_id: entity.entity_id,
                canonical_key: entity.key,
                label: entity.canonical_label,
                node_type: entity.entity_type,
                support_count: i32::try_from(entity.support_count).unwrap_or(i32::MAX),
                summary: entity.summary,
                aliases: entity.aliases,
                metadata_json: serde_json::json!({
                    "entity_state": entity.entity_state,
                    "freshness_generation": entity.freshness_generation,
                    "confidence": entity.confidence,
                }),
            })
            .collect::<Vec<_>>();
        let edges = self
            .list_relation_topology_by_library(library_id)
            .await?
            .into_iter()
            .map(|row| GraphViewEdgeWrite {
                edge_id: row.relation.relation_id,
                from_node_id: row.subject_entity_id,
                to_node_id: row.object_entity_id,
                relation_type: row.relation.predicate,
                canonical_key: row.relation.normalized_assertion,
                support_count: i32::try_from(row.relation.support_count).unwrap_or(i32::MAX),
                summary: None,
                weight: row.relation.confidence,
                metadata_json: serde_json::json!({
                    "relation_state": row.relation.relation_state,
                    "freshness_generation": row.relation.freshness_generation,
                    "contradiction_state": row.relation.contradiction_state,
                }),
            })
            .collect::<Vec<_>>();
        Ok(GraphViewData { nodes, edges })
    }
}
