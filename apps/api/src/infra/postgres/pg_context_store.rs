use anyhow::Context;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::infra::{
    knowledge_plane::{BundleStateUpdate, ContextStore},
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

mod bundles;
mod edges;
mod reference_sets;
mod traces;

#[async_trait]
impl ContextStore for PgContextStore {
    async fn upsert_bundle(
        &self,
        row: &KnowledgeContextBundleRow,
    ) -> anyhow::Result<KnowledgeContextBundleRow> {
        self.upsert_bundle_impl(row).await
    }

    async fn get_bundle(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeContextBundleRow>> {
        self.get_bundle_impl(bundle_id).await
    }

    async fn get_bundle_by_query_execution(
        &self,
        query_execution_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeContextBundleRow>> {
        self.get_bundle_by_query_execution_impl(query_execution_id).await
    }

    async fn list_bundles_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeContextBundleRow>> {
        self.list_bundles_by_library_impl(library_id).await
    }

    async fn update_bundle_state(
        &self,
        update: BundleStateUpdate<'_>,
    ) -> anyhow::Result<Option<KnowledgeContextBundleRow>> {
        self.update_bundle_state_impl(update).await
    }

    async fn upsert_trace(
        &self,
        row: &KnowledgeRetrievalTraceRow,
    ) -> anyhow::Result<KnowledgeRetrievalTraceRow> {
        self.upsert_trace_impl(row).await
    }

    async fn get_trace(
        &self,
        trace_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeRetrievalTraceRow>> {
        self.get_trace_impl(trace_id).await
    }

    async fn list_traces_by_bundle(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRetrievalTraceRow>> {
        self.list_traces_by_bundle_impl(bundle_id).await
    }

    async fn list_traces_by_query_execution(
        &self,
        query_execution_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRetrievalTraceRow>> {
        self.list_traces_by_query_execution_impl(query_execution_id).await
    }

    async fn update_trace_state(
        &self,
        trace_id: Uuid,
        trace_state: &str,
        diagnostics_json: serde_json::Value,
    ) -> anyhow::Result<Option<KnowledgeRetrievalTraceRow>> {
        self.update_trace_state_impl(trace_id, trace_state, diagnostics_json).await
    }

    async fn replace_bundle_chunk_edges(
        &self,
        bundle_id: Uuid,
        library_id: Uuid,
        edges: &[KnowledgeBundleChunkEdgeRow],
    ) -> anyhow::Result<Vec<KnowledgeBundleChunkEdgeRow>> {
        self.replace_bundle_chunk_edges_impl(bundle_id, library_id, edges).await
    }

    async fn replace_bundle_entity_edges(
        &self,
        bundle_id: Uuid,
        library_id: Uuid,
        edges: &[KnowledgeBundleEntityEdgeRow],
    ) -> anyhow::Result<Vec<KnowledgeBundleEntityEdgeRow>> {
        self.replace_bundle_entity_edges_impl(bundle_id, library_id, edges).await
    }

    async fn replace_bundle_relation_edges(
        &self,
        bundle_id: Uuid,
        library_id: Uuid,
        edges: &[KnowledgeBundleRelationEdgeRow],
    ) -> anyhow::Result<Vec<KnowledgeBundleRelationEdgeRow>> {
        self.replace_bundle_relation_edges_impl(bundle_id, library_id, edges).await
    }

    async fn replace_bundle_evidence_edges(
        &self,
        bundle_id: Uuid,
        library_id: Uuid,
        edges: &[KnowledgeBundleEvidenceEdgeRow],
    ) -> anyhow::Result<Vec<KnowledgeBundleEvidenceEdgeRow>> {
        self.replace_bundle_evidence_edges_impl(bundle_id, library_id, edges).await
    }

    async fn list_bundle_chunk_edges(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeBundleChunkEdgeRow>> {
        self.list_bundle_chunk_edges_impl(bundle_id).await
    }

    async fn list_bundle_entity_edges(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeBundleEntityEdgeRow>> {
        self.list_bundle_entity_edges_impl(bundle_id).await
    }

    async fn list_bundle_relation_edges(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeBundleRelationEdgeRow>> {
        self.list_bundle_relation_edges_impl(bundle_id).await
    }

    async fn list_bundle_evidence_edges(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeBundleEvidenceEdgeRow>> {
        self.list_bundle_evidence_edges_impl(bundle_id).await
    }

    async fn get_bundle_reference_set(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeContextBundleReferenceSetRow>> {
        self.get_bundle_reference_set_impl(bundle_id).await
    }

    async fn get_bundle_reference_set_by_query_execution(
        &self,
        query_execution_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeContextBundleReferenceSetRow>> {
        self.get_bundle_reference_set_by_query_execution_impl(query_execution_id).await
    }

    async fn list_bundle_reference_sets_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeContextBundleReferenceSetRow>> {
        self.list_bundle_reference_sets_by_library_impl(library_id).await
    }

    async fn delete_bundle_chunk_edges(&self, bundle_id: Uuid) -> anyhow::Result<u64> {
        self.delete_bundle_chunk_edges_impl(bundle_id).await
    }

    async fn delete_bundle_entity_edges(&self, bundle_id: Uuid) -> anyhow::Result<u64> {
        self.delete_bundle_entity_edges_impl(bundle_id).await
    }

    async fn delete_bundle_relation_edges(&self, bundle_id: Uuid) -> anyhow::Result<u64> {
        self.delete_bundle_relation_edges_impl(bundle_id).await
    }

    async fn delete_bundle_evidence_edges(&self, bundle_id: Uuid) -> anyhow::Result<u64> {
        self.delete_bundle_evidence_edges_impl(bundle_id).await
    }
}
