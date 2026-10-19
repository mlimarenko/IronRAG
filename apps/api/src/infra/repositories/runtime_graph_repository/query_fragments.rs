macro_rules! runtime_graph_node_columns {
    () => {
        "id, library_id, \u{63}anonical_key, label, node_type, aliases_json, summary,
            metadata_json, support_count, projection_version, created_at, updated_at"
    };
}

pub(super) use runtime_graph_node_columns;

macro_rules! runtime_graph_edge_columns {
    () => {
        "id, library_id, from_node_id, to_node_id, relation_type, \u{63}anonical_key,
            summary, weight, support_count, metadata_json, projection_version, created_at, updated_at"
    };
}

pub(super) use runtime_graph_edge_columns;

/// Aggregated graph labels and summaries cannot be separated from their source
/// evidence. Admit a target only when all its evidence is still current.
macro_rules! current_graph_target {
    ($target:literal, $kind:literal) => {
        concat!(
            "exists (select 1 from runtime_graph_evidence visible_evidence
             join content_current_revision current_revision
               on current_revision.document_id = visible_evidence.document_id
              and current_revision.revision_id = visible_evidence.revision_id
              and current_revision.library_id = visible_evidence.library_id
             where visible_evidence.target_id = ",
            $target,
            ".id
               and visible_evidence.library_id = ",
            $target,
            ".library_id
               and visible_evidence.target_kind = '",
            $kind,
            "')
             and not exists (select 1 from runtime_graph_evidence stale_evidence
             left join content_current_revision current_revision
               on current_revision.document_id = stale_evidence.document_id
              and current_revision.revision_id = stale_evidence.revision_id
              and current_revision.library_id = stale_evidence.library_id
             where stale_evidence.target_id = ",
            $target,
            ".id
               and stale_evidence.library_id = ",
            $target,
            ".library_id
               and stale_evidence.target_kind = '",
            $kind,
            "'
               and current_revision.revision_id is null)"
        )
    };
}

/// The shared `on conflict` body every runtime graph node upsert uses.
///
/// A longer non-empty summary wins over a shorter or absent one, so a later
/// merge pass cannot erase a richer summary an earlier pass produced. The
/// optional argument carries assignments only one caller needs, spliced into
/// the same `set` list.
macro_rules! runtime_graph_node_conflict_update {
    () => {
        runtime_graph_node_conflict_update!("")
    };
    ($extra_assignments:expr) => {
        concat!(
            "on conflict (library_id, \u{63}anonical_key, projection_version) do update
         set label = excluded.label,
             node_type = excluded.node_type,
             aliases_json = excluded.aliases_json,
             ",
            $extra_assignments,
            "summary = CASE
                 WHEN excluded.summary IS NOT NULL AND excluded.summary != ''
                      AND (runtime_graph_node.summary IS NULL OR runtime_graph_node.summary = ''
                           OR length(excluded.summary) > length(runtime_graph_node.summary))
                 THEN excluded.summary
                 ELSE runtime_graph_node.summary
             END,
             metadata_json = excluded.metadata_json,
             support_count = excluded.support_count,
             updated_at = now()"
        )
    };
}

pub(super) use runtime_graph_node_conflict_update;

pub(super) fn admitted_runtime_graph_nodes_query(extra_filter: &str) -> String {
    let node_visibility = current_graph_target!("node", "node");
    let edge_visibility = current_graph_target!("edge", "edge");
    format!(
        "with admitted_edges as (
            select edge.from_node_id, edge.to_node_id
            from runtime_graph_edge as edge
            where edge.library_id = $1
               and edge.projection_version = $2
               and {edge_visibility}
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
         select node.id, node.library_id, node.\u{63}anonical_key, node.label, node.node_type,
            node.aliases_json, node.summary, node.metadata_json, node.support_count,
            node.projection_version, node.created_at, node.updated_at
         from runtime_graph_node as node
         left join admitted_edge_endpoints as admitted on admitted.node_id = node.id
         where node.library_id = $1
            and node.projection_version = $2
            and {node_visibility}
           {extra_filter}
           and (
                node.node_type = 'document'
                or admitted.node_id is not null
           )
         order by node.node_type asc, node.label asc, node.created_at asc, node.id asc"
    )
}

pub(super) fn admitted_runtime_graph_counts_query() -> String {
    let node_visibility = current_graph_target!("node", "node");
    let edge_visibility = current_graph_target!("edge", "edge");
    runtime_graph_counts_query("", node_visibility, edge_visibility, "")
}

/// Substitutes only the exact leased candidate for its public source.
/// Other documents retain the public all-evidence membership fence.
pub(super) fn authorized_runtime_graph_counts_query() -> String {
    let sources = r"candidate as (
        select r.id from content_revision r
        join content_document d on d.id=r.document_id
          and d.workspace_id=r.workspace_id and d.library_id=r.library_id
        join ingest_job j on j.knowledge_revision_id=r.id
          and j.knowledge_document_id=d.id and j.workspace_id=r.workspace_id
          and j.library_id=r.library_id
        join ingest_attempt a on a.job_id=j.id
        left join content_mutation m on m.id=j.mutation_id
        left join content_mutation_item i on i.id=j.mutation_item_id
        where r.library_id=$1 and r.document_id=$3 and r.id=$4
          and a.id=$5 and r.workspace_id=$6
          and d.document_state='active' and d.deleted_at is null
          and j.queue_state='leased' and a.attempt_state='leased'
          and not exists (select 1 from ingest_attempt newer
              where newer.job_id=j.id and newer.attempt_number>a.attempt_number)
          and ((j.job_kind='content_mutation'
                and m.workspace_id=r.workspace_id and m.library_id=r.library_id
                and m.mutation_state in ('accepted','running','applied')
                and i.mutation_id=m.id and i.document_id=d.id
                and i.result_revision_id=r.id and i.item_state in ('pending','applied'))
            or (j.job_kind='graph_refresh' and j.mutation_id is null
                and j.mutation_item_id is null and exists (
                    select 1 from content_current_revision c
                    where c.document_id=d.id and c.revision_id=r.id
                      and c.workspace_id=r.workspace_id and c.library_id=r.library_id)))
      ), supported_targets as (
        select evidence.target_id, evidence.target_kind
        from runtime_graph_evidence evidence
        where evidence.library_id=$1
        group by evidence.target_id,evidence.target_kind
        having bool_and(coalesce(
            (evidence.document_id=$3 and evidence.revision_id=$4
             and evidence.activated_by_attempt_id=$5 and exists(select 1 from candidate))
            or (evidence.document_id<>$3 and exists (
                select 1 from content_current_revision c
                where c.document_id=evidence.document_id and c.revision_id=evidence.revision_id
                  and c.library_id=evidence.library_id)), false))
      ), ";
    runtime_graph_counts_query(
        sources,
        "exists(select 1 from supported_targets t where t.target_id=node.id and t.target_kind='node')",
        "exists(select 1 from supported_targets t where t.target_id=edge.id and t.target_kind='edge')",
        "where exists(select 1 from candidate)",
    )
}

/// Public and writer counts share topology and distinct cardinality semantics.
fn runtime_graph_counts_query(
    sources: &str,
    node_visibility: &str,
    edge_visibility: &str,
    authority_filter: &str,
) -> String {
    format!(
        "with {sources}admitted_edges as (
        select edge.id, edge.from_node_id, edge.to_node_id
        from runtime_graph_edge as edge
        where edge.library_id = $1
           and edge.projection_version = $2
           and {edge_visibility}
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
               and {node_visibility}
              and (
                    node.node_type = 'document'
                    or admitted.node_id is not null
              )
        ) as node_count,
        (
            select count(*)
            from admitted_edges
         ) as edge_count
         {authority_filter}"
    )
}
