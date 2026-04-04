#![allow(
    clippy::future_not_send,
    clippy::missing_const_for_fn,
    clippy::missing_errors_doc,
    clippy::needless_pass_by_value,
    clippy::too_many_arguments
)]

use std::sync::Arc;

use anyhow::{Context, anyhow};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use uuid::Uuid;

use crate::infra::arangodb::{
    client::ArangoClient,
    collections::{
        KNOWLEDGE_BUNDLE_CHUNK_EDGE, KNOWLEDGE_BUNDLE_ENTITY_EDGE, KNOWLEDGE_BUNDLE_EVIDENCE_EDGE,
        KNOWLEDGE_BUNDLE_RELATION_EDGE, KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION,
        KNOWLEDGE_RETRIEVAL_TRACE_COLLECTION,
    },
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeContextBundleRow {
    #[serde(rename = "_key")]
    pub key: String,
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    pub arango_id: Option<String>,
    #[serde(rename = "_rev", default, skip_serializing_if = "Option::is_none")]
    pub arango_rev: Option<String>,
    pub bundle_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub query_execution_id: Option<Uuid>,
    pub bundle_state: String,
    pub bundle_strategy: String,
    pub requested_mode: String,
    pub resolved_mode: String,
    #[serde(default)]
    pub selected_fact_ids: Vec<Uuid>,
    pub verification_state: String,
    #[serde(default)]
    pub verification_warnings: serde_json::Value,
    pub freshness_snapshot: serde_json::Value,
    pub candidate_summary: serde_json::Value,
    pub assembly_diagnostics: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeRetrievalTraceRow {
    #[serde(rename = "_key")]
    pub key: String,
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    pub arango_id: Option<String>,
    #[serde(rename = "_rev", default, skip_serializing_if = "Option::is_none")]
    pub arango_rev: Option<String>,
    pub trace_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub query_execution_id: Option<Uuid>,
    pub bundle_id: Uuid,
    pub trace_state: String,
    pub retrieval_strategy: String,
    pub candidate_counts: serde_json::Value,
    pub dropped_reasons: serde_json::Value,
    pub timing_breakdown: serde_json::Value,
    pub diagnostics_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBundleChunkEdgeRow {
    #[serde(rename = "_key")]
    pub key: String,
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    pub arango_id: Option<String>,
    #[serde(rename = "_rev", default, skip_serializing_if = "Option::is_none")]
    pub arango_rev: Option<String>,
    #[serde(rename = "_from")]
    pub from: String,
    #[serde(rename = "_to")]
    pub to: String,
    pub bundle_id: Uuid,
    pub chunk_id: Uuid,
    pub rank: i32,
    pub score: f64,
    pub inclusion_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBundleEntityEdgeRow {
    #[serde(rename = "_key")]
    pub key: String,
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    pub arango_id: Option<String>,
    #[serde(rename = "_rev", default, skip_serializing_if = "Option::is_none")]
    pub arango_rev: Option<String>,
    #[serde(rename = "_from")]
    pub from: String,
    #[serde(rename = "_to")]
    pub to: String,
    pub bundle_id: Uuid,
    pub entity_id: Uuid,
    pub rank: i32,
    pub score: f64,
    pub inclusion_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBundleRelationEdgeRow {
    #[serde(rename = "_key")]
    pub key: String,
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    pub arango_id: Option<String>,
    #[serde(rename = "_rev", default, skip_serializing_if = "Option::is_none")]
    pub arango_rev: Option<String>,
    #[serde(rename = "_from")]
    pub from: String,
    #[serde(rename = "_to")]
    pub to: String,
    pub bundle_id: Uuid,
    pub relation_id: Uuid,
    pub rank: i32,
    pub score: f64,
    pub inclusion_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBundleEvidenceEdgeRow {
    #[serde(rename = "_key")]
    pub key: String,
    #[serde(rename = "_id", default, skip_serializing_if = "Option::is_none")]
    pub arango_id: Option<String>,
    #[serde(rename = "_rev", default, skip_serializing_if = "Option::is_none")]
    pub arango_rev: Option<String>,
    #[serde(rename = "_from")]
    pub from: String,
    #[serde(rename = "_to")]
    pub to: String,
    pub bundle_id: Uuid,
    pub evidence_id: Uuid,
    pub rank: i32,
    pub score: f64,
    pub inclusion_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBundleChunkReferenceRow {
    #[serde(rename = "_key")]
    pub key: String,
    pub bundle_id: Uuid,
    pub chunk_id: Uuid,
    pub rank: i32,
    pub score: f64,
    pub inclusion_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBundleEntityReferenceRow {
    #[serde(rename = "_key")]
    pub key: String,
    pub bundle_id: Uuid,
    pub entity_id: Uuid,
    pub rank: i32,
    pub score: f64,
    pub inclusion_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBundleRelationReferenceRow {
    #[serde(rename = "_key")]
    pub key: String,
    pub bundle_id: Uuid,
    pub relation_id: Uuid,
    pub rank: i32,
    pub score: f64,
    pub inclusion_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBundleEvidenceReferenceRow {
    #[serde(rename = "_key")]
    pub key: String,
    pub bundle_id: Uuid,
    pub evidence_id: Uuid,
    pub rank: i32,
    pub score: f64,
    pub inclusion_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeContextBundleReferenceSetRow {
    pub bundle: KnowledgeContextBundleRow,
    pub chunk_references: Vec<KnowledgeBundleChunkReferenceRow>,
    pub entity_references: Vec<KnowledgeBundleEntityReferenceRow>,
    pub relation_references: Vec<KnowledgeBundleRelationReferenceRow>,
    pub evidence_references: Vec<KnowledgeBundleEvidenceReferenceRow>,
}

#[derive(Clone)]
pub struct ArangoContextStore {
    client: Arc<ArangoClient>,
}

impl ArangoContextStore {
    #[must_use]
    pub fn new(client: Arc<ArangoClient>) -> Self {
        Self { client }
    }

    #[must_use]
    pub fn client(&self) -> &Arc<ArangoClient> {
        &self.client
    }

    pub async fn upsert_bundle(
        &self,
        row: &KnowledgeContextBundleRow,
    ) -> anyhow::Result<KnowledgeContextBundleRow> {
        let cursor = self
            .client
            .query_json(
                "UPSERT { _key: @key }
                 INSERT {
                    _key: @key,
                    bundle_id: @bundle_id,
                    workspace_id: @workspace_id,
                    library_id: @library_id,
                    query_execution_id: @query_execution_id,
                    bundle_state: @bundle_state,
                    bundle_strategy: @bundle_strategy,
                    requested_mode: @requested_mode,
                    resolved_mode: @resolved_mode,
                    selected_fact_ids: @selected_fact_ids,
                    verification_state: @verification_state,
                    verification_warnings: @verification_warnings,
                    freshness_snapshot: @freshness_snapshot,
                    candidate_summary: @candidate_summary,
                    assembly_diagnostics: @assembly_diagnostics,
                    created_at: @created_at,
                    updated_at: @updated_at
                 }
                 UPDATE {
                    workspace_id: @workspace_id,
                    library_id: @library_id,
                    query_execution_id: @query_execution_id,
                    bundle_state: @bundle_state,
                    bundle_strategy: @bundle_strategy,
                    requested_mode: @requested_mode,
                    resolved_mode: @resolved_mode,
                    selected_fact_ids: @selected_fact_ids,
                    verification_state: @verification_state,
                    verification_warnings: @verification_warnings,
                    freshness_snapshot: @freshness_snapshot,
                    candidate_summary: @candidate_summary,
                    assembly_diagnostics: @assembly_diagnostics,
                    updated_at: @updated_at
                 }
                 IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION,
                    "key": row.key,
                    "bundle_id": row.bundle_id,
                    "workspace_id": row.workspace_id,
                    "library_id": row.library_id,
                    "query_execution_id": row.query_execution_id,
                    "bundle_state": row.bundle_state,
                    "bundle_strategy": row.bundle_strategy,
                    "requested_mode": row.requested_mode,
                    "resolved_mode": row.resolved_mode,
                    "selected_fact_ids": row.selected_fact_ids,
                    "verification_state": row.verification_state,
                    "verification_warnings": row.verification_warnings,
                    "freshness_snapshot": row.freshness_snapshot,
                    "candidate_summary": row.candidate_summary,
                    "assembly_diagnostics": row.assembly_diagnostics,
                    "created_at": row.created_at,
                    "updated_at": row.updated_at,
                }),
            )
            .await
            .context("failed to upsert knowledge context bundle")?;
        decode_single_result(cursor)
    }

    pub async fn get_bundle(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeContextBundleRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR bundle IN @@collection
                 FILTER bundle.bundle_id == @bundle_id
                 LIMIT 1
                 RETURN bundle",
                serde_json::json!({
                    "@collection": KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION,
                    "bundle_id": bundle_id,
                }),
            )
            .await
            .context("failed to get knowledge context bundle")?;
        decode_optional_single_result(cursor)
    }

    pub async fn get_bundle_by_query_execution(
        &self,
        query_execution_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeContextBundleRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR bundle IN @@collection
                 FILTER bundle.query_execution_id == @query_execution_id
                 LIMIT 1
                 RETURN bundle",
                serde_json::json!({
                    "@collection": KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION,
                    "query_execution_id": query_execution_id,
                }),
            )
            .await
            .context("failed to get knowledge context bundle by execution")?;
        decode_optional_single_result(cursor)
    }

    pub async fn list_bundles_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeContextBundleRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR bundle IN @@collection
                 FILTER bundle.library_id == @library_id
                 SORT bundle.updated_at DESC, bundle.bundle_id DESC
                 RETURN bundle",
                serde_json::json!({
                    "@collection": KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION,
                    "library_id": library_id,
                }),
            )
            .await
            .context("failed to list knowledge context bundles by library")?;
        decode_many_results(cursor)
    }

    pub async fn update_bundle_state(
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
        let cursor = self
            .client
            .query_json(
                "FOR bundle IN @@collection
                 FILTER bundle.bundle_id == @bundle_id
                 LIMIT 1
                 UPDATE bundle WITH {
                    bundle_state: @bundle_state,
                    selected_fact_ids: @selected_fact_ids,
                    verification_state: @verification_state,
                    verification_warnings: @verification_warnings,
                    freshness_snapshot: @freshness_snapshot,
                    candidate_summary: @candidate_summary,
                    assembly_diagnostics: @assembly_diagnostics,
                    updated_at: @updated_at
                 } IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION,
                    "bundle_id": bundle_id,
                    "bundle_state": bundle_state,
                    "selected_fact_ids": selected_fact_ids,
                    "verification_state": verification_state,
                    "verification_warnings": verification_warnings,
                    "freshness_snapshot": freshness_snapshot,
                    "candidate_summary": candidate_summary,
                    "assembly_diagnostics": assembly_diagnostics,
                    "updated_at": Utc::now(),
                }),
            )
            .await
            .context("failed to update knowledge context bundle state")?;
        decode_optional_single_result(cursor)
    }

    pub async fn upsert_trace(
        &self,
        row: &KnowledgeRetrievalTraceRow,
    ) -> anyhow::Result<KnowledgeRetrievalTraceRow> {
        let cursor = self
            .client
            .query_json(
                "UPSERT { _key: @key }
                 INSERT {
                    _key: @key,
                    trace_id: @trace_id,
                    workspace_id: @workspace_id,
                    library_id: @library_id,
                    query_execution_id: @query_execution_id,
                    bundle_id: @bundle_id,
                    trace_state: @trace_state,
                    retrieval_strategy: @retrieval_strategy,
                    candidate_counts: @candidate_counts,
                    dropped_reasons: @dropped_reasons,
                    timing_breakdown: @timing_breakdown,
                    diagnostics_json: @diagnostics_json,
                    created_at: @created_at,
                    updated_at: @updated_at
                 }
                 UPDATE {
                    workspace_id: @workspace_id,
                    library_id: @library_id,
                    query_execution_id: @query_execution_id,
                    bundle_id: @bundle_id,
                    trace_state: @trace_state,
                    retrieval_strategy: @retrieval_strategy,
                    candidate_counts: @candidate_counts,
                    dropped_reasons: @dropped_reasons,
                    timing_breakdown: @timing_breakdown,
                    diagnostics_json: @diagnostics_json,
                    updated_at: @updated_at
                 }
                 IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_RETRIEVAL_TRACE_COLLECTION,
                    "key": row.key,
                    "trace_id": row.trace_id,
                    "workspace_id": row.workspace_id,
                    "library_id": row.library_id,
                    "query_execution_id": row.query_execution_id,
                    "bundle_id": row.bundle_id,
                    "trace_state": row.trace_state,
                    "retrieval_strategy": row.retrieval_strategy,
                    "candidate_counts": row.candidate_counts,
                    "dropped_reasons": row.dropped_reasons,
                    "timing_breakdown": row.timing_breakdown,
                    "diagnostics_json": row.diagnostics_json,
                    "created_at": row.created_at,
                    "updated_at": row.updated_at,
                }),
            )
            .await
            .context("failed to upsert knowledge retrieval trace")?;
        decode_single_result(cursor)
    }

    pub async fn get_trace(
        &self,
        trace_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeRetrievalTraceRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR trace IN @@collection
                 FILTER trace.trace_id == @trace_id
                 LIMIT 1
                 RETURN trace",
                serde_json::json!({
                    "@collection": KNOWLEDGE_RETRIEVAL_TRACE_COLLECTION,
                    "trace_id": trace_id,
                }),
            )
            .await
            .context("failed to get knowledge retrieval trace")?;
        decode_optional_single_result(cursor)
    }

    pub async fn list_traces_by_bundle(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRetrievalTraceRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR trace IN @@collection
                 FILTER trace.bundle_id == @bundle_id
                 SORT trace.created_at DESC, trace.trace_id DESC
                 RETURN trace",
                serde_json::json!({
                    "@collection": KNOWLEDGE_RETRIEVAL_TRACE_COLLECTION,
                    "bundle_id": bundle_id,
                }),
            )
            .await
            .context("failed to list knowledge retrieval traces by bundle")?;
        decode_many_results(cursor)
    }

    pub async fn list_traces_by_query_execution(
        &self,
        query_execution_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRetrievalTraceRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR trace IN @@collection
                 FILTER trace.query_execution_id == @query_execution_id
                 SORT trace.created_at DESC, trace.trace_id DESC
                 RETURN trace",
                serde_json::json!({
                    "@collection": KNOWLEDGE_RETRIEVAL_TRACE_COLLECTION,
                    "query_execution_id": query_execution_id,
                }),
            )
            .await
            .context("failed to list knowledge retrieval traces by execution")?;
        decode_many_results(cursor)
    }

    pub async fn update_trace_state(
        &self,
        trace_id: Uuid,
        trace_state: &str,
        diagnostics_json: serde_json::Value,
    ) -> anyhow::Result<Option<KnowledgeRetrievalTraceRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR trace IN @@collection
                 FILTER trace.trace_id == @trace_id
                 LIMIT 1
                 UPDATE trace WITH {
                    trace_state: @trace_state,
                    diagnostics_json: @diagnostics_json,
                    updated_at: @updated_at
                 } IN @@collection
                 RETURN NEW",
                serde_json::json!({
                    "@collection": KNOWLEDGE_RETRIEVAL_TRACE_COLLECTION,
                    "trace_id": trace_id,
                    "trace_state": trace_state,
                    "diagnostics_json": diagnostics_json,
                    "updated_at": Utc::now(),
                }),
            )
            .await
            .context("failed to update knowledge retrieval trace state")?;
        decode_optional_single_result(cursor)
    }

    pub async fn replace_bundle_chunk_edges(
        &self,
        bundle_id: Uuid,
        edges: &[KnowledgeBundleChunkEdgeRow],
    ) -> anyhow::Result<Vec<KnowledgeBundleChunkEdgeRow>> {
        self.delete_bundle_chunk_edges(bundle_id).await?;
        self.insert_bundle_edges(KNOWLEDGE_BUNDLE_CHUNK_EDGE, bundle_id, edges, |edge| {
            arango_doc_id(
                crate::infra::arangodb::collections::KNOWLEDGE_CHUNK_COLLECTION,
                edge.chunk_id,
            )
        })
        .await
    }

    pub async fn replace_bundle_entity_edges(
        &self,
        bundle_id: Uuid,
        edges: &[KnowledgeBundleEntityEdgeRow],
    ) -> anyhow::Result<Vec<KnowledgeBundleEntityEdgeRow>> {
        self.delete_bundle_entity_edges(bundle_id).await?;
        self.insert_bundle_edges(KNOWLEDGE_BUNDLE_ENTITY_EDGE, bundle_id, edges, |edge| {
            arango_doc_id(
                crate::infra::arangodb::collections::KNOWLEDGE_ENTITY_COLLECTION,
                edge.entity_id,
            )
        })
        .await
    }

    pub async fn replace_bundle_relation_edges(
        &self,
        bundle_id: Uuid,
        edges: &[KnowledgeBundleRelationEdgeRow],
    ) -> anyhow::Result<Vec<KnowledgeBundleRelationEdgeRow>> {
        self.delete_bundle_relation_edges(bundle_id).await?;
        self.insert_bundle_edges(KNOWLEDGE_BUNDLE_RELATION_EDGE, bundle_id, edges, |edge| {
            arango_doc_id(
                crate::infra::arangodb::collections::KNOWLEDGE_RELATION_COLLECTION,
                edge.relation_id,
            )
        })
        .await
    }

    pub async fn replace_bundle_evidence_edges(
        &self,
        bundle_id: Uuid,
        edges: &[KnowledgeBundleEvidenceEdgeRow],
    ) -> anyhow::Result<Vec<KnowledgeBundleEvidenceEdgeRow>> {
        self.delete_bundle_evidence_edges(bundle_id).await?;
        self.insert_bundle_edges(KNOWLEDGE_BUNDLE_EVIDENCE_EDGE, bundle_id, edges, |edge| {
            arango_doc_id(
                crate::infra::arangodb::collections::KNOWLEDGE_EVIDENCE_COLLECTION,
                edge.evidence_id,
            )
        })
        .await
    }

    pub async fn list_bundle_chunk_edges(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeBundleChunkEdgeRow>> {
        self.list_bundle_edges(KNOWLEDGE_BUNDLE_CHUNK_EDGE, bundle_id).await
    }

    pub async fn list_bundle_entity_edges(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeBundleEntityEdgeRow>> {
        self.list_bundle_edges(KNOWLEDGE_BUNDLE_ENTITY_EDGE, bundle_id).await
    }

    pub async fn list_bundle_relation_edges(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeBundleRelationEdgeRow>> {
        self.list_bundle_edges(KNOWLEDGE_BUNDLE_RELATION_EDGE, bundle_id).await
    }

    pub async fn list_bundle_evidence_edges(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeBundleEvidenceEdgeRow>> {
        self.list_bundle_edges(KNOWLEDGE_BUNDLE_EVIDENCE_EDGE, bundle_id).await
    }

    pub async fn get_bundle_reference_set(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeContextBundleReferenceSetRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR bundle IN @@bundle_collection
                 FILTER bundle.bundle_id == @bundle_id
                 LIMIT 1
                 LET chunk_references = (
                    FOR edge IN @@chunk_edge_collection
                    FILTER edge.bundle_id == bundle.bundle_id
                    SORT edge.rank ASC, edge.score DESC, edge._key ASC
                    RETURN {
                        _key: edge._key,
                        bundle_id: edge.bundle_id,
                        chunk_id: edge.chunk_id,
                        rank: edge.rank,
                        score: edge.score,
                        inclusion_reason: edge.inclusion_reason,
                        created_at: edge.created_at
                    }
                 )
                 LET entity_references = (
                    FOR edge IN @@entity_edge_collection
                    FILTER edge.bundle_id == bundle.bundle_id
                    SORT edge.rank ASC, edge.score DESC, edge._key ASC
                    RETURN {
                        _key: edge._key,
                        bundle_id: edge.bundle_id,
                        entity_id: edge.entity_id,
                        rank: edge.rank,
                        score: edge.score,
                        inclusion_reason: edge.inclusion_reason,
                        created_at: edge.created_at
                    }
                 )
                 LET relation_references = (
                    FOR edge IN @@relation_edge_collection
                    FILTER edge.bundle_id == bundle.bundle_id
                    SORT edge.rank ASC, edge.score DESC, edge._key ASC
                    RETURN {
                        _key: edge._key,
                        bundle_id: edge.bundle_id,
                        relation_id: edge.relation_id,
                        rank: edge.rank,
                        score: edge.score,
                        inclusion_reason: edge.inclusion_reason,
                        created_at: edge.created_at
                    }
                 )
                 LET evidence_references = (
                    FOR edge IN @@evidence_edge_collection
                    FILTER edge.bundle_id == bundle.bundle_id
                    SORT edge.rank ASC, edge.score DESC, edge._key ASC
                    RETURN {
                        _key: edge._key,
                        bundle_id: edge.bundle_id,
                        evidence_id: edge.evidence_id,
                        rank: edge.rank,
                        score: edge.score,
                        inclusion_reason: edge.inclusion_reason,
                        created_at: edge.created_at
                    }
                 )
                 RETURN {
                    bundle: bundle,
                    chunk_references: chunk_references,
                    entity_references: entity_references,
                    relation_references: relation_references,
                    evidence_references: evidence_references
                 }",
                serde_json::json!({
                    "@bundle_collection": KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION,
                    "@chunk_edge_collection": KNOWLEDGE_BUNDLE_CHUNK_EDGE,
                    "@entity_edge_collection": KNOWLEDGE_BUNDLE_ENTITY_EDGE,
                    "@relation_edge_collection": KNOWLEDGE_BUNDLE_RELATION_EDGE,
                    "@evidence_edge_collection": KNOWLEDGE_BUNDLE_EVIDENCE_EDGE,
                    "bundle_id": bundle_id,
                }),
            )
            .await
            .context("failed to get materialized knowledge context bundle")?;
        decode_optional_single_result(cursor)
    }

    pub async fn get_bundle_reference_set_by_query_execution(
        &self,
        query_execution_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeContextBundleReferenceSetRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR bundle IN @@bundle_collection
                 FILTER bundle.query_execution_id == @query_execution_id
                 LIMIT 1
                 LET chunk_references = (
                    FOR edge IN @@chunk_edge_collection
                    FILTER edge.bundle_id == bundle.bundle_id
                    SORT edge.rank ASC, edge.score DESC, edge._key ASC
                    RETURN {
                        _key: edge._key,
                        bundle_id: edge.bundle_id,
                        chunk_id: edge.chunk_id,
                        rank: edge.rank,
                        score: edge.score,
                        inclusion_reason: edge.inclusion_reason,
                        created_at: edge.created_at
                    }
                 )
                 LET entity_references = (
                    FOR edge IN @@entity_edge_collection
                    FILTER edge.bundle_id == bundle.bundle_id
                    SORT edge.rank ASC, edge.score DESC, edge._key ASC
                    RETURN {
                        _key: edge._key,
                        bundle_id: edge.bundle_id,
                        entity_id: edge.entity_id,
                        rank: edge.rank,
                        score: edge.score,
                        inclusion_reason: edge.inclusion_reason,
                        created_at: edge.created_at
                    }
                 )
                 LET relation_references = (
                    FOR edge IN @@relation_edge_collection
                    FILTER edge.bundle_id == bundle.bundle_id
                    SORT edge.rank ASC, edge.score DESC, edge._key ASC
                    RETURN {
                        _key: edge._key,
                        bundle_id: edge.bundle_id,
                        relation_id: edge.relation_id,
                        rank: edge.rank,
                        score: edge.score,
                        inclusion_reason: edge.inclusion_reason,
                        created_at: edge.created_at
                    }
                 )
                 LET evidence_references = (
                    FOR edge IN @@evidence_edge_collection
                    FILTER edge.bundle_id == bundle.bundle_id
                    SORT edge.rank ASC, edge.score DESC, edge._key ASC
                    RETURN {
                        _key: edge._key,
                        bundle_id: edge.bundle_id,
                        evidence_id: edge.evidence_id,
                        rank: edge.rank,
                        score: edge.score,
                        inclusion_reason: edge.inclusion_reason,
                        created_at: edge.created_at
                    }
                 )
                 RETURN {
                    bundle: bundle,
                    chunk_references: chunk_references,
                    entity_references: entity_references,
                    relation_references: relation_references,
                    evidence_references: evidence_references
                 }",
                serde_json::json!({
                    "@bundle_collection": KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION,
                    "@chunk_edge_collection": KNOWLEDGE_BUNDLE_CHUNK_EDGE,
                    "@entity_edge_collection": KNOWLEDGE_BUNDLE_ENTITY_EDGE,
                    "@relation_edge_collection": KNOWLEDGE_BUNDLE_RELATION_EDGE,
                    "@evidence_edge_collection": KNOWLEDGE_BUNDLE_EVIDENCE_EDGE,
                    "query_execution_id": query_execution_id,
                }),
            )
            .await
            .context("failed to get materialized knowledge context bundle by execution")?;
        decode_optional_single_result(cursor)
    }

    pub async fn list_bundle_reference_sets_by_library(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeContextBundleReferenceSetRow>> {
        let cursor = self
            .client
            .query_json(
                "FOR bundle IN @@bundle_collection
                 FILTER bundle.library_id == @library_id
                 SORT bundle.updated_at DESC, bundle.bundle_id DESC
                 LET chunk_references = (
                    FOR edge IN @@chunk_edge_collection
                    FILTER edge.bundle_id == bundle.bundle_id
                    SORT edge.rank ASC, edge.score DESC, edge._key ASC
                    RETURN {
                        _key: edge._key,
                        bundle_id: edge.bundle_id,
                        chunk_id: edge.chunk_id,
                        rank: edge.rank,
                        score: edge.score,
                        inclusion_reason: edge.inclusion_reason,
                        created_at: edge.created_at
                    }
                 )
                 LET entity_references = (
                    FOR edge IN @@entity_edge_collection
                    FILTER edge.bundle_id == bundle.bundle_id
                    SORT edge.rank ASC, edge.score DESC, edge._key ASC
                    RETURN {
                        _key: edge._key,
                        bundle_id: edge.bundle_id,
                        entity_id: edge.entity_id,
                        rank: edge.rank,
                        score: edge.score,
                        inclusion_reason: edge.inclusion_reason,
                        created_at: edge.created_at
                    }
                 )
                 LET relation_references = (
                    FOR edge IN @@relation_edge_collection
                    FILTER edge.bundle_id == bundle.bundle_id
                    SORT edge.rank ASC, edge.score DESC, edge._key ASC
                    RETURN {
                        _key: edge._key,
                        bundle_id: edge.bundle_id,
                        relation_id: edge.relation_id,
                        rank: edge.rank,
                        score: edge.score,
                        inclusion_reason: edge.inclusion_reason,
                        created_at: edge.created_at
                    }
                 )
                 LET evidence_references = (
                    FOR edge IN @@evidence_edge_collection
                    FILTER edge.bundle_id == bundle.bundle_id
                    SORT edge.rank ASC, edge.score DESC, edge._key ASC
                    RETURN {
                        _key: edge._key,
                        bundle_id: edge.bundle_id,
                        evidence_id: edge.evidence_id,
                        rank: edge.rank,
                        score: edge.score,
                        inclusion_reason: edge.inclusion_reason,
                        created_at: edge.created_at
                    }
                 )
                 RETURN {
                    bundle: bundle,
                    chunk_references: chunk_references,
                    entity_references: entity_references,
                    relation_references: relation_references,
                    evidence_references: evidence_references
                 }",
                serde_json::json!({
                    "@bundle_collection": KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION,
                    "@chunk_edge_collection": KNOWLEDGE_BUNDLE_CHUNK_EDGE,
                    "@entity_edge_collection": KNOWLEDGE_BUNDLE_ENTITY_EDGE,
                    "@relation_edge_collection": KNOWLEDGE_BUNDLE_RELATION_EDGE,
                    "@evidence_edge_collection": KNOWLEDGE_BUNDLE_EVIDENCE_EDGE,
                    "library_id": library_id,
                }),
            )
            .await
            .context("failed to list materialized knowledge context bundles by library")?;
        decode_many_results(cursor)
    }

    pub async fn delete_bundle_chunk_edges(&self, bundle_id: Uuid) -> anyhow::Result<u64> {
        self.delete_bundle_edges(KNOWLEDGE_BUNDLE_CHUNK_EDGE, bundle_id).await
    }

    pub async fn delete_bundle_entity_edges(&self, bundle_id: Uuid) -> anyhow::Result<u64> {
        self.delete_bundle_edges(KNOWLEDGE_BUNDLE_ENTITY_EDGE, bundle_id).await
    }

    pub async fn delete_bundle_relation_edges(&self, bundle_id: Uuid) -> anyhow::Result<u64> {
        self.delete_bundle_edges(KNOWLEDGE_BUNDLE_RELATION_EDGE, bundle_id).await
    }

    pub async fn delete_bundle_evidence_edges(&self, bundle_id: Uuid) -> anyhow::Result<u64> {
        self.delete_bundle_edges(KNOWLEDGE_BUNDLE_EVIDENCE_EDGE, bundle_id).await
    }

    async fn list_bundle_edges<T>(
        &self,
        collection: &'static str,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<T>>
    where
        T: DeserializeOwned,
    {
        let cursor = self
            .client
            .query_json(
                "FOR edge IN @@collection
                 FILTER edge.bundle_id == @bundle_id
                 SORT edge.rank ASC, edge.created_at ASC, edge._key ASC
                 RETURN edge",
                serde_json::json!({
                    "@collection": collection,
                    "bundle_id": bundle_id,
                }),
            )
            .await
            .with_context(|| format!("failed to list bundle edges in {collection}"))?;
        decode_many_results(cursor)
    }

    async fn delete_bundle_edges(
        &self,
        collection: &'static str,
        bundle_id: Uuid,
    ) -> anyhow::Result<u64> {
        let cursor = self
            .client
            .query_json(
                "FOR edge IN @@collection
                 FILTER edge.bundle_id == @bundle_id
                 REMOVE edge IN @@collection
                 OPTIONS { ignoreErrors: true }
                 RETURN OLD",
                serde_json::json!({
                    "@collection": collection,
                    "bundle_id": bundle_id,
                }),
            )
            .await
            .with_context(|| format!("failed to delete bundle edges in {collection}"))?;
        let result = cursor
            .get("extra")
            .and_then(|extra| extra.get("stats"))
            .and_then(|stats| stats.get("writesExecuted"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        Ok(result)
    }

    async fn insert_bundle_edges<T, F>(
        &self,
        collection: &'static str,
        bundle_id: Uuid,
        edges: &[T],
        to_doc_id: F,
    ) -> anyhow::Result<Vec<T>>
    where
        T: Serialize + DeserializeOwned + Clone,
        F: Fn(&T) -> String,
    {
        let mut inserted = Vec::with_capacity(edges.len());
        for edge in edges {
            let edge_value = serde_json::to_value(edge).context("failed to encode bundle edge")?;
            let edge_object = edge_value
                .as_object()
                .cloned()
                .ok_or_else(|| anyhow!("bundle edge payload must be a JSON object"))?;
            let key = edge_value
                .get("_key")
                .and_then(|value| value.as_str())
                .ok_or_else(|| anyhow!("bundle edge is missing key"))?;
            let from = arango_doc_id(KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION, bundle_id);
            let to = to_doc_id(edge);
            let mut document = edge_object;
            document.insert("_key".to_string(), serde_json::json!(key));
            document.insert("_from".to_string(), serde_json::json!(from));
            document.insert("_to".to_string(), serde_json::json!(to));
            document.insert("bundle_id".to_string(), serde_json::json!(bundle_id));
            document.insert(
                "created_at".to_string(),
                edge_value
                    .get("created_at")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!(Utc::now())),
            );
            let cursor = self
                .client
                .query_json(
                    "UPSERT { _key: @key }
                     INSERT @document
                     UPDATE @document
                     IN @@collection
                     RETURN NEW",
                    serde_json::json!({
                        "@collection": collection,
                        "key": key,
                        "document": document
                    }),
                )
                .await
                .with_context(|| format!("failed to insert bundle edge in {collection}"))?;
            inserted.push(decode_single_result(cursor)?);
        }
        Ok(inserted)
    }
}

fn arango_doc_id(collection: &str, id: Uuid) -> String {
    format!("{collection}/{id}")
}

fn decode_single_result<T>(cursor: serde_json::Value) -> anyhow::Result<T>
where
    T: DeserializeOwned,
{
    decode_optional_single_result(cursor)?.ok_or_else(|| anyhow!("ArangoDB query returned no rows"))
}

fn decode_optional_single_result<T>(cursor: serde_json::Value) -> anyhow::Result<Option<T>>
where
    T: DeserializeOwned,
{
    let result = cursor
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow!("ArangoDB cursor response is missing result"))?;
    let mut rows: Vec<T> =
        serde_json::from_value(result).context("failed to decode ArangoDB result rows")?;
    Ok((!rows.is_empty()).then(|| rows.remove(0)))
}

fn decode_many_results<T>(cursor: serde_json::Value) -> anyhow::Result<Vec<T>>
where
    T: DeserializeOwned,
{
    let result = cursor
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow!("ArangoDB cursor response is missing result"))?;
    serde_json::from_value(result).context("failed to decode ArangoDB result rows")
}
