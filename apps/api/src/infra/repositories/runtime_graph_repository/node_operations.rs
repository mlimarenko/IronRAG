use sqlx::{Executor, PgPool, Postgres};
use uuid::Uuid;

use super::query_fragments::{runtime_graph_node_columns, runtime_graph_node_conflict_update};
use super::types::{RuntimeGraphNodeIdRow, RuntimeGraphNodeRow, RuntimeGraphSubTypeHintRow};

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
pub(super) const RUNTIME_GRAPH_LABEL_MAX_BYTES: usize = 2000;

/// Clamps a graph-node label to [`RUNTIME_GRAPH_LABEL_MAX_BYTES`] without
/// splitting a UTF-8 codepoint. Script-agnostic: it bounds the byte length
/// only and never inspects, transliterates, or matches against any
/// natural-language content. Returns the input unchanged when it already fits.
pub(super) fn clamp_runtime_graph_label(label: &str) -> &str {
    if label.len() <= RUNTIME_GRAPH_LABEL_MAX_BYTES {
        return label;
    }
    let mut end = RUNTIME_GRAPH_LABEL_MAX_BYTES;
    while end > 0 && !label.is_char_boundary(end) {
        end -= 1;
    }
    &label[..end]
}

/// Values required to upsert one active runtime graph node.
///
/// The named payload keeps the node's identity and text fields from being
/// passed positionally next to the projection and support counters.
pub struct RuntimeGraphNodeInput<'a> {
    pub library_id: Uuid,
    pub identity_key: &'a str,
    pub label: &'a str,
    pub node_type: &'a str,
    pub document_id: Option<Uuid>,
    pub aliases_json: serde_json::Value,
    pub summary: Option<&'a str>,
    pub metadata_json: serde_json::Value,
    pub support_count: i32,
    pub projection_version: i64,
}

/// Upserts a active runtime graph node.
///
/// # Errors
/// Returns any `SQLx` error raised while inserting or updating the graph node.
pub async fn upsert_runtime_graph_node(
    pool: &PgPool,
    input: RuntimeGraphNodeInput<'_>,
) -> Result<RuntimeGraphNodeRow, sqlx::Error> {
    let RuntimeGraphNodeInput {
        library_id,
        identity_key,
        label,
        node_type,
        document_id,
        aliases_json,
        summary,
        metadata_json,
        support_count,
        projection_version,
    } = input;
    sqlx::query_as::<_, RuntimeGraphNodeRow>(concat!(
        "insert into runtime_graph_node (
            id, library_id, \u{63}anonical_key, label, node_type, document_id, aliases_json, summary,
            metadata_json, support_count, projection_version
         ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
         ",
        runtime_graph_node_conflict_update!("document_id = excluded.document_id,\n             "),
        "
         returning id, library_id, \u{63}anonical_key, label, node_type, aliases_json, summary,
            metadata_json, support_count, projection_version, created_at, updated_at",
    ))
    .bind(Uuid::now_v7())
    .bind(library_id)
    .bind(identity_key)
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

/// Upserts the one authoritative source-document node for a logical document.
///
/// Document nodes have a second uniqueness contract: exactly one
/// `(library_id, identity_key, projection_version)` row whose
/// `identity_key` is derived from the document id. Multi-chunk graph
/// rebuilds may merge chunks in parallel, so this path uses the same global
/// identity-key conflict target that can fire during concurrent inserts.
pub async fn upsert_runtime_graph_document_node(
    pool: &PgPool,
    input: RuntimeGraphDocumentNodeInput<'_>,
) -> Result<RuntimeGraphNodeRow, sqlx::Error> {
    upsert_runtime_graph_document_node_with_executor(pool, input).await
}

pub async fn upsert_runtime_graph_document_node_with_executor<'e, E>(
    executor: E,
    input: RuntimeGraphDocumentNodeInput<'_>,
) -> Result<RuntimeGraphNodeRow, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, RuntimeGraphNodeRow>(concat!(
        "with document_node_lock as (
            select pg_advisory_xact_lock(
                hashtextextended($2::text || ':' || $5::text || ':' || $10::text, 0)
            )
         )
         insert into runtime_graph_node (
            id, library_id, \u{63}anonical_key, label, node_type, document_id, aliases_json, summary,
            metadata_json, support_count, projection_version
         )
         select $1, $2, $3, $4, 'document', $5, $6, $7, $8, $9, $10
         from document_node_lock
         ",
        runtime_graph_node_conflict_update!("document_id = excluded.document_id,\n             "),
        "
         returning id, library_id, \u{63}anonical_key, label, node_type, aliases_json, summary,
            metadata_json, support_count, projection_version, created_at, updated_at",
    ))
    .bind(Uuid::now_v7())
    .bind(input.library_id)
    .bind(input.identity_key)
    .bind(clamp_runtime_graph_label(input.label))
    .bind(input.document_id)
    .bind(input.aliases_json)
    .bind(input.summary)
    .bind(input.metadata_json)
    .bind(input.support_count)
    .bind(input.projection_version)
    .fetch_one(executor)
    .await
}

/// Values required to upsert the one authoritative source-document node.
///
/// The named payload keeps the document node's identity fields from being
/// passed positionally next to the projection and support counters.
pub struct RuntimeGraphDocumentNodeInput<'a> {
    pub library_id: Uuid,
    pub document_id: Uuid,
    pub identity_key: &'a str,
    pub label: &'a str,
    pub aliases_json: serde_json::Value,
    pub summary: Option<&'a str>,
    pub metadata_json: serde_json::Value,
    pub support_count: i32,
    pub projection_version: i64,
}

/// Loads one active runtime graph node for a projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph node.
pub async fn get_runtime_graph_node_by_key(
    pool: &PgPool,
    library_id: Uuid,
    identity_key: &str,
    projection_version: i64,
) -> Result<Option<RuntimeGraphNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphNodeRow>(concat!(
        "select ",
        runtime_graph_node_columns!(),
        "
         from runtime_graph_node
         where library_id = $1 and \u{63}anonical_key = $2 and projection_version = $3"
    ))
    .bind(library_id)
    .bind(identity_key)
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
    pub identity_key: String,
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
/// may contain duplicate identity keys; the last duplicate wins per
/// ON CONFLICT semantics, matching what the serial fan-out path did
/// under race conditions.
///
/// RETURNING order is not guaranteed to match input order. Callers
/// index the result by `identity_key`.
///
/// # Errors
/// Returns any `SQLx` error raised while persisting the graph nodes.
pub async fn bulk_upsert_runtime_graph_nodes(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    inputs: &[RuntimeGraphNodeUpsertInput],
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error> {
    bulk_upsert_runtime_graph_nodes_with_executor(pool, library_id, projection_version, inputs)
        .await
}

pub async fn bulk_upsert_runtime_graph_nodes_with_executor<'e, E>(
    executor: E,
    library_id: Uuid,
    projection_version: i64,
    inputs: &[RuntimeGraphNodeUpsertInput],
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    if inputs.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<Uuid> = (0..inputs.len()).map(|_| Uuid::now_v7()).collect();
    let identity_keys: Vec<&str> = inputs.iter().map(|i| i.identity_key.as_str()).collect();
    let labels: Vec<&str> =
        inputs.iter().map(|i| clamp_runtime_graph_label(i.label.as_str())).collect();
    let node_types: Vec<&str> = inputs.iter().map(|i| i.node_type.as_str()).collect();
    let aliases_jsons: Vec<serde_json::Value> =
        inputs.iter().map(|i| i.aliases_json.clone()).collect();
    let summaries: Vec<Option<&str>> = inputs.iter().map(|i| i.summary.as_deref()).collect();
    let metadatas: Vec<serde_json::Value> =
        inputs.iter().map(|i| i.metadata_json.clone()).collect();
    let supports: Vec<i32> = inputs.iter().map(|i| i.support_count).collect();

    sqlx::query_as::<_, RuntimeGraphNodeRow>(concat!(
        "insert into runtime_graph_node (
            id, library_id, \u{63}anonical_key, label, node_type, aliases_json, summary,
            metadata_json, support_count, projection_version
         )
         select
            t.id, $1::uuid, t.\u{63}anonical_key, t.label, t.node_type, t.aliases_json,
            t.summary, t.metadata_json, t.support_count, $2::bigint
         from unnest(
            $3::uuid[], $4::text[], $5::text[], $6::text[], $7::jsonb[],
            $8::text[], $9::jsonb[], $10::int[]
         ) as t(id, \u{63}anonical_key, label, node_type, aliases_json, summary, metadata_json, support_count)
         ",
        runtime_graph_node_conflict_update!(),
        "
         returning id, library_id, \u{63}anonical_key, label, node_type, aliases_json, summary,
            metadata_json, support_count, projection_version, created_at, updated_at",
    ))
    .bind(library_id)
    .bind(projection_version)
    .bind(&ids)
    .bind(&identity_keys)
    .bind(&labels)
    .bind(&node_types)
    .bind(&aliases_jsons)
    .bind(&summaries)
    .bind(&metadatas)
    .bind(&supports)
    .fetch_all(executor)
    .await
}

/// Bulk-loads active runtime graph nodes for a projection version by
/// identity key set. One round-trip replaces N single-key lookups — on a
/// chunk merge with 15 entities and 10 relations this collapses ~35
/// sequential `get_runtime_graph_node_by_key` calls into one indexed
/// range scan, reducing pool-hold time and lock-wait pressure during
/// `merge_chunk_graph_candidates`.
///
/// Returns the rows in the same order they appear in `identity_keys`.
/// Keys with no matching row are simply absent from the result.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph nodes.
pub async fn list_runtime_graph_nodes_by_identity_keys(
    pool: &PgPool,
    library_id: Uuid,
    identity_keys: &[String],
    projection_version: i64,
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error> {
    list_runtime_graph_nodes_by_identity_keys_with_executor(
        pool,
        library_id,
        identity_keys,
        projection_version,
    )
    .await
}

pub async fn list_runtime_graph_nodes_by_identity_keys_with_executor<'e, E>(
    executor: E,
    library_id: Uuid,
    identity_keys: &[String],
    projection_version: i64,
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    if identity_keys.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, RuntimeGraphNodeRow>(concat!(
        "select ",
        runtime_graph_node_columns!(),
        "
         from runtime_graph_node
         where library_id = $1
           and projection_version = $2
           and \u{63}anonical_key = any($3)"
    ))
    .bind(library_id)
    .bind(projection_version)
    .bind(identity_keys)
    .fetch_all(executor)
    .await
}

/// Loads one active runtime graph node by id.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph node.
pub async fn get_runtime_graph_node_by_id(
    pool: &PgPool,
    library_id: Uuid,
    id: Uuid,
) -> Result<Option<RuntimeGraphNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphNodeRow>(concat!(
        "select ",
        runtime_graph_node_columns!(),
        "
         from runtime_graph_node
         where library_id = $1 and id = $2"
    ))
    .bind(library_id)
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Lists active runtime graph nodes for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph nodes.
pub async fn list_runtime_graph_nodes_by_library(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphNodeRow>(concat!(
        "select ",
        runtime_graph_node_columns!(),
        "
         from runtime_graph_node
         where library_id = $1 and projection_version = $2
         order by node_type asc, label asc, created_at asc, id asc"
    ))
    .bind(library_id)
    .bind(projection_version)
    .fetch_all(pool)
    .await
}

/// Lists node ids only — the lean projection consumed by community
/// detection / label propagation. Skips the heavy text columns (`summary`,
/// `metadata_json`, `aliases_json`, `identity_key`, `label`, `node_type`)
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

/// Ranks the distinct observed `sub_type` values per `node_type` for one
/// library at a given projection version. Drives vocabulary-aware extraction:
/// the returned rows feed the `sub_type_hints` prompt section so the LLM
/// converges on terms already present in the graph instead of inventing fresh
/// near-duplicates.
///
/// Rows are ordered `node_type asc, occurrences desc, sub_type asc`. The
/// ordering is total and deterministic even on occurrence ties, so the caller's
/// top-N selection is reproducible for a fixed graph snapshot. The count is a
/// selection rank, never rendered: the caller renders the chosen set sorted
/// alphabetically and without counts, so the prompt does not drift while the
/// chosen set is stable (IRG-197).
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
