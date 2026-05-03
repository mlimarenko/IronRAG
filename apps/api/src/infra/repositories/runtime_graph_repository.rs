#![allow(clippy::missing_errors_doc, clippy::too_many_arguments, clippy::too_many_lines)]

mod coordination;
mod snapshot;

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::RuntimeGraphFilteredArtifactRow;
use crate::shared::text_tokens::normalized_alnum_token_sequence_by;

pub use coordination::*;
pub use snapshot::*;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphNodeRow {
    pub id: Uuid,
    pub library_id: Uuid,
    pub canonical_key: String,
    pub label: String,
    pub node_type: String,
    pub aliases_json: serde_json::Value,
    pub summary: Option<String>,
    pub metadata_json: serde_json::Value,
    pub support_count: i32,
    pub projection_version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphEdgeRow {
    pub id: Uuid,
    pub library_id: Uuid,
    pub from_node_id: Uuid,
    pub to_node_id: Uuid,
    pub relation_type: String,
    pub canonical_key: String,
    pub summary: Option<String>,
    pub weight: Option<f64>,
    pub support_count: i32,
    pub metadata_json: serde_json::Value,
    pub projection_version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphEvidenceRow {
    pub id: Uuid,
    pub library_id: Uuid,
    pub target_kind: String,
    pub target_id: Uuid,
    pub document_id: Option<Uuid>,
    pub chunk_id: Option<Uuid>,
    pub source_file_name: Option<String>,
    pub page_ref: Option<String>,
    pub evidence_text: String,
    pub confidence_score: Option<f64>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphEvidenceTargetRow {
    pub target_kind: String,
    pub target_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphEvidenceLifecycleRow {
    pub id: Uuid,
    pub library_id: Uuid,
    pub target_kind: String,
    pub target_id: Uuid,
    pub document_id: Option<Uuid>,
    pub revision_id: Option<Uuid>,
    pub activated_by_attempt_id: Option<Uuid>,
    pub chunk_id: Option<Uuid>,
    pub source_file_name: Option<String>,
    pub page_ref: Option<String>,
    pub evidence_text: String,
    pub confidence_score: Option<f64>,
    pub created_at: DateTime<Utc>,
}

const RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_QUERY_CAP: usize = 16;
const RUNTIME_GRAPH_EVIDENCE_LITERAL_SEARCH_QUERY_CAP: usize = 8;
const RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_TOKEN_CAP: usize = 16;
const RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_TOKEN_MIN_CHARS: usize = 4;
const RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_MIN_TOTAL_CHARS: usize = 11;
const RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_WINDOW_MIN_TOKENS: usize = 2;
const RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_WINDOW_MAX_TOKENS: usize = 4;
const RUNTIME_GRAPH_EVIDENCE_LITERAL_SEARCH_SHORT_MAX_TOKENS: usize = 6;
const RUNTIME_GRAPH_EVIDENCE_LITERAL_SEARCH_STRUCTURAL_MAX_TOKENS: usize = 20;
const RUNTIME_GRAPH_EVIDENCE_LITERAL_SEARCH_SHORT_MAX_CHARS: usize = 128;
const RUNTIME_GRAPH_EVIDENCE_LITERAL_SEARCH_STRUCTURAL_MAX_CHARS: usize = 220;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphProjectionCountsRow {
    pub node_count: i64,
    pub edge_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphDocumentLinkRow {
    pub document_id: Uuid,
    pub target_node_id: Uuid,
    pub target_node_type: String,
    pub relation_type: String,
    pub support_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeGraphSubTypeHintRow {
    pub node_type: String,
    pub sub_type: String,
    pub occurrences: i64,
}

fn runtime_graph_evidence_identity_key(
    target_kind: &str,
    target_id: Uuid,
    document_id: Option<Uuid>,
    revision_id: Option<Uuid>,
    activated_by_attempt_id: Option<Uuid>,
    chunk_id: Option<Uuid>,
    page_ref: Option<&str>,
    source_file_name: Option<&str>,
    evidence_context_key: &str,
) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}:{}:{}:{}",
        target_kind,
        target_id,
        document_id.map(|value| value.to_string()).unwrap_or_else(|| "none".to_string()),
        revision_id.map(|value| value.to_string()).unwrap_or_else(|| "none".to_string()),
        activated_by_attempt_id
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string()),
        chunk_id.map(|value| value.to_string()).unwrap_or_else(|| "none".to_string()),
        page_ref.unwrap_or("none"),
        source_file_name.unwrap_or("none"),
        evidence_context_key
    )
}

/// Persists one filtered graph artifact for later diagnostics.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the filtered artifact row.
pub async fn create_runtime_graph_filtered_artifact(
    pool: &PgPool,
    library_id: Uuid,
    ingestion_run_id: Option<Uuid>,
    revision_id: Option<Uuid>,
    target_kind: &str,
    candidate_key: &str,
    source_node_key: Option<&str>,
    target_node_key: Option<&str>,
    relation_type: Option<&str>,
    filter_reason: &str,
    summary: Option<&str>,
    metadata_json: serde_json::Value,
) -> Result<RuntimeGraphFilteredArtifactRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphFilteredArtifactRow>(
        "insert into runtime_graph_filtered_artifact (
            id, library_id, ingestion_run_id, revision_id, target_kind, candidate_key,
            source_node_key, target_node_key, relation_type, filter_reason, summary, metadata_json
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
         returning id, library_id, ingestion_run_id, revision_id, target_kind, candidate_key,
            source_node_key, target_node_key, relation_type, filter_reason, summary, metadata_json, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(library_id)
    .bind(ingestion_run_id)
    .bind(revision_id)
    .bind(target_kind)
    .bind(candidate_key)
    .bind(source_node_key)
    .bind(target_node_key)
    .bind(relation_type)
    .bind(filter_reason)
    .bind(summary)
    .bind(metadata_json)
    .fetch_one(pool)
    .await
}

/// Lists admitted runtime graph nodes for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying admitted graph nodes.
#[tracing::instrument(
    level = "debug",
    name = "runtime_graph.list_admitted_nodes_by_library",
    skip_all,
    fields(%library_id, projection_version)
)]
pub async fn list_admitted_runtime_graph_nodes_by_library(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphNodeRow>(&admitted_runtime_graph_nodes_query(""))
        .bind(library_id)
        .bind(projection_version)
        .fetch_all(pool)
        .await
}

/// Counts admitted non-document runtime graph nodes for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while counting graph nodes.
pub async fn count_admitted_runtime_graph_entities_by_library(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from runtime_graph_node
         where library_id = $1
           and projection_version = $2
           and node_type <> 'document'",
    )
    .bind(library_id)
    .bind(projection_version)
    .fetch_one(pool)
    .await
}

/// Counts document-typed nodes in the current projection of a library. This is
/// the canonical measure of "how many documents actually appear in the graph",
/// distinct from `revision.graph_state = 'ready'` which only reports LLM
/// extraction success and can diverge from the graph projection when the
/// reconcile stage fails after extraction.
pub async fn count_runtime_graph_document_nodes_by_library(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from runtime_graph_node
         where library_id = $1
           and projection_version = $2
           and node_type = 'document'",
    )
    .bind(library_id)
    .bind(projection_version)
    .fetch_one(pool)
    .await
}

/// Lists library documents whose active revision has NO extraction record
/// at all — neither `ready` nor `processing` nor `failed` — yet other
/// revisions of the same document do. These are "orphaned on revision
/// transition": when a document got a new revision, the old revision's
/// extraction records stayed put but no job ever ran extract_graph against
/// the new one. Surfaced by the graph re-extract pass so a new ingest job
/// can fill the gap.
pub async fn list_library_documents_needing_graph_reextract(
    pool: &PgPool,
    library_id: Uuid,
    limit: i64,
) -> Result<Vec<(Uuid, Uuid, Uuid)>, sqlx::Error> {
    sqlx::query_as::<_, (Uuid, Uuid, Uuid)>(
        "select d.workspace_id, h.document_id, h.active_revision_id
         from content_document_head h
         join content_document d on d.id = h.document_id
         where d.library_id = $1
           and h.active_revision_id is not null
           and not exists (
                select 1 from runtime_graph_node n
                 where n.library_id = $1
                   and n.node_type = 'document'
                   and n.document_id = h.document_id
           )
           and not exists (
                select 1 from runtime_graph_extraction e
                 where e.document_id = h.document_id
                   and e.raw_output_json #>> '{lifecycle,revision_id}'
                       = h.active_revision_id::text
           )
           and exists (
                select 1 from runtime_graph_extraction e
                 where e.document_id = h.document_id
           )
           and not exists (
                select 1 from ingest_job j
                 where j.knowledge_document_id = h.document_id
                   and j.queue_state in ('queued', 'leased')
           )
         order by h.document_id
         limit $2",
    )
    .bind(library_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Lists library documents whose active revision has ready extraction records
/// yet produced no document node in the graph projection. Emitted by the
/// graph backfill pass so a subsequent `reconcile_revision_graph` can merge
/// the already-persisted extraction into the projection without calling the
/// LLM again.
pub async fn list_library_documents_missing_graph_node(
    pool: &PgPool,
    library_id: Uuid,
    limit: i64,
) -> Result<Vec<(Uuid, Uuid)>, sqlx::Error> {
    sqlx::query_as::<_, (Uuid, Uuid)>(
        "select h.document_id, h.active_revision_id
         from content_document_head h
         join content_document d on d.id = h.document_id
         where d.library_id = $1
           and h.active_revision_id is not null
           and not exists (
                select 1 from runtime_graph_node n
                 where n.library_id = $1
                   and n.node_type = 'document'
                   and n.document_id = h.document_id
           )
           and exists (
                select 1 from runtime_graph_extraction e
                 where e.document_id = h.document_id
                   and e.status = 'ready'
                   and e.raw_output_json #>> '{lifecycle,revision_id}'
                       = h.active_revision_id::text
           )
         order by h.document_id
         limit $2",
    )
    .bind(library_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Lists the strongest admitted non-document runtime graph nodes for one
/// projection version, ranked by support count and label stability.
///
/// # Errors
/// Returns any `SQLx` error raised while querying ranked graph nodes.
pub async fn list_top_admitted_runtime_graph_entities_by_library(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    limit: usize,
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "select id, library_id, canonical_key, label, node_type, aliases_json, summary,
            metadata_json, support_count, projection_version, created_at, updated_at
         from runtime_graph_node
         where library_id = $1
           and projection_version = $2
           and node_type <> 'document'
         order by support_count desc, label asc, created_at asc, id asc
         limit $3",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(limit as i64)
    .fetch_all(pool)
    .await
}

/// Lists admitted runtime graph nodes by id for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying admitted graph nodes.
pub async fn list_admitted_runtime_graph_nodes_by_ids(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    node_ids: &[Uuid],
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error> {
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, RuntimeGraphNodeRow>(&admitted_runtime_graph_nodes_query(
        "and node.id = any($3)",
    ))
    .bind(library_id)
    .bind(projection_version)
    .bind(node_ids)
    .fetch_all(pool)
    .await
}

/// Lists admitted runtime graph edges for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying admitted graph edges.
#[tracing::instrument(
    level = "debug",
    name = "runtime_graph.list_admitted_edges_by_library",
    skip_all,
    fields(%library_id, projection_version)
)]
pub async fn list_admitted_runtime_graph_edges_by_library(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphEdgeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEdgeRow>(
        "select id, library_id, from_node_id, to_node_id, relation_type, canonical_key,
            summary, weight, support_count, metadata_json, projection_version, created_at, updated_at
         from runtime_graph_edge
         where library_id = $1
           and projection_version = $2
           and btrim(relation_type) <> ''
           and from_node_id <> to_node_id
         order by relation_type asc, created_at asc, id asc",
    )
    .bind(library_id)
    .bind(projection_version)
        .fetch_all(pool)
        .await
}

/// Compact edge row — only the columns consumed by the NDJSON topology
/// (`build_compact_topology` in `services/knowledge/graph_stream.rs`).
/// Dropping the wide columns cuts the row width ~5× and lets Postgres
/// serve the full result set from index leaf pages without heap fetches.
#[derive(Debug, Clone, FromRow)]
pub struct RuntimeGraphEdgeCompactRow {
    pub from_node_id: Uuid,
    pub to_node_id: Uuid,
    pub relation_type: String,
    pub support_count: i32,
}

pub async fn list_admitted_runtime_graph_edges_compact_by_library(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphEdgeCompactRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEdgeCompactRow>(
        "select from_node_id, to_node_id, relation_type, support_count
         from runtime_graph_edge
         where library_id = $1
           and projection_version = $2
           and btrim(relation_type) <> ''
           and from_node_id <> to_node_id
         order by relation_type asc, support_count desc, from_node_id asc, to_node_id asc",
    )
    .bind(library_id)
    .bind(projection_version)
    .fetch_all(pool)
    .await
}

/// Fetches the full node rows for a pre-computed set of admitted ids
/// plus every `document`-type node in the library+projection bucket.
/// Replaces `list_admitted_runtime_graph_nodes_by_library` on the
/// topology path so the node query no longer duplicates the edge scan
/// via the `admitted_edges` CTE — the caller derives the admitted ids
/// once from the compact edge list and passes them through here.
pub async fn list_runtime_graph_nodes_by_ids_or_document_type(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    admitted_ids: &[Uuid],
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "select id, library_id, canonical_key, label, node_type, aliases_json,
            summary, metadata_json, support_count, projection_version, created_at, updated_at
         from runtime_graph_node
         where library_id = $1
           and projection_version = $2
           and (node_type = 'document' or id = any($3::uuid[]))
         order by node_type asc, label asc, created_at asc, id asc",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(admitted_ids)
    .fetch_all(pool)
    .await
}

/// Counts admitted runtime graph relations whose endpoints are both non-document
/// nodes for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while counting graph edges.
pub async fn count_admitted_runtime_graph_relations_by_library(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from runtime_graph_edge as edge
         inner join runtime_graph_node as source
            on source.library_id = edge.library_id
           and source.id = edge.from_node_id
           and source.projection_version = edge.projection_version
           and source.node_type <> 'document'
         inner join runtime_graph_node as target
            on target.library_id = edge.library_id
           and target.id = edge.to_node_id
           and target.projection_version = edge.projection_version
           and target.node_type <> 'document'
         where edge.library_id = $1
           and edge.projection_version = $2
           and btrim(edge.relation_type) <> ''
           and edge.from_node_id <> edge.to_node_id",
    )
    .bind(library_id)
    .bind(projection_version)
    .fetch_one(pool)
    .await
}

/// Lists the strongest admitted runtime graph relations whose endpoints are
/// both non-document nodes.
///
/// # Errors
/// Returns any `SQLx` error raised while querying ranked graph edges.
pub async fn list_top_admitted_runtime_graph_relations_by_library(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    limit: usize,
) -> Result<Vec<RuntimeGraphEdgeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEdgeRow>(
        "select edge.id, edge.library_id, edge.from_node_id, edge.to_node_id, edge.relation_type,
            edge.canonical_key, edge.summary, edge.weight, edge.support_count, edge.metadata_json,
            edge.projection_version, edge.created_at, edge.updated_at
         from runtime_graph_edge as edge
         inner join runtime_graph_node as source
            on source.library_id = edge.library_id
           and source.id = edge.from_node_id
           and source.projection_version = edge.projection_version
           and source.node_type <> 'document'
         inner join runtime_graph_node as target
            on target.library_id = edge.library_id
           and target.id = edge.to_node_id
           and target.projection_version = edge.projection_version
           and target.node_type <> 'document'
         where edge.library_id = $1
           and edge.projection_version = $2
           and btrim(edge.relation_type) <> ''
           and edge.from_node_id <> edge.to_node_id
         order by edge.support_count desc, edge.relation_type asc, edge.created_at asc, edge.id asc
         limit $3",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(limit as i64)
    .fetch_all(pool)
    .await
}

/// Lists admitted runtime graph edges by id for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying admitted graph edges.
pub async fn list_admitted_runtime_graph_edges_by_ids(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    edge_ids: &[Uuid],
) -> Result<Vec<RuntimeGraphEdgeRow>, sqlx::Error> {
    if edge_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, RuntimeGraphEdgeRow>(
        "select id, library_id, from_node_id, to_node_id, relation_type, canonical_key,
            summary, weight, support_count, metadata_json, projection_version, created_at, updated_at
         from runtime_graph_edge
         where library_id = $1
           and projection_version = $2
           and id = any($3)
           and btrim(relation_type) <> ''
           and from_node_id <> to_node_id
         order by relation_type asc, created_at asc, id asc",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(edge_ids)
    .fetch_all(pool)
    .await
}

/// Lists admitted runtime graph edges that touch any of the supplied node ids.
///
/// # Errors
/// Returns any `SQLx` error raised while querying admitted graph edges.
pub async fn list_admitted_runtime_graph_edges_by_node_ids(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    node_ids: &[Uuid],
) -> Result<Vec<RuntimeGraphEdgeRow>, sqlx::Error> {
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, RuntimeGraphEdgeRow>(
        "select id, library_id, from_node_id, to_node_id, relation_type, canonical_key,
            summary, weight, support_count, metadata_json, projection_version, created_at, updated_at
         from runtime_graph_edge
         where library_id = $1
           and projection_version = $2
           and (from_node_id = any($3) or to_node_id = any($3))
           and btrim(relation_type) <> ''
           and from_node_id <> to_node_id
         order by relation_type asc, created_at asc, id asc",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(node_ids)
    .fetch_all(pool)
    .await
}

/// Upserts a canonical runtime graph node.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting or updating the graph node.
pub async fn upsert_runtime_graph_node(
    pool: &PgPool,
    library_id: Uuid,
    canonical_key: &str,
    label: &str,
    node_type: &str,
    document_id: Option<Uuid>,
    aliases_json: serde_json::Value,
    summary: Option<&str>,
    metadata_json: serde_json::Value,
    support_count: i32,
    projection_version: i64,
) -> Result<RuntimeGraphNodeRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "insert into runtime_graph_node (
            id, library_id, canonical_key, label, node_type, document_id, aliases_json, summary,
            metadata_json, support_count, projection_version
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
         on conflict (library_id, canonical_key, projection_version) do update
         set label = excluded.label,
             node_type = excluded.node_type,
             document_id = excluded.document_id,
             aliases_json = excluded.aliases_json,
             summary = CASE
                 WHEN excluded.summary IS NOT NULL AND excluded.summary != ''
                      AND (runtime_graph_node.summary IS NULL OR runtime_graph_node.summary = ''
                           OR length(excluded.summary) > length(runtime_graph_node.summary))
                 THEN excluded.summary
                 ELSE runtime_graph_node.summary
             END,
             metadata_json = excluded.metadata_json,
             support_count = excluded.support_count,
             updated_at = now()
         returning id, library_id, canonical_key, label, node_type, aliases_json, summary,
            metadata_json, support_count, projection_version, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(library_id)
    .bind(canonical_key)
    .bind(label)
    .bind(node_type)
    .bind(document_id)
    .bind(aliases_json)
    .bind(summary)
    .bind(metadata_json)
    .bind(support_count)
    .bind(projection_version)
    .fetch_one(pool)
    .await
}

/// Upserts the one canonical source-document node for a logical document.
///
/// Document nodes have a second uniqueness contract: exactly one
/// `(library_id, canonical_key, projection_version)` row whose
/// `canonical_key` is derived from the document id. Multi-chunk graph
/// rebuilds may merge chunks in parallel, so this path uses the same global
/// canonical-key conflict target that can fire during concurrent inserts.
pub async fn upsert_runtime_graph_document_node(
    pool: &PgPool,
    library_id: Uuid,
    document_id: Uuid,
    canonical_key: &str,
    label: &str,
    aliases_json: serde_json::Value,
    summary: Option<&str>,
    metadata_json: serde_json::Value,
    support_count: i32,
    projection_version: i64,
) -> Result<RuntimeGraphNodeRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "with document_node_lock as (
            select pg_advisory_xact_lock(
                hashtextextended($2::text || ':' || $5::text || ':' || $10::text, 0)
            )
         )
         insert into runtime_graph_node (
            id, library_id, canonical_key, label, node_type, document_id, aliases_json, summary,
            metadata_json, support_count, projection_version
         )
         select $1, $2, $3, $4, 'document', $5, $6, $7, $8, $9, $10
         from document_node_lock
         on conflict (library_id, canonical_key, projection_version) do update
         set label = excluded.label,
             node_type = 'document',
             document_id = excluded.document_id,
             aliases_json = excluded.aliases_json,
             summary = CASE
                 WHEN excluded.summary IS NOT NULL AND excluded.summary != ''
                      AND (runtime_graph_node.summary IS NULL OR runtime_graph_node.summary = ''
                           OR length(excluded.summary) > length(runtime_graph_node.summary))
                 THEN excluded.summary
                 ELSE runtime_graph_node.summary
             END,
             metadata_json = excluded.metadata_json,
             support_count = excluded.support_count,
             updated_at = now()
         returning id, library_id, canonical_key, label, node_type, aliases_json, summary,
            metadata_json, support_count, projection_version, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(library_id)
    .bind(canonical_key)
    .bind(label)
    .bind(document_id)
    .bind(aliases_json)
    .bind(summary)
    .bind(metadata_json)
    .bind(support_count)
    .bind(projection_version)
    .fetch_one(pool)
    .await
}

/// Loads one canonical runtime graph node for a projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph node.
pub async fn get_runtime_graph_node_by_key(
    pool: &PgPool,
    library_id: Uuid,
    canonical_key: &str,
    projection_version: i64,
) -> Result<Option<RuntimeGraphNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "select id, library_id, canonical_key, label, node_type, aliases_json, summary,
            metadata_json, support_count, projection_version, created_at, updated_at
         from runtime_graph_node
         where library_id = $1 and canonical_key = $2 and projection_version = $3",
    )
    .bind(library_id)
    .bind(canonical_key)
    .bind(projection_version)
    .fetch_optional(pool)
    .await
}

/// One row worth of input for `bulk_upsert_runtime_graph_nodes`. Kept
/// separate from `RuntimeGraphNodeRow` because the bulk path carries
/// only what the caller supplies — `id`, `created_at`, `updated_at`,
/// and `projection_version` are set by the DB.
#[derive(Debug, Clone)]
pub struct RuntimeGraphNodeUpsertInput {
    pub canonical_key: String,
    pub label: String,
    pub node_type: String,
    pub aliases_json: serde_json::Value,
    pub summary: Option<String>,
    pub metadata_json: serde_json::Value,
    pub support_count: i32,
}

/// Bulk UPSERT of runtime graph nodes. One round-trip replaces N
/// sequential `upsert_runtime_graph_node` calls — on a typical chunk
/// merge (15 entities + 10 relations × 2 endpoints = up to 35 node
/// upserts) this collapses 35 fan-out INSERT/UPDATE round-trips into
/// one, which (a) dramatically shortens pool-hold time and (b) lets
/// Postgres batch the WAL flush instead of fsyncing per row. `inputs`
/// may contain duplicate canonical keys; the last duplicate wins per
/// ON CONFLICT semantics, matching what the serial fan-out path did
/// under race conditions.
///
/// RETURNING order is not guaranteed to match input order. Callers
/// index the result by `canonical_key`.
///
/// # Errors
/// Returns any `SQLx` error raised while persisting the graph nodes.
pub async fn bulk_upsert_runtime_graph_nodes(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    inputs: &[RuntimeGraphNodeUpsertInput],
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error> {
    if inputs.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<Uuid> = (0..inputs.len()).map(|_| Uuid::now_v7()).collect();
    let canonical_keys: Vec<&str> = inputs.iter().map(|i| i.canonical_key.as_str()).collect();
    let labels: Vec<&str> = inputs.iter().map(|i| i.label.as_str()).collect();
    let node_types: Vec<&str> = inputs.iter().map(|i| i.node_type.as_str()).collect();
    let aliases_jsons: Vec<serde_json::Value> =
        inputs.iter().map(|i| i.aliases_json.clone()).collect();
    let summaries: Vec<Option<&str>> = inputs.iter().map(|i| i.summary.as_deref()).collect();
    let metadatas: Vec<serde_json::Value> =
        inputs.iter().map(|i| i.metadata_json.clone()).collect();
    let supports: Vec<i32> = inputs.iter().map(|i| i.support_count).collect();

    sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "insert into runtime_graph_node (
            id, library_id, canonical_key, label, node_type, aliases_json, summary,
            metadata_json, support_count, projection_version
         )
         select
            t.id, $1::uuid, t.canonical_key, t.label, t.node_type, t.aliases_json,
            t.summary, t.metadata_json, t.support_count, $2::bigint
         from unnest(
            $3::uuid[], $4::text[], $5::text[], $6::text[], $7::jsonb[],
            $8::text[], $9::jsonb[], $10::int[]
         ) as t(id, canonical_key, label, node_type, aliases_json, summary, metadata_json, support_count)
         on conflict (library_id, canonical_key, projection_version) do update
         set label = excluded.label,
             node_type = excluded.node_type,
             aliases_json = excluded.aliases_json,
             summary = CASE
                 WHEN excluded.summary IS NOT NULL AND excluded.summary != ''
                      AND (runtime_graph_node.summary IS NULL OR runtime_graph_node.summary = ''
                           OR length(excluded.summary) > length(runtime_graph_node.summary))
                 THEN excluded.summary
                 ELSE runtime_graph_node.summary
             END,
             metadata_json = excluded.metadata_json,
             support_count = excluded.support_count,
             updated_at = now()
         returning id, library_id, canonical_key, label, node_type, aliases_json, summary,
            metadata_json, support_count, projection_version, created_at, updated_at",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(&ids)
    .bind(&canonical_keys)
    .bind(&labels)
    .bind(&node_types)
    .bind(&aliases_jsons)
    .bind(&summaries)
    .bind(&metadatas)
    .bind(&supports)
    .fetch_all(pool)
    .await
}

/// Bulk-loads canonical runtime graph nodes for a projection version by
/// canonical key set. One round-trip replaces N single-key lookups — on a
/// chunk merge with 15 entities and 10 relations this collapses ~35
/// sequential `get_runtime_graph_node_by_key` calls into one indexed
/// range scan, reducing pool-hold time and lock-wait pressure during
/// `merge_chunk_graph_candidates`.
///
/// Returns the rows in the same order they appear in `canonical_keys`.
/// Keys with no matching row are simply absent from the result.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph nodes.
pub async fn list_runtime_graph_nodes_by_canonical_keys(
    pool: &PgPool,
    library_id: Uuid,
    canonical_keys: &[String],
    projection_version: i64,
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error> {
    if canonical_keys.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "select id, library_id, canonical_key, label, node_type, aliases_json, summary,
            metadata_json, support_count, projection_version, created_at, updated_at
         from runtime_graph_node
         where library_id = $1
           and projection_version = $2
           and canonical_key = any($3)",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(canonical_keys)
    .fetch_all(pool)
    .await
}

/// Loads one canonical runtime graph node by id.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph node.
pub async fn get_runtime_graph_node_by_id(
    pool: &PgPool,
    library_id: Uuid,
    id: Uuid,
) -> Result<Option<RuntimeGraphNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "select id, library_id, canonical_key, label, node_type, aliases_json, summary,
            metadata_json, support_count, projection_version, created_at, updated_at
         from runtime_graph_node
         where library_id = $1 and id = $2",
    )
    .bind(library_id)
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Lists canonical runtime graph nodes for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph nodes.
pub async fn list_runtime_graph_nodes_by_library(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "select id, library_id, canonical_key, label, node_type, aliases_json, summary,
            metadata_json, support_count, projection_version, created_at, updated_at
         from runtime_graph_node
         where library_id = $1 and projection_version = $2
         order by node_type asc, label asc, created_at asc, id asc",
    )
    .bind(library_id)
    .bind(projection_version)
    .fetch_all(pool)
    .await
}

/// Aggregates observed `sub_type` values per `node_type` for one library at a
/// given projection version. Drives vocabulary-aware extraction: the returned
/// rows feed the `sub_type_hints` prompt section so the LLM converges on terms
/// already present in the graph instead of inventing fresh near-duplicates.
///
/// Rows are ordered by `node_type asc, occurrences desc, sub_type asc`. The
/// caller (typically `revision.rs`) trims to top-N per `node_type`.
///
/// # Errors
/// Returns any `SQLx` error raised while running the aggregation.
pub async fn list_observed_sub_type_hints(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphSubTypeHintRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphSubTypeHintRow>(
        "select node_type,
                metadata_json->>'sub_type' as sub_type,
                count(*)::bigint as occurrences
         from runtime_graph_node
         where library_id = $1
           and projection_version = $2
           and metadata_json ? 'sub_type'
           and length(metadata_json->>'sub_type') > 0
         group by node_type, metadata_json->>'sub_type'
         order by node_type asc, occurrences desc, sub_type asc",
    )
    .bind(library_id)
    .bind(projection_version)
    .fetch_all(pool)
    .await
}

/// Upserts a canonical runtime graph edge.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting or updating the graph edge.
pub async fn upsert_runtime_graph_edge(
    pool: &PgPool,
    library_id: Uuid,
    from_node_id: Uuid,
    to_node_id: Uuid,
    relation_type: &str,
    canonical_key: &str,
    summary: Option<&str>,
    weight: Option<f64>,
    support_count: i32,
    metadata_json: serde_json::Value,
    projection_version: i64,
) -> Result<RuntimeGraphEdgeRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEdgeRow>(
        "insert into runtime_graph_edge (
            id, library_id, from_node_id, to_node_id, relation_type, canonical_key, summary,
            weight, support_count, metadata_json, projection_version
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
         on conflict (library_id, canonical_key, projection_version) do update
         set from_node_id = excluded.from_node_id,
             to_node_id = excluded.to_node_id,
             relation_type = excluded.relation_type,
             summary = excluded.summary,
             weight = excluded.weight,
             support_count = excluded.support_count,
             metadata_json = excluded.metadata_json,
             updated_at = now()
         returning id, library_id, from_node_id, to_node_id, relation_type, canonical_key,
            summary, weight, support_count, metadata_json, projection_version, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(library_id)
    .bind(from_node_id)
    .bind(to_node_id)
    .bind(relation_type)
    .bind(canonical_key)
    .bind(summary)
    .bind(weight)
    .bind(support_count)
    .bind(metadata_json)
    .bind(projection_version)
    .fetch_one(pool)
    .await
}

/// Loads one canonical runtime graph edge for a projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph edge.
pub async fn get_runtime_graph_edge_by_key(
    pool: &PgPool,
    library_id: Uuid,
    canonical_key: &str,
    projection_version: i64,
) -> Result<Option<RuntimeGraphEdgeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEdgeRow>(
        "select id, library_id, from_node_id, to_node_id, relation_type, canonical_key,
            summary, weight, support_count, metadata_json, projection_version, created_at, updated_at
         from runtime_graph_edge
         where library_id = $1 and canonical_key = $2 and projection_version = $3",
    )
    .bind(library_id)
    .bind(canonical_key)
    .bind(projection_version)
    .fetch_optional(pool)
    .await
}

/// Loads one canonical runtime graph edge by id.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph edge.
pub async fn get_runtime_graph_edge_by_id(
    pool: &PgPool,
    library_id: Uuid,
    id: Uuid,
) -> Result<Option<RuntimeGraphEdgeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEdgeRow>(
        "select id, library_id, from_node_id, to_node_id, relation_type, canonical_key,
            summary, weight, support_count, metadata_json, projection_version, created_at, updated_at
         from runtime_graph_edge
         where library_id = $1 and id = $2",
    )
    .bind(library_id)
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Lists canonical runtime graph edges for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph edges.
pub async fn list_runtime_graph_edges_by_library(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphEdgeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEdgeRow>(
        "select id, library_id, from_node_id, to_node_id, relation_type, canonical_key,
            summary, weight, support_count, metadata_json, projection_version, created_at, updated_at
         from runtime_graph_edge
         where library_id = $1 and projection_version = $2
         order by relation_type asc, created_at asc, id asc",
    )
    .bind(library_id)
    .bind(projection_version)
    .fetch_all(pool)
    .await
}

/// Creates a runtime graph evidence link.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the evidence record.
pub async fn create_runtime_graph_evidence(
    pool: &PgPool,
    library_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
    document_id: Option<Uuid>,
    revision_id: Option<Uuid>,
    activated_by_attempt_id: Option<Uuid>,
    chunk_id: Option<Uuid>,
    source_file_name: Option<&str>,
    page_ref: Option<&str>,
    evidence_text: &str,
    confidence_score: Option<f64>,
    evidence_context_key: &str,
) -> Result<RuntimeGraphEvidenceRow, sqlx::Error> {
    let evidence_identity_key = runtime_graph_evidence_identity_key(
        target_kind,
        target_id,
        document_id,
        revision_id,
        activated_by_attempt_id,
        chunk_id,
        page_ref,
        source_file_name,
        evidence_context_key,
    );
    sqlx::query_as::<_, RuntimeGraphEvidenceRow>(
        "insert into runtime_graph_evidence (
            id, library_id, evidence_identity_key, target_kind, target_id, document_id, revision_id, activated_by_attempt_id,
            chunk_id, source_file_name, page_ref, evidence_text, confidence_score
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
         on conflict (library_id, evidence_identity_key) do update
         set document_id = excluded.document_id,
             revision_id = excluded.revision_id,
             activated_by_attempt_id = excluded.activated_by_attempt_id,
             chunk_id = excluded.chunk_id,
             source_file_name = excluded.source_file_name,
             page_ref = excluded.page_ref,
             evidence_text = excluded.evidence_text,
             confidence_score = excluded.confidence_score
         returning id, library_id, target_kind, target_id, document_id, chunk_id, source_file_name,
            page_ref, evidence_text, confidence_score, created_at",
    )
    .bind(Uuid::now_v7())
    .bind(library_id)
    .bind(&evidence_identity_key)
    .bind(target_kind)
    .bind(target_id)
    .bind(document_id)
    .bind(revision_id)
    .bind(activated_by_attempt_id)
    .bind(chunk_id)
    .bind(source_file_name)
    .bind(page_ref)
    .bind(evidence_text)
    .bind(confidence_score)
    .fetch_one(pool)
    .await
}

/// Single per-row payload for `bulk_create_runtime_graph_evidence_for_chunk`.
/// All other evidence columns are constant per merge call (the chunk's
/// document_id / revision_id / attempt_id / chunk_id / source_file_name /
/// evidence_text), so the bulk insert sends N rows in one round-trip
/// instead of N separate INSERTs.
#[derive(Debug, Clone)]
pub struct GraphEvidenceTarget {
    pub target_kind: &'static str,
    pub target_id: Uuid,
    pub evidence_context_key: &'static str,
}

/// Bulk-inserts a batch of `runtime_graph_evidence` rows that share the same
/// chunk-level context (library / document / revision / attempt / chunk /
/// source_file_name / evidence_text). Replaces N sequential
/// `create_runtime_graph_evidence` calls with a single `INSERT ... SELECT
/// FROM unnest(...)` round-trip — for a typical chunk with 10 entities and
/// 10 relations, that's ~50 round-trips collapsed into 1.
///
/// # Errors
/// Returns any `SQLx` error raised while running the bulk insert.
#[allow(clippy::too_many_arguments)]
pub async fn bulk_create_runtime_graph_evidence_for_chunk(
    pool: &PgPool,
    library_id: Uuid,
    document_id: Option<Uuid>,
    revision_id: Option<Uuid>,
    activated_by_attempt_id: Option<Uuid>,
    chunk_id: Option<Uuid>,
    source_file_name: Option<&str>,
    evidence_text: &str,
    confidence_score: Option<f64>,
    targets: &[GraphEvidenceTarget],
) -> Result<(), sqlx::Error> {
    if targets.is_empty() {
        return Ok(());
    }
    // Postgres forbids `ON CONFLICT DO UPDATE` from touching the same
    // conflict target twice in one statement. The chunk merge happily
    // emits duplicate evidence rows when the same entity / edge gets
    // mentioned multiple times inside one chunk (e.g. an entity appears
    // both as itself and as the target of a relation), which produced
    // the runtime error
    //   ON CONFLICT DO UPDATE command cannot affect row a second time
    // and broke the entire chunk merge. Dedupe by `evidence_identity_key`
    // here so the bulk insert sees each unique row exactly once. Order
    // is preserved so the first occurrence wins.
    let count = targets.len();
    let mut seen = std::collections::HashSet::with_capacity(count);
    let mut ids = Vec::with_capacity(count);
    let mut identity_keys = Vec::with_capacity(count);
    let mut target_kinds = Vec::with_capacity(count);
    let mut target_ids = Vec::with_capacity(count);
    for target in targets {
        let identity_key = runtime_graph_evidence_identity_key(
            target.target_kind,
            target.target_id,
            document_id,
            revision_id,
            activated_by_attempt_id,
            chunk_id,
            None,
            source_file_name,
            target.evidence_context_key,
        );
        if !seen.insert(identity_key.clone()) {
            continue;
        }
        ids.push(Uuid::now_v7());
        identity_keys.push(identity_key);
        target_kinds.push(target.target_kind.to_string());
        target_ids.push(target.target_id);
    }
    if ids.is_empty() {
        return Ok(());
    }

    sqlx::query(
        "insert into runtime_graph_evidence (
            id, library_id, evidence_identity_key, target_kind, target_id,
            document_id, revision_id, activated_by_attempt_id, chunk_id,
            source_file_name, page_ref, evidence_text, confidence_score
         )
         select
            ids.id, $2, ids.identity_key, ids.target_kind, ids.target_id,
            $3, $4, $5, $6, $7, NULL, $8, $9
         from unnest($1::uuid[], $10::text[], $11::text[], $12::uuid[])
            as ids(id, identity_key, target_kind, target_id)
         on conflict (library_id, evidence_identity_key) do update
         set document_id = excluded.document_id,
             revision_id = excluded.revision_id,
             activated_by_attempt_id = excluded.activated_by_attempt_id,
             chunk_id = excluded.chunk_id,
             source_file_name = excluded.source_file_name,
             page_ref = excluded.page_ref,
             evidence_text = excluded.evidence_text,
             confidence_score = excluded.confidence_score",
    )
    .bind(&ids)
    .bind(library_id)
    .bind(document_id)
    .bind(revision_id)
    .bind(activated_by_attempt_id)
    .bind(chunk_id)
    .bind(source_file_name)
    .bind(evidence_text)
    .bind(confidence_score)
    .bind(&identity_keys)
    .bind(&target_kinds)
    .bind(&target_ids)
    .execute(pool)
    .await
    .map(|_| ())
}

/// Recalculates support counts for a targeted set of graph nodes.
///
/// # Errors
/// Returns any `SQLx` error raised while updating support counts.
pub const RECALCULATE_RUNTIME_GRAPH_NODE_SUPPORT_COUNTS_BY_IDS_SQL: &str = "with target_nodes as (
            select id, support_count
            from runtime_graph_node
            where library_id = $1
              and projection_version = $2
              and id = any($3)
         ),
         evidence_counts as (
            select evidence.target_id, count(*)::int as support_count
            from runtime_graph_evidence as evidence
            join content_document as document
              on document.id = evidence.document_id
             and document.library_id = $1
             and document.document_state = 'active'
             and document.deleted_at is null
            where evidence.library_id = $1
              and evidence.target_kind = 'node'
              and evidence.target_id = any($3)
            group by evidence.target_id
         ),
         desired_counts as (
            select target_nodes.id,
                   coalesce(evidence_counts.support_count, 0) as support_count
            from target_nodes
            left join evidence_counts on evidence_counts.target_id = target_nodes.id
         )
         update runtime_graph_node as node
         set support_count = desired_counts.support_count,
             updated_at = now()
         from desired_counts
         where node.id = desired_counts.id
           and node.support_count is distinct from desired_counts.support_count";

pub async fn recalculate_runtime_graph_node_support_counts_by_ids(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    node_ids: &[Uuid],
) -> Result<u64, sqlx::Error> {
    if node_ids.is_empty() {
        return Ok(0);
    }

    let result = sqlx::query(RECALCULATE_RUNTIME_GRAPH_NODE_SUPPORT_COUNTS_BY_IDS_SQL)
        .bind(library_id)
        .bind(projection_version)
        .bind(node_ids)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// Recalculates support counts for a targeted set of graph edges.
///
/// # Errors
/// Returns any `SQLx` error raised while updating support counts.
pub const RECALCULATE_RUNTIME_GRAPH_EDGE_SUPPORT_COUNTS_BY_IDS_SQL: &str = "with target_edges as (
            select id, support_count
            from runtime_graph_edge
            where library_id = $1
              and projection_version = $2
              and id = any($3)
         ),
         evidence_counts as (
            select evidence.target_id, count(*)::int as support_count
            from runtime_graph_evidence as evidence
            join content_document as document
              on document.id = evidence.document_id
             and document.library_id = $1
             and document.document_state = 'active'
             and document.deleted_at is null
            where evidence.library_id = $1
              and evidence.target_kind = 'edge'
              and evidence.target_id = any($3)
            group by evidence.target_id
         ),
         desired_counts as (
            select target_edges.id,
                   coalesce(evidence_counts.support_count, 0) as support_count
            from target_edges
            left join evidence_counts on evidence_counts.target_id = target_edges.id
         )
         update runtime_graph_edge as edge
         set support_count = desired_counts.support_count,
             updated_at = now()
         from desired_counts
         where edge.id = desired_counts.id
           and edge.support_count is distinct from desired_counts.support_count";

pub async fn recalculate_runtime_graph_edge_support_counts_by_ids(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    edge_ids: &[Uuid],
) -> Result<u64, sqlx::Error> {
    if edge_ids.is_empty() {
        return Ok(0);
    }

    let result = sqlx::query(RECALCULATE_RUNTIME_GRAPH_EDGE_SUPPORT_COUNTS_BY_IDS_SQL)
        .bind(library_id)
        .bind(projection_version)
        .bind(edge_ids)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// Lists runtime graph evidence for one target.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the evidence rows.
pub async fn list_runtime_graph_evidence_by_target(
    pool: &PgPool,
    library_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
) -> Result<Vec<RuntimeGraphEvidenceRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEvidenceRow>(
        "select id, library_id, target_kind, target_id, document_id, chunk_id, source_file_name,
            page_ref, evidence_text, confidence_score, created_at
         from runtime_graph_evidence
         where library_id = $1 and target_kind = $2 and target_id = $3
         order by created_at desc, id desc",
    )
    .bind(library_id)
    .bind(target_kind)
    .bind(target_id)
    .fetch_all(pool)
    .await
}

/// Lists runtime graph evidence for an ordered set of node / edge targets.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the evidence rows.
pub async fn list_runtime_graph_evidence_by_targets(
    pool: &PgPool,
    library_id: Uuid,
    targets: &[(String, Uuid)],
    limit: i64,
) -> Result<Vec<RuntimeGraphEvidenceRow>, sqlx::Error> {
    if targets.is_empty() || limit <= 0 {
        return Ok(Vec::new());
    }
    let target_kinds = targets.iter().map(|(kind, _)| kind.clone()).collect::<Vec<_>>();
    let target_ids = targets.iter().map(|(_, id)| *id).collect::<Vec<_>>();

    sqlx::query_as::<_, RuntimeGraphEvidenceRow>(
        "with requested(target_kind, target_id, ordinal) as (
             select *
             from unnest($2::text[], $3::uuid[]) with ordinality
         )
         select evidence.id, evidence.library_id, evidence.target_kind, evidence.target_id,
            evidence.document_id, evidence.chunk_id, evidence.source_file_name,
            evidence.page_ref, evidence.evidence_text, evidence.confidence_score,
            evidence.created_at
         from requested
         join runtime_graph_evidence as evidence
           on evidence.library_id = $1
          and evidence.target_kind = requested.target_kind
          and evidence.target_id = requested.target_id
         order by requested.ordinal asc, evidence.created_at desc, evidence.id desc
         limit $4",
    )
    .bind(library_id)
    .bind(target_kinds)
    .bind(target_ids)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Searches runtime graph evidence bodies using the same active evidence table
/// that powers graph support. This complements target-based evidence lookup for
/// rare facts whose text is more discriminative than their node label.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the evidence rows.
pub async fn search_runtime_graph_evidence_by_text(
    pool: &PgPool,
    library_id: Uuid,
    query_texts: &[String],
    limit: i64,
) -> Result<Vec<RuntimeGraphEvidenceRow>, sqlx::Error> {
    if query_texts.is_empty() || limit <= 0 {
        return Ok(Vec::new());
    }
    let search_queries = runtime_graph_evidence_text_search_queries(query_texts);
    let literal_queries = runtime_graph_evidence_literal_search_queries(query_texts);
    if search_queries.is_empty() && literal_queries.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, RuntimeGraphEvidenceRow>(
        "with requested_text(search_query, ordinal) as (
             select search_query, ordinal::integer
             from unnest($2::text[]) with ordinality as request(search_query, ordinal)
         ),
         requested_text_query as (
             select
                search_query,
                ordinal,
                to_tsquery('simple', search_query) as ts_query
             from requested_text
         ),
         requested_literal(literal_query, ordinal) as (
             select literal_query, ordinal::integer
             from unnest($3::text[]) with ordinality as request(literal_query, ordinal)
         ),
         requested_literal_query as (
             select
                literal_query,
                ordinal,
                '%' || replace(
                    replace(replace(literal_query, '~', '~~'), '%', '~%'),
                    '_',
                    '~_'
                ) || '%' as literal_pattern
             from requested_literal
         ),
         text_matched as (
             select
                evidence.id,
                evidence.library_id,
                evidence.target_kind,
                evidence.target_id,
                evidence.document_id,
                evidence.chunk_id,
                evidence.source_file_name,
                evidence.page_ref,
                evidence.evidence_text,
                evidence.confidence_score,
                evidence.created_at,
                evidence.body_key,
                evidence.first_query_ordinal,
                evidence.body_match,
                evidence.literal_match,
                evidence.body_rank
             from requested_text_query
             cross join lateral (
                 select
                    evidence.id,
                    evidence.library_id,
                    evidence.target_kind,
                    evidence.target_id,
                    evidence.document_id,
                    evidence.chunk_id,
                    evidence.source_file_name,
                    evidence.page_ref,
                    evidence.evidence_text,
                    evidence.confidence_score,
                    evidence.created_at,
                    md5(lower(regexp_replace(btrim(evidence.evidence_text), '[[:space:]]+', ' ', 'g'))) as body_key,
                    requested_text_query.ordinal as first_query_ordinal,
                    true as body_match,
                    false as literal_match,
                    ts_rank_cd(
                        to_tsvector(
                            'simple',
                            evidence.evidence_text || ' ' || coalesce(evidence.source_file_name, '')
                        ),
                        requested_text_query.ts_query
                    ) as body_rank
                 from runtime_graph_evidence as evidence
                 where evidence.library_id = $1
                   and btrim(evidence.evidence_text) <> ''
                   and to_tsvector(
                        'simple',
                        evidence.evidence_text || ' ' || coalesce(evidence.source_file_name, '')
                   ) @@ requested_text_query.ts_query
                 order by
                    body_rank desc,
                    evidence.confidence_score desc nulls last,
                    evidence.created_at desc,
                    evidence.id desc
                 limit $4
             ) as evidence
         ),
         literal_matched as (
             select
                evidence.id,
                evidence.library_id,
                evidence.target_kind,
                evidence.target_id,
                evidence.document_id,
                evidence.chunk_id,
                evidence.source_file_name,
                evidence.page_ref,
                evidence.evidence_text,
                evidence.confidence_score,
                evidence.created_at,
                evidence.body_key,
                evidence.first_query_ordinal,
                evidence.body_match,
                evidence.literal_match,
                evidence.body_rank
             from requested_literal_query
             cross join lateral (
                 select
                    evidence.id,
                    evidence.library_id,
                    evidence.target_kind,
                    evidence.target_id,
                    evidence.document_id,
                    evidence.chunk_id,
                    evidence.source_file_name,
                    evidence.page_ref,
                    evidence.evidence_text,
                    evidence.confidence_score,
                    evidence.created_at,
                    md5(lower(regexp_replace(btrim(evidence.evidence_text), '[[:space:]]+', ' ', 'g'))) as body_key,
                    requested_literal_query.ordinal as first_query_ordinal,
                    false as body_match,
                    true as literal_match,
                    0::real as body_rank
                 from runtime_graph_evidence as evidence
                 where evidence.library_id = $1
                   and btrim(evidence.evidence_text) <> ''
                   and lower(evidence.evidence_text || ' ' || coalesce(evidence.source_file_name, ''))
                       like requested_literal_query.literal_pattern escape '~'
                 order by
                    evidence.confidence_score desc nulls last,
                    evidence.created_at desc,
                    evidence.id desc
                 limit $4
             ) as evidence
         ),
         matched as (
             select distinct on (evidence.id)
                evidence.id,
                evidence.library_id,
                evidence.target_kind,
                evidence.target_id,
                evidence.document_id,
                evidence.chunk_id,
                evidence.source_file_name,
                evidence.page_ref,
                evidence.evidence_text,
                evidence.confidence_score,
                evidence.created_at,
                evidence.body_key,
                evidence.first_query_ordinal,
                evidence.body_match,
                evidence.literal_match,
                evidence.body_rank
             from (
                 select * from text_matched
                 union all
                 select * from literal_matched
             ) as evidence
             order by
                evidence.id,
                evidence.first_query_ordinal asc,
                evidence.literal_match desc,
                evidence.body_rank desc,
                evidence.body_match desc
         ),
         deduped as (
             select distinct on (body_key)
                id,
                library_id,
                target_kind,
                target_id,
                document_id,
                chunk_id,
                source_file_name,
                page_ref,
                evidence_text,
                confidence_score,
                created_at,
                first_query_ordinal,
                body_match,
                literal_match,
                body_rank
             from matched
             order by
                body_key,
                first_query_ordinal asc,
                literal_match desc,
                body_rank desc,
                body_match desc,
                confidence_score desc nulls last,
                created_at desc,
                id desc
         )
         select
            id,
            library_id,
            target_kind,
            target_id,
            document_id,
            chunk_id,
            source_file_name,
            page_ref,
            evidence_text,
            confidence_score,
            created_at
         from deduped
         order by
            first_query_ordinal asc,
            literal_match desc,
            body_rank desc,
            body_match desc,
            confidence_score desc nulls last,
            created_at desc,
            id desc
         limit $4",
    )
    .bind(library_id)
    .bind(search_queries)
    .bind(literal_queries)
    .bind(limit)
    .fetch_all(pool)
    .await
}

fn runtime_graph_evidence_text_search_queries(query_texts: &[String]) -> Vec<String> {
    let mut seen_queries = BTreeSet::new();
    let mut token_windows_by_query = Vec::new();
    for query_text in query_texts {
        let tokens = runtime_graph_evidence_text_search_tokens(query_text);
        if !runtime_graph_evidence_text_search_tokens_are_selective(&tokens) {
            continue;
        }
        token_windows_by_query.push(runtime_graph_evidence_text_search_token_windows(&tokens));
    }

    let mut search_queries = Vec::new();
    let mut window_index = 0usize;
    loop {
        let mut saw_window = false;
        for token_windows in &token_windows_by_query {
            let Some(token_window) = token_windows.get(window_index) else {
                continue;
            };
            saw_window = true;
            let search_query = runtime_graph_evidence_text_search_query(token_window);
            if seen_queries.insert(search_query.clone()) {
                search_queries.push(search_query);
                if search_queries.len() >= RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_QUERY_CAP {
                    return search_queries;
                }
            }
        }
        if !saw_window {
            break;
        }
        window_index += 1;
    }
    search_queries
}

fn runtime_graph_evidence_literal_search_queries(query_texts: &[String]) -> Vec<String> {
    let mut seen_queries = BTreeSet::new();
    let mut queries = Vec::new();
    for query_text in query_texts {
        let normalized = query_text.split_whitespace().collect::<Vec<_>>().join(" ");
        if !runtime_graph_evidence_literal_search_query_is_selective(&normalized) {
            continue;
        }
        let query = normalized.to_lowercase();
        if seen_queries.insert(query.clone()) {
            queries.push(query);
            if queries.len() >= RUNTIME_GRAPH_EVIDENCE_LITERAL_SEARCH_QUERY_CAP {
                break;
            }
        }
    }
    queries
}

fn runtime_graph_evidence_literal_search_query_is_selective(query_text: &str) -> bool {
    let alphanumeric_count = query_text.chars().filter(|ch| ch.is_alphanumeric()).count();
    if alphanumeric_count < 4 {
        return false;
    }
    let char_count = query_text.chars().count();
    let tokens = normalized_alnum_token_sequence_by(
        query_text,
        |token| !token.trim().is_empty(),
        Some(RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_TOKEN_CAP),
    );
    let has_numeric =
        tokens.iter().any(|token| runtime_graph_evidence_text_search_token_has_numeric(token));
    let structural_separator_count =
        query_text.chars().filter(|ch| !ch.is_alphanumeric() && !ch.is_whitespace()).count();
    let has_structural_separator = structural_separator_count > 0;
    let token_count = tokens.len();

    let strict_short_name_phrase = !has_structural_separator
        && !has_numeric
        && token_count == 2
        && alphanumeric_count >= 6
        && alphanumeric_count <= 48
        && char_count <= 64
        && query_text
            .split_whitespace()
            .all(runtime_graph_evidence_literal_plain_token_has_name_shape);
    let short_numeric_phrase = has_numeric
        && token_count <= RUNTIME_GRAPH_EVIDENCE_LITERAL_SEARCH_SHORT_MAX_TOKENS
        && char_count <= 96;
    let short_structural_phrase = has_structural_separator
        && token_count <= RUNTIME_GRAPH_EVIDENCE_LITERAL_SEARCH_SHORT_MAX_TOKENS
        && char_count <= RUNTIME_GRAPH_EVIDENCE_LITERAL_SEARCH_SHORT_MAX_CHARS;
    let dense_structural_phrase = structural_separator_count >= 2
        && structural_separator_count.saturating_mul(8) >= alphanumeric_count
        && token_count <= RUNTIME_GRAPH_EVIDENCE_LITERAL_SEARCH_STRUCTURAL_MAX_TOKENS
        && char_count <= RUNTIME_GRAPH_EVIDENCE_LITERAL_SEARCH_STRUCTURAL_MAX_CHARS;

    strict_short_name_phrase
        || short_numeric_phrase
        || short_structural_phrase
        || dense_structural_phrase
}

fn runtime_graph_evidence_literal_plain_token_has_name_shape(token: &str) -> bool {
    let mut saw_first_cased = false;
    let mut first_cased_is_upper = false;
    let mut saw_later_lower = false;

    for ch in token.chars().filter(|ch| ch.is_alphabetic()) {
        if !saw_first_cased {
            saw_first_cased = ch.is_uppercase() || ch.is_lowercase();
            first_cased_is_upper = ch.is_uppercase();
            continue;
        }
        saw_later_lower |= ch.is_lowercase();
    }

    saw_first_cased && first_cased_is_upper && saw_later_lower
}

fn runtime_graph_evidence_text_search_query(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|token| {
            let prefix = runtime_graph_evidence_text_search_token_prefix(token);
            if runtime_graph_evidence_text_search_token_has_numeric(token) {
                format!("'{prefix}'")
            } else {
                format!("'{prefix}':*")
            }
        })
        .collect::<Vec<_>>()
        .join(" & ")
}

fn runtime_graph_evidence_text_search_token_prefix(token: &str) -> String {
    if runtime_graph_evidence_text_search_token_has_numeric(token) {
        return token.to_string();
    }
    let char_count = token.chars().count();
    if char_count <= RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_TOKEN_MIN_CHARS {
        return token.to_string();
    }
    let suffix_budget = if char_count >= 7 { 2 } else { 1 };
    let prefix_len = char_count
        .saturating_sub(suffix_budget)
        .max(RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_TOKEN_MIN_CHARS);
    token.chars().take(prefix_len).collect()
}

fn runtime_graph_evidence_text_search_token_windows(tokens: &[String]) -> Vec<Vec<String>> {
    let mut candidates = Vec::new();
    if tokens.len() <= RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_WINDOW_MAX_TOKENS {
        let full_window = tokens.to_vec();
        candidates.push((
            usize::MAX,
            0,
            full_window.clone(),
            runtime_graph_evidence_text_search_window_query(&full_window),
        ));
    } else if let Some(distinctive_window) =
        runtime_graph_evidence_text_search_distinctive_window(tokens)
    {
        candidates.push((
            usize::MAX,
            0,
            distinctive_window.clone(),
            runtime_graph_evidence_text_search_window_query(&distinctive_window),
        ));
    }

    for width in (RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_WINDOW_MIN_TOKENS
        ..=RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_WINDOW_MAX_TOKENS)
        .rev()
    {
        if width > tokens.len() {
            continue;
        }
        for start in 0..=tokens.len().saturating_sub(width) {
            let window = tokens[start..start + width].to_vec();
            if !runtime_graph_evidence_text_search_tokens_are_selective(&window) {
                continue;
            }
            let query = runtime_graph_evidence_text_search_window_query(&window);
            candidates.push((
                runtime_graph_evidence_text_search_window_score(&window),
                start,
                window,
                query,
            ));
        }
    }

    candidates.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));

    let mut seen_queries = BTreeSet::new();
    candidates
        .into_iter()
        .filter_map(|(_, _, window, query)| seen_queries.insert(query).then_some(window))
        .collect()
}

fn runtime_graph_evidence_text_search_distinctive_window(tokens: &[String]) -> Option<Vec<String>> {
    let mut indexed_tokens = tokens.iter().enumerate().collect::<Vec<_>>();
    indexed_tokens.sort_by(|left, right| {
        runtime_graph_evidence_text_search_token_score(right.1)
            .cmp(&runtime_graph_evidence_text_search_token_score(left.1))
            .then_with(|| left.0.cmp(&right.0))
    });
    let mut selected = indexed_tokens
        .into_iter()
        .take(RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_WINDOW_MAX_TOKENS)
        .collect::<Vec<_>>();
    selected.sort_by(|left, right| left.0.cmp(&right.0));
    let window = selected.into_iter().map(|(_, token)| token.clone()).collect::<Vec<_>>();
    runtime_graph_evidence_text_search_tokens_are_selective(&window).then_some(window)
}

fn runtime_graph_evidence_text_search_window_query(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|token| runtime_graph_evidence_text_search_token_prefix(token))
        .collect::<Vec<_>>()
        .join("\u{0}")
}

fn runtime_graph_evidence_text_search_window_score(tokens: &[String]) -> usize {
    let token_score = tokens
        .iter()
        .map(|token| runtime_graph_evidence_text_search_token_score(token))
        .sum::<usize>();
    let width_score =
        tokens.len().saturating_mul(RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_TOKEN_MIN_CHARS);
    token_score.saturating_add(width_score)
}

fn runtime_graph_evidence_text_search_token_score(token: &str) -> usize {
    let numeric_bonus = runtime_graph_evidence_text_search_token_has_numeric(token) as usize
        * RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_MIN_TOTAL_CHARS;
    token.chars().count().saturating_add(numeric_bonus)
}

fn runtime_graph_evidence_text_search_tokens(query_text: &str) -> Vec<String> {
    normalized_alnum_token_sequence_by(
        query_text,
        |token| {
            runtime_graph_evidence_text_search_token_has_numeric(token)
                || token.chars().count() >= RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_TOKEN_MIN_CHARS
        },
        Some(RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_TOKEN_CAP),
    )
}

fn runtime_graph_evidence_text_search_tokens_are_selective(tokens: &[String]) -> bool {
    if tokens.len() < 2 {
        return false;
    }
    if tokens.len() >= 3 {
        return true;
    }
    if tokens.iter().any(|token| runtime_graph_evidence_text_search_token_has_numeric(token)) {
        return true;
    }
    let total_chars = tokens.iter().map(|token| token.chars().count()).sum::<usize>();
    total_chars >= RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_MIN_TOTAL_CHARS
}

fn runtime_graph_evidence_text_search_token_has_numeric(token: &str) -> bool {
    token.chars().any(char::is_numeric)
}

/// Lists active runtime graph evidence lifecycle rows for one target.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the evidence rows.
pub async fn list_active_runtime_graph_evidence_lifecycle_by_target(
    pool: &PgPool,
    library_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
) -> Result<Vec<RuntimeGraphEvidenceLifecycleRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEvidenceLifecycleRow>(
        "select id, library_id, target_kind, target_id, document_id, revision_id,
            activated_by_attempt_id, chunk_id, source_file_name,
            page_ref, evidence_text, confidence_score, created_at
         from runtime_graph_evidence
         where library_id = $1
           and target_kind = $2
           and target_id = $3
         order by created_at desc, id desc",
    )
    .bind(library_id)
    .bind(target_kind)
    .bind(target_id)
    .fetch_all(pool)
    .await
}

/// Lists document-to-runtime-graph links for the active projection.
///
/// # Errors
/// Returns any `SQLx` error raised while querying document link rows.
pub async fn list_runtime_graph_document_links_by_library(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphDocumentLinkRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphDocumentLinkRow>(
        "with active_node_links as (
            select
                evidence.document_id,
                evidence.target_id as target_node_id,
                'entity'::text as target_node_type,
                'supports'::text as relation_type,
                count(*)::bigint as support_count
            from runtime_graph_evidence as evidence
            inner join content_document as document
                on document.id = evidence.document_id
               and document.deleted_at is null
            inner join runtime_graph_node as node
                on node.library_id = evidence.library_id
               and node.id = evidence.target_id
               and node.projection_version = $2
            where evidence.library_id = $1
              and evidence.target_kind = 'node'
              and evidence.document_id is not null
            group by evidence.document_id, evidence.target_id
        ),
        active_edge_links as (
            select
                evidence.document_id,
                evidence.target_id as target_node_id,
                'topic'::text as target_node_type,
                'supports'::text as relation_type,
                count(*)::bigint as support_count
            from runtime_graph_evidence as evidence
            inner join content_document as document
                on document.id = evidence.document_id
               and document.deleted_at is null
            inner join runtime_graph_edge as edge
                on edge.library_id = evidence.library_id
               and edge.id = evidence.target_id
               and edge.projection_version = $2
            where evidence.library_id = $1
              and evidence.target_kind = 'edge'
              and evidence.document_id is not null
            group by evidence.document_id, evidence.target_id
        )
        select document_id, target_node_id, target_node_type, relation_type, support_count
        from (
            select * from active_node_links
            union all
            select * from active_edge_links
        ) as links
        order by support_count desc, document_id asc, target_node_id asc",
    )
    .bind(library_id)
    .bind(projection_version)
    .fetch_all(pool)
    .await
}

/// Lists document-to-runtime-graph links for the active projection, filtered
/// to one visible set of target ids.
///
/// # Errors
/// Returns any `SQLx` error raised while querying filtered document links.
pub async fn list_runtime_graph_document_links_by_target_ids(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    target_ids: &[Uuid],
) -> Result<Vec<RuntimeGraphDocumentLinkRow>, sqlx::Error> {
    if target_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, RuntimeGraphDocumentLinkRow>(
        "with active_node_links as (
            select
                evidence.document_id,
                evidence.target_id as target_node_id,
                'entity'::text as target_node_type,
                'supports'::text as relation_type,
                count(*)::bigint as support_count
            from runtime_graph_evidence as evidence
            inner join content_document as document
                on document.id = evidence.document_id
               and document.deleted_at is null
            inner join runtime_graph_node as node
                on node.library_id = evidence.library_id
               and node.id = evidence.target_id
               and node.projection_version = $2
            where evidence.library_id = $1
              and evidence.target_kind = 'node'
              and evidence.document_id is not null
              and evidence.target_id = any($3)
            group by evidence.document_id, evidence.target_id
        ),
        active_edge_links as (
            select
                evidence.document_id,
                evidence.target_id as target_node_id,
                'topic'::text as target_node_type,
                'supports'::text as relation_type,
                count(*)::bigint as support_count
            from runtime_graph_evidence as evidence
            inner join content_document as document
                on document.id = evidence.document_id
               and document.deleted_at is null
            inner join runtime_graph_edge as edge
                on edge.library_id = evidence.library_id
               and edge.id = evidence.target_id
               and edge.projection_version = $2
            where evidence.library_id = $1
              and evidence.target_kind = 'edge'
              and evidence.document_id is not null
              and evidence.target_id = any($3)
            group by evidence.document_id, evidence.target_id
        )
        select document_id, target_node_id, target_node_type, relation_type, support_count
        from (
            select * from active_node_links
            union all
            select * from active_edge_links
        ) as links
        order by support_count desc, document_id asc, target_node_id asc",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(target_ids)
    .fetch_all(pool)
    .await
}

/// Deletes graph evidence for the just-deleted document and self-heals any
/// orphan rows still surviving in the library.
///
/// Canonical contract: every `runtime_graph_evidence` row points at an
/// active `content_document`. The single-doc cleanup explicitly removes the
/// just-deleted doc's rows AND sweeps any rows in the same library whose
/// `document_id` is null (FK `ON DELETE SET NULL` legacy debris) or whose
/// referenced document is in `deleted` state — for example, evidence rows
/// stranded by an earlier delete whose graph-refresh failed soft and never
/// retried. Without this sweep those rows keep nodes alive forever via the
/// support-count recalculation.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the evidence rows.
pub async fn deactivate_runtime_graph_evidence_by_document(
    pool: &PgPool,
    library_id: Uuid,
    document_id: Uuid,
) -> Result<Vec<RuntimeGraphEvidenceTargetRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEvidenceTargetRow>(
        "delete from runtime_graph_evidence as evidence
         where evidence.library_id = $1
           and (
             evidence.document_id = $2
             or evidence.document_id is null
             or exists (
                 select 1 from content_document as document
                 where document.id = evidence.document_id
                   and document.library_id = $1
                   and (document.document_state = 'deleted' or document.deleted_at is not null)
             )
           )
         returning target_kind, target_id",
    )
    .bind(library_id)
    .bind(document_id)
    .fetch_all(pool)
    .await
}

/// Deletes graph evidence for a batch of just-deleted documents and self-heals
/// any orphan rows still surviving in the library.
///
/// Same canonical contract as [`deactivate_runtime_graph_evidence_by_document`]:
/// the orphan sweep makes batch delete idempotent against past failures so a
/// once-stranded document cannot keep its supported nodes/edges visible.
pub async fn deactivate_runtime_graph_evidence_by_documents(
    pool: &PgPool,
    library_id: Uuid,
    document_ids: &[Uuid],
) -> Result<Vec<RuntimeGraphEvidenceTargetRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEvidenceTargetRow>(
        "delete from runtime_graph_evidence as evidence
         where evidence.library_id = $1
           and (
             evidence.document_id = any($2)
             or evidence.document_id is null
             or exists (
                 select 1 from content_document as document
                 where document.id = evidence.document_id
                   and document.library_id = $1
                   and (document.document_state = 'deleted' or document.deleted_at is not null)
             )
           )
         returning target_kind, target_id",
    )
    .bind(library_id)
    .bind(document_ids)
    .fetch_all(pool)
    .await
}

/// Lists document graph nodes for soft-deleted documents, including nodes
/// created before evidence was flushed.
///
/// Failed graph rebuilds can leave the source-document node without a
/// corresponding `runtime_graph_evidence` row. Delete convergence still must
/// target that node so the file leaves no graph trace.
///
/// Returns the document-typed nodes for the explicit `document_ids` AND any
/// document-typed node in the library whose backing `content_document` is in
/// `deleted` state. The latter self-heals stranded nodes from previously
/// failed cleanup runs.
pub async fn list_runtime_graph_document_node_targets_by_documents(
    pool: &PgPool,
    library_id: Uuid,
    document_ids: &[Uuid],
) -> Result<Vec<RuntimeGraphEvidenceTargetRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEvidenceTargetRow>(
        "select 'node'::text as target_kind, node.id as target_id
         from runtime_graph_node as node
         where node.library_id = $1
           and node.node_type = 'document'
           and (
             node.document_id = any($2)
             or exists (
                 select 1 from content_document as document
                 where document.id = node.document_id
                   and document.library_id = $1
                   and (document.document_state = 'deleted' or document.deleted_at is not null)
             )
           )",
    )
    .bind(library_id)
    .bind(document_ids)
    .fetch_all(pool)
    .await
}

/// Deletes `runtime_graph_canonical_summary` rows whose target node or edge no
/// longer exists in the canonical graph tables.
///
/// `runtime_graph_canonical_summary` has no FK back to `runtime_graph_node` /
/// `runtime_graph_edge`, so node/edge prune does not cascade. Without this
/// sweep, deleted libraries accumulate stale summary rows that drift from the
/// graph projection (cf. the 15k summary / 27 node skew observed on prod
/// after batch delete).
///
/// The query is bounded by the candidate `node_ids` / `edge_ids` set returned
/// from the pruning pass, so it touches at most one row per pruned target.
///
/// # Errors
/// Returns any `SQLx` error raised while removing canonical summary rows.
pub async fn delete_runtime_graph_canonical_summaries_for_orphan_targets(
    pool: &PgPool,
    library_id: Uuid,
    node_ids: &[Uuid],
    edge_ids: &[Uuid],
) -> Result<u64, sqlx::Error> {
    if node_ids.is_empty() && edge_ids.is_empty() {
        return Ok(0);
    }
    let result = sqlx::query(
        "delete from runtime_graph_canonical_summary as summary
         where summary.library_id = $1
           and (
             (
                summary.target_kind = 'node'
                and summary.target_id = any($2)
                and not exists (
                    select 1 from runtime_graph_node as node
                    where node.id = summary.target_id
                )
             )
             or (
                summary.target_kind = 'edge'
                and summary.target_id = any($3)
                and not exists (
                    select 1 from runtime_graph_edge as edge
                    where edge.id = summary.target_id
                )
             )
           )",
    )
    .bind(library_id)
    .bind(node_ids)
    .bind(edge_ids)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Lists active graph evidence rows for one logical content revision.
///
/// # Errors
/// Returns any `SQLx` error raised while querying revision-scoped evidence rows.
pub async fn list_active_runtime_graph_evidence_by_content_revision(
    pool: &PgPool,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
) -> Result<Vec<RuntimeGraphEvidenceLifecycleRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEvidenceLifecycleRow>(
        "select id, library_id, target_kind, target_id, document_id, revision_id,
            activated_by_attempt_id, chunk_id, source_file_name,
            page_ref, evidence_text, confidence_score, created_at
         from runtime_graph_evidence
         where library_id = $1
           and document_id = $2
           and (revision_id = $3 or revision_id is null)
         order by created_at desc, id desc",
    )
    .bind(library_id)
    .bind(document_id)
    .bind(revision_id)
    .fetch_all(pool)
    .await
}

/// Lists target ids that still have active evidence outside one content revision.
///
/// # Errors
/// Returns any `SQLx` error raised while querying surviving evidence lineage.
pub async fn list_active_runtime_graph_target_ids_excluding_content_revision(
    pool: &PgPool,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    target_kind: &str,
    target_ids: &[Uuid],
) -> Result<Vec<Uuid>, sqlx::Error> {
    if target_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_scalar::<_, Uuid>(
        "select distinct target_id
         from runtime_graph_evidence
         where library_id = $1
           and target_kind = $4
           and target_id = any($5)
           and not (
                document_id = $2
                and (revision_id = $3 or revision_id is null)
           )
         order by target_id asc",
    )
    .bind(library_id)
    .bind(document_id)
    .bind(revision_id)
    .bind(target_kind)
    .bind(target_ids)
    .fetch_all(pool)
    .await
}

/// Marks active graph evidence for one logical content revision as inactive.
///
/// # Errors
/// Returns any `SQLx` error raised while updating revision-scoped evidence rows.
pub async fn deactivate_runtime_graph_evidence_by_content_revision(
    pool: &PgPool,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "delete from runtime_graph_evidence
         where library_id = $1
           and document_id = $2
           and (revision_id = $3 or revision_id is null)",
    )
    .bind(library_id)
    .bind(document_id)
    .bind(revision_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Recalculates graph node/edge support counters from surviving active evidence.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the canonical graph rows.
pub const RECALCULATE_RUNTIME_GRAPH_NODE_SUPPORT_COUNTS_SQL: &str = "with target_nodes as (
            select id, support_count
            from runtime_graph_node
            where library_id = $1
              and projection_version = $2
         ),
         evidence_counts as (
            select evidence.target_id, count(*)::int as support_count
            from runtime_graph_evidence as evidence
            join content_document as document
              on document.id = evidence.document_id
             and document.library_id = $1
             and document.document_state = 'active'
             and document.deleted_at is null
            where evidence.library_id = $1
              and evidence.target_kind = 'node'
            group by evidence.target_id
         ),
         desired_counts as (
            select target_nodes.id,
                   coalesce(evidence_counts.support_count, 0) as support_count
            from target_nodes
            left join evidence_counts on evidence_counts.target_id = target_nodes.id
         )
         update runtime_graph_node as node
         set support_count = desired_counts.support_count,
             updated_at = now()
         from desired_counts
         where node.id = desired_counts.id
           and node.support_count is distinct from desired_counts.support_count";

pub const RECALCULATE_RUNTIME_GRAPH_EDGE_SUPPORT_COUNTS_SQL: &str = "with target_edges as (
            select id, support_count
            from runtime_graph_edge
            where library_id = $1
              and projection_version = $2
         ),
         evidence_counts as (
            select evidence.target_id, count(*)::int as support_count
            from runtime_graph_evidence as evidence
            join content_document as document
              on document.id = evidence.document_id
             and document.library_id = $1
             and document.document_state = 'active'
             and document.deleted_at is null
            where evidence.library_id = $1
              and evidence.target_kind = 'edge'
            group by evidence.target_id
         ),
         desired_counts as (
            select target_edges.id,
                   coalesce(evidence_counts.support_count, 0) as support_count
            from target_edges
            left join evidence_counts on evidence_counts.target_id = target_edges.id
         )
         update runtime_graph_edge as edge
         set support_count = desired_counts.support_count,
             updated_at = now()
         from desired_counts
         where edge.id = desired_counts.id
           and edge.support_count is distinct from desired_counts.support_count";

pub async fn recalculate_runtime_graph_support_counts(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(RECALCULATE_RUNTIME_GRAPH_NODE_SUPPORT_COUNTS_SQL)
        .bind(library_id)
        .bind(projection_version)
        .execute(pool)
        .await?;

    sqlx::query(RECALCULATE_RUNTIME_GRAPH_EDGE_SUPPORT_COUNTS_SQL)
        .bind(library_id)
        .bind(projection_version)
        .execute(pool)
        .await?;

    Ok(())
}

/// Deletes canonical graph edges with zero surviving active evidence and returns their canonical keys.
///
/// # Errors
/// Returns any `SQLx` error raised while pruning unsupported graph edges.
pub async fn delete_runtime_graph_edges_without_support(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        "delete from runtime_graph_edge
         where library_id = $1
           and projection_version = $2
           and support_count <= 0
         returning canonical_key",
    )
    .bind(library_id)
    .bind(projection_version)
    .fetch_all(pool)
    .await
}

/// Deletes targeted canonical graph edges with zero surviving active evidence.
///
/// # Errors
/// Returns any `SQLx` error raised while pruning unsupported graph edges.
pub async fn delete_runtime_graph_edges_without_support_by_ids(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    edge_ids: &[Uuid],
) -> Result<Vec<String>, sqlx::Error> {
    if edge_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_scalar::<_, String>(
        "delete from runtime_graph_edge
         where library_id = $1
           and projection_version = $2
           and id = any($3)
           and support_count <= 0
         returning canonical_key",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(edge_ids)
    .fetch_all(pool)
    .await
}

/// Deletes canonical graph nodes with zero surviving active evidence and returns their canonical keys.
///
/// # Errors
/// Returns any `SQLx` error raised while pruning unsupported graph nodes.
pub async fn delete_runtime_graph_nodes_without_support(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        "delete from runtime_graph_node
         where library_id = $1
           and projection_version = $2
           and support_count <= 0
         returning canonical_key",
    )
    .bind(library_id)
    .bind(projection_version)
    .fetch_all(pool)
    .await
}

/// Deletes targeted canonical graph nodes with zero surviving active evidence.
///
/// # Errors
/// Returns any `SQLx` error raised while pruning unsupported graph nodes.
pub async fn delete_runtime_graph_nodes_without_support_by_ids(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    node_ids: &[Uuid],
) -> Result<Vec<String>, sqlx::Error> {
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_scalar::<_, String>(
        "delete from runtime_graph_node
         where library_id = $1
           and projection_version = $2
           and id = any($3)
           and support_count <= 0
         returning canonical_key",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(node_ids)
    .fetch_all(pool)
    .await
}

/// Counts admitted canonical graph nodes and relationships for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the canonical graph counts.
pub async fn count_admitted_runtime_graph_projection(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<RuntimeGraphProjectionCountsRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphProjectionCountsRow>(&admitted_runtime_graph_counts_query())
        .bind(library_id)
        .bind(projection_version)
        .fetch_one(pool)
        .await
}

fn admitted_runtime_graph_nodes_query(extra_filter: &str) -> String {
    format!(
        "with admitted_edges as (
            select edge.from_node_id, edge.to_node_id
            from runtime_graph_edge as edge
            where edge.library_id = $1
              and edge.projection_version = $2
              and btrim(edge.relation_type) <> ''
              and edge.from_node_id <> edge.to_node_id
         ),
         admitted_edge_endpoints as (
            select admitted_edges.from_node_id as node_id
            from admitted_edges
            union
            select admitted_edges.to_node_id as node_id
            from admitted_edges
         )
         select node.id, node.library_id, node.canonical_key, node.label, node.node_type,
            node.aliases_json, node.summary, node.metadata_json, node.support_count,
            node.projection_version, node.created_at, node.updated_at
         from runtime_graph_node as node
         left join admitted_edge_endpoints as admitted on admitted.node_id = node.id
         where node.library_id = $1
           and node.projection_version = $2
           {extra_filter}
           and (
                node.node_type = 'document'
                or admitted.node_id is not null
           )
         order by node.node_type asc, node.label asc, node.created_at asc, node.id asc"
    )
}

/// Searches `runtime_graph_node` by keyword overlap against graph node data.
///
/// Words shorter than 3 characters are ignored to avoid noise. Returns up to
/// `limit` non-document nodes ordered by `support_count` descending. The match
/// surface is deliberately limited to data already attached to the node: label,
/// canonical node type, summary, and extracted aliases.
///
/// # Errors
/// Returns any `SQLx` error raised during the query.
pub async fn search_runtime_graph_nodes_by_query_text(
    pool: &PgPool,
    library_id: Uuid,
    query_text: &str,
    limit: i64,
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "select
            n.id, n.library_id, n.canonical_key, n.label, n.node_type,
            n.aliases_json, n.summary, n.metadata_json, n.support_count,
            n.projection_version, n.created_at, n.updated_at
         from runtime_graph_node n
         where n.library_id = $1
           and n.node_type <> 'document'
           and exists (
               select 1 from unnest(string_to_array(lower($2), ' ')) as word
               where length(trim(word)) > 2
                 and (
                    lower(n.label) like '%' || trim(word) || '%'
                    or lower(n.node_type) like '%' || trim(word) || '%'
                    or coalesce(lower(n.summary), '') like '%' || trim(word) || '%'
                    or exists (
                        select 1
                        from jsonb_array_elements_text(n.aliases_json) as alias(value)
                        where lower(alias.value) like '%' || trim(word) || '%'
                    )
                 )
           )
         order by n.support_count desc, n.label asc, n.id asc
         limit $3",
    )
    .bind(library_id)
    .bind(query_text)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Searches admitted runtime graph entities for one projection version using
/// label, aliases, and summary text.
///
/// Exact label matches rank above prefix and substring matches; ties break on
/// `support_count` descending so the strongest canonical entity wins.
///
/// # Errors
/// Returns any `SQLx` error raised during the query.
pub async fn search_admitted_runtime_graph_entities_by_query_text(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    query_text: &str,
    limit: i64,
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error> {
    let normalized_query = query_text.trim().to_lowercase();
    if normalized_query.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "select
            n.id, n.library_id, n.canonical_key, n.label, n.node_type,
            n.aliases_json, n.summary, n.metadata_json, n.support_count,
            n.projection_version, n.created_at, n.updated_at
         from runtime_graph_node n
         where n.library_id = $1
           and n.projection_version = $2
           and n.node_type <> 'document'
           and (
                lower(n.label) like '%' || $3 || '%'
                or coalesce(lower(n.summary), '') like '%' || $3 || '%'
                or exists (
                    select 1
                    from jsonb_array_elements_text(n.aliases_json) as alias(value)
                    where lower(alias.value) like '%' || $3 || '%'
                )
                or exists (
                    select 1
                    from unnest(string_to_array($3, ' ')) as word
                    where length(word) > 2
                      and (
                            lower(n.label) like '%' || word || '%'
                            or coalesce(lower(n.summary), '') like '%' || word || '%'
                            or exists (
                                select 1
                                from jsonb_array_elements_text(n.aliases_json) as alias(value)
                                where lower(alias.value) like '%' || word || '%'
                            )
                      )
                )
           )
         order by
            case
                when lower(n.label) = $3 then 0
                when exists (
                    select 1
                    from jsonb_array_elements_text(n.aliases_json) as alias(value)
                    where lower(alias.value) = $3
                ) then 1
                when lower(n.label) like $3 || '%' then 2
                when exists (
                    select 1
                    from jsonb_array_elements_text(n.aliases_json) as alias(value)
                    where lower(alias.value) like $3 || '%'
                ) then 3
                when lower(n.label) like '%' || $3 || '%' then 4
                when exists (
                    select 1
                    from jsonb_array_elements_text(n.aliases_json) as alias(value)
                    where lower(alias.value) like '%' || $3 || '%'
                ) then 5
                when coalesce(lower(n.summary), '') like '%' || $3 || '%' then 6
                else 7
            end asc,
            n.support_count desc,
            n.label asc,
            n.created_at asc
         limit $4",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(normalized_query)
    .bind(limit)
    .fetch_all(pool)
    .await
}

fn admitted_runtime_graph_counts_query() -> String {
    "with admitted_edges as (
        select edge.id, edge.from_node_id, edge.to_node_id
        from runtime_graph_edge as edge
        where edge.library_id = $1
          and edge.projection_version = $2
          and btrim(edge.relation_type) <> ''
          and edge.from_node_id <> edge.to_node_id
     ),
     admitted_edge_endpoints as (
        select admitted_edges.from_node_id as node_id
        from admitted_edges
        union
        select admitted_edges.to_node_id as node_id
        from admitted_edges
     )
     select
        (
            select count(*)
            from runtime_graph_node as node
            left join admitted_edge_endpoints as admitted on admitted.node_id = node.id
            where node.library_id = $1
              and node.projection_version = $2
              and (
                    node.node_type = 'document'
                    or admitted.node_id is not null
              )
        ) as node_count,
        (
            select count(*)
            from admitted_edges
        ) as edge_count"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        runtime_graph_evidence_literal_search_queries, runtime_graph_evidence_text_search_queries,
        runtime_graph_evidence_text_search_token_prefix, runtime_graph_evidence_text_search_tokens,
    };

    #[test]
    fn evidence_text_search_tokens_keep_structural_literals_without_language_lists() {
        let tokens = runtime_graph_evidence_text_search_tokens(
            "Open alpha/report://needle?fontSize=12 and alpha.port=9407",
        );

        assert_eq!(
            tokens,
            vec![
                "open".to_string(),
                "alpha".to_string(),
                "report".to_string(),
                "needle".to_string(),
                "fontsize".to_string(),
                "12".to_string(),
                "port".to_string(),
                "9407".to_string(),
            ],
        );
    }

    #[test]
    fn evidence_text_search_query_uses_selective_suffix_tolerant_windows() {
        let queries = runtime_graph_evidence_text_search_queries(&[
            "Which parameter links Alpha Module to control service?".to_string(),
            "Alpha Module".to_string(),
            "Alpha".to_string(),
            "port 9407".to_string(),
            "Which parameter links Alpha Module to control service?".to_string(),
        ]);

        assert_eq!(
            queries.first().map(String::as_str),
            Some("'paramet':* & 'modul':* & 'contr':* & 'servi':*"),
        );
        assert_eq!(queries.get(1).map(String::as_str), Some("'alph':* & 'modul':*"));
        assert_eq!(queries.get(2).map(String::as_str), Some("'port':* & '9407'"));
        assert!(queries.contains(&"'alph':* & 'modul':*".to_string()));
        assert!(queries.contains(&"'port':* & '9407'".to_string()));
        assert!(!queries.iter().any(|query| {
            query.contains("'which':* & 'paramet':* & 'link':* & 'alph':* & 'modul':* & 'contr':*")
        }));
    }

    #[test]
    fn evidence_literal_search_queries_keep_exact_structural_spans() {
        let queries = runtime_graph_evidence_literal_search_queries(&[
            "Alpha".to_string(),
            "Mono Sans".to_string(),
            "alpha/report://needle?fontSize=12".to_string(),
            "report://detail?out=display&title=Alpha%20Report%20%(shift.num[d])&font=Mono%20Sans&fontSize=12".to_string(),
            "port 80".to_string(),
        ]);

        assert!(!queries.contains(&"alpha".to_string()));
        assert!(queries.contains(&"mono sans".to_string()));
        assert!(queries.contains(&"alpha/report://needle?fontsize=12".to_string()));
        assert!(queries.contains(
            &"report://detail?out=display&title=alpha%20report%20%(shift.num[d])&font=mono%20sans&fontsize=12".to_string()
        ));
        assert!(queries.contains(&"port 80".to_string()));
    }

    #[test]
    fn evidence_literal_search_queries_reject_long_prose_without_dense_structure() {
        let queries = runtime_graph_evidence_literal_search_queries(&[
            "Find the configuration paragraph that explains how the terminal connects to the control service, include the source document, and keep this cache marker 2026-05-01.".to_string(),
            "Which rare entity describes the escalation recipient, what fields are required in the message, and where is the source mentioned?".to_string(),
            "recent project".to_string(),
            "meeting notes".to_string(),
            "Alpha Module".to_string(),
        ]);

        assert_eq!(queries, vec!["alpha module".to_string()]);
    }

    #[test]
    fn evidence_text_search_token_prefix_preserves_numeric_literals() {
        assert_eq!(runtime_graph_evidence_text_search_token_prefix("alpha"), "alph");
        assert_eq!(runtime_graph_evidence_text_search_token_prefix("module"), "modul");
        assert_eq!(runtime_graph_evidence_text_search_token_prefix("alphacases"), "alphacas");
        assert_eq!(runtime_graph_evidence_text_search_token_prefix("9407"), "9407");
        assert_eq!(runtime_graph_evidence_text_search_token_prefix("build42"), "build42");
    }

    #[test]
    fn evidence_text_search_tokens_keep_short_numeric_literals_exact() {
        let tokens = runtime_graph_evidence_text_search_tokens("port 80 status 404 build42");
        let queries = runtime_graph_evidence_text_search_queries(&["port 80".to_string()]);

        assert_eq!(
            tokens,
            vec![
                "port".to_string(),
                "80".to_string(),
                "status".to_string(),
                "404".to_string(),
                "build42".to_string(),
            ],
        );
        assert_eq!(queries, vec!["'port':* & '80'".to_string()]);
    }

    #[test]
    fn evidence_text_search_query_includes_short_needle_windows() {
        let queries = runtime_graph_evidence_text_search_queries(&[
            "alphacases betagamma deltazeta epsilonkey zetaport thetakey".to_string(),
        ]);

        assert!(queries.contains(&"'deltaze':* & 'epsilonk':*".to_string()));
        assert!(queries.contains(&"'alphacas':* & 'betagam':*".to_string()));
    }

    #[test]
    fn evidence_text_search_query_expands_short_phrases_with_subwindows() {
        let queries = runtime_graph_evidence_text_search_queries(&[
            "alphacases betagamma deltazeta epsilonkey".to_string(),
        ]);

        assert_eq!(
            queries.first().map(String::as_str),
            Some("'alphacas':* & 'betagam':* & 'deltaze':* & 'epsilonk':*"),
        );
        assert!(queries.contains(&"'betagam':* & 'deltaze':*".to_string()));
        assert!(queries.contains(&"'deltaze':* & 'epsilonk':*".to_string()));
    }
}
