use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use crate::infra::knowledge_rows::{
    KnowledgeBundleChunkEdgeRow, KnowledgeBundleEntityEdgeRow, KnowledgeBundleEvidenceEdgeRow,
    KnowledgeBundleRelationEdgeRow, KnowledgeContextBundleReferenceSetRow,
    KnowledgeContextBundleRow, KnowledgeRetrievalTraceRow,
};

/// Shared handle for a [`ContextStore`] adapter.
pub type ContextStoreRef = Arc<dyn ContextStore>;

/// Mutable bundle-state payload applied by
/// [`ContextStore::update_bundle_state`]. The four JSON values carry the
/// verification, freshness, candidate-summary and assembly diagnostics written
/// verbatim to the bundle row.
pub struct BundleStateUpdate<'a> {
    pub bundle_id: Uuid,
    pub bundle_state: &'a str,
    pub selected_fact_ids: &'a [Uuid],
    pub verification_state: &'a str,
    pub verification_warnings: serde_json::Value,
    pub freshness_snapshot: serde_json::Value,
    pub candidate_summary: serde_json::Value,
    pub assembly_diagnostics: serde_json::Value,
}

/// Context-bundle materialization: bundle header, four ranked child reference
/// sets, retrieval traces, and the materialized read-back join.
#[async_trait]
pub trait ContextStore: Send + Sync {
    async fn upsert_bundle(
        &self,
        row: &KnowledgeContextBundleRow,
    ) -> anyhow::Result<KnowledgeContextBundleRow>;
    async fn get_bundle(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeContextBundleRow>>;
    async fn get_bundle_by_query_execution(
        &self,
        query_execution_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeContextBundleRow>>;
    async fn list_bundles_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeContextBundleRow>>;
    async fn update_bundle_state(
        &self,
        update: BundleStateUpdate<'_>,
    ) -> anyhow::Result<Option<KnowledgeContextBundleRow>>;
    async fn upsert_trace(
        &self,
        row: &KnowledgeRetrievalTraceRow,
    ) -> anyhow::Result<KnowledgeRetrievalTraceRow>;
    async fn get_trace(&self, trace_id: Uuid)
    -> anyhow::Result<Option<KnowledgeRetrievalTraceRow>>;
    async fn list_traces_by_bundle(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRetrievalTraceRow>>;
    async fn list_traces_by_query_execution(
        &self,
        query_execution_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRetrievalTraceRow>>;
    async fn update_trace_state(
        &self,
        trace_id: Uuid,
        trace_state: &str,
        diagnostics_json: serde_json::Value,
    ) -> anyhow::Result<Option<KnowledgeRetrievalTraceRow>>;
    async fn replace_bundle_chunk_edges(
        &self,
        bundle_id: Uuid,
        library_id: Uuid,
        edges: &[KnowledgeBundleChunkEdgeRow],
    ) -> anyhow::Result<Vec<KnowledgeBundleChunkEdgeRow>>;
    async fn replace_bundle_entity_edges(
        &self,
        bundle_id: Uuid,
        library_id: Uuid,
        edges: &[KnowledgeBundleEntityEdgeRow],
    ) -> anyhow::Result<Vec<KnowledgeBundleEntityEdgeRow>>;
    async fn replace_bundle_relation_edges(
        &self,
        bundle_id: Uuid,
        library_id: Uuid,
        edges: &[KnowledgeBundleRelationEdgeRow],
    ) -> anyhow::Result<Vec<KnowledgeBundleRelationEdgeRow>>;
    async fn replace_bundle_evidence_edges(
        &self,
        bundle_id: Uuid,
        library_id: Uuid,
        edges: &[KnowledgeBundleEvidenceEdgeRow],
    ) -> anyhow::Result<Vec<KnowledgeBundleEvidenceEdgeRow>>;
    async fn list_bundle_chunk_edges(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeBundleChunkEdgeRow>>;
    async fn list_bundle_entity_edges(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeBundleEntityEdgeRow>>;
    async fn list_bundle_relation_edges(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeBundleRelationEdgeRow>>;
    async fn list_bundle_evidence_edges(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeBundleEvidenceEdgeRow>>;
    /// Materialized read-back: bundle header + four ranked child reference sets
    /// in one round-trip. Child rows are ordered by `rank ASC, score DESC`. The
    /// PG adapter MUST keep this a single round-trip to hold the ≤30 s tool-call
    /// SLO.
    async fn get_bundle_reference_set(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeContextBundleReferenceSetRow>>;
    async fn get_bundle_reference_set_by_query_execution(
        &self,
        query_execution_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeContextBundleReferenceSetRow>>;
    async fn list_bundle_reference_sets_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeContextBundleReferenceSetRow>>;
    /// Returns the number of edges removed (§14.4(b)).
    async fn delete_bundle_chunk_edges(&self, bundle_id: Uuid) -> anyhow::Result<u64>;
    /// Returns the number of edges removed (§14.4(b)).
    async fn delete_bundle_entity_edges(&self, bundle_id: Uuid) -> anyhow::Result<u64>;
    /// Returns the number of edges removed (§14.4(b)).
    async fn delete_bundle_relation_edges(&self, bundle_id: Uuid) -> anyhow::Result<u64>;
    /// Returns the number of edges removed (§14.4(b)).
    async fn delete_bundle_evidence_edges(&self, bundle_id: Uuid) -> anyhow::Result<u64>;
}
