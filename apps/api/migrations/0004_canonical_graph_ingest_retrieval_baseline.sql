-- Canonical graph, ingest, retrieval, and integration baseline.
--
-- `content_document` is the only source of truth for documents that may be
-- shown as source documents in graph topology. Runtime graph rows with
-- `node_type = 'document'` are reserved for active content documents and carry
-- an explicit FK so soft-deleted/import-corrupt/model-produced document nodes
-- cannot survive as visible graph documents.
--
-- This baseline also owns the canonical post-rollup schema additions for
-- web-ingest URL filters, recognition policy, graph topology generation,
-- outbound webhooks, grounded-answer replay caches, and retrieval support
-- tables.

alter table runtime_graph_node
    add column if not exists document_id uuid references content_document(id) on delete cascade;

with document_node_candidates as (
    select
        node.id as node_id,
        node.library_id,
        node.projection_version,
        node.canonical_key,
        case
            when node.metadata_json ? 'document_id'
             and node.metadata_json->>'document_id'
                 ~* '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
            then (node.metadata_json->>'document_id')::uuid
            when node.canonical_key
                 ~* '^document:[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
            then substring(node.canonical_key from 10)::uuid
            else null
        end as document_id
    from runtime_graph_node as node
    where node.node_type = 'document'
),
valid_document_nodes as (
    select
        candidate.node_id,
        candidate.library_id,
        candidate.projection_version,
        document.id as document_id
    from document_node_candidates as candidate
    join content_document as document
      on document.id = candidate.document_id
     and document.library_id = candidate.library_id
     and document.document_state = 'active'
     and document.deleted_at is null
    where candidate.canonical_key = 'document:' || candidate.document_id::text
),
ranked_document_nodes as (
    select
        node_id,
        document_id,
        row_number() over (
            partition by library_id, projection_version, document_id
            order by node_id
        ) as rank_within_document
    from valid_document_nodes
),
canonical_document_nodes as (
    select node_id, document_id
    from ranked_document_nodes
    where rank_within_document = 1
)
update runtime_graph_node as node
set document_id = canonical.document_id
from canonical_document_nodes as canonical
where node.id = canonical.node_id;

with invalid_document_nodes as (
    select node.id
    from runtime_graph_node as node
    where node.node_type = 'document'
      and node.document_id is null
),
invalid_edges as (
    select edge.id
    from runtime_graph_edge as edge
    where edge.from_node_id in (select id from invalid_document_nodes)
       or edge.to_node_id in (select id from invalid_document_nodes)
),
deleted_node_evidence as (
    delete from runtime_graph_evidence as evidence
    where evidence.target_kind = 'node'
      and evidence.target_id in (select id from invalid_document_nodes)
    returning 1
),
deleted_edge_evidence as (
    delete from runtime_graph_evidence as evidence
    where evidence.target_kind = 'edge'
      and evidence.target_id in (select id from invalid_edges)
    returning 1
),
deleted_node_summaries as (
    delete from runtime_graph_canonical_summary as summary
    where summary.target_kind = 'node'
      and summary.target_id in (select id from invalid_document_nodes)
    returning 1
),
deleted_edge_summaries as (
    delete from runtime_graph_canonical_summary as summary
    where summary.target_kind = 'edge'
      and summary.target_id in (select id from invalid_edges)
    returning 1
)
delete from runtime_graph_node as node
where node.id in (select id from invalid_document_nodes);

delete from runtime_graph_evidence as evidence
where (
    evidence.target_kind = 'node'
    and not exists (
        select 1
        from runtime_graph_node as node
        where node.id = evidence.target_id
    )
)
or (
    evidence.target_kind = 'edge'
    and not exists (
        select 1
        from runtime_graph_edge as edge
        where edge.id = evidence.target_id
    )
);

delete from runtime_graph_canonical_summary as summary
where (
    summary.target_kind = 'node'
    and not exists (
        select 1
        from runtime_graph_node as node
        where node.id = summary.target_id
    )
)
or (
    summary.target_kind = 'edge'
    and not exists (
        select 1
        from runtime_graph_edge as edge
        where edge.id = summary.target_id
    )
);

with admitted_edges as (
    select
        edge.library_id,
        edge.projection_version,
        edge.id,
        edge.from_node_id,
        edge.to_node_id
    from runtime_graph_edge as edge
    join runtime_graph_node as source
      on source.library_id = edge.library_id
     and source.id = edge.from_node_id
     and source.projection_version = edge.projection_version
    join runtime_graph_node as target
      on target.library_id = edge.library_id
     and target.id = edge.to_node_id
     and target.projection_version = edge.projection_version
    left join content_document as source_document
      on source_document.id = source.document_id
     and source_document.library_id = source.library_id
     and source_document.document_state = 'active'
     and source_document.deleted_at is null
    left join content_document as target_document
      on target_document.id = target.document_id
     and target_document.library_id = target.library_id
     and target_document.document_state = 'active'
     and target_document.deleted_at is null
    join runtime_graph_snapshot as snapshot
      on snapshot.library_id = edge.library_id
     and snapshot.projection_version = edge.projection_version
    where btrim(edge.relation_type) <> ''
      and edge.from_node_id <> edge.to_node_id
      and (source.node_type <> 'document' or source_document.id is not null)
      and (target.node_type <> 'document' or target_document.id is not null)
),
admitted_edge_endpoints as (
    select library_id, projection_version, from_node_id as node_id
    from admitted_edges
    union
    select library_id, projection_version, to_node_id as node_id
    from admitted_edges
),
node_counts as (
    select
        snapshot.library_id,
        count(node.id)::integer as node_count
    from runtime_graph_snapshot as snapshot
    left join runtime_graph_node as node
      on node.library_id = snapshot.library_id
     and node.projection_version = snapshot.projection_version
    left join admitted_edge_endpoints as admitted
      on admitted.library_id = node.library_id
     and admitted.projection_version = node.projection_version
     and admitted.node_id = node.id
    left join content_document as document
      on document.id = node.document_id
     and document.library_id = node.library_id
     and document.document_state = 'active'
     and document.deleted_at is null
    where node.id is null
       or (
            node.node_type = 'document'
            and document.id is not null
       )
       or (
            node.node_type <> 'document'
            and admitted.node_id is not null
       )
    group by snapshot.library_id
),
edge_counts as (
    select
        snapshot.library_id,
        count(admitted_edges.id)::integer as edge_count
    from runtime_graph_snapshot as snapshot
    left join admitted_edges
      on admitted_edges.library_id = snapshot.library_id
     and admitted_edges.projection_version = snapshot.projection_version
    group by snapshot.library_id
)
update runtime_graph_snapshot as snapshot
set node_count = coalesce(node_counts.node_count, 0),
    edge_count = coalesce(edge_counts.edge_count, 0),
    updated_at = now()
from node_counts, edge_counts
where node_counts.library_id = snapshot.library_id
  and edge_counts.library_id = snapshot.library_id;

create index if not exists idx_runtime_graph_edge_projection_relation_created
    on runtime_graph_edge (
        library_id,
        projection_version,
        relation_type asc,
        created_at asc
    )
    where btrim(relation_type) <> ''
      and from_node_id <> to_node_id;

create index if not exists idx_runtime_graph_node_projection_type_label_created
    on runtime_graph_node (
        library_id,
        projection_version,
        node_type asc,
        label asc,
        created_at asc
    );

create table if not exists query_result_cache (
    cache_key text primary key,
    workspace_id uuid not null,
    library_id uuid not null,
    source_execution_id uuid not null references query_execution(id) on delete cascade,
    readable_content_fingerprint text not null,
    graph_projection_version bigint not null,
    graph_topology_generation bigint not null,
    binding_fingerprint text not null,
    hit_count integer not null default 0,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    foreign key (library_id, workspace_id)
        references catalog_library(id, workspace_id)
        on delete cascade
);

create index if not exists idx_query_result_cache_library_updated
    on query_result_cache (library_id, updated_at desc);

create index if not exists idx_query_result_cache_source_execution
    on query_result_cache (source_execution_id);

create table if not exists query_execution_replay (
    id uuid primary key default uuidv7(),
    workspace_id uuid not null,
    library_id uuid not null,
    conversation_id uuid not null references query_conversation(id) on delete cascade,
    request_turn_id uuid not null references query_turn(id) on delete cascade,
    response_turn_id uuid not null references query_turn(id) on delete cascade,
    source_execution_id uuid not null references query_execution(id) on delete cascade,
    cache_key text not null,
    created_at timestamptz not null default now(),
    foreign key (library_id, workspace_id)
        references catalog_library(id, workspace_id)
        on delete cascade
);

create unique index if not exists idx_query_execution_replay_request_turn
    on query_execution_replay (request_turn_id);

create unique index if not exists idx_query_execution_replay_response_turn
    on query_execution_replay (response_turn_id);

create index if not exists idx_query_execution_replay_source_execution
    on query_execution_replay (source_execution_id, created_at desc);

create index if not exists idx_query_execution_replay_conversation
    on query_execution_replay (conversation_id, created_at desc);

delete from query_result_cache;

delete from query_execution_replay;

delete from query_ir_cache;

create extension if not exists pg_trgm;

create index if not exists idx_runtime_graph_evidence_library_text_ready
    on runtime_graph_evidence (library_id, created_at desc, id desc)
    where btrim(evidence_text) <> '';

create index if not exists idx_runtime_graph_evidence_text_search
    on runtime_graph_evidence using gin (
        to_tsvector('simple', evidence_text || ' ' || coalesce(source_file_name, ''))
    )
    where btrim(evidence_text) <> '';

create index if not exists idx_runtime_graph_evidence_literal_text_trgm
    on runtime_graph_evidence using gin (
        lower(evidence_text || ' ' || coalesce(source_file_name, '')) gin_trgm_ops
    )
    where btrim(evidence_text) <> '';

create index if not exists idx_runtime_graph_node_document
    on runtime_graph_node (library_id, document_id, projection_version)
    where document_id is not null;

create unique index if not exists idx_runtime_graph_node_unique_document
    on runtime_graph_node (library_id, document_id, projection_version)
    where node_type = 'document';

create index if not exists idx_runtime_graph_extraction_document_revision_status
    on runtime_graph_extraction (
        document_id,
        (raw_output_json #>> '{lifecycle,revision_id}'),
        status
    );

create index if not exists idx_runtime_graph_extraction_cache_lookup
    on runtime_graph_extraction (
        library_id,
        extraction_version,
        provider_kind,
        model_name,
        prompt_hash,
        status,
        created_at desc
    );

alter table runtime_graph_node
    drop constraint if exists runtime_graph_node_document_identity_ck;

alter table runtime_graph_node
    add constraint runtime_graph_node_document_identity_ck
    check (
        (node_type = 'document' and document_id is not null)
        or (node_type <> 'document' and document_id is null)
    );

do $$
begin
    if not exists (select 1 from pg_type where typname = 'web_url_filter_mode') then
        create type web_url_filter_mode as enum ('blocklist', 'allowlist');
    end if;
end;
$$;

alter table catalog_library
    alter column web_ingest_policy set default '{
        "urlFilter": {
            "mode": "blocklist",
            "patterns": [
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
        }
    }'::jsonb;

with normalized_policy as (
    select
        id,
        coalesce(
            web_ingest_policy #>> '{urlFilter,mode}',
            web_ingest_policy ->> 'urlFilterMode',
            case when web_ingest_policy ? 'allowPatterns' then 'allowlist' else 'blocklist' end
        ) as requested_mode,
        coalesce(
            web_ingest_policy #> '{urlFilter,patterns}',
            web_ingest_policy -> 'patterns',
            web_ingest_policy -> 'allowPatterns',
            web_ingest_policy -> 'ignorePatterns',
            '[]'::jsonb
        ) as patterns
    from catalog_library
)
update catalog_library target_library
set web_ingest_policy = jsonb_build_object(
    'urlFilter',
    jsonb_build_object(
        'mode',
        case
            when normalized_policy.requested_mode = 'allowlist'
                 and jsonb_typeof(normalized_policy.patterns) = 'array'
                 and jsonb_array_length(normalized_policy.patterns) > 0
                then 'allowlist'
            else 'blocklist'
        end,
        'patterns',
        case
            when jsonb_typeof(normalized_policy.patterns) = 'array'
                then normalized_policy.patterns
            else '[]'::jsonb
        end
    )
)
from normalized_policy
where normalized_policy.id = target_library.id;

alter table content_web_ingest_run
    add column if not exists url_filter_mode web_url_filter_mode not null default 'blocklist',
    add column if not exists url_patterns jsonb not null default '[]'::jsonb;

do $$
begin
    if exists (
        select 1
        from information_schema.columns
        where table_name = 'content_web_ingest_run'
          and column_name = 'allow_patterns'
    ) then
        execute $sql$
            update content_web_ingest_run
            set url_patterns = case
                when url_filter_mode = 'allowlist'::web_url_filter_mode
                     and jsonb_typeof(allow_patterns) = 'array'
                     and jsonb_array_length(allow_patterns) > 0
                    then allow_patterns
                when jsonb_typeof(ignore_patterns) = 'array'
                    then ignore_patterns
                else '[]'::jsonb
            end
            where url_patterns = '[]'::jsonb
        $sql$;
    else
        update content_web_ingest_run
        set url_patterns = case
            when jsonb_typeof(ignore_patterns) = 'array'
                then ignore_patterns
            else '[]'::jsonb
        end
        where url_patterns = '[]'::jsonb;
    end if;
end;
$$;

alter table content_web_ingest_run
    drop constraint if exists content_web_ingest_run_ignore_patterns_array_check,
    drop constraint if exists content_web_ingest_run_allow_patterns_array_check,
    drop column if exists ignore_patterns,
    drop column if exists allow_patterns;

do $$
begin
    if not exists (
        select 1
        from pg_constraint
        where conname = 'content_web_ingest_run_url_patterns_array_check'
    ) then
        alter table content_web_ingest_run
            add constraint content_web_ingest_run_url_patterns_array_check
            check (jsonb_typeof(url_patterns) = 'array');
    end if;
end;
$$;

alter type ai_binding_purpose add value if not exists 'utility';

alter type ai_binding_purpose add value if not exists 'rerank';

create table if not exists entity_dedup_verification_cache (
    cache_key bytea primary key,
    library_id uuid not null,
    entity_a_key text not null,
    entity_b_key text not null,
    verdict boolean not null,
    created_at timestamptz not null default now()
);

create index entity_dedup_verification_cache_lib_idx
    on entity_dedup_verification_cache (library_id, created_at desc);

drop table if exists runtime_graph_community cascade;

alter table runtime_graph_node
    drop column if exists community_id,
    drop column if exists community_level;

create table runtime_graph_community (
    id                 uuid primary key default gen_random_uuid(),
    library_id         uuid not null,
    projection_version bigint not null,
    member_node_ids    uuid[] not null default '{}',
    summary_title      text,
    summary_text       text,
    summary_embedding  double precision[]
);

create index runtime_graph_community_lib_ver_idx
    on runtime_graph_community (library_id, projection_version);

create index runtime_graph_community_members_idx
    on runtime_graph_community using gin (member_node_ids);

alter table content_chunk
    add column if not exists source_page integer,
    add column if not exists source_bbox real[];

create index if not exists idx_content_chunk_revision_page
    on content_chunk (revision_id, source_page)
    where source_page is not null;

create index if not exists idx_content_chunk_text_checksum_revision
    on content_chunk (text_checksum, revision_id, id);

alter table content_chunk
    add column if not exists raptor_level smallint not null default 0;

alter table content_chunk
    drop constraint if exists content_chunk_raptor_level_nonnegative;

alter table content_chunk
    add constraint content_chunk_raptor_level_nonnegative
    check (raptor_level >= 0);

create index if not exists content_chunk_raptor_level_idx
    on content_chunk (revision_id, raptor_level)
    where raptor_level > 0;

alter table content_chunk
    add column if not exists window_text text;

alter table catalog_library
    add column if not exists chunking_template text not null default 'naive';

alter table catalog_library
    add column if not exists recognition_policy jsonb not null default '{
        "rasterImageEngine": "docling"
    }'::jsonb;

update catalog_library
set recognition_policy = '{"rasterImageEngine": "docling"}'::jsonb
where recognition_policy is null
   or jsonb_typeof(recognition_policy) <> 'object'
   or not recognition_policy ? 'rasterImageEngine'
   or recognition_policy->>'rasterImageEngine' not in ('docling', 'vision')
   or recognition_policy - 'rasterImageEngine' <> '{}'::jsonb;

do $$
begin
    if not exists (
        select 1
        from pg_constraint
        where conname = 'catalog_library_recognition_policy_object_check'
    ) then
        alter table catalog_library
            add constraint catalog_library_recognition_policy_object_check
            check (jsonb_typeof(recognition_policy) = 'object');
    end if;

    if not exists (
        select 1
        from pg_constraint
        where conname = 'catalog_library_recognition_policy_raster_engine_check'
    ) then
        alter table catalog_library
            add constraint catalog_library_recognition_policy_raster_engine_check
            check (
                recognition_policy ? 'rasterImageEngine'
                and recognition_policy->>'rasterImageEngine' in ('docling', 'vision')
                and recognition_policy - 'rasterImageEngine' = '{}'::jsonb
            );
    end if;
end;
$$;

alter table runtime_graph_snapshot
    add column if not exists topology_generation bigint not null default 0;

update runtime_graph_snapshot
set topology_generation = 1
where topology_generation = 0
  and graph_status in ('ready', 'empty', 'failed');

alter type ingest_job_kind add value if not exists 'webhook_delivery';

create type webhook_delivery_state as enum (
    'pending', 'delivering', 'delivered', 'failed', 'abandoned'
);

create table if not exists webhook_subscription (
    id uuid primary key default uuidv7(),
    workspace_id uuid not null references catalog_workspace(id) on delete cascade,
    library_id uuid references catalog_library(id) on delete cascade,
    display_name text not null,
    target_url text not null,
    secret text not null,
    event_types text[] not null default '{}',
    custom_headers_json jsonb not null default '{}'::jsonb,
    active boolean not null default true,
    created_by_principal_id uuid references iam_principal(id) on delete set null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint webhook_subscription_event_types_nonempty check (
        cardinality(event_types) > 0
    ),
    constraint webhook_subscription_target_url_format check (
        target_url like 'http://%' or target_url like 'https://%'
    )
);

create index if not exists idx_webhook_subscription_workspace_active
    on webhook_subscription (workspace_id, active)
    where active;

create index if not exists idx_webhook_subscription_library_active
    on webhook_subscription (library_id, active)
    where active and library_id is not null;

create table if not exists webhook_delivery_attempt (
    id uuid primary key default uuidv7(),
    subscription_id uuid not null references webhook_subscription(id) on delete cascade,
    workspace_id uuid not null references catalog_workspace(id) on delete cascade,
    library_id uuid references catalog_library(id) on delete cascade,
    event_type text not null,
    event_id text not null,
    payload_json jsonb not null,
    target_url text not null,
    attempt_number integer not null default 0,
    delivery_state webhook_delivery_state not null default 'pending',
    response_status integer,
    response_body_excerpt text,
    error_message text,
    job_id uuid references ingest_job(id) on delete set null,
    next_attempt_at timestamptz,
    delivered_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index if not exists idx_webhook_delivery_subscription_state
    on webhook_delivery_attempt (subscription_id, delivery_state, next_attempt_at);

create index if not exists idx_webhook_delivery_event
    on webhook_delivery_attempt (event_type, event_id);

create index if not exists idx_webhook_delivery_job
    on webhook_delivery_attempt (job_id)
    where job_id is not null;

-- Reconcile runtime graph rows whose source document is no longer active.
-- The canonical graph contract keeps evidence attached only to active
-- documents and removes unsupported nodes, edges, and summaries.
delete from runtime_graph_evidence as evidence
where evidence.document_id is null
   or exists (
       select 1
       from content_document as document
       where document.id = evidence.document_id
         and (document.document_state = 'deleted' or document.deleted_at is not null)
   );

with active_node_evidence as (
    select evidence.target_id, count(*)::int as support_count
    from runtime_graph_evidence as evidence
    join content_document as document
      on document.id = evidence.document_id
     and document.library_id = evidence.library_id
     and document.document_state = 'active'
     and document.deleted_at is null
    where evidence.target_kind = 'node'
    group by evidence.target_id
)
update runtime_graph_node as node
set support_count = coalesce(active_node_evidence.support_count, 0),
    updated_at = now()
from runtime_graph_node as base
left join active_node_evidence on active_node_evidence.target_id = base.id
where node.id = base.id
  and node.support_count is distinct from coalesce(active_node_evidence.support_count, 0);

with active_edge_evidence as (
    select evidence.target_id, count(*)::int as support_count
    from runtime_graph_evidence as evidence
    join content_document as document
      on document.id = evidence.document_id
     and document.library_id = evidence.library_id
     and document.document_state = 'active'
     and document.deleted_at is null
    where evidence.target_kind = 'edge'
    group by evidence.target_id
)
update runtime_graph_edge as edge
set support_count = coalesce(active_edge_evidence.support_count, 0),
    updated_at = now()
from runtime_graph_edge as base
left join active_edge_evidence on active_edge_evidence.target_id = base.id
where edge.id = base.id
  and edge.support_count is distinct from coalesce(active_edge_evidence.support_count, 0);

delete from runtime_graph_edge
where support_count <= 0;

delete from runtime_graph_node as node
where node.support_count <= 0
   or (
       node.node_type = 'document'
       and exists (
           select 1
           from content_document as document
           where document.id = node.document_id
             and document.library_id = node.library_id
             and (document.document_state = 'deleted' or document.deleted_at is not null)
       )
   );

delete from runtime_graph_canonical_summary as summary
where (
    summary.target_kind = 'node'
    and not exists (
        select 1 from runtime_graph_node as node where node.id = summary.target_id
    )
)
or (
    summary.target_kind = 'edge'
    and not exists (
        select 1 from runtime_graph_edge as edge where edge.id = summary.target_id
    )
);

with admitted_edges as (
    select
        edge.library_id,
        edge.projection_version,
        edge.id,
        edge.from_node_id,
        edge.to_node_id
    from runtime_graph_edge as edge
    join runtime_graph_node as source
      on source.library_id = edge.library_id
     and source.id = edge.from_node_id
     and source.projection_version = edge.projection_version
    join runtime_graph_node as target
      on target.library_id = edge.library_id
     and target.id = edge.to_node_id
     and target.projection_version = edge.projection_version
    join runtime_graph_snapshot as snapshot
      on snapshot.library_id = edge.library_id
     and snapshot.projection_version = edge.projection_version
    where btrim(edge.relation_type) <> ''
      and edge.from_node_id <> edge.to_node_id
),
admitted_endpoints as (
    select library_id, projection_version, from_node_id as node_id
    from admitted_edges
    union
    select library_id, projection_version, to_node_id as node_id
    from admitted_edges
),
node_counts as (
    select
        snapshot.library_id,
        count(node.id)::integer as node_count
    from runtime_graph_snapshot as snapshot
    left join runtime_graph_node as node
      on node.library_id = snapshot.library_id
     and node.projection_version = snapshot.projection_version
    left join admitted_endpoints as admitted
      on admitted.library_id = node.library_id
     and admitted.projection_version = node.projection_version
     and admitted.node_id = node.id
    where node.id is null
       or node.node_type = 'document'
       or (node.node_type <> 'document' and admitted.node_id is not null)
    group by snapshot.library_id
),
edge_counts as (
    select
        snapshot.library_id,
        count(admitted_edges.id)::integer as edge_count
    from runtime_graph_snapshot as snapshot
    left join admitted_edges
      on admitted_edges.library_id = snapshot.library_id
     and admitted_edges.projection_version = snapshot.projection_version
    group by snapshot.library_id
)
update runtime_graph_snapshot as snapshot
set node_count = coalesce(node_counts.node_count, 0),
    edge_count = coalesce(edge_counts.edge_count, 0),
    updated_at = now()
from node_counts, edge_counts
where node_counts.library_id = snapshot.library_id
  and edge_counts.library_id = snapshot.library_id;

-- ---------------------------------------------------------------------
-- Temporal-aware chunks: add structured occurred_at / occurred_until
-- bounds so retrieval can hard-filter by user-question time range
-- without parsing chunk header text at query time. Source of truth is
-- the JSONL ingest path; each record carries an occurred_at ISO
-- timestamp and a chunk inherits (MIN, MAX) of its records. Chunks
-- from non-temporal sources (PDF, image, generic markdown) leave both
-- columns NULL.
--
-- Canonical extractor: apps/api/src/shared/extraction/record_jsonl.rs
-- (`extract_chunk_temporal_bounds`). Used by the ingest write path
-- and the runtime backfill so a single helper feeds both stores.
-- ---------------------------------------------------------------------

alter table content_chunk
    add column if not exists occurred_at    timestamptz null,
    add column if not exists occurred_until timestamptz null;

-- partial index keeps the temporal range scan cheap for libraries that
-- mix temporal and non-temporal chunks (a chat library with attached
-- PDFs).
create index if not exists idx_content_chunk_occurred_at
    on content_chunk (revision_id, occurred_at, occurred_until)
    where occurred_at is not null;

-- canonical-only invariant: occurred_until must be >= occurred_at when
-- both present. Single-record chunks set occurred_until = occurred_at.
alter table content_chunk
    drop constraint if exists content_chunk_occurred_range_check;
alter table content_chunk
    add constraint content_chunk_occurred_range_check
    check (
        occurred_at is null
        or occurred_until is null
        or occurred_until >= occurred_at
    );


-- ---------------------------------------------------------------------
-- T-followup (0.4.1): comprehensive Ollama provider catalog. Seeds every
-- widely-used Ollama Library model — chat, vision, embedding — with
-- canonical default roles so users can pick a working binding from the
-- UI without guessing. Models not yet `ollama pull`-ed surface in the
-- dropdown anyway; selecting one and saving the binding triggers
-- `ollama pull` on first use.
--
-- Provider id 00000000-0000-0000-0000-000000000104 = ollama. Model ids
-- 0244..028b reserved for these entries.
-- ---------------------------------------------------------------------

insert into ai_model_catalog (id, provider_catalog_id, model_name, capability_kind, modality_kind, lifecycle_state, metadata_json) values
    ('00000000-0000-0000-0000-000000000244', '00000000-0000-0000-0000-000000000104', 'qwen3:0.6b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","utility"],"seedSource":"provider_catalog","parameterSize":"0.6B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-000000000245', '00000000-0000-0000-0000-000000000104', 'qwen3:1.7b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"1.7B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-000000000246', '00000000-0000-0000-0000-000000000104', 'qwen3:4b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"4B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-000000000247', '00000000-0000-0000-0000-000000000104', 'qwen3:8b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"8B","quantization":"Q4_K_M","recommendedFor":"query_compile,query_answer"}'::jsonb),
    ('00000000-0000-0000-0000-000000000248', '00000000-0000-0000-0000-000000000104', 'qwen3:14b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"14B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-000000000249', '00000000-0000-0000-0000-000000000104', 'qwen3:32b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"32B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-00000000024a', '00000000-0000-0000-0000-000000000104', 'qwen2.5:0.5b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","utility"],"seedSource":"provider_catalog","parameterSize":"0.5B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-00000000024b', '00000000-0000-0000-0000-000000000104', 'qwen2.5:1.5b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","utility"],"seedSource":"provider_catalog","parameterSize":"1.5B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-00000000024c', '00000000-0000-0000-0000-000000000104', 'qwen2.5:3b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","utility"],"seedSource":"provider_catalog","parameterSize":"3B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-00000000024d', '00000000-0000-0000-0000-000000000104', 'qwen2.5:7b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"7B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-00000000024e', '00000000-0000-0000-0000-000000000104', 'qwen2.5:14b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"14B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-00000000024f', '00000000-0000-0000-0000-000000000104', 'qwen2.5:32b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"32B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-000000000250', '00000000-0000-0000-0000-000000000104', 'qwen2.5:72b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"72B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-000000000251', '00000000-0000-0000-0000-000000000104', 'qwen2.5-coder:1.5b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","utility"],"seedSource":"provider_catalog","parameterSize":"1.5B","quantization":"Q4_K_M","specialty":"code"}'::jsonb),
    ('00000000-0000-0000-0000-000000000252', '00000000-0000-0000-0000-000000000104', 'qwen2.5-coder:7b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"7B","quantization":"Q4_K_M","specialty":"code"}'::jsonb),
    ('00000000-0000-0000-0000-000000000253', '00000000-0000-0000-0000-000000000104', 'qwen2.5-coder:14b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"14B","quantization":"Q4_K_M","specialty":"code"}'::jsonb),
    ('00000000-0000-0000-0000-000000000254', '00000000-0000-0000-0000-000000000104', 'qwen2.5-coder:32b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"32B","quantization":"Q4_K_M","specialty":"code"}'::jsonb),
    ('00000000-0000-0000-0000-000000000255', '00000000-0000-0000-0000-000000000104', 'qwq:32b', 'chat', 'text', 'active', '{"defaultRoles":["query_answer","query_compile","utility"],"seedSource":"provider_catalog","parameterSize":"32B","quantization":"Q4_K_M","reasoning":true}'::jsonb),
    ('00000000-0000-0000-0000-000000000256', '00000000-0000-0000-0000-000000000104', 'llama3.1:8b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"8B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-000000000257', '00000000-0000-0000-0000-000000000104', 'llama3.1:70b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"70B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-000000000258', '00000000-0000-0000-0000-000000000104', 'llama3.2:1b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","utility"],"seedSource":"provider_catalog","parameterSize":"1B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-000000000259', '00000000-0000-0000-0000-000000000104', 'llama3.2:3b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","utility"],"seedSource":"provider_catalog","parameterSize":"3B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-00000000025a', '00000000-0000-0000-0000-000000000104', 'llama3.3:70b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"70B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-00000000025b', '00000000-0000-0000-0000-000000000104', 'mistral:7b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"7B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-00000000025c', '00000000-0000-0000-0000-000000000104', 'mistral-nemo:12b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"12B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-00000000025d', '00000000-0000-0000-0000-000000000104', 'mixtral:8x7b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"47B","quantization":"Q4_K_M","architecture":"moe"}'::jsonb),
    ('00000000-0000-0000-0000-00000000025e', '00000000-0000-0000-0000-000000000104', 'mixtral:8x22b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"141B","quantization":"Q4_K_M","architecture":"moe"}'::jsonb),
    ('00000000-0000-0000-0000-00000000025f', '00000000-0000-0000-0000-000000000104', 'gemma2:2b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","utility"],"seedSource":"provider_catalog","parameterSize":"2B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-000000000260', '00000000-0000-0000-0000-000000000104', 'gemma2:9b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"9B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-000000000261', '00000000-0000-0000-0000-000000000104', 'gemma2:27b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"27B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-000000000262', '00000000-0000-0000-0000-000000000104', 'gemma3:1b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","utility"],"seedSource":"provider_catalog","parameterSize":"1B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-000000000263', '00000000-0000-0000-0000-000000000104', 'gemma3:4b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"4B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-000000000264', '00000000-0000-0000-0000-000000000104', 'gemma3:12b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"12B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-000000000265', '00000000-0000-0000-0000-000000000104', 'gemma3:27b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"27B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-000000000266', '00000000-0000-0000-0000-000000000104', 'phi3:3.8b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","utility"],"seedSource":"provider_catalog","parameterSize":"3.8B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-000000000267', '00000000-0000-0000-0000-000000000104', 'phi3:14b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"14B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-000000000268', '00000000-0000-0000-0000-000000000104', 'phi3.5:3.8b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","utility"],"seedSource":"provider_catalog","parameterSize":"3.8B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-000000000269', '00000000-0000-0000-0000-000000000104', 'phi4:14b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"14B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-00000000026a', '00000000-0000-0000-0000-000000000104', 'phi4-mini:latest', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"3.8B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-00000000026b', '00000000-0000-0000-0000-000000000104', 'deepseek-r1:1.5b', 'chat', 'text', 'active', '{"defaultRoles":["query_answer","query_compile","utility"],"seedSource":"provider_catalog","parameterSize":"1.5B","quantization":"Q4_K_M","reasoning":true}'::jsonb),
    ('00000000-0000-0000-0000-00000000026c', '00000000-0000-0000-0000-000000000104', 'deepseek-r1:7b', 'chat', 'text', 'active', '{"defaultRoles":["query_answer","query_compile","utility"],"seedSource":"provider_catalog","parameterSize":"7B","quantization":"Q4_K_M","reasoning":true}'::jsonb),
    ('00000000-0000-0000-0000-00000000026d', '00000000-0000-0000-0000-000000000104', 'deepseek-r1:8b', 'chat', 'text', 'active', '{"defaultRoles":["query_answer","query_compile","utility"],"seedSource":"provider_catalog","parameterSize":"8B","quantization":"Q4_K_M","reasoning":true}'::jsonb),
    ('00000000-0000-0000-0000-00000000026e', '00000000-0000-0000-0000-000000000104', 'deepseek-r1:14b', 'chat', 'text', 'active', '{"defaultRoles":["query_answer","query_compile","utility"],"seedSource":"provider_catalog","parameterSize":"14B","quantization":"Q4_K_M","reasoning":true}'::jsonb),
    ('00000000-0000-0000-0000-00000000026f', '00000000-0000-0000-0000-000000000104', 'deepseek-r1:32b', 'chat', 'text', 'active', '{"defaultRoles":["query_answer","query_compile","utility"],"seedSource":"provider_catalog","parameterSize":"32B","quantization":"Q4_K_M","reasoning":true}'::jsonb),
    ('00000000-0000-0000-0000-000000000270', '00000000-0000-0000-0000-000000000104', 'deepseek-r1:70b', 'chat', 'text', 'active', '{"defaultRoles":["query_answer","query_compile","utility"],"seedSource":"provider_catalog","parameterSize":"70B","quantization":"Q4_K_M","reasoning":true}'::jsonb),
    ('00000000-0000-0000-0000-000000000271', '00000000-0000-0000-0000-000000000104', 'deepseek-coder-v2:16b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"16B","quantization":"Q4_K_M","specialty":"code"}'::jsonb),
    ('00000000-0000-0000-0000-000000000272', '00000000-0000-0000-0000-000000000104', 'granite3.3:2b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","utility"],"seedSource":"provider_catalog","parameterSize":"2B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-000000000273', '00000000-0000-0000-0000-000000000104', 'granite3.3:8b', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","parameterSize":"8B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-000000000274', '00000000-0000-0000-0000-000000000104', 'qwen3-vl:2b', 'chat', 'multimodal', 'active', '{"defaultRoles":["extract_graph","query_answer","vision"],"seedSource":"provider_catalog","parameterSize":"2B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-000000000275', '00000000-0000-0000-0000-000000000104', 'qwen3-vl:4b', 'chat', 'multimodal', 'active', '{"defaultRoles":["extract_graph","query_answer","vision"],"seedSource":"provider_catalog","parameterSize":"4B","quantization":"Q4_K_M","recommendedFor":"vision"}'::jsonb),
    ('00000000-0000-0000-0000-000000000276', '00000000-0000-0000-0000-000000000104', 'qwen2.5vl:3b', 'chat', 'multimodal', 'active', '{"defaultRoles":["extract_graph","query_answer","vision"],"seedSource":"provider_catalog","parameterSize":"3B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-000000000277', '00000000-0000-0000-0000-000000000104', 'qwen2.5vl:7b', 'chat', 'multimodal', 'active', '{"defaultRoles":["extract_graph","query_answer","vision"],"seedSource":"provider_catalog","parameterSize":"7B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-000000000278', '00000000-0000-0000-0000-000000000104', 'qwen2.5vl:32b', 'chat', 'multimodal', 'active', '{"defaultRoles":["extract_graph","query_answer","vision"],"seedSource":"provider_catalog","parameterSize":"32B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-000000000279', '00000000-0000-0000-0000-000000000104', 'llama3.2-vision:11b', 'chat', 'multimodal', 'active', '{"defaultRoles":["extract_graph","query_answer","vision"],"seedSource":"provider_catalog","parameterSize":"11B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-00000000027a', '00000000-0000-0000-0000-000000000104', 'llama3.2-vision:90b', 'chat', 'multimodal', 'active', '{"defaultRoles":["extract_graph","query_answer","vision"],"seedSource":"provider_catalog","parameterSize":"90B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-00000000027b', '00000000-0000-0000-0000-000000000104', 'llava:7b', 'chat', 'multimodal', 'active', '{"defaultRoles":["extract_graph","query_answer","vision"],"seedSource":"provider_catalog","parameterSize":"7B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-00000000027c', '00000000-0000-0000-0000-000000000104', 'llava:13b', 'chat', 'multimodal', 'active', '{"defaultRoles":["extract_graph","query_answer","vision"],"seedSource":"provider_catalog","parameterSize":"13B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-00000000027d', '00000000-0000-0000-0000-000000000104', 'moondream:1.8b', 'chat', 'multimodal', 'active', '{"defaultRoles":["extract_graph","query_answer","vision"],"seedSource":"provider_catalog","parameterSize":"1.8B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-00000000027e', '00000000-0000-0000-0000-000000000104', 'minicpm-v:8b', 'chat', 'multimodal', 'active', '{"defaultRoles":["extract_graph","query_answer","vision"],"seedSource":"provider_catalog","parameterSize":"8B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-00000000027f', '00000000-0000-0000-0000-000000000104', 'qwen3-embedding:0.6b', 'embedding', 'text', 'active', '{"defaultRoles":["embed_chunk"],"seedSource":"provider_catalog","parameterSize":"0.6B","quantization":"Q8_0"}'::jsonb),
    ('00000000-0000-0000-0000-000000000280', '00000000-0000-0000-0000-000000000104', 'qwen3-embedding:4b', 'embedding', 'text', 'active', '{"defaultRoles":["embed_chunk"],"seedSource":"provider_catalog","parameterSize":"4B","quantization":"Q4_K_M"}'::jsonb),
    ('00000000-0000-0000-0000-000000000281', '00000000-0000-0000-0000-000000000104', 'nomic-embed-text:latest', 'embedding', 'text', 'active', '{"defaultRoles":["embed_chunk"],"seedSource":"provider_catalog","parameterSize":"137M","quantization":"F16"}'::jsonb),
    ('00000000-0000-0000-0000-000000000282', '00000000-0000-0000-0000-000000000104', 'mxbai-embed-large:latest', 'embedding', 'text', 'active', '{"defaultRoles":["embed_chunk"],"seedSource":"provider_catalog","parameterSize":"335M","quantization":"F16"}'::jsonb),
    ('00000000-0000-0000-0000-000000000283', '00000000-0000-0000-0000-000000000104', 'bge-m3:latest', 'embedding', 'text', 'active', '{"defaultRoles":["embed_chunk"],"seedSource":"provider_catalog","parameterSize":"567M","quantization":"F16","multilingual":true}'::jsonb),
    ('00000000-0000-0000-0000-000000000284', '00000000-0000-0000-0000-000000000104', 'snowflake-arctic-embed2', 'embedding', 'text', 'active', '{"defaultRoles":["embed_chunk"],"seedSource":"provider_catalog","parameterSize":"568M","quantization":"F16","multilingual":true}'::jsonb),
    ('00000000-0000-0000-0000-000000000285', '00000000-0000-0000-0000-000000000104', 'all-minilm:22m', 'embedding', 'text', 'active', '{"defaultRoles":["embed_chunk"],"seedSource":"provider_catalog","parameterSize":"22M","quantization":"F16"}'::jsonb),
    ('00000000-0000-0000-0000-000000000286', '00000000-0000-0000-0000-000000000104', 'granite-embedding:278m', 'embedding', 'text', 'active', '{"defaultRoles":["embed_chunk"],"seedSource":"provider_catalog","parameterSize":"278M","quantization":"F16","multilingual":true}'::jsonb),
    ('00000000-0000-0000-0000-000000000287', '00000000-0000-0000-0000-000000000104', 'deepseek-v4-flash:cloud', 'chat', 'text', 'active', '{"defaultRoles":["extract_graph","query_answer","query_compile","utility","rerank"],"seedSource":"provider_catalog","cloud":true,"remoteHost":"https://ollama.com:443"}'::jsonb)
on conflict (provider_catalog_id, model_name, capability_kind) do update set
    metadata_json   = excluded.metadata_json,
    modality_kind   = excluded.modality_kind,
    lifecycle_state = excluded.lifecycle_state;

-- Bootstrap instance-scoped presets so each Ollama model appears in the
-- UI dropdown out of the box. Idempotent via NOT EXISTS guard.
insert into ai_model_preset (id, model_catalog_id, preset_name, scope_kind)
select uuidv7(), m.id, 'Ollama ' || m.model_name, 'instance'::ai_scope_kind
  from ai_model_catalog m
  join ai_provider_catalog p on p.id = m.provider_catalog_id
 where p.provider_kind = 'ollama'
   and not exists (
       select 1 from ai_model_preset mp
       where mp.model_catalog_id = m.id and mp.preset_name = 'Ollama ' || m.model_name
   );
