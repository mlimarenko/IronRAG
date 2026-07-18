use anyhow::Context;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::infra::{
    knowledge_plane::ContextStore,
    knowledge_rows::{
        KnowledgeBundleChunkEdgeRow, KnowledgeBundleEntityEdgeRow, KnowledgeBundleEvidenceEdgeRow,
        KnowledgeBundleRelationEdgeRow, KnowledgeContextBundleReferenceSetRow,
        KnowledgeContextBundleRow, KnowledgeRetrievalTraceRow,
    },
};

#[derive(Clone)]
pub struct PgContextStore {
    pub pool: PgPool,
}

#[derive(Debug, Clone, FromRow)]
struct PgBundleRow {
    bundle_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    query_execution_id: Option<Uuid>,
    bundle_state: String,
    bundle_strategy: String,
    requested_mode: String,
    resolved_mode: String,
    selected_fact_ids: Vec<Uuid>,
    verification_state: String,
    verification_warnings: serde_json::Value,
    freshness_snapshot: serde_json::Value,
    candidate_summary: serde_json::Value,
    assembly_diagnostics: serde_json::Value,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<PgBundleRow> for KnowledgeContextBundleRow {
    fn from(row: PgBundleRow) -> Self {
        Self {
            bundle_id: row.bundle_id,
            workspace_id: row.workspace_id,
            library_id: row.library_id,
            query_execution_id: row.query_execution_id,
            bundle_state: row.bundle_state,
            bundle_strategy: row.bundle_strategy,
            requested_mode: row.requested_mode,
            resolved_mode: row.resolved_mode,
            selected_fact_ids: row.selected_fact_ids,
            verification_state: row.verification_state,
            verification_warnings: row.verification_warnings,
            freshness_snapshot: row.freshness_snapshot,
            candidate_summary: row.candidate_summary,
            assembly_diagnostics: row.assembly_diagnostics,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(Debug, Clone, FromRow)]
struct PgTraceRow {
    trace_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    query_execution_id: Option<Uuid>,
    bundle_id: Uuid,
    trace_state: String,
    retrieval_strategy: String,
    candidate_counts: serde_json::Value,
    dropped_reasons: serde_json::Value,
    timing_breakdown: serde_json::Value,
    diagnostics_json: serde_json::Value,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<PgTraceRow> for KnowledgeRetrievalTraceRow {
    fn from(row: PgTraceRow) -> Self {
        Self {
            trace_id: row.trace_id,
            workspace_id: row.workspace_id,
            library_id: row.library_id,
            query_execution_id: row.query_execution_id,
            bundle_id: row.bundle_id,
            trace_state: row.trace_state,
            retrieval_strategy: row.retrieval_strategy,
            candidate_counts: row.candidate_counts,
            dropped_reasons: row.dropped_reasons,
            timing_breakdown: row.timing_breakdown,
            diagnostics_json: row.diagnostics_json,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(Debug, Clone, FromRow)]
struct PgChunkEdgeRow {
    bundle_id: Uuid,
    chunk_id: Uuid,
    rank: i32,
    score: f64,
    inclusion_reason: Option<String>,
    created_at: DateTime<Utc>,
}

impl From<PgChunkEdgeRow> for KnowledgeBundleChunkEdgeRow {
    fn from(row: PgChunkEdgeRow) -> Self {
        Self {
            bundle_id: row.bundle_id,
            chunk_id: row.chunk_id,
            rank: row.rank,
            score: row.score,
            inclusion_reason: row.inclusion_reason,
            created_at: row.created_at,
        }
    }
}

#[derive(Debug, Clone, FromRow)]
struct PgEntityEdgeRow {
    bundle_id: Uuid,
    entity_id: Uuid,
    rank: i32,
    score: f64,
    inclusion_reason: Option<String>,
    created_at: DateTime<Utc>,
}

impl From<PgEntityEdgeRow> for KnowledgeBundleEntityEdgeRow {
    fn from(row: PgEntityEdgeRow) -> Self {
        Self {
            bundle_id: row.bundle_id,
            entity_id: row.entity_id,
            rank: row.rank,
            score: row.score,
            inclusion_reason: row.inclusion_reason,
            created_at: row.created_at,
        }
    }
}

#[derive(Debug, Clone, FromRow)]
struct PgRelationEdgeRow {
    bundle_id: Uuid,
    relation_id: Uuid,
    rank: i32,
    score: f64,
    inclusion_reason: Option<String>,
    created_at: DateTime<Utc>,
}

impl From<PgRelationEdgeRow> for KnowledgeBundleRelationEdgeRow {
    fn from(row: PgRelationEdgeRow) -> Self {
        Self {
            bundle_id: row.bundle_id,
            relation_id: row.relation_id,
            rank: row.rank,
            score: row.score,
            inclusion_reason: row.inclusion_reason,
            created_at: row.created_at,
        }
    }
}

#[derive(Debug, Clone, FromRow)]
struct PgEvidenceEdgeRow {
    bundle_id: Uuid,
    evidence_id: Uuid,
    rank: i32,
    score: f64,
    inclusion_reason: Option<String>,
    created_at: DateTime<Utc>,
}

impl From<PgEvidenceEdgeRow> for KnowledgeBundleEvidenceEdgeRow {
    fn from(row: PgEvidenceEdgeRow) -> Self {
        Self {
            bundle_id: row.bundle_id,
            evidence_id: row.evidence_id,
            rank: row.rank,
            score: row.score,
            inclusion_reason: row.inclusion_reason,
            created_at: row.created_at,
        }
    }
}

#[derive(Debug, Clone, FromRow)]
struct PgReferenceSetRow {
    bundle_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    query_execution_id: Option<Uuid>,
    bundle_state: String,
    bundle_strategy: String,
    requested_mode: String,
    resolved_mode: String,
    selected_fact_ids: Vec<Uuid>,
    verification_state: String,
    verification_warnings: serde_json::Value,
    freshness_snapshot: serde_json::Value,
    candidate_summary: serde_json::Value,
    assembly_diagnostics: serde_json::Value,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    chunk_references: serde_json::Value,
    entity_references: serde_json::Value,
    relation_references: serde_json::Value,
    evidence_references: serde_json::Value,
}

impl TryFrom<PgReferenceSetRow> for KnowledgeContextBundleReferenceSetRow {
    type Error = anyhow::Error;

    fn try_from(row: PgReferenceSetRow) -> Result<Self, Self::Error> {
        let bundle = KnowledgeContextBundleRow::from(PgBundleRow {
            bundle_id: row.bundle_id,
            workspace_id: row.workspace_id,
            library_id: row.library_id,
            query_execution_id: row.query_execution_id,
            bundle_state: row.bundle_state,
            bundle_strategy: row.bundle_strategy,
            requested_mode: row.requested_mode,
            resolved_mode: row.resolved_mode,
            selected_fact_ids: row.selected_fact_ids,
            verification_state: row.verification_state,
            verification_warnings: row.verification_warnings,
            freshness_snapshot: row.freshness_snapshot,
            candidate_summary: row.candidate_summary,
            assembly_diagnostics: row.assembly_diagnostics,
            created_at: row.created_at,
            updated_at: row.updated_at,
        });
        Ok(Self {
            bundle,
            chunk_references: serde_json::from_value(row.chunk_references)
                .context("failed to decode bundle chunk references")?,
            entity_references: serde_json::from_value(row.entity_references)
                .context("failed to decode bundle entity references")?,
            relation_references: serde_json::from_value(row.relation_references)
                .context("failed to decode bundle relation references")?,
            evidence_references: serde_json::from_value(row.evidence_references)
                .context("failed to decode bundle evidence references")?,
        })
    }
}

fn map_rows<T, U>(rows: Vec<T>) -> Vec<U>
where
    U: From<T>,
{
    rows.into_iter().map(U::from).collect()
}

async fn delete_edges_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    table: &'static str,
    bundle_id: Uuid,
) -> anyhow::Result<u64> {
    let result =
        sqlx::query(sqlx::AssertSqlSafe(format!("delete from {table} where bundle_id = $1")))
            .bind(bundle_id)
            .execute(&mut **tx)
            .await?;
    Ok(result.rows_affected())
}

#[async_trait]
impl ContextStore for PgContextStore {
    async fn upsert_bundle(
        &self,
        row: &KnowledgeContextBundleRow,
    ) -> anyhow::Result<KnowledgeContextBundleRow> {
        let row = sqlx::query_as::<_, PgBundleRow>(
            "insert into knowledge_context_bundle (
                bundle_id, workspace_id, library_id, query_execution_id, bundle_state,
                bundle_strategy, requested_mode, resolved_mode, selected_fact_ids,
                verification_state, verification_warnings, freshness_snapshot,
                candidate_summary, assembly_diagnostics, created_at, updated_at
             ) values (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16
             )
             on conflict (bundle_id) do update
             set workspace_id = excluded.workspace_id,
                 library_id = excluded.library_id,
                 query_execution_id = excluded.query_execution_id,
                 bundle_state = excluded.bundle_state,
                 bundle_strategy = excluded.bundle_strategy,
                 requested_mode = excluded.requested_mode,
                 resolved_mode = excluded.resolved_mode,
                 selected_fact_ids = excluded.selected_fact_ids,
                 verification_state = excluded.verification_state,
                 verification_warnings = excluded.verification_warnings,
                 freshness_snapshot = excluded.freshness_snapshot,
                 candidate_summary = excluded.candidate_summary,
                 assembly_diagnostics = excluded.assembly_diagnostics,
                 updated_at = excluded.updated_at
             returning bundle_id, workspace_id, library_id, query_execution_id,
                bundle_state, bundle_strategy, requested_mode, resolved_mode, selected_fact_ids,
                verification_state, verification_warnings, freshness_snapshot, candidate_summary,
                assembly_diagnostics, created_at, updated_at",
        )
        .bind(row.bundle_id)
        .bind(row.workspace_id)
        .bind(row.library_id)
        .bind(row.query_execution_id)
        .bind(&row.bundle_state)
        .bind(&row.bundle_strategy)
        .bind(&row.requested_mode)
        .bind(&row.resolved_mode)
        .bind(&row.selected_fact_ids)
        .bind(&row.verification_state)
        .bind(&row.verification_warnings)
        .bind(&row.freshness_snapshot)
        .bind(&row.candidate_summary)
        .bind(&row.assembly_diagnostics)
        .bind(row.created_at)
        .bind(row.updated_at)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.into())
    }

    async fn get_bundle(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeContextBundleRow>> {
        let row = sqlx::query_as::<_, PgBundleRow>(
            "select bundle_id, workspace_id, library_id, query_execution_id,
                bundle_state, bundle_strategy, requested_mode, resolved_mode, selected_fact_ids,
                verification_state, verification_warnings, freshness_snapshot, candidate_summary,
                assembly_diagnostics, created_at, updated_at
             from knowledge_context_bundle
             where bundle_id = $1
             limit 1",
        )
        .bind(bundle_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    async fn get_bundle_by_query_execution(
        &self,
        query_execution_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeContextBundleRow>> {
        let row = sqlx::query_as::<_, PgBundleRow>(
            "select b.bundle_id, b.workspace_id, b.library_id, b.query_execution_id,
                b.bundle_state, b.bundle_strategy, b.requested_mode, b.resolved_mode,
                b.selected_fact_ids, b.verification_state, b.verification_warnings,
                b.freshness_snapshot, b.candidate_summary, b.assembly_diagnostics,
                b.created_at, b.updated_at
             from query_execution execution
             join knowledge_context_bundle b
               on b.bundle_id = execution.context_bundle_id
              and b.query_execution_id = execution.id
              and b.workspace_id = execution.workspace_id
              and b.library_id = execution.library_id
             where execution.id = $1",
        )
        .bind(query_execution_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    async fn list_bundles_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeContextBundleRow>> {
        let rows = sqlx::query_as::<_, PgBundleRow>(
            "select bundle_id, workspace_id, library_id, query_execution_id,
                bundle_state, bundle_strategy, requested_mode, resolved_mode, selected_fact_ids,
                verification_state, verification_warnings, freshness_snapshot, candidate_summary,
                assembly_diagnostics, created_at, updated_at
             from knowledge_context_bundle
             where library_id = $1
             order by updated_at desc, bundle_id desc",
        )
        .bind(library_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(map_rows(rows))
    }

    async fn update_bundle_state(
        &self,
        bundle_id: Uuid,
        bundle_state: &str,
        selected_fact_ids: &[Uuid],
        verification_state: &str,
        verification_warnings: serde_json::Value,
        freshness_snapshot: serde_json::Value,
        candidate_summary: serde_json::Value,
        assembly_diagnostics: serde_json::Value,
    ) -> anyhow::Result<Option<KnowledgeContextBundleRow>> {
        let row = sqlx::query_as::<_, PgBundleRow>(
            "update knowledge_context_bundle
             set bundle_state = $2,
                 selected_fact_ids = $3,
                 verification_state = $4,
                 verification_warnings = $5,
                 freshness_snapshot = $6,
                 candidate_summary = $7,
                 assembly_diagnostics = $8,
                 updated_at = now()
             where bundle_id = $1
             returning bundle_id, workspace_id, library_id, query_execution_id,
                bundle_state, bundle_strategy, requested_mode, resolved_mode, selected_fact_ids,
                verification_state, verification_warnings, freshness_snapshot, candidate_summary,
                assembly_diagnostics, created_at, updated_at",
        )
        .bind(bundle_id)
        .bind(bundle_state)
        .bind(selected_fact_ids)
        .bind(verification_state)
        .bind(&verification_warnings)
        .bind(&freshness_snapshot)
        .bind(&candidate_summary)
        .bind(&assembly_diagnostics)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    async fn upsert_trace(
        &self,
        row: &KnowledgeRetrievalTraceRow,
    ) -> anyhow::Result<KnowledgeRetrievalTraceRow> {
        let row = sqlx::query_as::<_, PgTraceRow>(
            "insert into knowledge_retrieval_trace (
                trace_id, workspace_id, library_id, query_execution_id, bundle_id, trace_state,
                retrieval_strategy, candidate_counts, dropped_reasons, timing_breakdown,
                diagnostics_json, created_at, updated_at
             ) values (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13
             )
             on conflict (trace_id) do update
             set workspace_id = excluded.workspace_id,
                 library_id = excluded.library_id,
                 query_execution_id = excluded.query_execution_id,
                 bundle_id = excluded.bundle_id,
                 trace_state = excluded.trace_state,
                 retrieval_strategy = excluded.retrieval_strategy,
                 candidate_counts = excluded.candidate_counts,
                 dropped_reasons = excluded.dropped_reasons,
                 timing_breakdown = excluded.timing_breakdown,
                 diagnostics_json = excluded.diagnostics_json,
                 updated_at = excluded.updated_at
             returning trace_id, workspace_id, library_id, query_execution_id, bundle_id,
                trace_state, retrieval_strategy, candidate_counts, dropped_reasons,
                timing_breakdown, diagnostics_json, created_at, updated_at",
        )
        .bind(row.trace_id)
        .bind(row.workspace_id)
        .bind(row.library_id)
        .bind(row.query_execution_id)
        .bind(row.bundle_id)
        .bind(&row.trace_state)
        .bind(&row.retrieval_strategy)
        .bind(&row.candidate_counts)
        .bind(&row.dropped_reasons)
        .bind(&row.timing_breakdown)
        .bind(&row.diagnostics_json)
        .bind(row.created_at)
        .bind(row.updated_at)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.into())
    }

    async fn get_trace(
        &self,
        trace_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeRetrievalTraceRow>> {
        let row = sqlx::query_as::<_, PgTraceRow>(
            "select trace_id, workspace_id, library_id, query_execution_id, bundle_id,
                trace_state, retrieval_strategy, candidate_counts, dropped_reasons,
                timing_breakdown, diagnostics_json, created_at, updated_at
             from knowledge_retrieval_trace
             where trace_id = $1
             limit 1",
        )
        .bind(trace_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    async fn list_traces_by_bundle(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRetrievalTraceRow>> {
        let rows = sqlx::query_as::<_, PgTraceRow>(
            "select trace_id, workspace_id, library_id, query_execution_id, bundle_id,
                trace_state, retrieval_strategy, candidate_counts, dropped_reasons,
                timing_breakdown, diagnostics_json, created_at, updated_at
             from knowledge_retrieval_trace
             where bundle_id = $1
             order by created_at desc, trace_id desc",
        )
        .bind(bundle_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(map_rows(rows))
    }

    async fn list_traces_by_query_execution(
        &self,
        query_execution_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRetrievalTraceRow>> {
        let rows = sqlx::query_as::<_, PgTraceRow>(
            "select trace_id, workspace_id, library_id, query_execution_id, bundle_id,
                trace_state, retrieval_strategy, candidate_counts, dropped_reasons,
                timing_breakdown, diagnostics_json, created_at, updated_at
             from knowledge_retrieval_trace
             where query_execution_id = $1
             order by created_at desc, trace_id desc",
        )
        .bind(query_execution_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(map_rows(rows))
    }

    async fn update_trace_state(
        &self,
        trace_id: Uuid,
        trace_state: &str,
        diagnostics_json: serde_json::Value,
    ) -> anyhow::Result<Option<KnowledgeRetrievalTraceRow>> {
        let row = sqlx::query_as::<_, PgTraceRow>(
            "update knowledge_retrieval_trace
             set trace_state = $2,
                 diagnostics_json = $3,
                 updated_at = now()
             where trace_id = $1
             returning trace_id, workspace_id, library_id, query_execution_id, bundle_id,
                trace_state, retrieval_strategy, candidate_counts, dropped_reasons,
                timing_breakdown, diagnostics_json, created_at, updated_at",
        )
        .bind(trace_id)
        .bind(trace_state)
        .bind(&diagnostics_json)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    async fn replace_bundle_chunk_edges(
        &self,
        bundle_id: Uuid,
        library_id: Uuid,
        edges: &[KnowledgeBundleChunkEdgeRow],
    ) -> anyhow::Result<Vec<KnowledgeBundleChunkEdgeRow>> {
        let mut tx = self.pool.begin().await?;
        delete_edges_in_tx(&mut tx, "knowledge_bundle_chunk", bundle_id).await?;
        if edges.is_empty() {
            tx.commit().await?;
            return Ok(Vec::new());
        }

        let chunk_ids: Vec<Uuid> = edges.iter().map(|edge| edge.chunk_id).collect();
        let ranks: Vec<i32> = edges.iter().map(|edge| edge.rank).collect();
        let scores: Vec<f64> = edges.iter().map(|edge| edge.score).collect();
        let reasons: Vec<Option<&str>> =
            edges.iter().map(|edge| edge.inclusion_reason.as_deref()).collect();
        let created_ats: Vec<DateTime<Utc>> = edges.iter().map(|edge| edge.created_at).collect();
        let rows = sqlx::query_as::<_, PgChunkEdgeRow>(
            "with input as (
                select t.chunk_id, t.rank, t.score, t.inclusion_reason, t.created_at, t.ord
                from unnest(
                    $3::uuid[], $4::int[], $5::double precision[], $6::text[], $7::timestamptz[]
                ) with ordinality as t(chunk_id, rank, score, inclusion_reason, created_at, ord)
             ),
             dedup as (
                select distinct on (chunk_id)
                    chunk_id, rank, score, inclusion_reason, created_at, ord
                from input
                order by chunk_id, ord desc
             ),
             upserted as (
                insert into knowledge_bundle_chunk (
                    bundle_id, chunk_id, library_id, rank, score, inclusion_reason, created_at
                )
                select $1::uuid, d.chunk_id, $2::uuid, d.rank, d.score, d.inclusion_reason, d.created_at
                from dedup d
                join knowledge_chunk target
                  on target.chunk_id = d.chunk_id
                 and target.library_id = $2
                 and target.raptor_level is null
                on conflict (bundle_id, chunk_id) do update
                set library_id = excluded.library_id,
                    rank = excluded.rank,
                    score = excluded.score,
                    inclusion_reason = excluded.inclusion_reason,
                    created_at = excluded.created_at
                returning bundle_id, chunk_id, rank, score, inclusion_reason, created_at
             )
             select u.bundle_id, u.chunk_id, u.rank, u.score, u.inclusion_reason, u.created_at
             from upserted u
             join dedup d on d.chunk_id = u.chunk_id
             order by d.ord",
        )
        .bind(bundle_id)
        .bind(library_id)
        .bind(&chunk_ids)
        .bind(&ranks)
        .bind(&scores)
        .bind(&reasons)
        .bind(&created_ats)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(map_rows(rows))
    }

    async fn replace_bundle_entity_edges(
        &self,
        bundle_id: Uuid,
        library_id: Uuid,
        edges: &[KnowledgeBundleEntityEdgeRow],
    ) -> anyhow::Result<Vec<KnowledgeBundleEntityEdgeRow>> {
        let mut tx = self.pool.begin().await?;
        delete_edges_in_tx(&mut tx, "knowledge_bundle_entity", bundle_id).await?;
        if edges.is_empty() {
            tx.commit().await?;
            return Ok(Vec::new());
        }

        let entity_ids: Vec<Uuid> = edges.iter().map(|edge| edge.entity_id).collect();
        let ranks: Vec<i32> = edges.iter().map(|edge| edge.rank).collect();
        let scores: Vec<f64> = edges.iter().map(|edge| edge.score).collect();
        let reasons: Vec<Option<&str>> =
            edges.iter().map(|edge| edge.inclusion_reason.as_deref()).collect();
        let created_ats: Vec<DateTime<Utc>> = edges.iter().map(|edge| edge.created_at).collect();
        let rows = sqlx::query_as::<_, PgEntityEdgeRow>(
            "with input as (
                select t.entity_id, t.rank, t.score, t.inclusion_reason, t.created_at, t.ord
                from unnest(
                    $3::uuid[], $4::int[], $5::double precision[], $6::text[], $7::timestamptz[]
                ) with ordinality as t(entity_id, rank, score, inclusion_reason, created_at, ord)
             ),
             dedup as (
                select distinct on (entity_id)
                    entity_id, rank, score, inclusion_reason, created_at, ord
                from input
                order by entity_id, ord desc
             ),
             upserted as (
                insert into knowledge_bundle_entity (
                    bundle_id, entity_id, library_id, rank, score, inclusion_reason, created_at
                )
                select $1::uuid, d.entity_id, $2::uuid, d.rank, d.score, d.inclusion_reason, d.created_at
                from dedup d
                join runtime_graph_node target
                  on target.id = d.entity_id
                 and target.library_id = $2
                on conflict (bundle_id, entity_id) do update
                set library_id = excluded.library_id,
                    rank = excluded.rank,
                    score = excluded.score,
                    inclusion_reason = excluded.inclusion_reason,
                    created_at = excluded.created_at
                returning bundle_id, entity_id, rank, score, inclusion_reason, created_at
             )
             select u.bundle_id, u.entity_id, u.rank, u.score, u.inclusion_reason, u.created_at
             from upserted u
             join dedup d on d.entity_id = u.entity_id
             order by d.ord",
        )
        .bind(bundle_id)
        .bind(library_id)
        .bind(&entity_ids)
        .bind(&ranks)
        .bind(&scores)
        .bind(&reasons)
        .bind(&created_ats)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(map_rows(rows))
    }

    async fn replace_bundle_relation_edges(
        &self,
        bundle_id: Uuid,
        library_id: Uuid,
        edges: &[KnowledgeBundleRelationEdgeRow],
    ) -> anyhow::Result<Vec<KnowledgeBundleRelationEdgeRow>> {
        let mut tx = self.pool.begin().await?;
        delete_edges_in_tx(&mut tx, "knowledge_bundle_relation", bundle_id).await?;
        if edges.is_empty() {
            tx.commit().await?;
            return Ok(Vec::new());
        }

        let relation_ids: Vec<Uuid> = edges.iter().map(|edge| edge.relation_id).collect();
        let ranks: Vec<i32> = edges.iter().map(|edge| edge.rank).collect();
        let scores: Vec<f64> = edges.iter().map(|edge| edge.score).collect();
        let reasons: Vec<Option<&str>> =
            edges.iter().map(|edge| edge.inclusion_reason.as_deref()).collect();
        let created_ats: Vec<DateTime<Utc>> = edges.iter().map(|edge| edge.created_at).collect();
        let rows = sqlx::query_as::<_, PgRelationEdgeRow>(
            "with input as (
                select t.relation_id, t.rank, t.score, t.inclusion_reason, t.created_at, t.ord
                from unnest(
                    $3::uuid[], $4::int[], $5::double precision[], $6::text[], $7::timestamptz[]
                ) with ordinality as t(relation_id, rank, score, inclusion_reason, created_at, ord)
             ),
             dedup as (
                select distinct on (relation_id)
                    relation_id, rank, score, inclusion_reason, created_at, ord
                from input
                order by relation_id, ord desc
             ),
             upserted as (
                insert into knowledge_bundle_relation (
                    bundle_id, relation_id, library_id, rank, score, inclusion_reason, created_at
                )
                select $1::uuid, d.relation_id, $2::uuid, d.rank, d.score, d.inclusion_reason, d.created_at
                from dedup d
                join runtime_graph_edge target
                  on target.id = d.relation_id
                 and target.library_id = $2
                on conflict (bundle_id, relation_id) do update
                set library_id = excluded.library_id,
                    rank = excluded.rank,
                    score = excluded.score,
                    inclusion_reason = excluded.inclusion_reason,
                    created_at = excluded.created_at
                returning bundle_id, relation_id, rank, score, inclusion_reason, created_at
             )
             select u.bundle_id, u.relation_id, u.rank, u.score, u.inclusion_reason, u.created_at
             from upserted u
             join dedup d on d.relation_id = u.relation_id
             order by d.ord",
        )
        .bind(bundle_id)
        .bind(library_id)
        .bind(&relation_ids)
        .bind(&ranks)
        .bind(&scores)
        .bind(&reasons)
        .bind(&created_ats)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(map_rows(rows))
    }

    async fn replace_bundle_evidence_edges(
        &self,
        bundle_id: Uuid,
        library_id: Uuid,
        edges: &[KnowledgeBundleEvidenceEdgeRow],
    ) -> anyhow::Result<Vec<KnowledgeBundleEvidenceEdgeRow>> {
        let mut tx = self.pool.begin().await?;
        delete_edges_in_tx(&mut tx, "knowledge_bundle_evidence", bundle_id).await?;
        if edges.is_empty() {
            tx.commit().await?;
            return Ok(Vec::new());
        }

        let evidence_ids: Vec<Uuid> = edges.iter().map(|edge| edge.evidence_id).collect();
        let ranks: Vec<i32> = edges.iter().map(|edge| edge.rank).collect();
        let scores: Vec<f64> = edges.iter().map(|edge| edge.score).collect();
        let reasons: Vec<Option<&str>> =
            edges.iter().map(|edge| edge.inclusion_reason.as_deref()).collect();
        let created_ats: Vec<DateTime<Utc>> = edges.iter().map(|edge| edge.created_at).collect();
        let rows = sqlx::query_as::<_, PgEvidenceEdgeRow>(
            "with input as (
                select t.evidence_id, t.rank, t.score, t.inclusion_reason, t.created_at, t.ord
                from unnest(
                    $3::uuid[], $4::int[], $5::double precision[], $6::text[], $7::timestamptz[]
                ) with ordinality as t(evidence_id, rank, score, inclusion_reason, created_at, ord)
             ),
             dedup as (
                select distinct on (evidence_id)
                    evidence_id, rank, score, inclusion_reason, created_at, ord
                from input
                order by evidence_id, ord desc
             ),
             upserted as (
                insert into knowledge_bundle_evidence (
                    bundle_id, evidence_id, library_id, rank, score, inclusion_reason, created_at
                )
                select $1::uuid, d.evidence_id, $2::uuid, d.rank, d.score, d.inclusion_reason, d.created_at
                from dedup d
                join runtime_graph_evidence target
                  on target.id = d.evidence_id
                 and target.library_id = $2
                on conflict (bundle_id, evidence_id) do update
                set library_id = excluded.library_id,
                    rank = excluded.rank,
                    score = excluded.score,
                    inclusion_reason = excluded.inclusion_reason,
                    created_at = excluded.created_at
                returning bundle_id, evidence_id, rank, score, inclusion_reason, created_at
             )
             select u.bundle_id, u.evidence_id, u.rank, u.score, u.inclusion_reason, u.created_at
             from upserted u
             join dedup d on d.evidence_id = u.evidence_id
             order by d.ord",
        )
        .bind(bundle_id)
        .bind(library_id)
        .bind(&evidence_ids)
        .bind(&ranks)
        .bind(&scores)
        .bind(&reasons)
        .bind(&created_ats)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(map_rows(rows))
    }

    async fn list_bundle_chunk_edges(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeBundleChunkEdgeRow>> {
        let rows = sqlx::query_as::<_, PgChunkEdgeRow>(
            "select bundle_id, chunk_id, rank, score, inclusion_reason, created_at
             from knowledge_bundle_chunk
             where bundle_id = $1
             order by rank asc, created_at asc, chunk_id asc",
        )
        .bind(bundle_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(map_rows(rows))
    }

    async fn list_bundle_entity_edges(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeBundleEntityEdgeRow>> {
        let rows = sqlx::query_as::<_, PgEntityEdgeRow>(
            "select bundle_id, entity_id, rank, score, inclusion_reason, created_at
             from knowledge_bundle_entity
             where bundle_id = $1
             order by rank asc, created_at asc, entity_id asc",
        )
        .bind(bundle_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(map_rows(rows))
    }

    async fn list_bundle_relation_edges(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeBundleRelationEdgeRow>> {
        let rows = sqlx::query_as::<_, PgRelationEdgeRow>(
            "select bundle_id, relation_id, rank, score, inclusion_reason, created_at
             from knowledge_bundle_relation
             where bundle_id = $1
             order by rank asc, created_at asc, relation_id asc",
        )
        .bind(bundle_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(map_rows(rows))
    }

    async fn list_bundle_evidence_edges(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeBundleEvidenceEdgeRow>> {
        let rows = sqlx::query_as::<_, PgEvidenceEdgeRow>(
            "select bundle_id, evidence_id, rank, score, inclusion_reason, created_at
             from knowledge_bundle_evidence
             where bundle_id = $1
             order by rank asc, created_at asc, evidence_id asc",
        )
        .bind(bundle_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(map_rows(rows))
    }

    async fn get_bundle_reference_set(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeContextBundleReferenceSetRow>> {
        let row = sqlx::query_as::<_, PgReferenceSetRow>(
            "select
                b.bundle_id, b.workspace_id, b.library_id, b.query_execution_id,
                b.bundle_state, b.bundle_strategy, b.requested_mode, b.resolved_mode,
                b.selected_fact_ids, b.verification_state, b.verification_warnings,
                b.freshness_snapshot, b.candidate_summary, b.assembly_diagnostics,
                b.created_at, b.updated_at,
                coalesce(chunk_refs.items, '[]'::jsonb) as chunk_references,
                coalesce(entity_refs.items, '[]'::jsonb) as entity_references,
                coalesce(relation_refs.items, '[]'::jsonb) as relation_references,
                coalesce(evidence_refs.items, '[]'::jsonb) as evidence_references
             from knowledge_context_bundle b
             left join lateral (
                select jsonb_agg(
                    jsonb_build_object(
                        'bundle_id', edge.bundle_id,
                        'chunk_id', edge.chunk_id,
                        'rank', edge.rank,
                        'score', edge.score,
                        'inclusion_reason', edge.inclusion_reason,
                        'created_at', edge.created_at
                    )
                    order by edge.rank asc, edge.score desc, edge.chunk_id asc
                ) as items
                from knowledge_bundle_chunk edge
                where edge.bundle_id = b.bundle_id
             ) chunk_refs on true
             left join lateral (
                select jsonb_agg(
                    jsonb_build_object(
                        'bundle_id', edge.bundle_id,
                        'entity_id', edge.entity_id,
                        'rank', edge.rank,
                        'score', edge.score,
                        'inclusion_reason', edge.inclusion_reason,
                        'created_at', edge.created_at
                    )
                    order by edge.rank asc, edge.score desc, edge.entity_id asc
                ) as items
                from knowledge_bundle_entity edge
                where edge.bundle_id = b.bundle_id
             ) entity_refs on true
             left join lateral (
                select jsonb_agg(
                    jsonb_build_object(
                        'bundle_id', edge.bundle_id,
                        'relation_id', edge.relation_id,
                        'rank', edge.rank,
                        'score', edge.score,
                        'inclusion_reason', edge.inclusion_reason,
                        'created_at', edge.created_at
                    )
                    order by edge.rank asc, edge.score desc, edge.relation_id asc
                ) as items
                from knowledge_bundle_relation edge
                where edge.bundle_id = b.bundle_id
             ) relation_refs on true
             left join lateral (
                select jsonb_agg(
                    jsonb_build_object(
                        'bundle_id', edge.bundle_id,
                        'evidence_id', edge.evidence_id,
                        'rank', edge.rank,
                        'score', edge.score,
                        'inclusion_reason', edge.inclusion_reason,
                        'created_at', edge.created_at
                    )
                    order by edge.rank asc, edge.score desc, edge.evidence_id asc
                ) as items
                from knowledge_bundle_evidence edge
                where edge.bundle_id = b.bundle_id
             ) evidence_refs on true
             where b.bundle_id = $1
             limit 1",
        )
        .bind(bundle_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(TryInto::try_into).transpose()
    }

    async fn get_bundle_reference_set_by_query_execution(
        &self,
        query_execution_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeContextBundleReferenceSetRow>> {
        let row = sqlx::query_as::<_, PgReferenceSetRow>(
            "select
                b.bundle_id, b.workspace_id, b.library_id, b.query_execution_id,
                b.bundle_state, b.bundle_strategy, b.requested_mode, b.resolved_mode,
                b.selected_fact_ids, b.verification_state, b.verification_warnings,
                b.freshness_snapshot, b.candidate_summary, b.assembly_diagnostics,
                b.created_at, b.updated_at,
                coalesce(chunk_refs.items, '[]'::jsonb) as chunk_references,
                coalesce(entity_refs.items, '[]'::jsonb) as entity_references,
                coalesce(relation_refs.items, '[]'::jsonb) as relation_references,
                coalesce(evidence_refs.items, '[]'::jsonb) as evidence_references
             from query_execution execution
             join knowledge_context_bundle b
               on b.bundle_id = execution.context_bundle_id
              and b.query_execution_id = execution.id
              and b.workspace_id = execution.workspace_id
              and b.library_id = execution.library_id
             left join lateral (
                select jsonb_agg(
                    jsonb_build_object(
                        'bundle_id', edge.bundle_id,
                        'chunk_id', edge.chunk_id,
                        'rank', edge.rank,
                        'score', edge.score,
                        'inclusion_reason', edge.inclusion_reason,
                        'created_at', edge.created_at
                    )
                    order by edge.rank asc, edge.score desc, edge.chunk_id asc
                ) as items
                from knowledge_bundle_chunk edge
                where edge.bundle_id = b.bundle_id
             ) chunk_refs on true
             left join lateral (
                select jsonb_agg(
                    jsonb_build_object(
                        'bundle_id', edge.bundle_id,
                        'entity_id', edge.entity_id,
                        'rank', edge.rank,
                        'score', edge.score,
                        'inclusion_reason', edge.inclusion_reason,
                        'created_at', edge.created_at
                    )
                    order by edge.rank asc, edge.score desc, edge.entity_id asc
                ) as items
                from knowledge_bundle_entity edge
                where edge.bundle_id = b.bundle_id
             ) entity_refs on true
             left join lateral (
                select jsonb_agg(
                    jsonb_build_object(
                        'bundle_id', edge.bundle_id,
                        'relation_id', edge.relation_id,
                        'rank', edge.rank,
                        'score', edge.score,
                        'inclusion_reason', edge.inclusion_reason,
                        'created_at', edge.created_at
                    )
                    order by edge.rank asc, edge.score desc, edge.relation_id asc
                ) as items
                from knowledge_bundle_relation edge
                where edge.bundle_id = b.bundle_id
             ) relation_refs on true
             left join lateral (
                select jsonb_agg(
                    jsonb_build_object(
                        'bundle_id', edge.bundle_id,
                        'evidence_id', edge.evidence_id,
                        'rank', edge.rank,
                        'score', edge.score,
                        'inclusion_reason', edge.inclusion_reason,
                        'created_at', edge.created_at
                    )
                    order by edge.rank asc, edge.score desc, edge.evidence_id asc
                ) as items
                from knowledge_bundle_evidence edge
                where edge.bundle_id = b.bundle_id
             ) evidence_refs on true
             where execution.id = $1",
        )
        .bind(query_execution_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(TryInto::try_into).transpose()
    }

    async fn list_bundle_reference_sets_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeContextBundleReferenceSetRow>> {
        let rows = sqlx::query_as::<_, PgReferenceSetRow>(
            "select
                b.bundle_id, b.workspace_id, b.library_id, b.query_execution_id,
                b.bundle_state, b.bundle_strategy, b.requested_mode, b.resolved_mode,
                b.selected_fact_ids, b.verification_state, b.verification_warnings,
                b.freshness_snapshot, b.candidate_summary, b.assembly_diagnostics,
                b.created_at, b.updated_at,
                coalesce(chunk_refs.items, '[]'::jsonb) as chunk_references,
                coalesce(entity_refs.items, '[]'::jsonb) as entity_references,
                coalesce(relation_refs.items, '[]'::jsonb) as relation_references,
                coalesce(evidence_refs.items, '[]'::jsonb) as evidence_references
             from knowledge_context_bundle b
             left join lateral (
                select jsonb_agg(
                    jsonb_build_object(
                        'bundle_id', edge.bundle_id,
                        'chunk_id', edge.chunk_id,
                        'rank', edge.rank,
                        'score', edge.score,
                        'inclusion_reason', edge.inclusion_reason,
                        'created_at', edge.created_at
                    )
                    order by edge.rank asc, edge.score desc, edge.chunk_id asc
                ) as items
                from knowledge_bundle_chunk edge
                where edge.bundle_id = b.bundle_id
             ) chunk_refs on true
             left join lateral (
                select jsonb_agg(
                    jsonb_build_object(
                        'bundle_id', edge.bundle_id,
                        'entity_id', edge.entity_id,
                        'rank', edge.rank,
                        'score', edge.score,
                        'inclusion_reason', edge.inclusion_reason,
                        'created_at', edge.created_at
                    )
                    order by edge.rank asc, edge.score desc, edge.entity_id asc
                ) as items
                from knowledge_bundle_entity edge
                where edge.bundle_id = b.bundle_id
             ) entity_refs on true
             left join lateral (
                select jsonb_agg(
                    jsonb_build_object(
                        'bundle_id', edge.bundle_id,
                        'relation_id', edge.relation_id,
                        'rank', edge.rank,
                        'score', edge.score,
                        'inclusion_reason', edge.inclusion_reason,
                        'created_at', edge.created_at
                    )
                    order by edge.rank asc, edge.score desc, edge.relation_id asc
                ) as items
                from knowledge_bundle_relation edge
                where edge.bundle_id = b.bundle_id
             ) relation_refs on true
             left join lateral (
                select jsonb_agg(
                    jsonb_build_object(
                        'bundle_id', edge.bundle_id,
                        'evidence_id', edge.evidence_id,
                        'rank', edge.rank,
                        'score', edge.score,
                        'inclusion_reason', edge.inclusion_reason,
                        'created_at', edge.created_at
                    )
                    order by edge.rank asc, edge.score desc, edge.evidence_id asc
                ) as items
                from knowledge_bundle_evidence edge
                where edge.bundle_id = b.bundle_id
             ) evidence_refs on true
             where b.library_id = $1
             order by b.updated_at desc, b.bundle_id desc",
        )
        .bind(library_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    async fn delete_bundle_chunk_edges(&self, bundle_id: Uuid) -> anyhow::Result<u64> {
        let result = sqlx::query("delete from knowledge_bundle_chunk where bundle_id = $1")
            .bind(bundle_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    async fn delete_bundle_entity_edges(&self, bundle_id: Uuid) -> anyhow::Result<u64> {
        let result = sqlx::query("delete from knowledge_bundle_entity where bundle_id = $1")
            .bind(bundle_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    async fn delete_bundle_relation_edges(&self, bundle_id: Uuid) -> anyhow::Result<u64> {
        let result = sqlx::query("delete from knowledge_bundle_relation where bundle_id = $1")
            .bind(bundle_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    async fn delete_bundle_evidence_edges(&self, bundle_id: Uuid) -> anyhow::Result<u64> {
        let result = sqlx::query("delete from knowledge_bundle_evidence where bundle_id = $1")
            .bind(bundle_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}
