-- IronRAG consolidated schema rollup.
--
-- Consolidates the post-init schema into one canonical migration for fresh
-- clones: ingest-job document indexing, content-revision dedup indexing,
-- legacy table/column cleanup, billing library attribution,
-- query-conversation surface, graph topology indexes, web-candidate
-- materialized state, and library-owned web-ingest URL ignore policy.

-- ---------------------------------------------------------------------
-- ingest_job (library_id, knowledge_document_id) keyset index for the
-- dashboard/documents-page LATERAL. Without it the per-document filter
-- degrades to a seq-scan of the whole ingest_job table.
-- ---------------------------------------------------------------------
create index if not exists idx_ingest_job_knowledge_document
    on ingest_job (knowledge_document_id, queue_state, queued_at desc)
    where knowledge_document_id is not null;

-- ---------------------------------------------------------------------
-- content_revision per-library dedup probe index.
-- Upload and web-ingest paths check this index before creating a new
-- content_document; `(library_id, checksum)` is the hot composite key.
-- ---------------------------------------------------------------------
create index if not exists idx_content_revision_library_checksum
    on content_revision (library_id, checksum);

-- ---------------------------------------------------------------------
-- Legacy cleanup: runtime_vector_target (pgvector-era store, nothing
-- reads it; vector search runs in Arango now), the `superseded_at`
-- flag column that was always null on live rows, and the `is_active`
-- flag on runtime_graph_evidence that was always true on live rows.
-- ---------------------------------------------------------------------
drop table if exists runtime_vector_target;

alter table runtime_graph_canonical_summary
    drop column if exists superseded_at;

drop index if exists idx_runtime_graph_evidence_library_document_active;
drop index if exists idx_runtime_graph_evidence_library_document_revision_active;
drop index if exists idx_runtime_graph_evidence_library_target_active;

alter table runtime_graph_evidence
    drop column if exists is_active,
    drop column if exists deactivated_by_mutation_id;

create index if not exists idx_runtime_graph_evidence_library_document
    on runtime_graph_evidence (library_id, document_id, created_at desc);
create index if not exists idx_runtime_graph_evidence_library_document_revision
    on runtime_graph_evidence (library_id, document_id, revision_id, created_at desc);
create index if not exists idx_runtime_graph_evidence_library_target
    on runtime_graph_evidence (library_id, target_kind, target_id, created_at desc);

-- ---------------------------------------------------------------------
-- Billing library attribution + hot-path indexes. Without the
-- attribution columns, get_library_cost_summary had to re-resolve
-- execution ownership through billing_provider_call, which doubled
-- totals on executions with multiple provider calls. The FK columns
-- used on the billing read path were also unindexed.
-- ---------------------------------------------------------------------
alter table billing_execution_cost
    add column if not exists workspace_id uuid,
    add column if not exists library_id uuid,
    add column if not exists knowledge_document_id uuid;

update billing_execution_cost bec
set workspace_id = sub.workspace_id,
    library_id = sub.library_id,
    knowledge_document_id = sub.knowledge_document_id
from (
    select distinct on (bpc.owning_execution_kind, bpc.owning_execution_id)
        bpc.owning_execution_kind,
        bpc.owning_execution_id,
        bpc.workspace_id,
        bpc.library_id,
        coalesce(ij.knowledge_document_id, rge.document_id) as knowledge_document_id
    from billing_provider_call bpc
    left join ingest_attempt ia
        on ia.id = bpc.owning_execution_id
        and bpc.owning_execution_kind = 'ingest_attempt'
    left join ingest_job ij
        on ij.id = ia.job_id
    left join runtime_graph_extraction rge
        on rge.id = bpc.owning_execution_id
        and bpc.owning_execution_kind = 'graph_extraction_attempt'
) sub
where bec.owning_execution_kind = sub.owning_execution_kind
  and bec.owning_execution_id = sub.owning_execution_id
  and (bec.library_id is null or bec.workspace_id is null);

update billing_execution_cost bec
set workspace_id = qe.workspace_id,
    library_id = qe.library_id
from query_execution qe
where bec.owning_execution_kind = 'query_execution'::billing_owning_execution_kind
  and bec.owning_execution_id = qe.id
  and bec.library_id is null;

do $$
declare
    orphan_count bigint;
begin
    select count(*) into orphan_count
    from billing_execution_cost
    where library_id is null;

    if orphan_count > 0 then
        raise notice
            'billing_execution_cost: dropping % orphan row(s) with unresolvable library_id',
            orphan_count;
    end if;
end;
$$;

delete from billing_execution_cost where library_id is null;

do $$
begin
    if exists (
        select 1 from information_schema.columns
        where table_name = 'billing_execution_cost'
          and column_name = 'workspace_id'
          and is_nullable = 'YES'
    ) then
        alter table billing_execution_cost alter column workspace_id set not null;
    end if;
    if exists (
        select 1 from information_schema.columns
        where table_name = 'billing_execution_cost'
          and column_name = 'library_id'
          and is_nullable = 'YES'
    ) then
        alter table billing_execution_cost alter column library_id set not null;
    end if;
end;
$$;

do $$
begin
    if not exists (
        select 1 from pg_constraint
        where conname = 'billing_execution_cost_library_workspace_fkey'
    ) then
        alter table billing_execution_cost
            add constraint billing_execution_cost_library_workspace_fkey
                foreign key (library_id, workspace_id)
                references catalog_library(id, workspace_id)
                on delete cascade;
    end if;
end;
$$;

create index if not exists idx_billing_execution_cost_library
    on billing_execution_cost (library_id);

create index if not exists idx_billing_execution_cost_library_document
    on billing_execution_cost (library_id, knowledge_document_id)
    where knowledge_document_id is not null;

create index if not exists idx_billing_provider_call_library
    on billing_provider_call (library_id);

create index if not exists idx_billing_usage_provider_call
    on billing_usage (provider_call_id);

create index if not exists idx_billing_charge_usage
    on billing_charge (usage_id);

-- ---------------------------------------------------------------------
-- query_conversation.request_surface — lets the /v1/query/sessions UI
-- listing exclude conversations spawned inside MCP tool calls without
-- a LATERAL over per-turn surface_kind.
-- ---------------------------------------------------------------------
alter table query_conversation
    add column if not exists request_surface surface_kind;

update query_conversation
set request_surface = case
    when title like '[MCP]%' then 'mcp'::surface_kind
    else 'ui'::surface_kind
end
where request_surface is null;

do $$
begin
    if exists (
        select 1 from information_schema.columns
        where table_name = 'query_conversation'
          and column_name = 'request_surface'
          and is_nullable = 'YES'
    ) then
        alter table query_conversation alter column request_surface set not null;
    end if;
end;
$$;

alter table query_conversation
    alter column request_surface set default 'ui'::surface_kind;

create index if not exists idx_query_conversation_library_surface_updated
    on query_conversation (library_id, request_surface, updated_at desc);

-- ---------------------------------------------------------------------
-- Graph topology query indexes. Cut ~25 s off the cold-cache
-- /v1/knowledge/libraries/{id}/graph cumulative cost on a mid-size
-- corpus (edges + nodes + evidence aggregates).
-- ---------------------------------------------------------------------
create index if not exists idx_runtime_graph_evidence_library_target_document
    on runtime_graph_evidence (library_id, target_kind, document_id, target_id)
    where document_id is not null;

create index if not exists idx_runtime_graph_node_library_projection_type
    on runtime_graph_node (library_id, projection_version, node_type);

-- ---------------------------------------------------------------------
-- web_candidate_state enum: add `materialized` for the post-fetch /
-- pre-promote gap so a web page is not reported terminal-successful
-- until the downstream content-mutation worker promotes the document
-- head. ALTER TYPE ADD VALUE is allowed inside a transaction since
-- Postgres 12 as long as the type was not created in the same
-- transaction (this one was created in 0001_init.sql, not here).
-- ---------------------------------------------------------------------
alter type web_candidate_state add value if not exists 'materialized' after 'processing';

-- ---------------------------------------------------------------------
-- Web-ingest URL ignore policy. Libraries own the reusable policy; each
-- run stores an immutable snapshot of the exact ignore rules used during
-- discovery so historical runs stay explainable after the library policy
-- changes.
-- ---------------------------------------------------------------------
alter table catalog_library
    add column if not exists web_ingest_policy jsonb not null default '{
        "ignorePatterns": [
            {"kind": "path_prefix", "value": "/aboutconfluencepage.action"},
            {"kind": "path_prefix", "value": "/collector/pages.action"},
            {"kind": "path_prefix", "value": "/dashboard/configurerssfeed.action"},
            {"kind": "path_prefix", "value": "/exportword"},
            {"kind": "path_prefix", "value": "/forgotuserpassword.action"},
            {"kind": "path_prefix", "value": "/labels/viewlabel.action"},
            {"kind": "path_prefix", "value": "/login.action"},
            {"kind": "path_prefix", "value": "/pages/diffpages.action"},
            {"kind": "path_prefix", "value": "/pages/diffpagesbyversion.action"},
            {"kind": "path_prefix", "value": "/pages/listundefinedpages.action"},
            {"kind": "path_prefix", "value": "/pages/reorderpages.action"},
            {"kind": "path_prefix", "value": "/pages/viewinfo.action"},
            {"kind": "path_prefix", "value": "/pages/viewpageattachments.action"},
            {"kind": "path_prefix", "value": "/pages/viewpreviousversions.action"},
            {"kind": "path_prefix", "value": "/plugins/viewsource/viewpagesrc.action"},
            {"kind": "path_prefix", "value": "/spacedirectory/view.action"},
            {"kind": "path_prefix", "value": "/spaces/flyingpdf/pdfpageexport.action"},
            {"kind": "path_prefix", "value": "/spaces/listattachmentsforspace.action"},
            {"kind": "path_prefix", "value": "/spaces/listrssfeeds.action"},
            {"kind": "path_prefix", "value": "/spaces/viewspacesummary.action"},
            {"kind": "glob", "value": "*/display/~*"},
            {"kind": "glob", "value": "*os_destination=*"},
            {"kind": "glob", "value": "*permissionviolation=*"}
        ]
    }'::jsonb;

alter table content_web_ingest_run
    add column if not exists ignore_patterns jsonb not null default '[]'::jsonb;

alter table content_web_discovered_page
    add column if not exists classification_detail text;

do $$
begin
    if not exists (
        select 1
        from pg_constraint
        where conname = 'catalog_library_web_ingest_policy_object_check'
    ) then
        alter table catalog_library
            add constraint catalog_library_web_ingest_policy_object_check
            check (jsonb_typeof(web_ingest_policy) = 'object');
    end if;

    if not exists (
        select 1
        from pg_constraint
        where conname = 'content_web_ingest_run_ignore_patterns_array_check'
    ) then
        alter table content_web_ingest_run
            add constraint content_web_ingest_run_ignore_patterns_array_check
            check (jsonb_typeof(ignore_patterns) = 'array');
    end if;
end;
$$;
