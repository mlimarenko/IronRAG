use sqlx::PgPool;
use uuid::Uuid;

use super::super::types::RuntimeGraphDocumentLinkRow;
use super::scan_transaction::begin_topology_scan_tx;

fn runtime_graph_document_links_sql(filter_target_ids: bool) -> String {
    let target_filter = if filter_target_ids { "and evidence.target_id = any($3)" } else { "" };
    format!(
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
               and document.document_state = 'active'
               and document.deleted_at is null
            inner join runtime_graph_node as node
                on node.library_id = evidence.library_id
               and node.id = evidence.target_id
               and node.projection_version = $2
            where evidence.library_id = $1
              and evidence.target_kind = 'node'
              and evidence.document_id is not null
              {target_filter}
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
               and document.document_state = 'active'
               and document.deleted_at is null
            inner join runtime_graph_edge as edge
                on edge.library_id = evidence.library_id
               and edge.id = evidence.target_id
               and edge.projection_version = $2
            where evidence.library_id = $1
              and evidence.target_kind = 'edge'
              and evidence.document_id is not null
              {target_filter}
            group by evidence.document_id, evidence.target_id
        )
        select document_id, target_node_id, target_node_type, relation_type, support_count
        from (
            select * from active_node_links
            union all
            select * from active_edge_links
        ) as links
        order by support_count desc, document_id asc, target_node_id asc"
    )
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
    let rows = sqlx::query_as::<_, RuntimeGraphDocumentLinkRow>(sqlx::AssertSqlSafe(
        runtime_graph_document_links_sql(false),
    ))
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

    sqlx::query_as::<_, RuntimeGraphDocumentLinkRow>(sqlx::AssertSqlSafe(
        runtime_graph_document_links_sql(true),
    ))
    .bind(library_id)
    .bind(projection_version)
    .bind(target_ids)
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::runtime_graph_document_links_sql;

    #[test]
    fn unfiltered_document_links_query_has_no_target_parameter() {
        assert!(!runtime_graph_document_links_sql(false).contains("$3"));
    }

    #[test]
    fn filtered_document_links_query_applies_target_ids_to_both_lanes() {
        assert_eq!(
            runtime_graph_document_links_sql(true).matches("evidence.target_id = any($3)").count(),
            2
        );
    }
}
