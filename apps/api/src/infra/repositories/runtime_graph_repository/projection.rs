use std::collections::BTreeSet;

use sqlx::PgPool;
use uuid::Uuid;

use super::coordination::{acquire_runtime_library_graph_lock, release_runtime_library_graph_lock};
use super::query_fragments::{
    admitted_runtime_graph_counts_query, authorized_runtime_graph_counts_query,
};
use super::types::RuntimeGraphProjectionCountsRow;

/// Deletes persisted runtime-graph summary rows whose target node or edge no
/// longer exists in the graph tables.
///
/// The summary table has no FK back to `runtime_graph_node` /
/// `runtime_graph_edge`, so node/edge prune does not cascade. Without this
/// sweep, deleted libraries accumulate stale summary rows that drift from the
/// graph projection (cf. the 15k summary / 27 node skew observed on prod
/// after batch delete).
///
/// The query is bounded by the candidate `node_ids` / `edge_ids` set returned
/// from the pruning pass, so it touches at most one row per pruned target.
///
/// # Errors
/// Returns any `SQLx` error raised while removing active graph summary rows.
pub async fn delete_runtime_graph_summaries_for_orphan_targets(
    pool: &PgPool,
    library_id: Uuid,
    node_ids: &[Uuid],
    edge_ids: &[Uuid],
) -> Result<u64, sqlx::Error> {
    if node_ids.is_empty() && edge_ids.is_empty() {
        return Ok(0);
    }
    let result = sqlx::query(
        "delete from runtime_graph_\u{63}anonical_summary as summary
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

pub async fn delete_runtime_graph_summaries_for_orphan_targets_with_connection(
    connection: &mut sqlx::PgConnection,
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
             (summary.target_kind = 'node' and summary.target_id = any($2)
              and not exists (select 1 from runtime_graph_node node where node.id = summary.target_id))
             or
             (summary.target_kind = 'edge' and summary.target_id = any($3)
              and not exists (select 1 from runtime_graph_edge edge where edge.id = summary.target_id))
           )",
    )
    .bind(library_id)
    .bind(node_ids)
    .bind(edge_ids)
    .execute(&mut *connection)
    .await?;
    Ok(result.rows_affected())
}

/// Recalculates graph node/edge support counters from surviving active evidence.
///
/// # Errors
/// Returns any `SQLx` error raised while updating the graph rows.
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

impl RuntimeGraphProjectionPruneOutcome {
    fn combined_with(self, other: Self) -> Self {
        Self {
            deleted_node_count: self.deleted_node_count.saturating_add(other.deleted_node_count),
            deleted_edge_count: self.deleted_edge_count.saturating_add(other.deleted_edge_count),
            deleted_summary_count: self
                .deleted_summary_count
                .saturating_add(other.deleted_summary_count),
        }
    }
}

/// Recalculates library-wide support counts and prunes unsupported and
/// superseded projection rows, their summaries, and historical orphan evidence
/// in one advisory-locked transaction.
///
/// # Errors
/// Returns any `SQLx` error and rolls the entire support/prune unit back.
pub async fn synchronize_runtime_graph_support_and_prune(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<RuntimeGraphProjectionPruneOutcome, sqlx::Error> {
    let mut transaction = acquire_runtime_library_graph_lock(pool, library_id).await?;
    let outcome = synchronize_runtime_graph_support_and_prune_with_connection(
        &mut transaction,
        library_id,
        projection_version,
    )
    .await?;
    release_runtime_library_graph_lock(transaction, library_id).await?;
    Ok(outcome)
}

pub async fn synchronize_runtime_graph_support_and_prune_with_connection(
    connection: &mut sqlx::PgConnection,
    library_id: Uuid,
    projection_version: i64,
) -> Result<RuntimeGraphProjectionPruneOutcome, sqlx::Error> {
    sqlx::query(RECALCULATE_RUNTIME_GRAPH_NODE_SUPPORT_COUNTS_SQL)
        .bind(library_id)
        .bind(projection_version)
        .execute(&mut *connection)
        .await?;
    sqlx::query(RECALCULATE_RUNTIME_GRAPH_EDGE_SUPPORT_COUNTS_SQL)
        .bind(library_id)
        .bind(projection_version)
        .execute(&mut *connection)
        .await?;
    let unsupported_outcome = prune_runtime_graph_projection_rows_without_support_with_connection(
        connection,
        library_id,
        projection_version,
        &[],
        &[],
    )
    .await?;
    let superseded_outcome = prune_runtime_graph_projection_versions_except_with_connection(
        connection,
        library_id,
        projection_version,
    )
    .await?;
    let deleted_orphan_evidence_count =
        delete_orphan_runtime_graph_evidence_with_connection(connection, library_id).await?;
    tracing::debug!(
        %library_id,
        projection_version,
        deleted_orphan_evidence_count,
        "swept orphan runtime graph evidence during support synchronization",
    );
    Ok(unsupported_outcome.combined_with(superseded_outcome))
}

async fn delete_runtime_graph_evidence_for_targets_with_connection(
    connection: &mut sqlx::PgConnection,
    library_id: Uuid,
    node_ids: &[Uuid],
    edge_ids: &[Uuid],
) -> Result<u64, sqlx::Error> {
    if node_ids.is_empty() && edge_ids.is_empty() {
        return Ok(0);
    }
    sqlx::query(
        "delete from runtime_graph_evidence
         where library_id = $1
           and (
                (target_kind = 'node' and target_id = any($2))
                or (target_kind = 'edge' and target_id = any($3))
           )",
    )
    .bind(library_id)
    .bind(node_ids)
    .bind(edge_ids)
    .execute(&mut *connection)
    .await
    .map(|result| result.rows_affected())
}

async fn delete_orphan_runtime_graph_evidence_with_connection(
    connection: &mut sqlx::PgConnection,
    library_id: Uuid,
) -> Result<u64, sqlx::Error> {
    sqlx::query(
        "delete from runtime_graph_evidence as evidence
         where evidence.library_id = $1
           and not (
             (
               evidence.target_kind = 'node'
               and exists (
                 select 1
                 from runtime_graph_node as node
                 where node.library_id = $1
                   and node.id = evidence.target_id
               )
             )
             or (
               evidence.target_kind = 'edge'
               and exists (
                 select 1
                 from runtime_graph_edge as edge
                 where edge.library_id = $1
                   and edge.id = evidence.target_id
               )
             )
           )",
    )
    .bind(library_id)
    .execute(&mut *connection)
    .await
    .map(|result| result.rows_affected())
}

async fn resolve_runtime_graph_projection_prune_targets_with_connection(
    connection: &mut sqlx::PgConnection,
    library_id: Uuid,
    projection_version: i64,
    node_ids: &[Uuid],
    edge_ids: &[Uuid],
    unsupported_only: bool,
) -> Result<(Vec<Uuid>, Vec<Uuid>), sqlx::Error> {
    let select_entire_projection = node_ids.is_empty() && edge_ids.is_empty();
    let resolved_node_ids = if select_entire_projection {
        sqlx::query_scalar::<_, Uuid>(
            "select id
             from runtime_graph_node
             where library_id = $1
               and projection_version = $2
               and support_count <= 0
             order by id",
        )
        .bind(library_id)
        .bind(projection_version)
        .fetch_all(&mut *connection)
        .await?
    } else if node_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_scalar::<_, Uuid>(
            "select id
             from runtime_graph_node
             where library_id = $1
               and projection_version = $2
               and id = any($3)
               and (not $4 or support_count <= 0)
             order by id",
        )
        .bind(library_id)
        .bind(projection_version)
        .bind(node_ids)
        .bind(unsupported_only)
        .fetch_all(&mut *connection)
        .await?
    };
    let resolved_edge_ids = if select_entire_projection {
        sqlx::query_scalar::<_, Uuid>(
            "select id
             from runtime_graph_edge
             where library_id = $1
               and projection_version = $2
               and support_count <= 0
             order by id",
        )
        .bind(library_id)
        .bind(projection_version)
        .fetch_all(&mut *connection)
        .await?
    } else if edge_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_scalar::<_, Uuid>(
            "select id
             from runtime_graph_edge
             where library_id = $1
               and projection_version = $2
               and id = any($3)
               and (not $4 or support_count <= 0)
             order by id",
        )
        .bind(library_id)
        .bind(projection_version)
        .bind(edge_ids)
        .bind(unsupported_only)
        .fetch_all(&mut *connection)
        .await?
    };
    Ok((resolved_node_ids, resolved_edge_ids))
}

async fn delete_runtime_graph_projection_targets_with_connection(
    connection: &mut sqlx::PgConnection,
    library_id: Uuid,
    node_ids: &[Uuid],
    edge_ids: &[Uuid],
) -> Result<RuntimeGraphProjectionPruneOutcome, sqlx::Error> {
    if node_ids.is_empty() && edge_ids.is_empty() {
        return Ok(RuntimeGraphProjectionPruneOutcome::default());
    }

    // A node delete cascades through both edge endpoint FKs, but polymorphic
    // evidence and summaries have no target FK. Resolve every incident edge
    // before deleting anything so its evidence cannot survive the cascade.
    // Do not projection-filter this lookup: an old cross-version edge still
    // references the same node and will be cascade-deleted with it.
    let incident_edge_ids = list_runtime_graph_edge_ids_by_node_ids_with_executor(
        &mut *connection,
        library_id,
        node_ids,
    )
    .await?;
    let all_edge_ids = edge_ids
        .iter()
        .copied()
        .chain(incident_edge_ids)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    delete_runtime_graph_evidence_for_targets_with_connection(
        connection,
        library_id,
        node_ids,
        &all_edge_ids,
    )
    .await?;
    let deleted_edge_count = sqlx::query(
        "delete from runtime_graph_edge
         where library_id = $1 and id = any($2)",
    )
    .bind(library_id)
    .bind(&all_edge_ids)
    .execute(&mut *connection)
    .await?
    .rows_affected();
    let deleted_node_count = sqlx::query(
        "delete from runtime_graph_node
         where library_id = $1 and id = any($2)",
    )
    .bind(library_id)
    .bind(node_ids)
    .execute(&mut *connection)
    .await?
    .rows_affected();
    let deleted_summary_count = delete_runtime_graph_summaries_for_orphan_targets_with_connection(
        connection,
        library_id,
        node_ids,
        &all_edge_ids,
    )
    .await?;
    Ok(RuntimeGraphProjectionPruneOutcome {
        deleted_node_count,
        deleted_edge_count,
        deleted_summary_count,
    })
}

async fn prune_runtime_graph_projection_versions_except_with_connection(
    connection: &mut sqlx::PgConnection,
    library_id: Uuid,
    projection_version: i64,
) -> Result<RuntimeGraphProjectionPruneOutcome, sqlx::Error> {
    let node_ids = sqlx::query_scalar::<_, Uuid>(
        "select id
         from runtime_graph_node
         where library_id = $1
           and projection_version <> $2
         order by id",
    )
    .bind(library_id)
    .bind(projection_version)
    .fetch_all(&mut *connection)
    .await?;
    let edge_ids = sqlx::query_scalar::<_, Uuid>(
        "select id
         from runtime_graph_edge
         where library_id = $1
           and projection_version <> $2
         order by id",
    )
    .bind(library_id)
    .bind(projection_version)
    .fetch_all(&mut *connection)
    .await?;

    delete_runtime_graph_projection_targets_with_connection(
        connection, library_id, &node_ids, &edge_ids,
    )
    .await
}

/// Prunes zero-support rows from an entire projection or from the supplied
/// target subset, including evidence for edges removed through node cascades.
///
/// The caller owns the surrounding transaction and library graph lock.
///
/// # Errors
/// Returns any `SQLx` error and leaves rollback to the caller.
pub async fn prune_runtime_graph_projection_rows_without_support_with_connection(
    connection: &mut sqlx::PgConnection,
    library_id: Uuid,
    projection_version: i64,
    node_ids: &[Uuid],
    edge_ids: &[Uuid],
) -> Result<RuntimeGraphProjectionPruneOutcome, sqlx::Error> {
    let (node_ids, edge_ids) = resolve_runtime_graph_projection_prune_targets_with_connection(
        connection,
        library_id,
        projection_version,
        node_ids,
        edge_ids,
        true,
    )
    .await?;
    delete_runtime_graph_projection_targets_with_connection(
        connection, library_id, &node_ids, &edge_ids,
    )
    .await
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
    let (node_ids, edge_ids) = resolve_runtime_graph_projection_prune_targets_with_connection(
        &mut transaction,
        library_id,
        projection_version,
        node_ids,
        edge_ids,
        false,
    )
    .await?;
    let outcome = delete_runtime_graph_projection_targets_with_connection(
        &mut transaction,
        library_id,
        &node_ids,
        &edge_ids,
    )
    .await?;
    release_runtime_library_graph_lock(transaction, library_id).await?;
    Ok(outcome)
}

pub async fn prune_runtime_graph_projection_rows_with_connection(
    connection: &mut sqlx::PgConnection,
    library_id: Uuid,
    projection_version: i64,
    node_ids: &[Uuid],
    edge_ids: &[Uuid],
) -> Result<RuntimeGraphProjectionPruneOutcome, sqlx::Error> {
    if node_ids.is_empty() && edge_ids.is_empty() {
        return Ok(RuntimeGraphProjectionPruneOutcome::default());
    }
    let (node_ids, edge_ids) = resolve_runtime_graph_projection_prune_targets_with_connection(
        connection,
        library_id,
        projection_version,
        node_ids,
        edge_ids,
        false,
    )
    .await?;
    delete_runtime_graph_projection_targets_with_connection(
        connection, library_id, &node_ids, &edge_ids,
    )
    .await
}

/// Deletes targeted graph edges with zero surviving active evidence.
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
         returning \u{63}anonical_key",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(edge_ids)
    .fetch_all(pool)
    .await
}

/// Lists graph edge ids incident to any node in `node_ids`.
///
/// # Errors
/// Returns any `SQLx` error raised while querying graph edges.
pub async fn list_runtime_graph_edge_ids_by_node_ids(
    pool: &PgPool,
    library_id: Uuid,
    _projection_version: i64,
    node_ids: &[Uuid],
) -> Result<Vec<Uuid>, sqlx::Error> {
    list_runtime_graph_edge_ids_by_node_ids_with_executor(pool, library_id, node_ids).await
}

pub async fn list_runtime_graph_edge_ids_by_node_ids_with_executor<'e, E>(
    executor: E,
    library_id: Uuid,
    node_ids: &[Uuid],
) -> Result<Vec<Uuid>, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_scalar::<_, Uuid>(
        "select id
         from runtime_graph_edge
         where library_id = $1
           and (from_node_id = any($2) or to_node_id = any($2))",
    )
    .bind(library_id)
    .bind(node_ids)
    .fetch_all(executor)
    .await
}

/// Deletes targeted graph nodes with zero surviving active evidence.
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
         returning \u{63}anonical_key",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(node_ids)
    .fetch_all(pool)
    .await
}

/// Counts admitted graph nodes and relationships for one projection version.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the graph counts.
pub async fn count_admitted_runtime_graph_projection(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
) -> Result<RuntimeGraphProjectionCountsRow, sqlx::Error> {
    count_admitted_runtime_graph_projection_with_executor(pool, library_id, projection_version)
        .await
}

pub async fn count_admitted_runtime_graph_projection_with_executor<'e, E>(
    executor: E,
    library_id: Uuid,
    projection_version: i64,
) -> Result<RuntimeGraphProjectionCountsRow, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query_as::<_, RuntimeGraphProjectionCountsRow>(sqlx::AssertSqlSafe(
        admitted_runtime_graph_counts_query(),
    ))
    .bind(library_id)
    .bind(projection_version)
    .fetch_one(executor)
    .await
}

/// Counts staged materialization on the guard connection, never public membership.
/// Missing exact leased authority is an error rather than a successful empty graph.
pub(crate) async fn count_authorized_runtime_graph_projection_with_connection(
    connection: &mut sqlx::PgConnection,
    authority: crate::infra::knowledge_plane::IngestRevisionWriteAuthority,
    document_id: Uuid,
    projection_version: i64,
) -> Result<RuntimeGraphProjectionCountsRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphProjectionCountsRow>(sqlx::AssertSqlSafe(
        authorized_runtime_graph_counts_query(),
    ))
    .bind(authority.library_id)
    .bind(projection_version)
    .bind(document_id)
    .bind(authority.revision_id)
    .bind(authority.attempt_id)
    .bind(authority.workspace_id)
    .fetch_one(connection)
    .await
}
