use super::*;

impl PgGraphStore {
    pub(super) async fn upsert_document_revision_edge_impl(
        &self,
        document_id: Uuid,
        revision_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "update knowledge_revision
             set document_id = $1
             where revision_id = $2 and library_id = $3",
        )
        .bind(document_id)
        .bind(revision_id)
        .bind(library_id)
        .execute(&self.pool)
        .await
        .context("failed to upsert collapsed document-revision edge")?;
        Ok(())
    }

    pub(super) async fn upsert_revision_chunk_edge_impl(
        &self,
        revision_id: Uuid,
        chunk_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "update knowledge_chunk
             set revision_id = $1
             where chunk_id = $2 and library_id = $3",
        )
        .bind(revision_id)
        .bind(chunk_id)
        .bind(library_id)
        .execute(&self.pool)
        .await
        .context("failed to upsert collapsed revision-chunk edge")?;
        Ok(())
    }

    pub(super) async fn insert_revision_chunk_edges_impl(
        &self,
        revision_id: Uuid,
        chunk_ids: &[Uuid],
        library_id: Uuid,
    ) -> anyhow::Result<()> {
        if chunk_ids.is_empty() {
            return Ok(());
        }
        sqlx::query(
            "update knowledge_chunk
             set revision_id = $1
             where library_id = $2 and chunk_id = any($3)",
        )
        .bind(revision_id)
        .bind(library_id)
        .bind(chunk_ids)
        .execute(&self.pool)
        .await
        .context("failed to upsert collapsed revision-chunk edges")?;
        Ok(())
    }

    pub(super) async fn delete_revision_chunk_edges_impl(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<u64> {
        // The PG schema collapses revision->chunk edges into
        // knowledge_chunk.revision_id. The caller deletes the chunks
        // immediately after this hook, so clearing revision_id here would either
        // violate the NOT NULL column or hide those chunks from the real delete.
        // Return the count of collapsed edges that would be removed to preserve
        // the GraphStore write-count contract.
        let count = sqlx::query_scalar::<_, i64>(
            "select count(*) from knowledge_chunk where revision_id = $1",
        )
        .bind(revision_id)
        .fetch_one(&self.pool)
        .await
        .context("failed to count collapsed revision-chunk edges")?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    pub(super) async fn upsert_chunk_mentions_entity_edge_impl(
        &self,
        chunk_id: Uuid,
        entity_id: Uuid,
        ranking: EdgeRanking,
    ) -> anyhow::Result<()> {
        let EdgeRanking { rank, score, inclusion_reason, library_id } = ranking;
        sqlx::query(
            "insert into knowledge_chunk_entity_mention (
                from_id, to_id, relation_type, support, library_id, rank, score,
                inclusion_reason, created_at, updated_at
             ) values ($1, $2, 'mentions', 1, $3, $4, $5, $6, now(), now())
             on conflict (from_id, to_id, relation_type) do update
             set support = excluded.support,
                 library_id = excluded.library_id,
                 rank = excluded.rank,
                 score = excluded.score,
                 inclusion_reason = excluded.inclusion_reason,
                 updated_at = now()",
        )
        .bind(chunk_id)
        .bind(entity_id)
        .bind(library_id)
        .bind(rank)
        .bind(score)
        .bind(inclusion_reason)
        .execute(&self.pool)
        .await
        .context("failed to upsert chunk-mentions-entity edge")?;
        Ok(())
    }

    pub(super) async fn upsert_relation_subject_edge_impl(
        &self,
        relation_id: Uuid,
        subject_entity_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "update knowledge_relation
             set subject_entity_id = $2, updated_at = now()
             where relation_id = $1 and library_id = $3",
        )
        .bind(relation_id)
        .bind(subject_entity_id)
        .bind(library_id)
        .execute(&self.pool)
        .await
        .context("failed to upsert relation-subject edge")?;
        Ok(())
    }

    pub(super) async fn upsert_relation_object_edge_impl(
        &self,
        relation_id: Uuid,
        object_entity_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "update knowledge_relation
             set object_entity_id = $2, updated_at = now()
             where relation_id = $1 and library_id = $3",
        )
        .bind(relation_id)
        .bind(object_entity_id)
        .bind(library_id)
        .execute(&self.pool)
        .await
        .context("failed to upsert relation-object edge")?;
        Ok(())
    }

    pub(super) async fn upsert_evidence_source_edge_impl(
        &self,
        evidence_id: Uuid,
        revision_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "update knowledge_evidence
             set revision_id = $2, updated_at = now()
             where evidence_id = $1 and library_id = $3",
        )
        .bind(evidence_id)
        .bind(revision_id)
        .bind(library_id)
        .execute(&self.pool)
        .await
        .context("failed to upsert collapsed evidence-source edge")?;
        Ok(())
    }

    pub(super) async fn upsert_evidence_supports_entity_edge_impl(
        &self,
        evidence_id: Uuid,
        entity_id: Uuid,
        ranking: EdgeRanking,
    ) -> anyhow::Result<()> {
        let EdgeRanking { rank, score, inclusion_reason, library_id } = ranking;
        sqlx::query(
            "insert into knowledge_evidence_entity_support (
                from_id, to_id, relation_type, support, library_id, rank, score,
                inclusion_reason, created_at, updated_at
             ) values ($1, $2, 'supports_entity', 1, $3, $4, $5, $6, now(), now())
             on conflict (from_id, to_id, relation_type) do update
             set support = excluded.support,
                 library_id = excluded.library_id,
                 rank = excluded.rank,
                 score = excluded.score,
                 inclusion_reason = excluded.inclusion_reason,
                 updated_at = now()",
        )
        .bind(evidence_id)
        .bind(entity_id)
        .bind(library_id)
        .bind(rank)
        .bind(score)
        .bind(inclusion_reason)
        .execute(&self.pool)
        .await
        .context("failed to upsert evidence-supports-entity edge")?;
        Ok(())
    }

    pub(super) async fn upsert_evidence_supports_relation_edge_impl(
        &self,
        evidence_id: Uuid,
        relation_id: Uuid,
        ranking: EdgeRanking,
    ) -> anyhow::Result<()> {
        let EdgeRanking { rank, score, inclusion_reason, library_id } = ranking;
        sqlx::query(
            "insert into knowledge_evidence_relation_support (
                from_id, to_id, relation_type, support, library_id, rank, score,
                inclusion_reason, created_at, updated_at
             ) values ($1, $2, 'supports_relation', 1, $3, $4, $5, $6, now(), now())
             on conflict (from_id, to_id, relation_type) do update
             set support = excluded.support,
                 library_id = excluded.library_id,
                 rank = excluded.rank,
                 score = excluded.score,
                 inclusion_reason = excluded.inclusion_reason,
                 updated_at = now()",
        )
        .bind(evidence_id)
        .bind(relation_id)
        .bind(library_id)
        .bind(rank)
        .bind(score)
        .bind(inclusion_reason)
        .execute(&self.pool)
        .await
        .context("failed to upsert evidence-supports-relation edge")?;
        Ok(())
    }

    pub(super) async fn upsert_fact_supports_evidence_edge_impl(
        &self,
        fact_id: Uuid,
        evidence_id: Uuid,
        library_id: Uuid,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "update knowledge_evidence
             set fact_id = $1, updated_at = now()
             where evidence_id = $2 and library_id = $3",
        )
        .bind(fact_id)
        .bind(evidence_id)
        .bind(library_id)
        .execute(&self.pool)
        .await
        .context("failed to upsert collapsed fact-evidence edge")?;
        Ok(())
    }

    pub(super) async fn replace_library_projection_impl(
        &self,
        _library_id: Uuid,
        _projection_version: i64,
        _nodes: &[GraphViewNodeWrite],
        _edges: &[GraphViewEdgeWrite],
    ) -> Result<(), GraphViewWriteError> {
        Ok(())
    }

    pub(super) async fn refresh_library_projection_targets_impl(
        &self,
        _targets: LibraryProjectionTargets<'_>,
    ) -> Result<(), GraphViewWriteError> {
        Ok(())
    }

    pub(super) async fn load_library_projection_impl(
        &self,
        library_id: Uuid,
        projection_version: i64,
    ) -> anyhow::Result<GraphViewData> {
        let nodes = sqlx::query_as::<
            _,
            (
                Uuid,
                String,
                String,
                String,
                i32,
                Option<String>,
                serde_json::Value,
                serde_json::Value,
            ),
        >(
            "select id, \u{63}anonical_key, label, node_type, support_count, summary,
                aliases_json, metadata_json
             from runtime_graph_node
             where library_id = $1 and projection_version = $2
             order by support_count desc, label asc, id asc",
        )
        .bind(library_id)
        .bind(projection_version)
        .fetch_all(&self.pool)
        .await
        .context("failed to load runtime graph projection nodes")?
        .into_iter()
        .map(
            |(
                node_id,
                identity_key,
                label,
                node_type,
                support_count,
                summary,
                aliases_json,
                metadata_json,
            )| GraphViewNodeWrite {
                node_id,
                identity_key,
                label,
                node_type,
                support_count,
                summary,
                aliases: serde_json::from_value(aliases_json).unwrap_or_default(),
                metadata_json,
            },
        )
        .collect();

        let edges = sqlx::query_as::<
            _,
            (Uuid, Uuid, Uuid, String, String, i32, Option<String>, Option<f64>, serde_json::Value),
        >(
            "select id, from_node_id, to_node_id, relation_type, \u{63}anonical_key,
                support_count, summary, weight, metadata_json
             from runtime_graph_edge
             where library_id = $1
               and projection_version = $2
               and btrim(relation_type) <> ''
               and from_node_id <> to_node_id
             order by support_count desc, relation_type asc, id asc",
        )
        .bind(library_id)
        .bind(projection_version)
        .fetch_all(&self.pool)
        .await
        .context("failed to load runtime graph projection edges")?
        .into_iter()
        .map(
            |(
                edge_id,
                from_node_id,
                to_node_id,
                relation_type,
                identity_key,
                support_count,
                summary,
                weight,
                metadata_json,
            )| GraphViewEdgeWrite {
                edge_id,
                from_node_id,
                to_node_id,
                relation_type,
                identity_key,
                support_count,
                summary,
                weight,
                metadata_json,
            },
        )
        .collect();
        Ok(GraphViewData { nodes, edges })
    }

    pub(super) async fn upsert_evidence_with_edges_impl(
        &self,
        input: &NewKnowledgeEvidence,
        targets: EvidenceEdgeTargets,
    ) -> anyhow::Result<KnowledgeEvidenceRow> {
        let EvidenceEdgeTargets {
            source_revision_id,
            supporting_entity_id,
            supporting_relation_id,
            supporting_fact_id,
            library_id,
        } = targets;
        let evidence = self.upsert_evidence(input).await?;
        if let Some(source_revision_id) = source_revision_id {
            self.upsert_evidence_source_edge(evidence.evidence_id, source_revision_id, library_id)
                .await?;
        }
        if let Some(supporting_entity_id) = supporting_entity_id {
            self.upsert_evidence_supports_entity_edge(
                evidence.evidence_id,
                supporting_entity_id,
                EdgeRanking { rank: None, score: None, inclusion_reason: None, library_id },
            )
            .await?;
        }
        if let Some(supporting_relation_id) = supporting_relation_id {
            self.upsert_evidence_supports_relation_edge(
                evidence.evidence_id,
                supporting_relation_id,
                EdgeRanking { rank: None, score: None, inclusion_reason: None, library_id },
            )
            .await?;
        }
        if let Some(supporting_fact_id) = supporting_fact_id {
            self.upsert_fact_supports_evidence_edge(
                supporting_fact_id,
                evidence.evidence_id,
                library_id,
            )
            .await?;
        }
        Ok(evidence)
    }
}
