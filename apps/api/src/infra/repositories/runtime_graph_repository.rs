mod coordination;
mod snapshot;

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::RuntimeGraphFilteredArtifactRow;
use crate::shared::text_tokens::{literal_wildcard_prefixes, normalized_alnum_token_sequence_by};

pub use coordination::*;
pub use snapshot::*;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, utoipa::ToSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, utoipa::ToSchema)]
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

/// Lean projection of a graph node for adjacency-only consumers (community
/// detection / label propagation). Holds only the canonical id — no heavy
/// text columns (`summary`, `label`, `canonical_key`, JSON blobs) — so the
/// in-RAM footprint per row stays at 16 bytes instead of multi-KB.
///
/// See `list_runtime_graph_node_ids_by_library`.
#[derive(Debug, Clone, Copy, FromRow)]
pub struct RuntimeGraphNodeIdRow {
    pub id: Uuid,
}

/// Lean projection of a graph edge for adjacency-only consumers. Holds only
/// the columns label-propagation actually reads (`from`, `to`,
/// `support_count`) so large libraries avoid materializing heavy edge payloads
/// in memory.
///
/// See `list_runtime_graph_edges_adjacency_by_library`.
#[derive(Debug, Clone, Copy, FromRow)]
pub struct RuntimeGraphEdgeAdjacencyRow {
    pub from_node_id: Uuid,
    pub to_node_id: Uuid,
    pub support_count: i32,
}

/// Slim node row for the query-time graph index. Drops `metadata_json`
/// (the largest per-row allocation — a `serde_json::Value` heap object
/// that query-time callers never read) while keeping every other column
/// that `graph_retrieval.rs`, `retrieve.rs`, and `context.rs` actually
/// access: `id`, `library_id`, `canonical_key`, `label`, `node_type`,
/// `aliases_json`, `summary`, `support_count`, `projection_version`,
/// `created_at`, `updated_at`.
///
/// On a large corpus this drops one `serde_json::Value` heap allocation per
/// node on every cache-miss load. Per-row savings depend on the average
/// `metadata_json` payload size; typical `sub_type` objects are 50–300 bytes,
/// so at six-figure node counts this saves on the order of tens of MB for
/// nodes alone.
///
/// See `list_runtime_graph_query_nodes_by_ids_or_document_type`.
#[derive(Debug, Clone, FromRow)]
pub struct RuntimeGraphQueryNodeRow {
    pub id: Uuid,
    pub library_id: Uuid,
    pub canonical_key: String,
    pub label: String,
    pub node_type: String,
    pub aliases_json: serde_json::Value,
    pub summary: Option<String>,
    pub support_count: i32,
    pub projection_version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Slim edge row for the query-time graph index. Drops `metadata_json`
/// and `canonical_key` — neither is accessed by any query-time caller
/// (`graph_retrieval.rs`, `retrieve.rs`, `context.rs`). The remaining
/// columns cover all accesses: `id`, `library_id`, `from_node_id`,
/// `to_node_id`, `relation_type`, `summary`, `weight`, `support_count`,
/// `projection_version`, `created_at`, `updated_at`.
///
/// On a large corpus this drops one `serde_json::Value` heap allocation plus
/// the `canonical_key` String per edge. Savings scale with edge count and
/// `metadata_json` population density — on the order of tens to a couple
/// hundred MB at six-figure edge counts.
///
/// See `list_admitted_runtime_graph_query_edges_by_library`.
#[derive(Debug, Clone, FromRow)]
pub struct RuntimeGraphQueryEdgeRow {
    pub id: Uuid,
    pub library_id: Uuid,
    pub from_node_id: Uuid,
    pub to_node_id: Uuid,
    pub relation_type: String,
    pub summary: Option<String>,
    pub weight: Option<f64>,
    pub support_count: i32,
    pub projection_version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, utoipa::ToSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, utoipa::ToSchema)]
pub struct RuntimeGraphEvidenceTargetRow {
    pub target_kind: String,
    pub target_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, utoipa::ToSchema)]
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

const RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_QUERY_CAP: usize = 6;
const RUNTIME_GRAPH_EVIDENCE_LITERAL_SEARCH_QUERY_CAP: usize = 8;
const RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_TOKEN_CAP: usize = 16;
const RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_TOKEN_MIN_CHARS: usize = 4;
const RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_MIN_TOTAL_CHARS: usize = 11;
const RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_WINDOW_MIN_TOKENS: usize = 2;
const RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_WINDOW_MAX_TOKENS: usize = 4;
const RUNTIME_GRAPH_EVIDENCE_LITERAL_SEARCH_MAX_TOKENS: usize = 20;
const RUNTIME_GRAPH_EVIDENCE_LITERAL_SEARCH_MAX_CHARS: usize = 220;
const RUNTIME_GRAPH_ENTITY_SEARCH_TOKEN_CAP: usize = 8;
const RUNTIME_GRAPH_ENTITY_SEARCH_TOKEN_MIN_CHARS: usize = 3;

/// A graph-evidence query with explicit lane provenance.
///
/// Natural-language questions stay in the full-text lexical lane. Only values
/// emitted by the typed query compiler (or by a formal-syntax parser) may use
/// the literal `LIKE` lane. The repository deliberately does not infer this
/// distinction from casing, vocabulary, or other raw-text shape heuristics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeGraphEvidenceSearchQuery {
    Lexical(String),
    LiteralOrFormal(String),
}

impl RuntimeGraphEvidenceSearchQuery {
    #[must_use]
    pub fn text(&self) -> &str {
        match self {
            Self::Lexical(text) | Self::LiteralOrFormal(text) => text,
        }
    }

    fn literal_or_formal_text(&self) -> Option<&str> {
        match self {
            Self::Lexical(_) => None,
            Self::LiteralOrFormal(text) => Some(text),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, utoipa::ToSchema)]
pub struct RuntimeGraphProjectionCountsRow {
    pub node_count: i64,
    pub edge_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, utoipa::ToSchema)]
pub struct RuntimeGraphDocumentLinkRow {
    pub document_id: Uuid,
    pub target_node_id: Uuid,
    pub target_node_type: String,
    pub relation_type: String,
    pub support_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, utoipa::ToSchema)]
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
        document_id.map_or_else(|| "none".to_string(), |value| value.to_string()),
        revision_id.map_or_else(|| "none".to_string(), |value| value.to_string()),
        activated_by_attempt_id.map_or_else(|| "none".to_string(), |value| value.to_string()),
        chunk_id.map_or_else(|| "none".to_string(), |value| value.to_string()),
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
    sqlx::query_as::<_, RuntimeGraphNodeRow>(sqlx::AssertSqlSafe(
        admitted_runtime_graph_nodes_query(""),
    ))
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
/// extraction records stayed put but no job ever ran `extract_graph` against
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

    sqlx::query_as::<_, RuntimeGraphNodeRow>(sqlx::AssertSqlSafe(
        admitted_runtime_graph_nodes_query("and node.id = any($3)"),
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

/// Lists admitted runtime graph edges for one projection version using the slim
/// query row type — drops `metadata_json` and `canonical_key` to cut
/// per-row heap usage for the query-time graph index path.
///
/// This is the canonical replacement for `list_admitted_runtime_graph_edges_by_library`
/// on the query path. Ingest and graph-stream callers that need the full row
/// continue to use the fat variant.
///
/// # Errors
/// Returns any `SQLx` error raised while querying admitted graph edges.
pub async fn list_admitted_runtime_graph_query_edges_by_library(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphQueryEdgeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphQueryEdgeRow>(
        "select id, library_id, from_node_id, to_node_id, relation_type,
            summary, weight, support_count, projection_version, created_at, updated_at
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

/// Fetches slim node rows for a pre-computed set of admitted ids plus every
/// `document`-type node. Mirrors
/// `list_runtime_graph_nodes_by_ids_or_document_type` but selects only the
/// columns consumed by the query-time graph index, omitting `metadata_json`.
///
/// # Errors
/// Returns any `SQLx` error raised while querying graph nodes.
pub async fn list_runtime_graph_query_nodes_by_ids_or_document_type(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    admitted_ids: &[Uuid],
) -> Result<Vec<RuntimeGraphQueryNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphQueryNodeRow>(
        "select id, library_id, canonical_key, label, node_type, aliases_json,
            summary, support_count, projection_version, created_at, updated_at
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

/// Opens a transaction with Postgres parallel query workers disabled for
/// its scope, used by the graph-topology scans below.
///
/// The topology document-link aggregate and the full edge scan are large
/// enough that the planner picks a parallel plan. Each parallel gather
/// allocates a dynamic-shared-memory segment in the Postgres server's
/// `/dev/shm`. On the stock container that mount defaults to 64 MiB, and
/// under concurrent graph loads (e.g. a browser retrying the topology
/// endpoint while a projection republish invalidates the Redis cache)
/// those segments exhaust it, surfacing as
/// `could not resize shared memory segment ... No space left on device`
/// and a HTTP 500 with no graph. Forcing a non-parallel plan keeps each
/// scan single-process so it never touches `/dev/shm`, making the topology
/// build robust on any deployment regardless of the configured shm size.
/// `SET LOCAL` is scoped to the transaction, so the connection returns to
/// the pool with the default parallel settings intact. The topology bytes
/// are cached in Redis for 24h, so this slower single-process path runs
/// only on cache miss / projection publish, never per UI request.
async fn begin_topology_scan_tx(
    pool: &PgPool,
) -> Result<sqlx::Transaction<'_, sqlx::Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("set local max_parallel_workers_per_gather = 0").execute(&mut *tx).await?;
    Ok(tx)
}

pub async fn list_admitted_runtime_graph_edges_compact_by_library(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphEdgeCompactRow>, sqlx::Error> {
    let mut tx = begin_topology_scan_tx(pool).await?;
    let rows = sqlx::query_as::<_, RuntimeGraphEdgeCompactRow>(
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
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
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
    let mut tx = begin_topology_scan_tx(pool).await?;
    let rows = sqlx::query_as::<_, RuntimeGraphNodeRow>(
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
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}

/// Fetches runtime graph nodes by id for one projection version without
/// re-evaluating the admitted-edge predicate. Use this only when the caller
/// already proved the ids came from an admitted graph edge/entity query.
///
/// # Errors
/// Returns any `SQLx` error raised while querying graph nodes.
pub async fn list_runtime_graph_nodes_by_ids(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    node_ids: &[Uuid],
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error> {
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "select id, library_id, canonical_key, label, node_type, aliases_json,
            summary, metadata_json, support_count, projection_version, created_at, updated_at
         from runtime_graph_node
         where library_id = $1
           and projection_version = $2
           and id = any($3::uuid[])
         order by label asc, created_at asc, id asc",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(node_ids)
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

/// Maximum byte length of a persisted `runtime_graph_node.label`.
///
/// `runtime_graph_node` carries btree indexes that include `label` (and
/// `lower(label)`). `PostgreSQL` caps a single btree index tuple at ~2704 bytes,
/// so an oversized label aborts the INSERT — and inside a whole-library restore
/// transaction that rolls back the entire import (Workstream R / R4). The cap
/// is a pure byte bound, NOT a natural-language rule: real entity names are
/// short, and a multi-kilobyte "label" is extraction noise, not a name. We keep
/// the bound comfortably under 2704 to leave room for the fixed index columns
/// and for case-folding growth in `lower(label)`.
const RUNTIME_GRAPH_LABEL_MAX_BYTES: usize = 2000;

/// Clamps a graph-node label to [`RUNTIME_GRAPH_LABEL_MAX_BYTES`] without
/// splitting a UTF-8 codepoint. Script-agnostic: it bounds the byte length
/// only and never inspects, transliterates, or matches against any
/// natural-language content. Returns the input unchanged when it already fits.
fn clamp_runtime_graph_label(label: &str) -> &str {
    if label.len() <= RUNTIME_GRAPH_LABEL_MAX_BYTES {
        return label;
    }
    let mut end = RUNTIME_GRAPH_LABEL_MAX_BYTES;
    while end > 0 && !label.is_char_boundary(end) {
        end -= 1;
    }
    &label[..end]
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
    .bind(clamp_runtime_graph_label(label))
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
    .bind(clamp_runtime_graph_label(label))
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
    let labels: Vec<&str> =
        inputs.iter().map(|i| clamp_runtime_graph_label(i.label.as_str())).collect();
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

/// Lists node ids only — the lean projection consumed by community
/// detection / label propagation. Skips the heavy text columns (`summary`,
/// `metadata_json`, `aliases_json`, `canonical_key`, `label`, `node_type`)
/// so a large library doesn't materialise multi-KB-per-row in worker RAM.
///
/// # Errors
/// Returns any `SQLx` error raised while querying node ids.
#[tracing::instrument(skip(pool), fields(library_id = %library_id, projection_version))]
pub async fn list_runtime_graph_node_ids_by_library(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphNodeIdRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphNodeIdRow>(
        "select id
         from runtime_graph_node
         where library_id = $1 and projection_version = $2",
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

/// Lists `(from, to, support_count)` triples for every edge — the lean
/// projection consumed by community detection / label propagation. Skips
/// heavy text columns (`summary`, `relation_type`, `canonical_key`,
/// `metadata_json`) and `ORDER BY` because label propagation produces its
/// own deterministic node ordering. Cuts the in-RAM Vec from KB-scale edge
/// payloads to the adjacency fields required by the algorithm.
///
/// # Errors
/// Returns any `SQLx` error raised while querying edges.
#[tracing::instrument(skip(pool), fields(library_id = %library_id, projection_version))]
pub async fn list_runtime_graph_edges_adjacency_by_library(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphEdgeAdjacencyRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEdgeAdjacencyRow>(
        "select from_node_id, to_node_id, support_count
         from runtime_graph_edge
         where library_id = $1 and projection_version = $2",
    )
    .bind(library_id)
    .bind(projection_version)
    .fetch_all(pool)
    .await
}

/// Values required to create or refresh one runtime graph evidence link.
///
/// The named payload makes the evidence provenance explicit at call sites and
/// prevents unrelated optional identifiers from being swapped positionally.
pub struct CreateRuntimeGraphEvidenceInput<'a> {
    pub library_id: Uuid,
    pub target_kind: &'a str,
    pub target_id: Uuid,
    pub document_id: Option<Uuid>,
    pub revision_id: Option<Uuid>,
    pub activated_by_attempt_id: Option<Uuid>,
    pub chunk_id: Option<Uuid>,
    pub source_file_name: Option<&'a str>,
    pub page_ref: Option<&'a str>,
    pub evidence_text: &'a str,
    pub confidence_score: Option<f64>,
    pub evidence_context_key: &'a str,
}

/// Creates a runtime graph evidence link.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting the evidence record.
pub async fn create_runtime_graph_evidence(
    pool: &PgPool,
    input: CreateRuntimeGraphEvidenceInput<'_>,
) -> Result<RuntimeGraphEvidenceRow, sqlx::Error> {
    let evidence_identity_key = runtime_graph_evidence_identity_key(
        input.target_kind,
        input.target_id,
        input.document_id,
        input.revision_id,
        input.activated_by_attempt_id,
        input.chunk_id,
        input.page_ref,
        input.source_file_name,
        input.evidence_context_key,
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
    .bind(input.library_id)
    .bind(&evidence_identity_key)
    .bind(input.target_kind)
    .bind(input.target_id)
    .bind(input.document_id)
    .bind(input.revision_id)
    .bind(input.activated_by_attempt_id)
    .bind(input.chunk_id)
    .bind(input.source_file_name)
    .bind(input.page_ref)
    .bind(input.evidence_text)
    .bind(input.confidence_score)
    .fetch_one(pool)
    .await
}

/// Single per-row payload for `bulk_create_runtime_graph_evidence_for_chunk`.
///
/// All other evidence columns are constant per merge call (the chunk's
/// `document_id` / `revision_id` / `attempt_id` / `chunk_id` / `source_file_name` /
/// `evidence_text`), so the bulk insert sends N rows in one round-trip
/// instead of N separate INSERTs.
#[derive(Debug, Clone)]
pub struct GraphEvidenceTarget {
    pub target_kind: &'static str,
    pub target_id: Uuid,
    pub evidence_context_key: &'static str,
}

/// Bulk-inserts a batch of `runtime_graph_evidence` rows that share the same
/// chunk-level context (library / document / revision / attempt / chunk /
/// `source_file_name` / `evidence_text`). Replaces N sequential
/// `create_runtime_graph_evidence` calls with a single `INSERT ... SELECT
/// FROM unnest(...)` round-trip — for a typical chunk with 10 entities and
/// 10 relations, that's ~50 round-trips collapsed into 1.
///
/// # Errors
/// Returns any `SQLx` error raised while running the bulk insert.
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

const SUPPORT_COUNT_RECALCULATION_BATCH_SIZE: usize = 1_000;

async fn recalculate_runtime_graph_support_counts_by_ids(
    pool: &PgPool,
    sql: &str,
    library_id: Uuid,
    projection_version: i64,
    target_ids: &[Uuid],
) -> Result<u64, sqlx::Error> {
    if target_ids.is_empty() {
        return Ok(0);
    }

    let mut rows_affected = 0_u64;
    for batch in target_ids.chunks(SUPPORT_COUNT_RECALCULATION_BATCH_SIZE) {
        let result = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(library_id)
            .bind(projection_version)
            .bind(batch)
            .execute(pool)
            .await?;
        rows_affected = rows_affected.saturating_add(result.rows_affected());
    }
    Ok(rows_affected)
}

pub async fn recalculate_runtime_graph_node_support_counts_by_ids(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    node_ids: &[Uuid],
) -> Result<u64, sqlx::Error> {
    recalculate_runtime_graph_support_counts_by_ids(
        pool,
        RECALCULATE_RUNTIME_GRAPH_NODE_SUPPORT_COUNTS_BY_IDS_SQL,
        library_id,
        projection_version,
        node_ids,
    )
    .await
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
    recalculate_runtime_graph_support_counts_by_ids(
        pool,
        RECALCULATE_RUNTIME_GRAPH_EDGE_SUPPORT_COUNTS_BY_IDS_SQL,
        library_id,
        projection_version,
        edge_ids,
    )
    .await
}

/// Recalculates targeted node and edge support counts in one transaction.
///
/// # Errors
/// Returns any `SQLx` error and rolls both target classes back together.
pub async fn recalculate_runtime_graph_support_counts_by_ids_atomically(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    node_ids: &[Uuid],
    edge_ids: &[Uuid],
) -> Result<(u64, u64), sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let mut node_rows_affected = 0_u64;
    for batch in node_ids.chunks(SUPPORT_COUNT_RECALCULATION_BATCH_SIZE) {
        let result = sqlx::query(RECALCULATE_RUNTIME_GRAPH_NODE_SUPPORT_COUNTS_BY_IDS_SQL)
            .bind(library_id)
            .bind(projection_version)
            .bind(batch)
            .execute(&mut *transaction)
            .await?;
        node_rows_affected = node_rows_affected.saturating_add(result.rows_affected());
    }
    let mut edge_rows_affected = 0_u64;
    for batch in edge_ids.chunks(SUPPORT_COUNT_RECALCULATION_BATCH_SIZE) {
        let result = sqlx::query(RECALCULATE_RUNTIME_GRAPH_EDGE_SUPPORT_COUNTS_BY_IDS_SQL)
            .bind(library_id)
            .bind(projection_version)
            .bind(batch)
            .execute(&mut *transaction)
            .await?;
        edge_rows_affected = edge_rows_affected.saturating_add(result.rows_affected());
    }
    transaction.commit().await?;
    Ok((node_rows_affected, edge_rows_affected))
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
    let per_target_limit = (limit as usize).div_ceil(targets.len()).max(1) as i64;

    sqlx::query_as::<_, RuntimeGraphEvidenceRow>(
        "with requested(target_kind, target_id, ordinal) as (
             select target_kind, target_id, ordinal::integer
             from unnest($2::text[], $3::uuid[]) with ordinality
                as request(target_kind, target_id, ordinal)
         ),
         ranked_evidence as (
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
                requested.ordinal,
                row_number() over (
                    partition by requested.ordinal
                    order by evidence.created_at desc, evidence.id desc
                ) as target_rank
             from requested
             join runtime_graph_evidence as evidence
               on evidence.library_id = $1
              and evidence.target_kind = requested.target_kind
              and evidence.target_id = requested.target_id
         )
         select id, library_id, target_kind, target_id,
            evidence.document_id, evidence.chunk_id, evidence.source_file_name,
            page_ref, evidence_text, confidence_score, created_at
         from ranked_evidence as evidence
         where target_rank <= $4
         order by ordinal asc, target_rank asc, created_at desc, id desc",
    )
    .bind(library_id)
    .bind(target_kinds)
    .bind(target_ids)
    .bind(per_target_limit)
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
    query_texts: &[RuntimeGraphEvidenceSearchQuery],
    document_ids: &[Uuid],
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
    let per_query_candidate_limit = runtime_graph_evidence_per_query_candidate_limit(
        limit,
        search_queries.len().saturating_add(literal_queries.len()),
    );
    let document_filter =
        if document_ids.is_empty() { "" } else { " and evidence.document_id = any($6::uuid[])" };

    let sql = format!(
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
                evidence.literal_match
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
                    false as literal_match
                 from runtime_graph_evidence as evidence
                 where evidence.library_id = $1
                   and btrim(evidence.evidence_text) <> ''
                   {document_filter}
                   and to_tsvector('simple'::regconfig, evidence.evidence_text)
                       @@ requested_text_query.ts_query
                 order by
                    evidence.confidence_score desc nulls last,
                    evidence.created_at desc,
                    evidence.id desc
                 limit $5
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
                evidence.literal_match
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
                    true as literal_match
                 from runtime_graph_evidence as evidence
                 where evidence.library_id = $1
                   and btrim(evidence.evidence_text) <> ''
                   {document_filter}
                   and lower(evidence.evidence_text) like requested_literal_query.literal_pattern escape '~'
                 order by
                    evidence.confidence_score desc nulls last,
                    evidence.created_at desc,
                    evidence.id desc
                 limit $5
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
                evidence.literal_match
             from (
                 select * from text_matched
                 union all
                 select * from literal_matched
             ) as evidence
             order by
                evidence.id,
                evidence.first_query_ordinal asc,
                evidence.literal_match desc,
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
                literal_match
             from matched
             order by
                body_key,
                first_query_ordinal asc,
                literal_match desc,
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
            body_match desc,
            confidence_score desc nulls last,
            created_at desc,
            id desc
         limit $4"
    );

    let mut query = sqlx::query_as::<_, RuntimeGraphEvidenceRow>(sqlx::AssertSqlSafe(&*sql))
        .bind(library_id)
        .bind(search_queries)
        .bind(literal_queries)
        .bind(limit)
        .bind(per_query_candidate_limit);
    if !document_ids.is_empty() {
        query = query.bind(document_ids);
    }
    query.fetch_all(pool).await
}

fn runtime_graph_evidence_per_query_candidate_limit(limit: i64, query_count: usize) -> i64 {
    if limit <= 0 || query_count == 0 {
        return 0;
    }
    if query_count <= 1 {
        return limit;
    }
    limit.min(24)
}

fn runtime_graph_evidence_text_search_queries(
    query_texts: &[RuntimeGraphEvidenceSearchQuery],
) -> Vec<String> {
    let mut seen_queries = BTreeSet::new();
    let mut token_windows_by_query = Vec::new();
    for query in query_texts {
        let query_text = query.text();
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

fn runtime_graph_evidence_literal_search_queries(
    query_texts: &[RuntimeGraphEvidenceSearchQuery],
) -> Vec<String> {
    let mut seen_queries = BTreeSet::new();
    let mut queries = Vec::new();
    for query_text in
        query_texts.iter().filter_map(RuntimeGraphEvidenceSearchQuery::literal_or_formal_text)
    {
        let normalized = query_text.split_whitespace().collect::<Vec<_>>().join(" ");
        if !runtime_graph_evidence_literal_search_query_is_bounded(&normalized) {
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

fn runtime_graph_evidence_literal_search_query_is_bounded(query_text: &str) -> bool {
    let alphanumeric_count = query_text.chars().filter(|ch| ch.is_alphanumeric()).count();
    if alphanumeric_count < 4 {
        return false;
    }
    let char_count = query_text.chars().count();
    let tokens = normalized_alnum_token_sequence_by(
        query_text,
        |token| !token.trim().is_empty(),
        Some(RUNTIME_GRAPH_EVIDENCE_LITERAL_SEARCH_MAX_TOKENS + 1),
    );
    !tokens.is_empty()
        && tokens.len() <= RUNTIME_GRAPH_EVIDENCE_LITERAL_SEARCH_MAX_TOKENS
        && char_count <= RUNTIME_GRAPH_EVIDENCE_LITERAL_SEARCH_MAX_CHARS
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
    selected.sort_by_key(|left| left.0);
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
    let numeric_bonus = usize::from(runtime_graph_evidence_text_search_token_has_numeric(token))
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
    if tokens.len() == 2 {
        return tokens
            .iter()
            .any(|token| runtime_graph_evidence_text_search_token_has_numeric(token));
    }
    if tokens.len() >= 3 {
        return true;
    }
    false
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
    let mut tx = begin_topology_scan_tx(pool).await?;
    let rows = sqlx::query_as::<_, RuntimeGraphDocumentLinkRow>(
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
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
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
/// `document_id` is null (FK `ON DELETE SET NULL` orphan debris) or whose
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

async fn delete_runtime_graph_canonical_summaries_for_orphan_targets_with_executor(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
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
    .execute(&mut **transaction)
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

/// Deletes active graph evidence for one logical content revision and returns
/// the node/edge targets it pointed at.
///
/// The returned targets are the exact nodes/edges whose support count must be
/// recalculated after the superseded revision's evidence is gone — a
/// content-shrinking re-revision can drop those targets to zero support, which
/// the caller then prunes by id. Mirrors the contract of
/// [`deactivate_runtime_graph_evidence_by_documents`] so a re-revision supersede
/// stays bounded to the document's own graph footprint instead of forcing a
/// library-wide projection rebuild.
///
/// # Errors
/// Returns any `SQLx` error raised while deleting revision-scoped evidence rows.
pub async fn deactivate_runtime_graph_evidence_by_content_revision(
    pool: &PgPool,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
) -> Result<Vec<RuntimeGraphEvidenceTargetRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphEvidenceTargetRow>(
        "delete from runtime_graph_evidence
         where library_id = $1
           and document_id = $2
           and (revision_id = $3 or revision_id is null)
         returning target_kind, target_id",
    )
    .bind(library_id)
    .bind(document_id)
    .bind(revision_id)
    .fetch_all(pool)
    .await
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeGraphProjectionPruneOutcome {
    pub deleted_node_count: u64,
    pub deleted_edge_count: u64,
    pub deleted_summary_count: u64,
}

/// Recalculates library-wide support counts and prunes unsupported projection
/// rows plus their summary projections in one advisory-locked transaction.
///
/// # Errors
/// Returns any `SQLx` error and rolls the entire support/prune unit back.
pub async fn synchronize_runtime_graph_support_and_prune(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<RuntimeGraphProjectionPruneOutcome, sqlx::Error> {
    let mut transaction = acquire_runtime_library_graph_lock(pool, library_id).await?;
    sqlx::query(RECALCULATE_RUNTIME_GRAPH_NODE_SUPPORT_COUNTS_SQL)
        .bind(library_id)
        .bind(projection_version)
        .execute(&mut *transaction)
        .await?;
    sqlx::query(RECALCULATE_RUNTIME_GRAPH_EDGE_SUPPORT_COUNTS_SQL)
        .bind(library_id)
        .bind(projection_version)
        .execute(&mut *transaction)
        .await?;

    let deleted_edges = sqlx::query_as::<_, (Uuid, String)>(
        "delete from runtime_graph_edge
         where library_id = $1
           and projection_version = $2
           and support_count <= 0
         returning id, canonical_key",
    )
    .bind(library_id)
    .bind(projection_version)
    .fetch_all(&mut *transaction)
    .await?;
    let deleted_nodes = sqlx::query_as::<_, (Uuid, String)>(
        "delete from runtime_graph_node
         where library_id = $1
           and projection_version = $2
           and support_count <= 0
         returning id, canonical_key",
    )
    .bind(library_id)
    .bind(projection_version)
    .fetch_all(&mut *transaction)
    .await?;
    let deleted_edge_ids = deleted_edges.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    let deleted_node_ids = deleted_nodes.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    let deleted_summary_count =
        delete_runtime_graph_canonical_summaries_for_orphan_targets_with_executor(
            &mut transaction,
            library_id,
            &deleted_node_ids,
            &deleted_edge_ids,
        )
        .await?;
    release_runtime_library_graph_lock(transaction, library_id).await?;

    Ok(RuntimeGraphProjectionPruneOutcome {
        deleted_node_count: u64::try_from(deleted_nodes.len()).unwrap_or(u64::MAX),
        deleted_edge_count: u64::try_from(deleted_edges.len()).unwrap_or(u64::MAX),
        deleted_summary_count,
    })
}

/// Atomically prunes explicit projection targets and any now-orphaned summary
/// rows under the same library advisory lock used by publication.
///
/// # Errors
/// Returns any `SQLx` error and rolls the entire prune unit back.
pub async fn prune_runtime_graph_projection_rows(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    node_ids: &[Uuid],
    edge_ids: &[Uuid],
) -> Result<RuntimeGraphProjectionPruneOutcome, sqlx::Error> {
    if node_ids.is_empty() && edge_ids.is_empty() {
        return Ok(RuntimeGraphProjectionPruneOutcome::default());
    }
    let mut transaction = acquire_runtime_library_graph_lock(pool, library_id).await?;
    let deleted_edge_count = sqlx::query(
        "delete from runtime_graph_edge
         where library_id = $1
           and projection_version = $2
           and id = any($3)",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(edge_ids)
    .execute(&mut *transaction)
    .await?
    .rows_affected();
    let deleted_node_count = sqlx::query(
        "delete from runtime_graph_node
         where library_id = $1
           and projection_version = $2
           and id = any($3)",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(node_ids)
    .execute(&mut *transaction)
    .await?
    .rows_affected();
    let deleted_summary_count =
        delete_runtime_graph_canonical_summaries_for_orphan_targets_with_executor(
            &mut transaction,
            library_id,
            node_ids,
            edge_ids,
        )
        .await?;
    release_runtime_library_graph_lock(transaction, library_id).await?;
    Ok(RuntimeGraphProjectionPruneOutcome {
        deleted_node_count,
        deleted_edge_count,
        deleted_summary_count,
    })
}

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

/// Deletes explicit canonical graph edges by id.
///
/// # Errors
/// Returns any `SQLx` error raised while pruning graph edges.
pub async fn delete_runtime_graph_edges_by_ids(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    edge_ids: &[Uuid],
) -> Result<Vec<Uuid>, sqlx::Error> {
    if edge_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_scalar::<_, Uuid>(
        "delete from runtime_graph_edge
         where library_id = $1
           and projection_version = $2
           and id = any($3)
         returning id",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(edge_ids)
    .fetch_all(pool)
    .await
}

/// Lists canonical graph edge ids incident to any node in `node_ids`.
///
/// # Errors
/// Returns any `SQLx` error raised while querying graph edges.
pub async fn list_runtime_graph_edge_ids_by_node_ids(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    node_ids: &[Uuid],
) -> Result<Vec<Uuid>, sqlx::Error> {
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_scalar::<_, Uuid>(
        "select id
         from runtime_graph_edge
         where library_id = $1
           and projection_version = $2
           and (from_node_id = any($3) or to_node_id = any($3))",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(node_ids)
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

/// Deletes explicit canonical graph nodes by id.
///
/// # Errors
/// Returns any `SQLx` error raised while pruning graph nodes.
pub async fn delete_runtime_graph_nodes_by_ids(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    node_ids: &[Uuid],
) -> Result<Vec<Uuid>, sqlx::Error> {
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_scalar::<_, Uuid>(
        "delete from runtime_graph_node
         where library_id = $1
           and projection_version = $2
           and id = any($3)
         returning id",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(node_ids)
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
    sqlx::query_as::<_, RuntimeGraphProjectionCountsRow>(sqlx::AssertSqlSafe(
        admitted_runtime_graph_counts_query(),
    ))
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

    let wildcard_prefixes = runtime_graph_entity_wildcard_prefixes(&normalized_query);
    if !wildcard_prefixes.is_empty() {
        let rows = search_admitted_runtime_graph_entities_by_wildcard_prefixes(
            pool,
            library_id,
            projection_version,
            &wildcard_prefixes,
            limit,
        )
        .await?;
        if !rows.is_empty() {
            return Ok(rows);
        }
    }

    let search_terms = runtime_graph_entity_search_terms(&normalized_query);
    let candidate_limit = limit.saturating_mul(6).min(1_000).max(limit);
    let mut rows = sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "with search_terms(value) as (
            select unnest($4::text[])
         ),
         matched_ids as (
            select id, min(match_rank) as match_rank
            from (
                select id, 0::integer as match_rank
                from (
                    select n.id
                    from runtime_graph_node n
                    where n.library_id = $1
                      and n.projection_version = $2
                      and n.node_type <> 'document'
                      and md5(lower(n.label)) = md5($3)
                      and lower(n.label) = $3
                    order by n.support_count desc, n.label asc, n.created_at asc, n.id asc
                    limit $6
                ) label_exact
                union all
                select id, 1::integer as match_rank
                from (
                    select n.id
                    from runtime_graph_node n
                    where n.library_id = $1
                      and n.projection_version = $2
                      and n.node_type <> 'document'
                      and lower(n.aliases_json::text) like '%\"' || $3 || '\"%'
                    order by n.support_count desc, n.label asc, n.created_at asc, n.id asc
                    limit $6
                ) alias_exact
                union all
                select id, 2::integer as match_rank
                from (
                    select n.id
                    from runtime_graph_node n
                    where n.library_id = $1
                      and n.projection_version = $2
                      and n.node_type <> 'document'
                      and lower(n.label) like $3 || '%'
                    order by n.support_count desc, n.label asc, n.created_at asc, n.id asc
                    limit $6
                ) label_prefix
                union all
                select id, 3::integer as match_rank
                from (
                    select n.id
                    from runtime_graph_node n
                    where n.library_id = $1
                      and n.projection_version = $2
                      and n.node_type <> 'document'
                      and lower(n.aliases_json::text) like '%\"' || $3 || '%'
                    order by n.support_count desc, n.label asc, n.created_at asc, n.id asc
                    limit $6
                ) alias_prefix
                union all
                select id, 4::integer as match_rank
                from (
                    select n.id
                    from runtime_graph_node n
                    where n.library_id = $1
                      and n.projection_version = $2
                      and n.node_type <> 'document'
                      and lower(n.label) like '%' || $3 || '%'
                    order by n.support_count desc, n.label asc, n.created_at asc, n.id asc
                    limit $6
                ) label_phrase
                union all
                select id, 5::integer as match_rank
                from (
                    select n.id
                    from runtime_graph_node n
                    where n.library_id = $1
                      and n.projection_version = $2
                      and n.node_type <> 'document'
                      and lower(n.aliases_json::text) like '%' || $3 || '%'
                    order by n.support_count desc, n.label asc, n.created_at asc, n.id asc
                    limit $6
                ) alias_phrase
                union all
                select id, 6::integer as match_rank
                from (
                    select n.id
                    from runtime_graph_node n
                    join search_terms term on lower(n.label) like '%' || term.value || '%'
                    where n.library_id = $1
                      and n.projection_version = $2
                      and n.node_type <> 'document'
                    order by n.support_count desc, n.label asc, n.created_at asc, n.id asc
                    limit $6
                ) label_terms
            ) matches
            group by id
            order by min(match_rank) asc, id asc
            limit $6
         )
         select
            n.id, n.library_id, n.canonical_key, n.label, n.node_type,
            n.aliases_json, n.summary, n.metadata_json, n.support_count,
            n.projection_version, n.created_at, n.updated_at
         from matched_ids matched
         join runtime_graph_node n on n.id = matched.id
         order by
            matched.match_rank asc,
            n.support_count desc,
            n.label asc,
            n.created_at asc
         limit $5",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(&normalized_query)
    .bind(&search_terms)
    .bind(limit)
    .bind(candidate_limit)
    .fetch_all(pool)
    .await?;

    let remaining_limit = limit.saturating_sub(rows.len() as i64);
    if remaining_limit <= 0 {
        return Ok(rows);
    }

    let matched_node_ids = rows.iter().map(|row| row.id).collect::<Vec<_>>();
    let mut summary_rows = sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "select
            n.id, n.library_id, n.canonical_key, n.label, n.node_type,
            n.aliases_json, n.summary, n.metadata_json, n.support_count,
            n.projection_version, n.created_at, n.updated_at
         from runtime_graph_node n
         where n.library_id = $1
           and n.projection_version = $2
           and n.node_type <> 'document'
           and not (n.id = any($5::uuid[]))
           and (
                coalesce(lower(n.summary), '') like '%' || $3 || '%'
                or exists (
                    select 1
                    from unnest($4::text[]) as term(value)
                    where coalesce(lower(n.summary), '') like '%' || term.value || '%'
                )
           )
         order by
            case
                when coalesce(lower(n.summary), '') like '%' || $3 || '%' then 0
                else 1
            end asc,
            n.support_count desc,
            n.label asc,
            n.created_at asc
         limit $6",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(&normalized_query)
    .bind(&search_terms)
    .bind(&matched_node_ids)
    .bind(remaining_limit)
    .fetch_all(pool)
    .await?;

    rows.append(&mut summary_rows);
    Ok(rows)
}

async fn search_admitted_runtime_graph_entities_by_wildcard_prefixes(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    wildcard_prefixes: &[String],
    limit: i64,
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error> {
    let candidate_limit = limit.saturating_mul(6).min(1_000).max(limit);
    sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "with wildcard_prefixes(value) as (
            select unnest($3::text[])
         ),
         matched_ids as (
            select id, min(match_rank) as match_rank
            from (
                select id, 0::integer as match_rank
                from (
                    select n.id
                    from runtime_graph_node n
                    join wildcard_prefixes prefix
                      on lower(n.label) like prefix.value || '%' escape '~'
                    where n.library_id = $1
                      and n.projection_version = $2
                      and n.node_type <> 'document'
                    order by n.support_count desc, n.label asc, n.created_at asc, n.id asc
                    limit $5
                ) label_prefix
                union all
                select id, 1::integer as match_rank
                from (
                    select n.id
                    from runtime_graph_node n
                    where n.library_id = $1
                      and n.projection_version = $2
                      and n.node_type <> 'document'
                      and exists (
                        select 1
                        from jsonb_array_elements_text(
                            case
                                when jsonb_typeof(n.aliases_json) = 'array'
                                then n.aliases_json
                                else '[]'::jsonb
                            end
                        ) as alias(value)
                        join wildcard_prefixes prefix
                          on lower(alias.value) like prefix.value || '%' escape '~'
                      )
                    order by n.support_count desc, n.label asc, n.created_at asc, n.id asc
                    limit $5
                ) alias_prefix
            ) matches
            group by id
            order by min(match_rank) asc, id asc
            limit $5
         )
         select
            n.id, n.library_id, n.canonical_key, n.label, n.node_type,
            n.aliases_json, n.summary, n.metadata_json, n.support_count,
            n.projection_version, n.created_at, n.updated_at
         from matched_ids matched
         join runtime_graph_node n on n.id = matched.id
         order by
            matched.match_rank asc,
            n.support_count desc,
            n.label asc,
            n.created_at asc
         limit $4",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(wildcard_prefixes)
    .bind(limit)
    .bind(candidate_limit)
    .fetch_all(pool)
    .await
}

fn runtime_graph_entity_wildcard_prefixes(query_text: &str) -> Vec<String> {
    literal_wildcard_prefixes(query_text, 2)
        .into_iter()
        .map(|prefix| escape_sql_like_literal(&prefix))
        .collect()
}

fn escape_sql_like_literal(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '~' | '%' | '_') {
            escaped.push('~');
        }
        escaped.push(ch);
    }
    escaped
}

fn runtime_graph_entity_search_terms(query_text: &str) -> Vec<String> {
    normalized_alnum_token_sequence_by(
        query_text,
        |token| {
            token.chars().any(char::is_numeric)
                || token.chars().count() >= RUNTIME_GRAPH_ENTITY_SEARCH_TOKEN_MIN_CHARS
        },
        Some(RUNTIME_GRAPH_ENTITY_SEARCH_TOKEN_CAP),
    )
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

/// One multi-word entity node eligible for acronym-alias backfill.
#[derive(Debug, Clone, FromRow)]
pub struct AcronymBackfillNodeRow {
    pub id: Uuid,
    pub label: String,
}

/// Lists non-document entity nodes of the active projection whose label is a
/// multi-word phrase, keyset-paginated by `id`. Only multi-word labels can
/// carry a per-word-initials acronym, so single-token labels are filtered out
/// in SQL to keep the backfill scan bounded on large libraries.
///
/// The multi-word predicate is structural (an alphanumeric run, a non-alnum
/// gap, then another alphanumeric run) and script-agnostic.
///
/// # Errors
/// Returns any `SQLx` error raised during the query.
pub async fn list_multiword_runtime_graph_nodes_for_acronym_backfill(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    after_id: Option<Uuid>,
    limit: i64,
) -> Result<Vec<AcronymBackfillNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, AcronymBackfillNodeRow>(
        "select n.id, n.label
         from runtime_graph_node n
         where n.library_id = $1
           and n.projection_version = $2
           and n.node_type <> 'document'
           and n.label ~ '[[:alnum:]][^[:alnum:]]+[[:alnum:]]'
           and ($3::uuid is null or n.id > $3)
         order by n.id asc
         limit $4",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(after_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Lists non-document entity nodes of the active projection whose label is a
/// single token (no internal non-alphanumeric gap) and is not pure lowercase
/// prose, keyset-paginated by `id`. These are the candidate short-form acronym
/// nodes for the reverse corpus-gloss backfill (a node labelled `AS` gaining the
/// full-form phrase `Alpha Suite` from a `<phrase> ( <short> )` gloss in its own
/// evidence).
///
/// Both structural predicates are script-agnostic. The single-token predicate is
/// the complement of the multi-word predicate used by
/// [`list_multiword_runtime_graph_nodes_for_acronym_backfill`]. A label that is
/// purely lowercase letters can never be identifier-shaped (no separator, digit,
/// or uppercase), so excluding those in SQL is recall-complete while keeping the
/// scan bounded; the precise identifier-shape gate runs caller-side before any
/// evidence is loaded.
///
/// # Errors
/// Returns any `SQLx` error raised during the query.
pub async fn list_short_token_runtime_graph_nodes_for_acronym_backfill(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    after_id: Option<Uuid>,
    limit: i64,
) -> Result<Vec<AcronymBackfillNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, AcronymBackfillNodeRow>(
        "select n.id, n.label
         from runtime_graph_node n
         where n.library_id = $1
           and n.projection_version = $2
           and n.node_type <> 'document'
           and n.label !~ '[[:alnum:]][^[:alnum:]]+[[:alnum:]]'
           and n.label !~ '^[[:space:]]*[[:lower:]]+[[:space:]]*$'
           and char_length(btrim(n.label)) >= 2
           and ($3::uuid is null or n.id > $3)
         order by n.id asc
         limit $4",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(after_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// One evidence chunk text attached to a graph node.
#[derive(Debug, Clone, FromRow)]
pub struct NodeEvidenceChunkTextRow {
    pub node_id: Uuid,
    pub chunk_text: String,
}

/// Loads up to `per_node_limit` evidence chunk texts for each node in
/// `node_ids`, joining `runtime_graph_evidence` to `content_chunk`. Used by the
/// acronym backfill to re-run the structural detectors over existing data
/// without re-ingesting documents.
///
/// Only chunks that actually contain the node's label as a substring are
/// returned. Both structural detectors require the full-form label to appear in
/// the chunk (the parenthetical gloss is `<label> ( <short> )`; the standalone
/// detector requires the label phrase to be present), so this filter is
/// recall-complete for them while bounding the text volume to the chunks that
/// can possibly carry a gloss — no arbitrary per-node cap that could drop a
/// gloss chunk on a high-evidence node.
///
/// # Errors
/// Returns any `SQLx` error raised during the query.
pub async fn list_runtime_graph_node_evidence_chunk_texts(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    node_ids: &[Uuid],
) -> Result<Vec<NodeEvidenceChunkTextRow>, sqlx::Error> {
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, NodeEvidenceChunkTextRow>(
        "select distinct e.target_id as node_id, c.normalized_text as chunk_text
         from runtime_graph_evidence e
         join runtime_graph_node n
           on n.id = e.target_id
          and n.library_id = $1
          and n.projection_version = $3
         join content_chunk c on c.id = e.chunk_id
         where e.library_id = $1
           and e.target_kind = 'node'
           and e.target_id = any($2::uuid[])
           and e.chunk_id is not null
           and position(lower(n.label) in lower(c.normalized_text)) > 0",
    )
    .bind(library_id)
    .bind(node_ids)
    .bind(projection_version)
    .fetch_all(pool)
    .await
}

/// Idempotently unions `new_aliases` into a node's `aliases_json`.
///
/// The update is a no-op (zero rows affected) when every alias in
/// `new_aliases` is already present, so re-running the backfill never
/// duplicates an alias and never churns `updated_at`. Existing aliases are
/// preserved; the merged set is deduplicated and sorted.
///
/// Returns the number of rows updated (0 or 1).
///
/// # Errors
/// Returns any `SQLx` error raised during the update.
pub async fn add_runtime_graph_node_aliases(
    pool: &PgPool,
    node_id: Uuid,
    projection_version: i64,
    new_aliases: &[String],
) -> Result<u64, sqlx::Error> {
    let trimmed: Vec<String> = new_aliases
        .iter()
        .map(|alias| alias.trim().to_string())
        .filter(|alias| !alias.is_empty())
        .collect();
    if trimmed.is_empty() {
        return Ok(0);
    }
    let incoming_json = serde_json::Value::Array(
        trimmed.iter().map(|alias| serde_json::Value::String(alias.clone())).collect(),
    );
    let result = sqlx::query(
        "update runtime_graph_node n
         set aliases_json = (
                 select coalesce(jsonb_agg(distinct value order by value), '[]'::jsonb)
                 from (
                     select btrim(value) as value
                     from jsonb_array_elements_text(
                         case when jsonb_typeof(n.aliases_json) = 'array'
                              then n.aliases_json else '[]'::jsonb end
                     ) as existing(value)
                     where btrim(value) <> ''
                     union
                     select btrim(value) as value
                     from unnest($3::text[]) as incoming(value)
                     where btrim(value) <> ''
                 ) merged
             ),
             updated_at = now()
         where n.id = $1
           and n.projection_version = $2
           and not (coalesce(n.aliases_json, '[]'::jsonb) @> $4::jsonb)",
    )
    .bind(node_id)
    .bind(projection_version)
    .bind(&trimmed)
    .bind(incoming_json)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Recomputes the structural (parenthetical-gloss) acronym alias subset for a
/// node.
///
/// The operation is a **replace-within-slot**, not a plain union:
/// - Any existing alias that is identifier-shaped AND equals `label_initials`
///   (i.e. was written by a previous structural-alias pass) is removed from the
///   stored set if it is absent from `justified_aliases`.
/// - Every alias in `justified_aliases` is added.
/// - All other existing aliases (LLM-extracted, manually set) are preserved.
///
/// This deterministically reverts wrongly attached initials-aliases from
/// earlier passes while keeping aliases from other sources intact. Re-running
/// with the same `justified_aliases` is a no-op.
///
/// `label_initials` must be the uppercased per-word initials of the node label
/// (e.g. `"AS"` for `"Alpha Suite"`). It is used as the discriminating key: an
/// existing alias is only eligible for removal when it matches this value
/// case-insensitively.
///
/// Returns the number of rows updated (0 or 1).
///
/// # Errors
/// Returns any `SQLx` error raised during the update.
pub async fn recompute_runtime_graph_node_structural_aliases(
    pool: &PgPool,
    node_id: Uuid,
    projection_version: i64,
    label_initials: &str,
    justified_aliases: &[String],
) -> Result<u64, sqlx::Error> {
    // Build the expected final set: existing aliases that are NOT the stale
    // initials-slot, plus the newly justified ones.
    // We do this in SQL so it is a single round-trip and consistent even under
    // concurrent writes.
    let upper_initials = label_initials.to_uppercase();
    let justified_trimmed: Vec<String> =
        justified_aliases.iter().map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect();
    let justified_json = serde_json::Value::Array(
        justified_trimmed.iter().map(|a| serde_json::Value::String(a.clone())).collect(),
    );
    let result = sqlx::query(
        // $1 = node_id, $2 = projection_version,
        // $3 = upper_initials (the stale-slot discriminator),
        // $4 = justified_trimmed (incoming set as text[]),
        // $5 = justified_json (for the @> no-op guard on the incoming side)
        "update runtime_graph_node n
         set aliases_json = (
                 select coalesce(jsonb_agg(distinct value order by value), '[]'::jsonb)
                 from (
                     -- keep existing aliases that are NOT the initials slot
                     select btrim(value) as value
                     from jsonb_array_elements_text(
                         case when jsonb_typeof(n.aliases_json) = 'array'
                              then n.aliases_json else '[]'::jsonb end
                     ) as existing(value)
                     where btrim(value) <> ''
                       and upper(btrim(value)) <> $3
                     union
                     -- add the freshly justified aliases
                     select btrim(value) as value
                     from unnest($4::text[]) as incoming(value)
                     where btrim(value) <> ''
                 ) merged
             ),
             updated_at = now()
         where n.id = $1
           and n.projection_version = $2
           and (
               -- there is a stale initials-alias that needs removal
               exists (
                   select 1
                   from jsonb_array_elements_text(
                       case when jsonb_typeof(n.aliases_json) = 'array'
                            then n.aliases_json else '[]'::jsonb end
                   ) as existing(value)
                   where upper(btrim(value)) = $3
                     and not ($5::jsonb @> jsonb_build_array(btrim(value)))
               )
               or
               -- there is a justified alias not yet in the stored set
               not (coalesce(n.aliases_json, '[]'::jsonb) @> $5::jsonb)
           )",
    )
    .bind(node_id)
    .bind(projection_version)
    .bind(&upper_initials)
    .bind(&justified_trimmed)
    .bind(justified_json)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Recomputes the structural (parenthetical-gloss) **full-form** alias subset
/// for a short-token node — the reverse of
/// [`recompute_runtime_graph_node_structural_aliases`].
///
/// The forward pass attaches the short acronym to a multi-word node and keys the
/// stale slot by *literal equality* with the acronym. This reverse pass attaches
/// the full-form phrase to a node whose label *is* the short acronym and keys the
/// stale slot by *per-word initials*: an existing alias is part of the recomputed
/// slot iff its uppercased per-word initials equal `short_label_upper`.
///
/// - Any existing alias whose initials equal `short_label_upper` is removed if it
///   is absent from `justified_aliases` (the gloss that justified it is gone).
/// - Every alias in `justified_aliases` is added.
/// - All other existing aliases (single-token, foreign-initials, LLM-extracted,
///   manually set) are preserved — `is distinct from` keeps NULL-initials values.
///
/// `short_label_upper` must be the uppercased node label (e.g. `"AS"`). The two
/// passes never run on the same node: the forward pass scans multi-word labels,
/// this one scans single-token labels. Re-running with the same justified set is
/// a no-op.
///
/// Returns the number of rows updated (0 or 1).
///
/// # Errors
/// Returns any `SQLx` error raised during the update.
pub async fn recompute_runtime_graph_node_fullform_aliases(
    pool: &PgPool,
    node_id: Uuid,
    projection_version: i64,
    justified_aliases: &[String],
) -> Result<u64, sqlx::Error> {
    let justified_trimmed: Vec<String> =
        justified_aliases.iter().map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect();
    if justified_trimmed.is_empty() {
        return Ok(0);
    }
    let justified_json = serde_json::Value::Array(
        justified_trimmed.iter().map(|a| serde_json::Value::String(a.clone())).collect(),
    );
    // ADD-ONLY by design: the reverse direction unions gloss-justified
    // full-form aliases into the node's alias set and never removes anything.
    // Unlike the forward slot (the exact short string, unambiguously owned by
    // the gloss capture), a "full-form" slot has no safe ownership marker — a
    // removal pass keyed on per-word initials was observed stripping
    // legitimate extraction-derived aliases of OTHER senses on polysemous
    // short-labeled nodes. A stale gloss-derived full form lingering after a
    // gloss disappears is the lesser harm and is corrected by re-ingest.
    let result = sqlx::query(
        // $1 = node_id, $2 = projection_version,
        // $3 = justified aliases as text[], $4 = same as jsonb (no-op guard).
        "update runtime_graph_node n
         set aliases_json = (
                 select coalesce(jsonb_agg(distinct value order by value), '[]'::jsonb)
                 from (
                     select btrim(value) as value
                     from jsonb_array_elements_text(
                         case when jsonb_typeof(n.aliases_json) = 'array'
                              then n.aliases_json else '[]'::jsonb end
                     ) as existing(value)
                     where btrim(value) <> ''
                     union
                     select btrim(value) as value
                     from unnest($3::text[]) as incoming(value)
                     where btrim(value) <> ''
                 ) merged
             ),
             updated_at = now()
         where n.id = $1
           and n.projection_version = $2
           and not (coalesce(n.aliases_json, '[]'::jsonb) @> $4::jsonb)",
    )
    .bind(node_id)
    .bind(projection_version)
    .bind(&justified_trimmed)
    .bind(justified_json)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::{
        RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_QUERY_CAP, RUNTIME_GRAPH_LABEL_MAX_BYTES,
        RuntimeGraphEvidenceSearchQuery, clamp_runtime_graph_label,
        runtime_graph_entity_search_terms, runtime_graph_entity_wildcard_prefixes,
        runtime_graph_evidence_literal_search_queries,
        runtime_graph_evidence_per_query_candidate_limit,
        runtime_graph_evidence_text_search_queries,
        runtime_graph_evidence_text_search_token_prefix, runtime_graph_evidence_text_search_tokens,
    };

    fn lexical_evidence_query(value: &str) -> RuntimeGraphEvidenceSearchQuery {
        RuntimeGraphEvidenceSearchQuery::Lexical(value.to_string())
    }

    fn literal_evidence_query(value: &str) -> RuntimeGraphEvidenceSearchQuery {
        RuntimeGraphEvidenceSearchQuery::LiteralOrFormal(value.to_string())
    }

    #[test]
    fn clamp_runtime_graph_label_leaves_short_labels_untouched() {
        let label = "Alpha Suite payment module";
        assert_eq!(clamp_runtime_graph_label(label), label);
    }

    #[test]
    fn clamp_runtime_graph_label_bounds_oversized_labels_by_bytes() {
        // A ~1.6 KB ASCII "label" — the extraction-noise shape that overflowed
        // the btree index and aborted whole-library restores (R4).
        let oversized = "x".repeat(1600 * 4);
        let clamped = clamp_runtime_graph_label(&oversized);
        assert!(clamped.len() <= RUNTIME_GRAPH_LABEL_MAX_BYTES);
        assert!(oversized.starts_with(clamped));
    }

    #[test]
    fn clamp_runtime_graph_label_never_splits_a_codepoint() {
        // Each codepoint here is multi-byte; the cap must fall back to a char
        // boundary rather than slice mid-codepoint (which would panic / corrupt).
        // Script-agnostic: we assert structural validity, not any language.
        let multibyte = "\u{1F680}".repeat(RUNTIME_GRAPH_LABEL_MAX_BYTES); // 4 bytes each
        let clamped = clamp_runtime_graph_label(&multibyte);
        assert!(clamped.len() <= RUNTIME_GRAPH_LABEL_MAX_BYTES);
        // Re-borrowing as &str proves we landed on a valid UTF-8 boundary.
        assert!(clamped.chars().all(|ch| ch == '\u{1F680}'));
    }

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
    fn entity_search_terms_keep_structural_needles_without_language_lists() {
        let terms =
            runtime_graph_entity_search_terms("Alpha/report://needle?fontSize=12 alpha alpha 80");

        assert_eq!(
            terms,
            vec![
                "alpha".to_string(),
                "report".to_string(),
                "needle".to_string(),
                "fontsize".to_string(),
                "12".to_string(),
                "80".to_string(),
            ],
        );
    }

    #[test]
    fn entity_wildcard_prefixes_keep_literal_prefix_semantics() {
        let prefixes =
            runtime_graph_entity_wildcard_prefixes("show alpha-* and beta_module* entries");

        assert_eq!(prefixes, vec!["alpha-".to_string(), "beta~_module".to_string()]);
    }

    #[test]
    fn evidence_text_search_query_uses_selective_suffix_tolerant_windows() {
        let queries = runtime_graph_evidence_text_search_queries(&[
            lexical_evidence_query("Which parameter links Alpha Module to control service?"),
            lexical_evidence_query("Alpha Module"),
            lexical_evidence_query("Alpha"),
            lexical_evidence_query("port 9407"),
            lexical_evidence_query("Which parameter links Alpha Module to control service?"),
        ]);

        assert_eq!(
            queries.first().map(String::as_str),
            Some("'paramet':* & 'modul':* & 'contr':* & 'servi':*"),
        );
        assert_eq!(queries.get(1).map(String::as_str), Some("'port':* & '9407'"));
        assert!(!queries.contains(&"'alph':* & 'modul':*".to_string()));
        assert!(queries.contains(&"'port':* & '9407'".to_string()));
        assert!(!queries.iter().any(|query| {
            query.contains("'which':* & 'paramet':* & 'link':* & 'alph':* & 'modul':* & 'contr':*")
        }));
        assert!(queries.len() <= RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_QUERY_CAP);
    }

    #[test]
    fn evidence_literal_search_queries_keep_exact_structural_spans() {
        let queries = runtime_graph_evidence_literal_search_queries(&[
            lexical_evidence_query("Alpha"),
            literal_evidence_query("Mono Sans"),
            literal_evidence_query("alpha/report://needle?fontSize=12"),
            literal_evidence_query(
                "report://detail?out=display&title=Alpha%20Report%20%(shift.num[d])&font=Mono%20Sans&fontSize=12",
            ),
            literal_evidence_query("port 80"),
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
    fn evidence_literal_search_queries_require_typed_lane_not_name_casing() {
        let queries = runtime_graph_evidence_literal_search_queries(&[
            RuntimeGraphEvidenceSearchQuery::Lexical("Alpha Module".to_string()),
            RuntimeGraphEvidenceSearchQuery::Lexical("mIxEd CaSiNg".to_string()),
            RuntimeGraphEvidenceSearchQuery::LiteralOrFormal("caseless target".to_string()),
        ]);

        assert_eq!(queries, vec!["caseless target".to_string()]);
    }

    #[test]
    fn evidence_literal_search_queries_ignore_lexical_prose_regardless_of_shape() {
        let queries = runtime_graph_evidence_literal_search_queries(&[
            lexical_evidence_query(
                "Find the configuration paragraph that explains how the terminal connects to the control service, include the source document, and keep this cache marker 2026-05-01.",
            ),
            lexical_evidence_query(
                "Which rare entity describes the escalation recipient, what fields are required in the message, and where is the source mentioned?",
            ),
            lexical_evidence_query("recent project"),
            lexical_evidence_query("meeting notes"),
            literal_evidence_query("Alpha Module"),
        ]);

        assert_eq!(queries, vec!["alpha module".to_string()]);
    }

    #[test]
    fn graph_evidence_text_search_bounds_per_query_candidates_under_fanout() {
        assert_eq!(runtime_graph_evidence_per_query_candidate_limit(72, 0), 0);
        assert_eq!(runtime_graph_evidence_per_query_candidate_limit(72, 1), 72);
        assert_eq!(runtime_graph_evidence_per_query_candidate_limit(72, 2), 24);
        assert_eq!(runtime_graph_evidence_per_query_candidate_limit(12, 4), 12);
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
        let queries =
            runtime_graph_evidence_text_search_queries(&[lexical_evidence_query("port 80")]);

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
        let queries = runtime_graph_evidence_text_search_queries(&[lexical_evidence_query(
            "alphacases betagamma deltazeta epsilonkey zetaport thetakey",
        )]);

        assert!(queries.iter().any(|query| query.contains("'deltaze':* & 'epsilonk':*")));
        assert!(!queries.contains(&"'alphacas':* & 'betagam':*".to_string()));
    }

    #[test]
    fn evidence_text_search_query_expands_short_phrases_with_subwindows() {
        let queries = runtime_graph_evidence_text_search_queries(&[lexical_evidence_query(
            "alphacases betagamma deltazeta epsilonkey",
        )]);

        assert_eq!(
            queries.first().map(String::as_str),
            Some("'alphacas':* & 'betagam':* & 'deltaze':* & 'epsilonk':*"),
        );
        assert!(queries.contains(&"'betagam':* & 'deltaze':* & 'epsilonk':*".to_string()));
        assert!(!queries.contains(&"'betagam':* & 'deltaze':*".to_string()));
    }
}
