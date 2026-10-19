use super::*;

impl PgGraphStore {
    pub(super) async fn upsert_entity_impl(
        &self,
        input: &NewKnowledgeEntity,
    ) -> anyhow::Result<KnowledgeEntityRow> {
        let sql = format!(
            "with upserted as (
                insert into knowledge_entity (
                    entity_id, workspace_id, library_id, \u{63}anonical_label, aliases, entity_type,
                    entity_sub_type, summary, confidence, support_count, freshness_generation,
                    entity_state, created_at, updated_at
                ) values (
                    $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                    coalesce($13, now()), coalesce($14, now())
                )
                on conflict (entity_id) do update
                set workspace_id = excluded.workspace_id,
                    library_id = excluded.library_id,
                    \u{63}anonical_label = excluded.\u{63}anonical_label,
                    aliases = coalesce((
                        select array_agg(alias order by first_seen)
                        from (
                            select alias, min(ord) as first_seen
                            from unnest(coalesce(knowledge_entity.aliases, '{{}}'::text[])
                                        || excluded.aliases) with ordinality as merged(alias, ord)
                            group by alias
                        ) aliases
                    ), '{{}}'::text[]),
                    entity_type = excluded.entity_type,
                    entity_sub_type = excluded.entity_sub_type,
                    summary = excluded.summary,
                    confidence = excluded.confidence,
                    support_count = excluded.support_count,
                    freshness_generation = excluded.freshness_generation,
                    entity_state = excluded.entity_state,
                    updated_at = excluded.updated_at
                returning *
            )
            select to_jsonb(row) from ({ENTITY_ROW_SQL} from upserted) row"
        );
        let row = sqlx::query_scalar::<_, serde_json::Value>(sqlx::AssertSqlSafe(&*sql))
            .bind(input.entity_id)
            .bind(input.workspace_id)
            .bind(input.library_id)
            .bind(&input.primary_label)
            .bind(&input.aliases)
            .bind(&input.entity_type)
            .bind(input.entity_sub_type.as_deref())
            .bind(input.summary.as_deref())
            .bind(input.confidence)
            .bind(input.support_count)
            .bind(input.freshness_generation)
            .bind(&input.entity_state)
            .bind(input.created_at.as_ref())
            .bind(input.updated_at.as_ref())
            .fetch_one(&self.pool)
            .await
            .context("failed to upsert knowledge entity")?;
        decode_json(row, "failed to decode knowledge entity row")
    }

    pub(super) async fn upsert_entities_impl(
        &self,
        inputs: &[NewKnowledgeEntity],
    ) -> anyhow::Result<Vec<KnowledgeEntityRow>> {
        let mut rows = Vec::with_capacity(inputs.len());
        for input in inputs {
            rows.push(self.upsert_entity(input).await?);
        }
        Ok(rows)
    }

    pub(super) async fn get_entity_by_id_impl(
        &self,
        entity_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeEntityRow>> {
        let sql = format!(
            "select to_jsonb(row)
             from ({ENTITY_ROW_SQL}
                   from knowledge_entity
                   where entity_id = $1
                   limit 1) row"
        );
        let row = sqlx::query_scalar::<_, serde_json::Value>(sqlx::AssertSqlSafe(&*sql))
            .bind(entity_id)
            .fetch_optional(&self.pool)
            .await
            .context("failed to get knowledge entity")?;
        row.map(|value| decode_json(value, "failed to decode knowledge entity row")).transpose()
    }

    pub(super) async fn get_entity_by_library_and_label_impl(
        &self,
        library_id: Uuid,
        primary_label: &str,
    ) -> anyhow::Result<Option<KnowledgeEntityRow>> {
        let sql = format!(
            "select to_jsonb(row)
             from ({ENTITY_ROW_SQL}
                   from knowledge_entity
                   where library_id = $1 and \u{63}anonical_label = $2
                   order by updated_at desc, entity_id desc
                   limit 1) row"
        );
        let row = sqlx::query_scalar::<_, serde_json::Value>(sqlx::AssertSqlSafe(&*sql))
            .bind(library_id)
            .bind(primary_label)
            .fetch_optional(&self.pool)
            .await
            .context("failed to lookup knowledge entity by label")?;
        row.map(|value| decode_json(value, "failed to decode knowledge entity row")).transpose()
    }

    pub(super) async fn list_entities_by_library_impl(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityRow>> {
        let sql = format!(
            "select to_jsonb(row)
             from ({ENTITY_ROW_SQL}
                   from knowledge_entity
                   where library_id = $1
                   order by support_count desc, updated_at desc, entity_id desc
                   limit 5000) row"
        );
        let rows = sqlx::query_scalar::<_, serde_json::Value>(sqlx::AssertSqlSafe(&*sql))
            .bind(library_id)
            .fetch_all(&self.pool)
            .await
            .context("failed to list knowledge entities by library")?;
        decode_json_rows(rows, "failed to decode knowledge entity rows")
    }

    pub(super) async fn upsert_relation_impl(
        &self,
        input: &NewKnowledgeRelation,
    ) -> anyhow::Result<KnowledgeRelationRow> {
        let sql = format!(
            "with upserted as (
                insert into knowledge_relation (
                    relation_id, workspace_id, library_id, predicate, normalized_assertion,
                    confidence, support_count, contradiction_state, freshness_generation,
                    relation_state, created_at, updated_at
                ) values (
                    $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                    coalesce($11, now()), coalesce($12, now())
                )
                on conflict (relation_id) do update
                set workspace_id = excluded.workspace_id,
                    library_id = excluded.library_id,
                    predicate = excluded.predicate,
                    normalized_assertion = excluded.normalized_assertion,
                    confidence = excluded.confidence,
                    support_count = excluded.support_count,
                    contradiction_state = excluded.contradiction_state,
                    freshness_generation = excluded.freshness_generation,
                    relation_state = excluded.relation_state,
                    updated_at = excluded.updated_at
                returning *
            )
            select to_jsonb(row) from ({RELATION_ROW_SQL} from upserted) row"
        );
        let row = sqlx::query_scalar::<_, serde_json::Value>(sqlx::AssertSqlSafe(&*sql))
            .bind(input.relation_id)
            .bind(input.workspace_id)
            .bind(input.library_id)
            .bind(&input.predicate)
            .bind(&input.normalized_assertion)
            .bind(input.confidence)
            .bind(input.support_count)
            .bind(&input.contradiction_state)
            .bind(input.freshness_generation)
            .bind(&input.relation_state)
            .bind(input.created_at.as_ref())
            .bind(input.updated_at.as_ref())
            .fetch_one(&self.pool)
            .await
            .context("failed to upsert knowledge relation")?;
        decode_json(row, "failed to decode knowledge relation row")
    }

    pub(super) async fn upsert_relation_with_endpoints_impl(
        &self,
        input: &NewKnowledgeRelation,
        subject_entity_id: Option<Uuid>,
        object_entity_id: Option<Uuid>,
        library_id: Uuid,
    ) -> anyhow::Result<KnowledgeRelationRow> {
        let relation = self.upsert_relation(input).await?;
        if let Some(subject_entity_id) = subject_entity_id {
            self.upsert_relation_subject_edge(relation.relation_id, subject_entity_id, library_id)
                .await?;
        }
        if let Some(object_entity_id) = object_entity_id {
            self.upsert_relation_object_edge(relation.relation_id, object_entity_id, library_id)
                .await?;
        }
        Ok(relation)
    }

    pub(super) async fn upsert_relations_impl(
        &self,
        inputs: &[NewKnowledgeRelation],
    ) -> anyhow::Result<Vec<KnowledgeRelationRow>> {
        let mut rows = Vec::with_capacity(inputs.len());
        for input in inputs {
            rows.push(self.upsert_relation(input).await?);
        }
        Ok(rows)
    }

    pub(super) async fn get_relation_by_id_impl(
        &self,
        relation_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeRelationRow>> {
        let sql = format!(
            "select to_jsonb(row)
             from ({RELATION_ROW_SQL}
                   from knowledge_relation
                   where relation_id = $1
                   limit 1) row"
        );
        let row = sqlx::query_scalar::<_, serde_json::Value>(sqlx::AssertSqlSafe(&*sql))
            .bind(relation_id)
            .fetch_optional(&self.pool)
            .await
            .context("failed to get knowledge relation")?;
        row.map(|value| decode_json(value, "failed to decode knowledge relation row")).transpose()
    }

    pub(super) async fn get_relation_by_library_and_assertion_impl(
        &self,
        library_id: Uuid,
        normalized_assertion: &str,
    ) -> anyhow::Result<Option<KnowledgeRelationRow>> {
        let sql = format!(
            "select to_jsonb(row)
             from ({RELATION_ROW_SQL}
                   from knowledge_relation
                   where library_id = $1 and normalized_assertion = $2
                   order by updated_at desc, relation_id desc
                   limit 1) row"
        );
        let row = sqlx::query_scalar::<_, serde_json::Value>(sqlx::AssertSqlSafe(&*sql))
            .bind(library_id)
            .bind(normalized_assertion)
            .fetch_optional(&self.pool)
            .await
            .context("failed to lookup knowledge relation by assertion")?;
        row.map(|value| decode_json(value, "failed to decode knowledge relation row")).transpose()
    }

    pub(super) async fn list_relations_by_library_impl(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationRow>> {
        let sql = format!(
            "select to_jsonb(row)
             from ({RELATION_ROW_SQL}
                   from knowledge_relation
                   where library_id = $1
                   order by support_count desc, updated_at desc, relation_id desc) row"
        );
        let rows = sqlx::query_scalar::<_, serde_json::Value>(sqlx::AssertSqlSafe(&*sql))
            .bind(library_id)
            .fetch_all(&self.pool)
            .await
            .context("failed to list knowledge relations by library")?;
        decode_json_rows(rows, "failed to decode knowledge relation rows")
    }

    pub(super) async fn delete_entities_by_identity_keys_impl(
        &self,
        library_id: Uuid,
        keys: &[String],
    ) -> anyhow::Result<u64> {
        if keys.is_empty() {
            return Ok(0);
        }
        let result = sqlx::query(
            "delete from runtime_graph_node
             where library_id = $1 and \u{63}anonical_key = any($2)",
        )
        .bind(library_id)
        .bind(keys)
        .execute(&self.pool)
        .await
        .context("failed to delete runtime graph nodes by identity keys")?;
        Ok(result.rows_affected())
    }

    pub(super) async fn delete_relations_by_identity_keys_impl(
        &self,
        library_id: Uuid,
        keys: &[String],
    ) -> anyhow::Result<u64> {
        if keys.is_empty() {
            return Ok(0);
        }
        let result = sqlx::query(
            "delete from runtime_graph_edge
             where library_id = $1 and \u{63}anonical_key = any($2)",
        )
        .bind(library_id)
        .bind(keys)
        .execute(&self.pool)
        .await
        .context("failed to delete runtime graph edges by identity keys")?;
        Ok(result.rows_affected())
    }

    pub(super) async fn upsert_evidence_impl(
        &self,
        input: &NewKnowledgeEvidence,
    ) -> anyhow::Result<KnowledgeEvidenceRow> {
        let sql = format!(
            "with upserted as (
                insert into knowledge_evidence (
                    evidence_id, workspace_id, library_id, document_id, revision_id, chunk_id,
                    block_id, fact_id, span_start, span_end, quote_text, literal_spans_json,
                    evidence_kind, extraction_method, confidence, evidence_state,
                    freshness_generation, created_at, updated_at
                ) values (
                    $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14,
                    $15, $16, $17, coalesce($18, now()), coalesce($19, now())
                )
                on conflict (evidence_id) do update
                set workspace_id = excluded.workspace_id,
                    library_id = excluded.library_id,
                    document_id = excluded.document_id,
                    revision_id = excluded.revision_id,
                    chunk_id = excluded.chunk_id,
                    block_id = excluded.block_id,
                    fact_id = excluded.fact_id,
                    span_start = excluded.span_start,
                    span_end = excluded.span_end,
                    quote_text = excluded.quote_text,
                    literal_spans_json = excluded.literal_spans_json,
                    evidence_kind = excluded.evidence_kind,
                    extraction_method = excluded.extraction_method,
                    confidence = excluded.confidence,
                    evidence_state = excluded.evidence_state,
                    freshness_generation = excluded.freshness_generation,
                    updated_at = excluded.updated_at
                returning *
            )
            select to_jsonb(row) from ({EVIDENCE_ROW_SQL} from upserted) row"
        );
        let row = sqlx::query_scalar::<_, serde_json::Value>(sqlx::AssertSqlSafe(&*sql))
            .bind(input.evidence_id)
            .bind(input.workspace_id)
            .bind(input.library_id)
            .bind(input.document_id)
            .bind(input.revision_id)
            .bind(input.chunk_id)
            .bind(input.block_id)
            .bind(input.fact_id)
            .bind(input.span_start)
            .bind(input.span_end)
            .bind(&input.quote_text)
            .bind(&input.literal_spans_json)
            .bind(&input.evidence_kind)
            .bind(&input.extraction_method)
            .bind(input.confidence)
            .bind(&input.evidence_state)
            .bind(input.freshness_generation)
            .bind(input.created_at.as_ref())
            .bind(input.updated_at.as_ref())
            .fetch_one(&self.pool)
            .await
            .context("failed to upsert knowledge evidence")?;
        decode_json(row, "failed to decode knowledge evidence row")
    }

    pub(super) async fn get_evidence_by_id_impl(
        &self,
        evidence_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeEvidenceRow>> {
        let mut rows = self.list_evidence_by_ids(&[evidence_id]).await?;
        Ok(rows.pop())
    }

    pub(super) async fn list_evidence_by_ids_impl(
        &self,
        evidence_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeEvidenceRow>> {
        if evidence_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query_scalar::<_, serde_json::Value>(
            "select to_jsonb(row) from (
                select evidence_id, workspace_id, library_id,
                    document_id, revision_id, chunk_id, block_id, fact_id, span_start, span_end,
                    quote_text, literal_spans_json, evidence_kind, extraction_method, confidence,
                    evidence_state, freshness_generation, created_at, updated_at
                from knowledge_evidence
                where evidence_id = any($1)
                union all
                select evidence.id as evidence_id,
                    library.workspace_id, evidence.library_id, evidence.document_id,
                    evidence.revision_id, evidence.chunk_id, null::uuid as block_id,
                    null::uuid as fact_id, null::integer as span_start, null::integer as span_end,
                    evidence.evidence_text as quote_text, '{}'::jsonb as literal_spans_json,
                    evidence.target_kind as evidence_kind, 'runtime_graph'::text as extraction_method,
                    evidence.confidence_score as confidence, 'active'::text as evidence_state,
                    0::bigint as freshness_generation, evidence.created_at,
                    evidence.created_at as updated_at
                from runtime_graph_evidence evidence
                join catalog_library library on library.id = evidence.library_id
                where evidence.id = any($1)
                  and evidence.document_id is not null
                  and evidence.revision_id is not null
            ) row
            order by created_at asc, evidence_id asc",
        )
        .bind(evidence_ids)
        .fetch_all(&self.pool)
        .await
        .context("failed to list knowledge evidence by ids")?;
        decode_json_rows(rows, "failed to decode knowledge evidence rows")
    }

    pub(super) async fn list_evidence_by_revision_impl(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEvidenceRow>> {
        let rows = sqlx::query_scalar::<_, serde_json::Value>(
            "select to_jsonb(row) from (
                select evidence_id, workspace_id, library_id,
                    document_id, revision_id, chunk_id, block_id, fact_id, span_start, span_end,
                    quote_text, literal_spans_json, evidence_kind, extraction_method, confidence,
                    evidence_state, freshness_generation, created_at, updated_at
                from knowledge_evidence
                where revision_id = $1
                union all
                select evidence.id as evidence_id,
                    library.workspace_id, evidence.library_id, evidence.document_id,
                    evidence.revision_id, evidence.chunk_id, null::uuid as block_id,
                    null::uuid as fact_id, null::integer as span_start, null::integer as span_end,
                    evidence.evidence_text as quote_text, '{}'::jsonb as literal_spans_json,
                    evidence.target_kind as evidence_kind, 'runtime_graph'::text as extraction_method,
                    evidence.confidence_score as confidence, 'active'::text as evidence_state,
                    0::bigint as freshness_generation, evidence.created_at,
                    evidence.created_at as updated_at
                from runtime_graph_evidence evidence
                join catalog_library library on library.id = evidence.library_id
                where evidence.revision_id = $1
                  and evidence.document_id is not null
                  and evidence.revision_id is not null
            ) row
            order by created_at asc, evidence_id asc",
        )
        .bind(revision_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list knowledge evidence by revision")?;
        decode_json_rows(rows, "failed to decode knowledge evidence rows")
    }

    pub(super) async fn list_evidence_by_chunk_impl(
        &self,
        chunk_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEvidenceRow>> {
        let rows = sqlx::query_scalar::<_, serde_json::Value>(
            "select to_jsonb(row) from (
                select evidence_id, workspace_id, library_id,
                    document_id, revision_id, chunk_id, block_id, fact_id, span_start, span_end,
                    quote_text, literal_spans_json, evidence_kind, extraction_method, confidence,
                    evidence_state, freshness_generation, created_at, updated_at
                from knowledge_evidence
                where chunk_id = $1
                union all
                select evidence.id as evidence_id,
                    library.workspace_id, evidence.library_id, evidence.document_id,
                    evidence.revision_id, evidence.chunk_id, null::uuid as block_id,
                    null::uuid as fact_id, null::integer as span_start, null::integer as span_end,
                    evidence.evidence_text as quote_text, '{}'::jsonb as literal_spans_json,
                    evidence.target_kind as evidence_kind, 'runtime_graph'::text as extraction_method,
                    evidence.confidence_score as confidence, 'active'::text as evidence_state,
                    0::bigint as freshness_generation, evidence.created_at,
                    evidence.created_at as updated_at
                from runtime_graph_evidence evidence
                join catalog_library library on library.id = evidence.library_id
                where evidence.chunk_id = $1
                  and evidence.document_id is not null
                  and evidence.revision_id is not null
            ) row
            order by created_at asc, evidence_id asc",
        )
        .bind(chunk_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list knowledge evidence by chunk")?;
        decode_json_rows(rows, "failed to decode knowledge evidence rows")
    }
}
