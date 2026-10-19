use sqlx::{Executor, PgPool, Postgres};
use uuid::Uuid;

use super::types::RuntimeGraphEvidenceRow;

/// Provenance columns a runtime graph evidence identity is built from.
///
/// The named payload keeps the target identity plus every optional identifier
/// from being passed positionally; the same fields also appear on
/// [`CreateRuntimeGraphEvidenceInput`] and [`GraphEvidenceChunkContext`].
pub(super) struct RuntimeGraphEvidenceIdentityInput<'a> {
    pub target_kind: &'a str,
    pub target_id: Uuid,
    pub document_id: Option<Uuid>,
    pub revision_id: Option<Uuid>,
    pub activated_by_attempt_id: Option<Uuid>,
    pub chunk_id: Option<Uuid>,
    pub page_ref: Option<&'a str>,
    pub source_file_name: Option<&'a str>,
}

pub(super) fn runtime_graph_evidence_identity_prefix(
    input: RuntimeGraphEvidenceIdentityInput<'_>,
) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}:{}:{}:",
        input.target_kind,
        input.target_id,
        input.document_id.map_or_else(|| "none".to_string(), |value| value.to_string()),
        input.revision_id.map_or_else(|| "none".to_string(), |value| value.to_string()),
        input.activated_by_attempt_id.map_or_else(|| "none".to_string(), |value| value.to_string()),
        input.chunk_id.map_or_else(|| "none".to_string(), |value| value.to_string()),
        input.page_ref.unwrap_or("none"),
        input.source_file_name.unwrap_or("none"),
    )
}

pub(super) fn runtime_graph_evidence_identity_key(
    input: RuntimeGraphEvidenceIdentityInput<'_>,
    evidence_context_key: &str,
) -> String {
    format!("{}{}", runtime_graph_evidence_identity_prefix(input), evidence_context_key)
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
        RuntimeGraphEvidenceIdentityInput {
            target_kind: input.target_kind,
            target_id: input.target_id,
            document_id: input.document_id,
            revision_id: input.revision_id,
            activated_by_attempt_id: input.activated_by_attempt_id,
            chunk_id: input.chunk_id,
            page_ref: input.page_ref,
            source_file_name: input.source_file_name,
        },
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

/// Chunk-level context shared by every row of one bulk evidence insert.
///
/// The named payload keeps the six optional identifiers and the shared text
/// columns from being passed positionally.
pub struct GraphEvidenceChunkContext<'a> {
    pub library_id: Uuid,
    pub document_id: Option<Uuid>,
    pub revision_id: Option<Uuid>,
    pub activated_by_attempt_id: Option<Uuid>,
    pub chunk_id: Option<Uuid>,
    pub source_file_name: Option<&'a str>,
    pub evidence_text: &'a str,
    pub confidence_score: Option<f64>,
    pub targets: &'a [GraphEvidenceTarget],
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
pub async fn bulk_create_runtime_graph_evidence_for_chunk_with_executor<'e, E>(
    executor: E,
    context: GraphEvidenceChunkContext<'_>,
) -> Result<(), sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    if context.targets.is_empty() {
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
    let count = context.targets.len();
    let mut seen = std::collections::HashSet::with_capacity(count);
    let mut ids = Vec::with_capacity(count);
    let mut identity_keys = Vec::with_capacity(count);
    let mut target_kinds = Vec::with_capacity(count);
    let mut target_ids = Vec::with_capacity(count);
    for target in context.targets {
        let identity_key = runtime_graph_evidence_identity_key(
            RuntimeGraphEvidenceIdentityInput {
                target_kind: target.target_kind,
                target_id: target.target_id,
                document_id: context.document_id,
                revision_id: context.revision_id,
                activated_by_attempt_id: context.activated_by_attempt_id,
                chunk_id: context.chunk_id,
                page_ref: None,
                source_file_name: context.source_file_name,
            },
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
    .bind(context.library_id)
    .bind(context.document_id)
    .bind(context.revision_id)
    .bind(context.activated_by_attempt_id)
    .bind(context.chunk_id)
    .bind(context.source_file_name)
    .bind(context.evidence_text)
    .bind(context.confidence_score)
    .bind(&identity_keys)
    .bind(&target_kinds)
    .bind(&target_ids)
    .execute(executor)
    .await
    .map(|_| ())
}

pub async fn recalculate_runtime_graph_support_counts_by_ids_with_executor(
    connection: &mut sqlx::PgConnection,
    library_id: Uuid,
    projection_version: i64,
    node_ids: &[Uuid],
    edge_ids: &[Uuid],
) -> Result<(u64, u64), sqlx::Error> {
    let mut node_rows_affected = 0_u64;
    for batch in node_ids.chunks(SUPPORT_COUNT_RECALCULATION_BATCH_SIZE) {
        let result = sqlx::query(RECALCULATE_RUNTIME_GRAPH_NODE_SUPPORT_COUNTS_BY_IDS_SQL)
            .bind(library_id)
            .bind(projection_version)
            .bind(batch)
            .execute(&mut *connection)
            .await?;
        node_rows_affected = node_rows_affected.saturating_add(result.rows_affected());
    }
    let mut edge_rows_affected = 0_u64;
    for batch in edge_ids.chunks(SUPPORT_COUNT_RECALCULATION_BATCH_SIZE) {
        let result = sqlx::query(RECALCULATE_RUNTIME_GRAPH_EDGE_SUPPORT_COUNTS_BY_IDS_SQL)
            .bind(library_id)
            .bind(projection_version)
            .bind(batch)
            .execute(&mut *connection)
            .await?;
        edge_rows_affected = edge_rows_affected.saturating_add(result.rows_affected());
    }
    Ok((node_rows_affected, edge_rows_affected))
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

#[cfg(test)]
mod tests {
    use super::{
        RuntimeGraphEvidenceIdentityInput, runtime_graph_evidence_identity_key,
        runtime_graph_evidence_identity_prefix,
    };
    use uuid::Uuid;

    #[test]
    fn identity_prefix_renders_none_sentinels_and_optionals() {
        let target_id = Uuid::from_u128(7);
        let prefix = runtime_graph_evidence_identity_prefix(RuntimeGraphEvidenceIdentityInput {
            target_kind: "node",
            target_id,
            document_id: None,
            revision_id: Some(Uuid::from_u128(3)),
            activated_by_attempt_id: None,
            chunk_id: None,
            page_ref: None,
            source_file_name: Some("file.md"),
        });
        assert_eq!(
            prefix,
            format!("node:{target_id}:none:{}:none:none:none:file.md:", Uuid::from_u128(3))
        );
    }

    #[test]
    fn identity_key_appends_context_to_prefix() {
        let input = || RuntimeGraphEvidenceIdentityInput {
            target_kind: "edge",
            target_id: Uuid::from_u128(9),
            document_id: None,
            revision_id: None,
            activated_by_attempt_id: None,
            chunk_id: None,
            page_ref: Some("p1"),
            source_file_name: None,
        };
        let prefix = runtime_graph_evidence_identity_prefix(input());
        assert_eq!(
            runtime_graph_evidence_identity_key(input(), "entity:foo"),
            format!("{prefix}entity:foo")
        );
    }
}
