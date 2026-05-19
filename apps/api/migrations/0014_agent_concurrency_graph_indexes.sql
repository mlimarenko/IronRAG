-- Generic read-path indexes for concurrent agent/MCP graph exploration.
--
-- Keep the post-release schema change in this single SQLx migration. SQLx
-- executes one migration file as one SQL string, so PostgreSQL cannot run
-- several CREATE INDEX CONCURRENTLY statements here. These additive indexes
-- target the graph read path and are idempotent for pre-release replays.
-- Populated installations should apply this during a normal maintenance
-- window with a role that can create the pg_trgm extension.

create extension if not exists pg_trgm;

create index if not exists idx_runtime_graph_node_projection_entity_support
    on runtime_graph_node (
        library_id,
        projection_version,
        support_count desc,
        label asc,
        created_at asc,
        id asc
    )
    where node_type <> 'document';

create index if not exists idx_runtime_graph_node_entity_label_trgm
    on runtime_graph_node using gin (
        lower(label) gin_trgm_ops
    )
    where node_type <> 'document';

create index if not exists idx_runtime_graph_node_entity_label_exact
    on runtime_graph_node (
        library_id,
        projection_version,
        lower(label),
        support_count desc,
        label asc,
        created_at asc,
        id asc
    )
    where node_type <> 'document';

create index if not exists idx_runtime_graph_node_entity_summary_trgm
    on runtime_graph_node using gin (
        lower(coalesce(summary, '')) gin_trgm_ops
    )
    where node_type <> 'document'
      and summary is not null
      and btrim(summary) <> '';

do $$
begin
    if exists (
        select 1
        from pg_indexes
        where schemaname = current_schema()
          and indexname = 'idx_runtime_graph_node_entity_aliases_trgm'
          and (
              indexdef not like '%lower((aliases_json)::text) gin_trgm_ops%'
              or indexdef not like '%node_type <> ''document''%'
          )
    ) then
        drop index idx_runtime_graph_node_entity_aliases_trgm;
    end if;
end $$;

create index if not exists idx_runtime_graph_node_entity_aliases_trgm
    on runtime_graph_node using gin (
        lower(aliases_json::text) gin_trgm_ops
    )
    where node_type <> 'document';

create index if not exists idx_runtime_graph_edge_projection_support_admitted
    on runtime_graph_edge (
        library_id,
        projection_version,
        support_count desc,
        relation_type asc,
        created_at asc,
        id asc
    )
    where btrim(relation_type) <> ''
      and from_node_id <> to_node_id;

create index if not exists idx_runtime_graph_community_projection_size
    on runtime_graph_community (
        library_id,
        projection_version,
        (cardinality(member_node_ids)) desc,
        id asc
    );
