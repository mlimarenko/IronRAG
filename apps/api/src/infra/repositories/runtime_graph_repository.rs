#![allow(clippy::missing_errors_doc, clippy::too_many_arguments, clippy::too_many_lines)]

mod coordination;
mod snapshot;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::RuntimeGraphFilteredArtifactRow;

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
                   and n.canonical_key = 'document:' || h.document_id::text
           )
           and not exists (
                select 1 from runtime_graph_extraction e
                 where e.document_id = h.document_id
                   and (e.raw_output_json->'lifecycle'->>'revision_id')::uuid
                       = h.active_revision_id
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
                   and n.canonical_key = 'document:' || h.document_id::text
           )
           and exists (
                select 1 from runtime_graph_extraction e
                 where e.document_id = h.document_id
                   and e.status = 'ready'
                   and (e.raw_output_json->'lifecycle'->>'revision_id')::uuid
                       = h.active_revision_id
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
         order by support_count desc, label asc, created_at asc
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
         order by relation_type asc, created_at asc",
    )
    .bind(library_id)
    .bind(projection_version)
        .fetch_all(pool)
        .await
}

/// Compact edge row — only the columns consumed by the NDJSON topology
/// (`build_compact_topology` in `services/knowledge/graph_stream.rs`).
/// Dropping the wide columns (`summary`, `canonical_key`, `metadata_json`,
/// `weight`, timestamps) cuts the row width ~5× and the heap-fetch cost
/// accordingly: a reference library with 155 k edges used to spend
/// ~5.7 s just materialising the wide rows; the slim variant returns
/// the same 155 k rows in ~1.5 s because Postgres can stream directly
/// from the `idx_runtime_graph_edge_library_projection_nodes` leaf
/// pages without a separate heap touch for the JSON payloads.
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
         order by relation_type asc, support_count desc",
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
         order by node_type asc, label asc, created_at asc",
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
         order by edge.support_count desc, edge.relation_type asc, edge.created_at asc
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
         order by relation_type asc, created_at asc",
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
         order by relation_type asc, created_at asc",
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
    aliases_json: serde_json::Value,
    summary: Option<&str>,
    metadata_json: serde_json::Value,
    support_count: i32,
    projection_version: i64,
) -> Result<RuntimeGraphNodeRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "insert into runtime_graph_node (
            id, library_id, canonical_key, label, node_type, aliases_json, summary,
            metadata_json, support_count, projection_version
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
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
    .bind(Uuid::now_v7())
    .bind(library_id)
    .bind(canonical_key)
    .bind(label)
    .bind(node_type)
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
         order by node_type asc, label asc, created_at asc",
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
         order by relation_type asc, created_at asc",
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
         order by created_at desc",
    )
    .bind(library_id)
    .bind(target_kind)
    .bind(target_id)
    .fetch_all(pool)
    .await
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
         order by created_at desc",
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

/// Marks all active graph evidence for one document as inactive.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the evidence rows.
pub async fn deactivate_runtime_graph_evidence_by_document(
    pool: &PgPool,
    library_id: Uuid,
    document_id: Uuid,
) -> Result<Vec<RuntimeGraphEvidenceTargetRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEvidenceTargetRow>(
        "delete from runtime_graph_evidence
         where library_id = $1 and document_id = $2
         returning target_kind, target_id",
    )
    .bind(library_id)
    .bind(document_id)
    .fetch_all(pool)
    .await
}

/// Marks all graph evidence for a set of documents as inactive.
///
/// Batch delete runs this after child deletes as a final guard against
/// evidence admitted by work that was already in flight when deletion began.
pub async fn deactivate_runtime_graph_evidence_by_documents(
    pool: &PgPool,
    library_id: Uuid,
    document_ids: &[Uuid],
) -> Result<Vec<RuntimeGraphEvidenceTargetRow>, sqlx::Error> {
    if document_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, RuntimeGraphEvidenceTargetRow>(
        "delete from runtime_graph_evidence
         where library_id = $1
           and document_id = any($2)
         returning target_kind, target_id",
    )
    .bind(library_id)
    .bind(document_ids)
    .fetch_all(pool)
    .await
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
         order by created_at desc",
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
         order by node.node_type asc, node.label asc, node.created_at asc"
    )
}

/// Searches `runtime_graph_node` by keyword overlap against the node label.
///
/// Words shorter than 4 characters are ignored to avoid noise. Returns up to
/// `limit` nodes ordered by `support_count` descending.
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
        "select distinct on (n.id)
            n.id, n.library_id, n.canonical_key, n.label, n.node_type,
            n.aliases_json, n.summary, n.metadata_json, n.support_count,
            n.projection_version, n.created_at, n.updated_at
         from runtime_graph_node n
         where n.library_id = $1
           and n.node_type in ('entity', 'topic')
           and exists (
               select 1 from unnest(string_to_array(lower($2), ' ')) as word
               where length(word) > 3
                 and lower(n.label) like '%' || word || '%'
           )
         order by n.id, n.support_count desc
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
