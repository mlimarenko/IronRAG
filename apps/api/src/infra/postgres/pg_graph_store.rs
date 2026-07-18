use anyhow::Context;
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use sqlx::PgPool;
use uuid::Uuid;

use crate::infra::{
    knowledge_plane::GraphStore,
    knowledge_rows::{
        GraphViewData, GraphViewEdgeWrite, GraphViewNodeWrite, GraphViewWriteError,
        KnowledgeEntityCandidateRow, KnowledgeEntityRow, KnowledgeEvidenceRow,
        KnowledgeGraphTraversalRow, KnowledgeRelationCandidateRow,
        KnowledgeRelationEvidenceLookupRow, KnowledgeRelationRow, KnowledgeRelationTopologyRow,
        NewKnowledgeEntity, NewKnowledgeEntityCandidate, NewKnowledgeEvidence,
        NewKnowledgeRelation, NewKnowledgeRelationCandidate,
    },
};

const KNOWLEDGE_CHUNK_COLLECTION: &str = "knowledge_chunk";
const KNOWLEDGE_DOCUMENT_COLLECTION: &str = "knowledge_document";
const KNOWLEDGE_ENTITY_COLLECTION: &str = "knowledge_entity";
const KNOWLEDGE_EVIDENCE_COLLECTION: &str = "knowledge_evidence";
const KNOWLEDGE_RELATION_COLLECTION: &str = "knowledge_relation";
const ENTITY_CANDIDATE_ROW_SQL: &str = "select candidate_id, workspace_id,
    library_id, revision_id, chunk_id, candidate_label,
    candidate_type, candidate_sub_type, normalization_key, confidence, extraction_method,
    candidate_state, created_at, updated_at";
const RELATION_CANDIDATE_ROW_SQL: &str = "select candidate_id, workspace_id,
    library_id, revision_id, chunk_id, subject_label,
    subject_candidate_key, predicate, object_label, object_candidate_key, normalized_assertion,
    confidence, extraction_method, candidate_state, created_at, updated_at";
const ENTITY_ROW_SQL: &str = "select entity_id, workspace_id, library_id,
    canonical_label, aliases, entity_type, entity_sub_type, summary, confidence,
    support_count, freshness_generation, entity_state, created_at, updated_at";
const RELATION_ROW_SQL: &str = "select relation_id, workspace_id, library_id,
    predicate, normalized_assertion, confidence, support_count, contradiction_state,
    freshness_generation, relation_state, created_at, updated_at";
const EVIDENCE_ROW_SQL: &str = "select evidence_id, workspace_id, library_id,
    document_id, revision_id, chunk_id, block_id, fact_id, span_start, span_end,
    quote_text, literal_spans_json, evidence_kind, extraction_method, confidence,
    evidence_state, freshness_generation, created_at, updated_at";

#[derive(Clone)]
pub struct PgGraphStore {
    pub pool: PgPool,
}

fn decode_json<T: DeserializeOwned>(value: serde_json::Value, context: &str) -> anyhow::Result<T> {
    serde_json::from_value(value).with_context(|| context.to_string())
}

fn decode_json_rows<T: DeserializeOwned>(
    values: Vec<serde_json::Value>,
    context: &str,
) -> anyhow::Result<Vec<T>> {
    values.into_iter().map(|value| decode_json(value, context)).collect()
}

fn clamp_limit(limit: usize) -> i64 {
    i64::try_from(limit.max(1)).unwrap_or(i64::MAX)
}

fn clamp_depth(max_depth: usize) -> i32 {
    i32::try_from(max_depth.clamp(1, 2)).unwrap_or(2)
}

impl PgGraphStore {
    async fn active_projection_version(&self, library_id: Uuid) -> anyhow::Result<i64> {
        sqlx::query_scalar::<_, i64>(
            "select coalesce(
                (
                    select projection_version
                    from runtime_graph_snapshot
                    where library_id = $1
                      and projection_version > 0
                      and graph_status <> 'empty'
                    limit 1
                ),
                (
                    select max(projection_version)
                    from runtime_graph_node
                    where library_id = $1
                ),
                1
            )",
        )
        .bind(library_id)
        .fetch_one(&self.pool)
        .await
        .context("failed to load active runtime graph projection version")
    }

    async fn traverse_runtime_graph(
        &self,
        start_kind: &str,
        start_id: Uuid,
        library_id: Uuid,
        max_depth: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeGraphTraversalRow>> {
        let rows = sqlx::query_scalar::<_, serde_json::Value>(
            "with recursive active_projection(projection_version) as (
                select coalesce(
                    (
                        select projection_version
                        from runtime_graph_snapshot
                        where library_id = $1
                          and projection_version > 0
                          and graph_status <> 'empty'
                        limit 1
                    ),
                    (
                        select max(projection_version)
                        from runtime_graph_node
                        where library_id = $1
                    ),
                    1
                )
            ),
            walk(vertex_kind, vertex_id, depth, via_edge_id, path) as (
                select $2::text, $3::uuid, 0::int, null::uuid, array[$3::uuid]
                union all
                select next.vertex_kind, next.vertex_id, walk.depth + 1, next.via_edge_id,
                    walk.path || next.vertex_id
                from walk
                join lateral (
                    select 'runtime_node'::text as vertex_kind,
                        edge.to_node_id as vertex_id,
                        edge.id as via_edge_id,
                        edge.support_count
                    from runtime_graph_edge edge
                    where walk.vertex_kind = 'runtime_node'
                      and edge.library_id = $1
                      and edge.projection_version = (select projection_version from active_projection)
                      and btrim(edge.relation_type) <> ''
                      and edge.from_node_id <> edge.to_node_id
                      and edge.from_node_id = walk.vertex_id
                    union all
                    select 'runtime_node'::text as vertex_kind,
                        edge.from_node_id as vertex_id,
                        edge.id as via_edge_id,
                        edge.support_count
                    from runtime_graph_edge edge
                    where walk.vertex_kind = 'runtime_node'
                      and edge.library_id = $1
                      and edge.projection_version = (select projection_version from active_projection)
                      and btrim(edge.relation_type) <> ''
                      and edge.from_node_id <> edge.to_node_id
                      and edge.to_node_id = walk.vertex_id
                    union all
                    select 'runtime_edge'::text, edge.id, edge.id, edge.support_count
                    from runtime_graph_edge edge
                    where walk.vertex_kind = 'runtime_node'
                      and edge.library_id = $1
                      and edge.projection_version = (select projection_version from active_projection)
                      and btrim(edge.relation_type) <> ''
                      and edge.from_node_id <> edge.to_node_id
                      and edge.from_node_id = walk.vertex_id
                    union all
                    select 'runtime_edge'::text, edge.id, edge.id, edge.support_count
                    from runtime_graph_edge edge
                    where walk.vertex_kind = 'runtime_node'
                      and edge.library_id = $1
                      and edge.projection_version = (select projection_version from active_projection)
                      and btrim(edge.relation_type) <> ''
                      and edge.from_node_id <> edge.to_node_id
                      and edge.to_node_id = walk.vertex_id
                    union all
                    select 'runtime_node'::text, edge.from_node_id, edge.id, edge.support_count
                    from runtime_graph_edge edge
                    where walk.vertex_kind = 'runtime_edge'
                      and edge.id = walk.vertex_id
                      and edge.library_id = $1
                      and edge.projection_version = (select projection_version from active_projection)
                    union all
                    select 'runtime_node'::text, edge.to_node_id, edge.id, edge.support_count
                    from runtime_graph_edge edge
                    where walk.vertex_kind = 'runtime_edge'
                      and edge.id = walk.vertex_id
                      and edge.library_id = $1
                      and edge.projection_version = (select projection_version from active_projection)
                    union all
                    select 'runtime_evidence'::text, evidence.id, null::uuid, 0::int
                    from runtime_graph_evidence evidence
                    where evidence.library_id = $1
                      and (
                        (walk.vertex_kind = 'runtime_node'
                         and evidence.target_kind = 'node'
                         and evidence.target_id = walk.vertex_id)
                        or
                        (walk.vertex_kind = 'runtime_edge'
                         and evidence.target_kind = 'edge'
                         and evidence.target_id = walk.vertex_id)
                      )
                    order by support_count desc, vertex_id asc
                ) next on true
                where walk.depth < $4
                  and not next.vertex_id = any(walk.path)
            ),
            ranked as (
                select vertex_kind, vertex_id, min(depth)::bigint as path_length,
                    (array_agg(via_edge_id order by depth asc))[1] as via_edge_id
                from walk
                group by vertex_kind, vertex_id
            ),
            limited as (
                select *
                from ranked
                order by path_length asc, vertex_kind asc, vertex_id asc
                limit $5
            ),
            projected as (
                select limited.path_length,
                    case
                        when limited.vertex_kind = 'runtime_node' and node.node_type = 'document'
                            then $10
                        when limited.vertex_kind = 'runtime_node' then $6
                        when limited.vertex_kind = 'runtime_edge' then $7
                        when limited.vertex_kind = 'runtime_evidence' then $8
                        else $9
                    end as vertex_kind,
                    limited.vertex_id,
                    case when edge.id is null then null else 'runtime_graph_edge' end as edge_kind,
                    edge.canonical_key as edge_key,
                    edge.rank as edge_rank,
                    coalesce(edge.edge_score, edge.weight) as edge_score,
                    edge.inclusion_reason as edge_inclusion_reason,
                    case
                        when limited.vertex_kind = 'runtime_node' then to_jsonb(node)
                        when limited.vertex_kind = 'runtime_edge' then to_jsonb(relation_edge)
                        when limited.vertex_kind = 'runtime_evidence' then to_jsonb(evidence)
                        else '{}'::jsonb
                    end as vertex
                from limited
                left join runtime_graph_node node
                  on limited.vertex_kind = 'runtime_node'
                 and node.id = limited.vertex_id
                 and node.library_id = $1
                 and node.projection_version = (select projection_version from active_projection)
                left join runtime_graph_edge relation_edge
                  on limited.vertex_kind = 'runtime_edge'
                 and relation_edge.id = limited.vertex_id
                 and relation_edge.library_id = $1
                 and relation_edge.projection_version = (select projection_version from active_projection)
                left join runtime_graph_evidence evidence
                  on limited.vertex_kind = 'runtime_evidence'
                 and evidence.id = limited.vertex_id
                 and evidence.library_id = $1
                left join runtime_graph_edge edge
                  on edge.id = limited.via_edge_id
                 and edge.library_id = $1
                 and edge.projection_version = (select projection_version from active_projection)
                where (limited.vertex_kind = 'runtime_node' and node.id is not null)
                   or (limited.vertex_kind = 'runtime_edge' and relation_edge.id is not null)
                   or (limited.vertex_kind = 'runtime_evidence' and evidence.id is not null)
            )
            select jsonb_build_object(
                'path_length', projected.path_length,
                'vertex_kind', projected.vertex_kind,
                'vertex_id', projected.vertex_id,
                'edge_kind', projected.edge_kind,
                'edge_key', projected.edge_key,
                'edge_rank', projected.edge_rank,
                'edge_score', projected.edge_score,
                'edge_inclusion_reason', projected.edge_inclusion_reason,
                'vertex', projected.vertex
            )
            from projected
            order by projected.path_length asc, projected.vertex_kind asc, projected.vertex_id asc",
        )
        .bind(library_id)
        .bind(start_kind)
        .bind(start_id)
        .bind(clamp_depth(max_depth))
        .bind(clamp_limit(limit))
        .bind(KNOWLEDGE_ENTITY_COLLECTION)
        .bind(KNOWLEDGE_RELATION_COLLECTION)
        .bind(KNOWLEDGE_EVIDENCE_COLLECTION)
        .bind(KNOWLEDGE_CHUNK_COLLECTION)
        .bind(KNOWLEDGE_DOCUMENT_COLLECTION)
        .fetch_all(&self.pool)
        .await
        .context("failed to traverse runtime graph")?;
        decode_json_rows(rows, "failed to decode runtime graph traversal rows")
    }
}

#[async_trait]
impl GraphStore for PgGraphStore {
    async fn ping(&self) -> anyhow::Result<()> {
        sqlx::query("select 1").execute(&self.pool).await.context("postgres graph ping failed")?;
        Ok(())
    }

    async fn upsert_entity_candidate(
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

    async fn upsert_entity_candidates(
        &self,
        inputs: &[NewKnowledgeEntityCandidate],
    ) -> anyhow::Result<Vec<KnowledgeEntityCandidateRow>> {
        let mut rows = Vec::with_capacity(inputs.len());
        for input in inputs {
            rows.push(self.upsert_entity_candidate(input).await?);
        }
        Ok(rows)
    }

    async fn list_entity_candidates_by_revision(
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

    async fn list_entity_candidates_by_library(
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

    async fn delete_entity_candidates_by_revision(
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

    async fn delete_entity_candidates_by_library(
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

    async fn upsert_relation_candidate(
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

    async fn upsert_relation_candidates(
        &self,
        inputs: &[NewKnowledgeRelationCandidate],
    ) -> anyhow::Result<Vec<KnowledgeRelationCandidateRow>> {
        let mut rows = Vec::with_capacity(inputs.len());
        for input in inputs {
            rows.push(self.upsert_relation_candidate(input).await?);
        }
        Ok(rows)
    }

    async fn list_relation_candidates_by_revision(
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

    async fn list_relation_candidates_by_library(
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

    async fn delete_relation_candidates_by_revision(
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

    async fn delete_relation_candidates_by_library(
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

    async fn upsert_document_revision_edge(
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

    async fn upsert_revision_chunk_edge(
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

    async fn insert_revision_chunk_edges(
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

    async fn delete_revision_chunk_edges(&self, revision_id: Uuid) -> anyhow::Result<u64> {
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

    async fn upsert_chunk_mentions_entity_edge(
        &self,
        chunk_id: Uuid,
        entity_id: Uuid,
        rank: Option<i32>,
        score: Option<f64>,
        inclusion_reason: Option<String>,
        library_id: Uuid,
    ) -> anyhow::Result<()> {
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

    async fn upsert_relation_subject_edge(
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

    async fn upsert_relation_object_edge(
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

    async fn upsert_evidence_source_edge(
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

    async fn upsert_evidence_supports_entity_edge(
        &self,
        evidence_id: Uuid,
        entity_id: Uuid,
        rank: Option<i32>,
        score: Option<f64>,
        inclusion_reason: Option<String>,
        library_id: Uuid,
    ) -> anyhow::Result<()> {
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

    async fn upsert_evidence_supports_relation_edge(
        &self,
        evidence_id: Uuid,
        relation_id: Uuid,
        rank: Option<i32>,
        score: Option<f64>,
        inclusion_reason: Option<String>,
        library_id: Uuid,
    ) -> anyhow::Result<()> {
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

    async fn upsert_fact_supports_evidence_edge(
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

    async fn replace_library_projection(
        &self,
        _library_id: Uuid,
        _projection_version: i64,
        _nodes: &[GraphViewNodeWrite],
        _edges: &[GraphViewEdgeWrite],
    ) -> Result<(), GraphViewWriteError> {
        Ok(())
    }

    async fn refresh_library_projection_targets(
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

    async fn load_library_projection(
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
            "select id, canonical_key, label, node_type, support_count, summary,
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
                canonical_key,
                label,
                node_type,
                support_count,
                summary,
                aliases_json,
                metadata_json,
            )| GraphViewNodeWrite {
                node_id,
                canonical_key,
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
            "select id, from_node_id, to_node_id, relation_type, canonical_key,
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
                canonical_key,
                support_count,
                summary,
                weight,
                metadata_json,
            )| GraphViewEdgeWrite {
                edge_id,
                from_node_id,
                to_node_id,
                relation_type,
                canonical_key,
                support_count,
                summary,
                weight,
                metadata_json,
            },
        )
        .collect();
        Ok(GraphViewData { nodes, edges })
    }

    async fn upsert_evidence_with_edges(
        &self,
        input: &NewKnowledgeEvidence,
        source_revision_id: Option<Uuid>,
        supporting_entity_id: Option<Uuid>,
        supporting_relation_id: Option<Uuid>,
        supporting_fact_id: Option<Uuid>,
        library_id: Uuid,
    ) -> anyhow::Result<KnowledgeEvidenceRow> {
        let evidence = self.upsert_evidence(input).await?;
        if let Some(source_revision_id) = source_revision_id {
            self.upsert_evidence_source_edge(evidence.evidence_id, source_revision_id, library_id)
                .await?;
        }
        if let Some(supporting_entity_id) = supporting_entity_id {
            self.upsert_evidence_supports_entity_edge(
                evidence.evidence_id,
                supporting_entity_id,
                None,
                None,
                None,
                library_id,
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
                library_id,
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

    async fn reset_library_materialized_graph(&self, library_id: Uuid) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await.context("failed to begin graph reset transaction")?;
        for sql in [
            "delete from knowledge_chunk_entity_mention where library_id = $1",
            "delete from knowledge_evidence_entity_support where library_id = $1",
            "delete from knowledge_evidence_relation_support where library_id = $1",
            "delete from knowledge_bundle_entity where library_id = $1",
            "delete from knowledge_bundle_relation where library_id = $1",
            "delete from knowledge_bundle_evidence where library_id = $1",
            "delete from knowledge_evidence where library_id = $1",
            "delete from knowledge_relation where library_id = $1",
            "delete from knowledge_entity where library_id = $1",
            "delete from runtime_graph_evidence where library_id = $1",
            "delete from runtime_graph_edge where library_id = $1",
            "delete from runtime_graph_node where library_id = $1",
            "delete from runtime_graph_community where library_id = $1",
            "delete from runtime_graph_canonical_summary where library_id = $1",
        ] {
            sqlx::query(sql)
                .bind(library_id)
                .execute(&mut *tx)
                .await
                .with_context(|| format!("failed to run graph reset statement `{sql}`"))?;
        }
        tx.commit().await.context("failed to commit graph reset transaction")?;
        Ok(())
    }

    async fn upsert_entity(
        &self,
        input: &NewKnowledgeEntity,
    ) -> anyhow::Result<KnowledgeEntityRow> {
        let sql = format!(
            "with upserted as (
                insert into knowledge_entity (
                    entity_id, workspace_id, library_id, canonical_label, aliases, entity_type,
                    entity_sub_type, summary, confidence, support_count, freshness_generation,
                    entity_state, created_at, updated_at
                ) values (
                    $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                    coalesce($13, now()), coalesce($14, now())
                )
                on conflict (entity_id) do update
                set workspace_id = excluded.workspace_id,
                    library_id = excluded.library_id,
                    canonical_label = excluded.canonical_label,
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
            .bind(&input.canonical_label)
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

    async fn upsert_entities(
        &self,
        inputs: &[NewKnowledgeEntity],
    ) -> anyhow::Result<Vec<KnowledgeEntityRow>> {
        let mut rows = Vec::with_capacity(inputs.len());
        for input in inputs {
            rows.push(self.upsert_entity(input).await?);
        }
        Ok(rows)
    }

    async fn get_entity_by_id(
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

    async fn get_entity_by_library_and_label(
        &self,
        library_id: Uuid,
        canonical_label: &str,
    ) -> anyhow::Result<Option<KnowledgeEntityRow>> {
        let sql = format!(
            "select to_jsonb(row)
             from ({ENTITY_ROW_SQL}
                   from knowledge_entity
                   where library_id = $1 and canonical_label = $2
                   order by updated_at desc, entity_id desc
                   limit 1) row"
        );
        let row = sqlx::query_scalar::<_, serde_json::Value>(sqlx::AssertSqlSafe(&*sql))
            .bind(library_id)
            .bind(canonical_label)
            .fetch_optional(&self.pool)
            .await
            .context("failed to lookup knowledge entity by label")?;
        row.map(|value| decode_json(value, "failed to decode knowledge entity row")).transpose()
    }

    async fn list_entities_by_library(
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

    async fn upsert_relation(
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

    async fn upsert_relation_with_endpoints(
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

    async fn upsert_relations(
        &self,
        inputs: &[NewKnowledgeRelation],
    ) -> anyhow::Result<Vec<KnowledgeRelationRow>> {
        let mut rows = Vec::with_capacity(inputs.len());
        for input in inputs {
            rows.push(self.upsert_relation(input).await?);
        }
        Ok(rows)
    }

    async fn get_relation_by_id(
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

    async fn get_relation_by_library_and_assertion(
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

    async fn list_relations_by_library(
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

    async fn delete_entities_by_canonical_keys(
        &self,
        library_id: Uuid,
        keys: &[String],
    ) -> anyhow::Result<u64> {
        if keys.is_empty() {
            return Ok(0);
        }
        let result = sqlx::query(
            "delete from runtime_graph_node
             where library_id = $1 and canonical_key = any($2)",
        )
        .bind(library_id)
        .bind(keys)
        .execute(&self.pool)
        .await
        .context("failed to delete runtime graph nodes by canonical keys")?;
        Ok(result.rows_affected())
    }

    async fn delete_relations_by_canonical_keys(
        &self,
        library_id: Uuid,
        keys: &[String],
    ) -> anyhow::Result<u64> {
        if keys.is_empty() {
            return Ok(0);
        }
        let result = sqlx::query(
            "delete from runtime_graph_edge
             where library_id = $1 and canonical_key = any($2)",
        )
        .bind(library_id)
        .bind(keys)
        .execute(&self.pool)
        .await
        .context("failed to delete runtime graph edges by canonical keys")?;
        Ok(result.rows_affected())
    }

    async fn upsert_evidence(
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

    async fn get_evidence_by_id(
        &self,
        evidence_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeEvidenceRow>> {
        let mut rows = self.list_evidence_by_ids(&[evidence_id]).await?;
        Ok(rows.pop())
    }

    async fn list_evidence_by_ids(
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

    async fn list_evidence_by_revision(
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

    async fn list_evidence_by_chunk(
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

    async fn list_relation_topology_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRelationTopologyRow>> {
        let projection_version = self.active_projection_version(library_id).await?;
        let rows = sqlx::query_scalar::<_, serde_json::Value>(
            "select jsonb_build_object(
                'relation_id', edge.id,
                'workspace_id', library.workspace_id,
                'library_id', edge.library_id,
                'predicate', edge.relation_type,
                'normalized_assertion', edge.canonical_key,
                'confidence', edge.weight,
                'support_count', edge.support_count::bigint,
                'contradiction_state', coalesce(edge.metadata_json->>'contradiction_state', ''),
                'freshness_generation', edge.projection_version,
                'relation_state', coalesce(edge.metadata_json->>'relation_state', 'active'),
                'created_at', edge.created_at,
                'updated_at', edge.updated_at,
                'subject_entity_id', edge.from_node_id,
                'object_entity_id', edge.to_node_id
            )
            from runtime_graph_edge edge
            join catalog_library library on library.id = edge.library_id
            where edge.library_id = $1
              and edge.projection_version = $2
              and btrim(edge.relation_type) <> ''
              and edge.from_node_id <> edge.to_node_id
            order by edge.support_count desc, edge.updated_at desc, edge.id desc
            limit 10000",
        )
        .bind(library_id)
        .bind(projection_version)
        .fetch_all(&self.pool)
        .await
        .context("failed to list runtime relation topology by library")?;
        decode_json_rows(rows, "failed to decode relation topology rows")
    }

    async fn get_relation_topology_by_id(
        &self,
        relation_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeRelationTopologyRow>> {
        let row = sqlx::query_scalar::<_, serde_json::Value>(
            "select jsonb_build_object(
                'relation_id', edge.id,
                'workspace_id', library.workspace_id,
                'library_id', edge.library_id,
                'predicate', edge.relation_type,
                'normalized_assertion', edge.canonical_key,
                'confidence', edge.weight,
                'support_count', edge.support_count::bigint,
                'contradiction_state', coalesce(edge.metadata_json->>'contradiction_state', ''),
                'freshness_generation', edge.projection_version,
                'relation_state', coalesce(edge.metadata_json->>'relation_state', 'active'),
                'created_at', edge.created_at,
                'updated_at', edge.updated_at,
                'subject_entity_id', edge.from_node_id,
                'object_entity_id', edge.to_node_id
            )
            from runtime_graph_edge edge
            join catalog_library library on library.id = edge.library_id
            where edge.id = $1
              and btrim(edge.relation_type) <> ''
              and edge.from_node_id <> edge.to_node_id
            limit 1",
        )
        .bind(relation_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get runtime relation topology by id")?;
        row.map(|value| decode_json(value, "failed to decode relation topology row")).transpose()
    }

    async fn list_entity_neighborhood(
        &self,
        entity_id: Uuid,
        library_id: Uuid,
        max_depth: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeGraphTraversalRow>> {
        self.traverse_runtime_graph("runtime_node", entity_id, library_id, max_depth, limit).await
    }

    async fn expand_relation_centric(
        &self,
        relation_id: Uuid,
        library_id: Uuid,
        max_depth: usize,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeGraphTraversalRow>> {
        self.traverse_runtime_graph("runtime_edge", relation_id, library_id, max_depth, limit).await
    }

    async fn list_relation_evidence_lookup(
        &self,
        relation_id: Uuid,
        library_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeRelationEvidenceLookupRow>> {
        // runtime_graph_evidence does not persist the source edge's created_at.
        // Rank, evidence created_at, and id are the closest deterministic
        // ordering available in the PG runtime model.
        let rows = sqlx::query_scalar::<_, serde_json::Value>(
            "select jsonb_build_object(
                'relation', jsonb_build_object(
                    'relation_id', edge.id,
                    'workspace_id', library.workspace_id,
                    'library_id', edge.library_id,
                    'predicate', edge.relation_type,
                    'normalized_assertion', edge.canonical_key,
                    'confidence', edge.weight,
                    'support_count', edge.support_count::bigint,
                    'contradiction_state', coalesce(edge.metadata_json->>'contradiction_state', ''),
                    'freshness_generation', edge.projection_version,
                    'relation_state', coalesce(edge.metadata_json->>'relation_state', 'active'),
                    'created_at', edge.created_at,
                    'updated_at', edge.updated_at
                ),
                'evidence', jsonb_build_object(
                    'evidence_id', evidence.id,
                    'workspace_id', library.workspace_id,
                    'library_id', evidence.library_id,
                    'document_id', evidence.document_id,
                    'revision_id', evidence.revision_id,
                    'chunk_id', evidence.chunk_id,
                    'block_id', null,
                    'fact_id', null,
                    'span_start', null,
                    'span_end', null,
                    'quote_text', evidence.evidence_text,
                    'literal_spans_json', '{}'::jsonb,
                    'evidence_kind', evidence.target_kind,
                    'extraction_method', 'runtime_graph',
                    'confidence', evidence.confidence_score,
                    'evidence_state', 'active',
                    'freshness_generation', 0,
                    'created_at', evidence.created_at,
                    'updated_at', evidence.created_at
                ),
                'support_edge_rank', evidence.rank,
                'support_edge_score', coalesce(evidence.score, evidence.support_edge_score),
                'support_edge_inclusion_reason',
                    coalesce(evidence.inclusion_reason, evidence.support_edge_inclusion_reason),
                'source_document',
                    case when document.id is null then null else to_jsonb(doc_row) end,
                'source_revision',
                    case when revision.id is null then null else to_jsonb(rev_row) end,
                'source_chunk',
                    case when chunk.id is null then null else to_jsonb(chunk_row) end
            )
            from runtime_graph_edge edge
            join catalog_library library on library.id = edge.library_id
            join runtime_graph_evidence evidence
              on evidence.library_id = edge.library_id
             and evidence.target_kind = 'edge'
             and evidence.target_id = edge.id
            left join content_document document
              on document.id = evidence.document_id
             and document.library_id = evidence.library_id
            left join content_document_head document_head
              on document_head.document_id = document.id
            left join lateral (
                select max(revision_number)::bigint as latest_revision_no
                from content_revision
                where document_id = document.id
            ) latest_revision on document.id is not null
            left join lateral (
                select document.id as document_id,
                    document.workspace_id, document.library_id, document.external_key,
                    null::text as file_name, null::text as title,
                    document.document_state::text as document_state,
                    document_head.active_revision_id, document_head.readable_revision_id,
                    latest_revision.latest_revision_no, document.created_at,
                    coalesce(document_head.head_updated_at, document.created_at) as updated_at,
                    document.deleted_at
            ) doc_row on document.id is not null
            left join content_revision revision
              on revision.id = evidence.revision_id
             and revision.library_id = evidence.library_id
            left join lateral (
                select revision.id as revision_id,
                    revision.workspace_id, revision.library_id, revision.document_id,
                    revision.revision_number::bigint as revision_number,
                    'active'::text as revision_state,
                    revision.content_source_kind::text as revision_kind,
                    revision.storage_key as storage_ref,
                    revision.source_uri, revision.document_hint,
                    revision.mime_type, revision.checksum, revision.title, revision.byte_size,
                    null::text as normalized_text,
                    revision.checksum as text_checksum,
                    null::text as image_checksum,
                    'text_readable'::text as text_state,
                    'ready'::text as vector_state,
                    'ready'::text as graph_state,
                    null::timestamptz as text_readable_at,
                    null::timestamptz as vector_ready_at,
                    null::timestamptz as graph_ready_at,
                    null::uuid as superseded_by_revision_id,
                    revision.created_at
            ) rev_row on revision.id is not null
            left join content_chunk chunk
              on chunk.id = evidence.chunk_id
            left join content_revision chunk_revision
              on chunk_revision.id = chunk.revision_id
            left join lateral (
                select chunk.id as chunk_id,
                    chunk_revision.workspace_id, chunk_revision.library_id,
                    chunk_revision.document_id, chunk.revision_id, chunk.chunk_index,
                    null::text as chunk_kind,
                    chunk.normalized_text as content_text,
                    chunk.normalized_text,
                    chunk.start_offset as span_start,
                    chunk.end_offset as span_end,
                    chunk.token_count,
                    '{}'::uuid[] as support_block_ids,
                    '{}'::text[] as section_path,
                    '{}'::text[] as heading_trail,
                    chunk.text_checksum as literal_digest,
                    'active'::text as chunk_state,
                    null::bigint as text_generation,
                    null::bigint as vector_generation,
                    null::real as quality_score,
                    chunk.window_text, chunk.raptor_level::integer as raptor_level,
                    chunk.occurred_at, chunk.occurred_until
            ) chunk_row on chunk.id is not null and chunk_revision.id is not null
            where edge.id = $1
              and edge.library_id = $2
            order by evidence.rank asc nulls last, evidence.created_at asc, evidence.id asc
            limit $3",
        )
        .bind(relation_id)
        .bind(library_id)
        .bind(clamp_limit(limit))
        .fetch_all(&self.pool)
        .await
        .context("failed to lookup runtime relation evidence")?;
        decode_json_rows(rows, "failed to decode relation evidence lookup rows")
    }
}
