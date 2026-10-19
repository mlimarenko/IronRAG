\set ON_ERROR_STOP on
\pset pager off
\pset null '<null>'

-- IRG-61: read-only inventory of PostgreSQL vector lifecycle state.
--
-- Run with a role that can read pg_catalog and the IronRAG schema:
--   psql -X --set=ON_ERROR_STOP=1 --file=scripts/ops/vector-lifecycle-inventory.sql
--
-- The row classification is MVCC-consistent. pg_stat_* counters are estimates
-- maintained outside that snapshot, and EXPLAIN ANALYZE warms caches even
-- though it performs no writes. This artifact deliberately creates no helper
-- functions, views, or temporary relations.

begin transaction isolation level repeatable read read only;

select
    'baseline_metadata' as section,
    clock_timestamp() as observed_at,
    current_database() as database_name,
    (select oid::text from pg_catalog.pg_database where datname = current_database()) as database_oid,
    (select system_identifier::text from pg_catalog.pg_control_system()) as system_identifier,
    current_setting('server_version') as server_version,
    txid_current_snapshot() as transaction_snapshot,
    current_setting('default_transaction_read_only') as default_transaction_read_only;

-- Physical relations are authoritative. Manifest names are runtime data and
-- are never interpolated into executable SQL.
with physical as (
    select
        namespace.nspname,
        relation.relname,
        relation.oid,
        case
            when relation.relname like 'knowledge_chunk_vector_d%' then 'chunk'
            else 'entity'
        end as relation_kind,
        substring(relation.relname from '_d([1-9][0-9]*)$')::integer as dimensions
    from pg_catalog.pg_class relation
    join pg_catalog.pg_namespace namespace on namespace.oid = relation.relnamespace
    where namespace.nspname = 'public'
      and relation.relkind in ('r', 'p')
      and relation.relname ~ '^knowledge_(chunk|entity)_vector_d[1-9][0-9]*$'
), manifest as (
    select
        relation_name,
        dim,
        vector_kind,
        count(*)::bigint as manifest_rows,
        sum(row_count)::bigint as manifest_claimed_vector_rows
    from public.knowledge_vector_relation_manifest
    group by relation_name, dim, vector_kind
)
select
    'relation_manifest_reconciliation' as section,
    coalesce(physical.relname, manifest.relation_name) as relation_name,
    physical.relation_kind,
    physical.dimensions as physical_dimensions,
    manifest.dim as manifest_dimensions,
    manifest.vector_kind,
    coalesce(manifest.manifest_rows, 0) as manifest_rows,
    coalesce(manifest.manifest_claimed_vector_rows, 0) as manifest_claimed_vector_rows,
    case
        when physical.oid is null then 'manifest_only'
        when manifest.relation_name is null then 'physical_only'
        when physical.dimensions <> manifest.dim then 'dimension_mismatch'
        when physical.relation_kind = 'chunk' and manifest.vector_kind <> 'chunk_embedding'
            then 'kind_mismatch'
        when physical.relation_kind = 'entity' and manifest.vector_kind <> 'entity_embedding'
            then 'kind_mismatch'
        else 'aligned'
    end as reconciliation_state
from physical
full outer join manifest on manifest.relation_name = physical.relname
order by relation_name, manifest_dimensions, vector_kind;

-- Exact physical bytes appear once per relation and once per HNSW index. They
-- are shared by libraries and must not be duplicated into per-library rows.
select
    'physical_relation' as section,
    namespace.nspname as schema_name,
    relation.relname as relation_name,
    case
        when relation.relname like 'knowledge_chunk_vector_d%' then 'chunk'
        else 'entity'
    end as relation_kind,
    pg_catalog.pg_relation_size(relation.oid) as main_fork_bytes,
    pg_catalog.pg_table_size(relation.oid) as table_bytes,
    pg_catalog.pg_indexes_size(relation.oid) as all_index_bytes,
    pg_catalog.pg_total_relation_size(relation.oid) as total_bytes,
    statistics.n_live_tup as estimated_live_tuples,
    statistics.n_dead_tup as estimated_dead_tuples,
    statistics.last_analyze,
    statistics.last_autoanalyze,
    statistics.last_vacuum,
    statistics.last_autovacuum,
    database_statistics.stats_reset as database_stats_reset
from pg_catalog.pg_class relation
join pg_catalog.pg_namespace namespace on namespace.oid = relation.relnamespace
left join pg_catalog.pg_stat_all_tables statistics on statistics.relid = relation.oid
left join pg_catalog.pg_stat_database database_statistics
  on database_statistics.datname = current_database()
where namespace.nspname = 'public'
  and relation.relkind in ('r', 'p')
  and relation.relname ~ '^knowledge_(chunk|entity)_vector_d[1-9][0-9]*$'
order by relation.relname;

select
    'hnsw_index' as section,
    table_relation.relname as relation_name,
    index_relation.relname as index_name,
    pg_catalog.pg_relation_size(index_relation.oid) as index_bytes
from pg_catalog.pg_index index_metadata
join pg_catalog.pg_class table_relation on table_relation.oid = index_metadata.indrelid
join pg_catalog.pg_namespace namespace on namespace.oid = table_relation.relnamespace
join pg_catalog.pg_class index_relation on index_relation.oid = index_metadata.indexrelid
join pg_catalog.pg_am access_method on access_method.oid = index_relation.relam
where namespace.nspname = 'public'
  and table_relation.relname ~ '^knowledge_(chunk|entity)_vector_d[1-9][0-9]*$'
  and access_method.amname = 'hnsw'
order by table_relation.relname, index_relation.relname;

-- Generate one classification query per catalog-validated chunk relation.
-- format('%I.%I') is the only identifier interpolation boundary.
select pg_catalog.format($query$
-- BEGIN CHUNK VECTOR LIFECYCLE CLASSIFICATION
with classified as (
    select
        vector_row.key,
        vector_row.vector_id,
        vector_row.workspace_id,
        vector_row.library_id,
        vector_row.revision_id,
        vector_row.chunk_id,
        vector_row.embedding_model_key,
        vector_row.vector_kind,
        vector_row.dimensions,
        vector_row.embedding::text as embedding_text,
        pg_column_size(vector_row.embedding)::bigint as logical_embedding_value_bytes,
        vector_row.created_at,
        catalog_library.slug as library_slug,
        (
            catalog_library.id is not null
            and content_document.id is not null
            and content_document.workspace_id = vector_row.workspace_id
            and content_document.library_id = vector_row.library_id
            and content_document.document_state = 'active'
            and content_document.deleted_at is null
            and content_head.document_id is not null
            and content_revision.id is not null
            and content_revision.document_id = content_document.id
            and content_revision.workspace_id = vector_row.workspace_id
            and content_revision.library_id = vector_row.library_id
            and knowledge_document.document_id is not null
            and knowledge_document.workspace_id = vector_row.workspace_id
            and knowledge_document.library_id = vector_row.library_id
            and knowledge_document.document_state = 'active'
            and knowledge_document.deleted_at is null
            and knowledge_revision.revision_id is not null
            and knowledge_revision.document_id = content_document.id
            and knowledge_revision.workspace_id = vector_row.workspace_id
            and knowledge_revision.library_id = vector_row.library_id
            and knowledge_chunk.chunk_id is not null
            and knowledge_chunk.revision_id = vector_row.revision_id
            and knowledge_chunk.document_id = content_document.id
            and knowledge_chunk.workspace_id = vector_row.workspace_id
            and knowledge_chunk.library_id = vector_row.library_id
            and vector_row.dimensions = %1$s
        ) as identity_consistent,
        (
            content_head.readable_revision_id is not distinct from knowledge_document.readable_revision_id
            and content_head.active_revision_id is not distinct from knowledge_document.active_revision_id
        ) as heads_consistent,
        (
            content_head.readable_revision_id is not null
            or content_head.active_revision_id is not null
        ) as has_head,
        (
            knowledge_chunk.chunk_state = 'ready'
            and knowledge_revision.text_state = 'text_readable'
            and knowledge_revision.vector_state = 'ready'
            and structured_revision.revision_id is not null
            and structured_revision.document_id = content_document.id
            and structured_revision.workspace_id = vector_row.workspace_id
            and structured_revision.library_id = vector_row.library_id
            and structured_revision.preparation_state = 'prepared'
        ) as retrieval_ready,
        exists (
            select 1
            from public.ingest_job ingest_job
            where ingest_job.workspace_id = vector_row.workspace_id
              and ingest_job.library_id = vector_row.library_id
              and ingest_job.knowledge_document_id = content_document.id
              and ingest_job.knowledge_revision_id = vector_row.revision_id
              and ingest_job.job_kind = 'content_mutation'
              and ingest_job.queue_state = 'leased'
              and ingest_job.completed_at is null
              and exists (
                  select 1
                  from public.ingest_attempt ingest_attempt
                  where ingest_attempt.job_id = ingest_job.id
                    and ingest_attempt.attempt_state = 'leased'
                    and ingest_attempt.finished_at is null
                    and not exists (
                        select 1
                        from public.ingest_attempt newer_attempt
                        where newer_attempt.job_id = ingest_attempt.job_id
                          and newer_attempt.attempt_number > ingest_attempt.attempt_number
                    )
              )
        ) as has_ingest_authority,
        content_head.readable_revision_id,
        content_head.active_revision_id,
        knowledge_document.readable_revision_id as knowledge_readable_revision_id,
        knowledge_document.active_revision_id as knowledge_active_revision_id
    from %2$s vector_row
    left join public.knowledge_chunk knowledge_chunk
      on knowledge_chunk.chunk_id = vector_row.chunk_id
    left join public.knowledge_revision knowledge_revision
      on knowledge_revision.revision_id = vector_row.revision_id
    left join public.content_revision content_revision
      on content_revision.id = vector_row.revision_id
    left join public.content_document content_document
      on content_document.id = coalesce(
          knowledge_chunk.document_id,
          knowledge_revision.document_id,
          content_revision.document_id
      )
    left join public.content_document_head content_head
      on content_head.document_id = content_document.id
    left join public.knowledge_document knowledge_document
      on knowledge_document.document_id = content_document.id
    left join public.knowledge_structured_revision structured_revision
      on structured_revision.revision_id = vector_row.revision_id
    left join public.catalog_library catalog_library
      on catalog_library.id = vector_row.library_id
     and catalog_library.workspace_id = vector_row.workspace_id
    where %3$L::uuid is null or vector_row.library_id = %3$L::uuid
), categorized as (
    select
        *,
        case
            when not identity_consistent or not heads_consistent or not has_head
                then 'inconsistent_or_orphan'
            when readable_revision_id = revision_id
             and knowledge_readable_revision_id = revision_id
             and retrieval_ready
                then 'current_readable'
            when active_revision_id = revision_id
             and knowledge_active_revision_id = revision_id
             and has_ingest_authority
                then 'protected_active_ingest'
            when retrieval_ready
                then 'superseded_readable_history'
            else 'inconsistent_or_orphan'
        end as lifecycle_category
    from classified
)
-- END CHUNK VECTOR LIFECYCLE CLASSIFICATION
select section, relation_name, row_identity, lifecycle_category
from (
    select
        0 as sort_key,
        'vector_identity_relation'::text as section,
        %4$L::text as relation_name,
        count(*)::text as row_identity,
        ''::text as lifecycle_category
    from categorized
    where library_id = %5$L::uuid
    union all
    select
        1,
        'vector_identity',
        %6$L,
        jsonb_build_array(
            key, vector_id, workspace_id, library_id, revision_id, chunk_id,
            embedding_model_key, vector_kind, dimensions, embedding_text
        )::text,
        lifecycle_category
    from categorized
    where library_id = %7$L::uuid
) identity_stream
order by sort_key, row_identity, lifecycle_category;
$query$,
    substring(relation.relname from '_d([1-9][0-9]*)$')::integer,
    pg_catalog.format('%I.%I', namespace.nspname, relation.relname),
    :'library_id',
    relation.relname,
    :'library_id',
    relation.relname,
    :'library_id'
)
from pg_catalog.pg_class relation
join pg_catalog.pg_namespace namespace on namespace.oid = relation.relnamespace
where namespace.nspname = 'public'
  and relation.relkind in ('r', 'p')
  and relation.relname ~ '^knowledge_chunk_vector_d[1-9][0-9]*$'
order by relation.relname
\gexec

-- Entity vectors are intentionally outside revision lifecycle categories.
select pg_catalog.format($query$
select
    'entity_consistency_library' as section,
    %L as relation_name,
    %s as dimensions,
    vector_row.library_id,
    coalesce(catalog_library.slug, '<missing>') as library_slug,
    case
        when catalog_library.id is not null
         and knowledge_entity.entity_id is not null
         and knowledge_entity.workspace_id = vector_row.workspace_id
         and knowledge_entity.library_id = vector_row.library_id
         and vector_row.dimensions = %s
            then 'consistent'
        else 'inconsistent_or_orphan'
    end as consistency_category,
    count(*)::bigint as vector_rows,
    count(distinct vector_row.entity_id)::bigint as entities,
    (select count(*)::bigint from %s) as source_rows,
    (select count(distinct key)::bigint from %s) as distinct_keys,
    ((select count(*) from %s) = (select count(distinct key) from %s)) as unique_keys
from %s vector_row
left join public.knowledge_entity knowledge_entity
  on knowledge_entity.entity_id = vector_row.entity_id
left join public.catalog_library catalog_library
  on catalog_library.id = vector_row.library_id
 and catalog_library.workspace_id = vector_row.workspace_id
group by vector_row.library_id, catalog_library.slug, consistency_category
order by library_slug, consistency_category;
$query$,
    relation.relname,
    substring(relation.relname from '_d([1-9][0-9]*)$')::integer,
    substring(relation.relname from '_d([1-9][0-9]*)$')::integer,
    pg_catalog.format('%I.%I', namespace.nspname, relation.relname),
    pg_catalog.format('%I.%I', namespace.nspname, relation.relname),
    pg_catalog.format('%I.%I', namespace.nspname, relation.relname),
    pg_catalog.format('%I.%I', namespace.nspname, relation.relname),
    pg_catalog.format('%I.%I', namespace.nspname, relation.relname)
)
from pg_catalog.pg_class relation
join pg_catalog.pg_namespace namespace on namespace.oid = relation.relnamespace
where namespace.nspname = 'public'
  and relation.relkind in ('r', 'p')
  and relation.relname ~ '^knowledge_entity_vector_d[1-9][0-9]*$'
order by relation.relname
\gexec

-- Optional representative ANN plan. Enable with: psql -v run_explain=1 ...
\if :{?run_explain}
\if :run_explain
select pg_catalog.format($query$
set local plan_cache_mode = force_custom_plan;
set local hnsw.iterative_scan = strict_order;
set local hnsw.ef_search = 400;
set local hnsw.max_scan_tuples = 50000;
set local hnsw.scan_mem_multiplier = 2;

select
    'retrieval_plan_metadata' as section,
    manifest.library_id,
    catalog_library.slug as library_slug,
    manifest.embedding_model_key,
    manifest.row_count as manifest_claimed_vector_rows
from public.knowledge_vector_relation_manifest manifest
join public.catalog_library catalog_library on catalog_library.id = manifest.library_id
where manifest.relation_name = %L
  and manifest.vector_kind = 'chunk_embedding'
order by manifest.row_count desc, manifest.library_id
limit 1;

explain (analyze, buffers, wal, settings, summary, format text)
with representative_lane as materialized (
    select manifest.library_id, manifest.embedding_model_key, manifest.vector_kind
    from public.knowledge_vector_relation_manifest manifest
    where manifest.relation_name = %L
      and manifest.vector_kind = 'chunk_embedding'
    order by manifest.row_count desc, manifest.library_id
    limit 1
), query_lane as materialized (
    select
        vector_row.library_id,
        vector_row.embedding_model_key,
        vector_row.vector_kind,
        vector_row.embedding
    from %s vector_row
    join representative_lane
      on representative_lane.library_id = vector_row.library_id
     and representative_lane.embedding_model_key = vector_row.embedding_model_key
     and representative_lane.vector_kind = vector_row.vector_kind
    join public.knowledge_chunk knowledge_chunk
      on knowledge_chunk.chunk_id = vector_row.chunk_id
     and knowledge_chunk.revision_id = vector_row.revision_id
     and knowledge_chunk.workspace_id = vector_row.workspace_id
     and knowledge_chunk.library_id = vector_row.library_id
     and knowledge_chunk.chunk_state = 'ready'
    join public.knowledge_document knowledge_document
      on knowledge_document.document_id = knowledge_chunk.document_id
     and knowledge_document.workspace_id = vector_row.workspace_id
     and knowledge_document.library_id = vector_row.library_id
     and knowledge_document.readable_revision_id = vector_row.revision_id
     and knowledge_document.document_state = 'active'
     and knowledge_document.deleted_at is null
    join public.knowledge_structured_revision structured_revision
      on structured_revision.revision_id = vector_row.revision_id
     and structured_revision.document_id = knowledge_chunk.document_id
     and structured_revision.workspace_id = vector_row.workspace_id
     and structured_revision.library_id = vector_row.library_id
     and structured_revision.preparation_state = 'prepared'
    limit 1
)
select vector_row.vector_id
from %s vector_row
join public.knowledge_chunk knowledge_chunk
  on knowledge_chunk.chunk_id = vector_row.chunk_id
 and knowledge_chunk.revision_id = vector_row.revision_id
 and knowledge_chunk.workspace_id = vector_row.workspace_id
 and knowledge_chunk.library_id = vector_row.library_id
 and knowledge_chunk.chunk_state = 'ready'
join public.knowledge_document knowledge_document
  on knowledge_document.document_id = knowledge_chunk.document_id
 and knowledge_document.workspace_id = vector_row.workspace_id
 and knowledge_document.library_id = vector_row.library_id
 and knowledge_document.readable_revision_id = vector_row.revision_id
 and knowledge_document.document_state = 'active'
 and knowledge_document.deleted_at is null
join public.knowledge_structured_revision structured_revision
  on structured_revision.revision_id = vector_row.revision_id
 and structured_revision.document_id = knowledge_chunk.document_id
 and structured_revision.workspace_id = vector_row.workspace_id
 and structured_revision.library_id = vector_row.library_id
 and structured_revision.preparation_state = 'prepared'
where vector_row.library_id = (select library_id from query_lane)
  and vector_row.embedding_model_key = (select embedding_model_key from query_lane)
  and vector_row.vector_kind = (select vector_kind from query_lane)
order by vector_row.embedding <=> (select embedding from query_lane), vector_row.chunk_id
limit 8;
$query$,
    relation.relname,
    relation.relname,
    pg_catalog.format('%I.%I', namespace.nspname, relation.relname),
    pg_catalog.format('%I.%I', namespace.nspname, relation.relname)
)
from pg_catalog.pg_class relation
join pg_catalog.pg_namespace namespace on namespace.oid = relation.relnamespace
where namespace.nspname = 'public'
  and relation.relkind in ('r', 'p')
  and relation.relname ~ '^knowledge_chunk_vector_d[1-9][0-9]*$'
order by relation.relname
limit 1
\gexec
\endif
\endif

-- The canonical machine collector enables this section and hashes the ordered
-- rows while streaming stdout. Raw identifiers are never persisted in the
-- normalized evidence artifact. One transaction therefore binds lifecycle
-- counts, physical bytes, logical corpus identity and exact vector-row identity
-- to the same MVCC snapshot.
\if :{?machine_identity}
select pg_catalog.format(
    'select %L as section, %L as relation_name, count(*)::bigint as actual_rows from %s',
    'relation_actual_count',
    relation.relname,
    pg_catalog.format('%I.%I', namespace.nspname, relation.relname)
)
from pg_catalog.pg_class relation
join pg_catalog.pg_namespace namespace on namespace.oid = relation.relnamespace
where namespace.nspname = 'public'
  and relation.relkind in ('r', 'p')
  and relation.relname ~ '^knowledge_(chunk|entity)_vector_d[1-9][0-9]*$'
order by relation.relname
\gexec

select 'physical_configuration_identity' as section, row_kind, row_identity
from (
    select 'vector_relation' as row_kind,
        jsonb_build_object(
            'schema', namespace.nspname, 'relation', relation.relname,
            'relkind', relation.relkind, 'relpersistence', relation.relpersistence,
            'reloptions', relation.reloptions
        )::text as row_identity
    from pg_catalog.pg_class relation
    join pg_catalog.pg_namespace namespace on namespace.oid = relation.relnamespace
    where namespace.nspname = 'public'
      and relation.relkind in ('r', 'p')
      and relation.relname ~ '^knowledge_(chunk|entity)_vector_d[1-9][0-9]*$'
    union all
    select 'vector_index',
        jsonb_build_object(
            'schema', namespace.nspname, 'table', table_relation.relname,
            'index', index_relation.relname, 'accessMethod', access_method.amname,
            'definition', pg_catalog.pg_get_indexdef(index_relation.oid),
            'reloptions', index_relation.reloptions, 'valid', index_metadata.indisvalid,
            'ready', index_metadata.indisready
        )::text
    from pg_catalog.pg_index index_metadata
    join pg_catalog.pg_class table_relation on table_relation.oid = index_metadata.indrelid
    join pg_catalog.pg_namespace namespace on namespace.oid = table_relation.relnamespace
    join pg_catalog.pg_class index_relation on index_relation.oid = index_metadata.indexrelid
    join pg_catalog.pg_am access_method on access_method.oid = index_relation.relam
    where namespace.nspname = 'public'
      and table_relation.relname ~ '^knowledge_(chunk|entity)_vector_d[1-9][0-9]*$'
    union all
    select 'extension',
        jsonb_build_object('name', extension.extname, 'version', extension.extversion)::text
    from pg_catalog.pg_extension extension
) physical_configuration
order by row_kind, row_identity;

select
    'logical_identity' as section,
    logical_row.row_kind,
    logical_row.row_identity
from (
    select schema_relation.row_kind, '__query_visible_schema_v1__'::text as row_identity
    from (values
        ('catalog_library'), ('content_document'), ('content_document_head'),
        ('content_revision'), ('content_chunk'), ('knowledge_document'),
        ('knowledge_revision'), ('knowledge_chunk'), ('knowledge_structured_revision'),
        ('knowledge_structured_block'), ('knowledge_technical_fact'), ('knowledge_entity'),
        ('knowledge_chunk_entity_mention'), ('knowledge_evidence'),
        ('knowledge_evidence_entity_support'), ('knowledge_evidence_relation_support'),
        ('knowledge_relation'), ('runtime_graph_snapshot'), ('runtime_graph_node'),
        ('runtime_graph_edge'), ('runtime_graph_evidence'), ('runtime_graph_community'),
        ('runtime_graph_canonical_summary'), ('knowledge_vector_relation_manifest'),
        ('ai_binding'), ('ai_account'), ('ai_model_catalog'),
        ('ai_provider_catalog')
    ) schema_relation(row_kind)
    union all
    select
        'catalog_library',
        jsonb_build_object(
            'id', library_row.id,
            'workspace_id', library_row.workspace_id,
            'slug', library_row.slug,
            'display_name', library_row.display_name,
            'description', library_row.description,
            'lifecycle_state', library_row.lifecycle_state,
            'readable_projection_version', library_row.readable_projection_version,
            'extraction_prompt', library_row.extraction_prompt,
            'ai_summary', library_row.ai_summary,
            'created_by_principal_id', library_row.created_by_principal_id,
            'created_at', library_row.created_at,
            'web_ingest_policy', library_row.web_ingest_policy,
            'chunking_template', library_row.chunking_template,
            'recognition_policy', library_row.recognition_policy,
            'include_document_hint_in_mcp_answers',
                library_row.include_document_hint_in_mcp_answers,
            'retrieval_config', library_row.retrieval_config
        )::text
    from public.catalog_library library_row
    where id = :'library_id'::uuid
    union all
    select
        'content_document' as row_kind,
        to_jsonb(document_row)::text as row_identity
    from public.content_document document_row
    where library_id = :'library_id'::uuid
    union all
    select
        'content_document_head',
        to_jsonb(head_row)::text
    from public.content_document_head head_row
    where document_id in (
        select id from public.content_document where library_id = :'library_id'::uuid
    )
    union all
    select
        'content_revision',
        to_jsonb(revision_row)::text
    from public.content_revision revision_row
    where library_id = :'library_id'::uuid
    union all
    select
        'content_chunk',
        to_jsonb(chunk_row)::text
    from public.content_chunk chunk_row
    where revision_id in (
        select id from public.content_revision where library_id = :'library_id'::uuid
    )
    union all
    select
        'knowledge_document',
        to_jsonb(document_row)::text
    from public.knowledge_document document_row
    where library_id = :'library_id'::uuid
    union all
    select
        'knowledge_revision',
        to_jsonb(revision_row)::text
    from public.knowledge_revision revision_row
    where library_id = :'library_id'::uuid
    union all
    select
        'knowledge_chunk',
        to_jsonb(chunk_row)::text
    from public.knowledge_chunk chunk_row
    where library_id = :'library_id'::uuid
    union all
    select 'knowledge_structured_revision', to_jsonb(row_value)::text
    from public.knowledge_structured_revision row_value
    where library_id = :'library_id'::uuid
    union all
    select 'knowledge_structured_block', to_jsonb(row_value)::text
    from public.knowledge_structured_block row_value
    where library_id = :'library_id'::uuid
    union all
    select 'knowledge_technical_fact', to_jsonb(row_value)::text
    from public.knowledge_technical_fact row_value
    where library_id = :'library_id'::uuid
    union all
    select 'knowledge_entity', to_jsonb(row_value)::text
    from public.knowledge_entity row_value
    where library_id = :'library_id'::uuid
    union all
    select 'knowledge_chunk_entity_mention', to_jsonb(row_value)::text
    from public.knowledge_chunk_entity_mention row_value
    where library_id = :'library_id'::uuid
    union all
    select 'knowledge_evidence', to_jsonb(row_value)::text
    from public.knowledge_evidence row_value
    where library_id = :'library_id'::uuid
    union all
    select 'knowledge_evidence_entity_support', to_jsonb(row_value)::text
    from public.knowledge_evidence_entity_support row_value
    where library_id = :'library_id'::uuid
    union all
    select 'knowledge_evidence_relation_support', to_jsonb(row_value)::text
    from public.knowledge_evidence_relation_support row_value
    where library_id = :'library_id'::uuid
    union all
    select 'knowledge_relation', to_jsonb(row_value)::text
    from public.knowledge_relation row_value
    where library_id = :'library_id'::uuid
    union all
    select 'runtime_graph_snapshot', to_jsonb(row_value)::text
    from public.runtime_graph_snapshot row_value
    where library_id = :'library_id'::uuid
    union all
    select 'runtime_graph_node', to_jsonb(row_value)::text
    from public.runtime_graph_node row_value
    where library_id = :'library_id'::uuid
    union all
    select 'runtime_graph_edge', to_jsonb(row_value)::text
    from public.runtime_graph_edge row_value
    where library_id = :'library_id'::uuid
    union all
    select 'runtime_graph_evidence', to_jsonb(row_value)::text
    from public.runtime_graph_evidence row_value
    where library_id = :'library_id'::uuid
    union all
    select 'runtime_graph_community', to_jsonb(row_value)::text
    from public.runtime_graph_community row_value
    where library_id = :'library_id'::uuid
    union all
    select 'runtime_graph_canonical_summary', to_jsonb(row_value)::text
    from public.runtime_graph_canonical_summary row_value
    where library_id = :'library_id'::uuid
    union all
    -- row_count is reconciled physical vector state and intentionally changes
    -- when superseded rows are deleted. Only routing/query configuration is
    -- part of the logical corpus identity.
    select 'knowledge_vector_relation_manifest', jsonb_build_object(
        'library_id', row_value.library_id,
        'dim', row_value.dim,
        'vector_kind', row_value.vector_kind,
        'embedding_model_key', row_value.embedding_model_key,
        'relation_name', row_value.relation_name,
        'is_default', row_value.is_default,
        'promoted', row_value.promoted
    )::text
    from public.knowledge_vector_relation_manifest row_value
    where library_id = :'library_id'::uuid
    union all
    select 'ai_binding', to_jsonb(binding_row)::text
    from public.ai_binding binding_row
    where scope_kind = 'instance'
       or workspace_id = (
           select workspace_id from public.catalog_library where id = :'library_id'::uuid
       )
       or library_id = :'library_id'::uuid
    union all
    select 'ai_account', jsonb_build_object(
        'id', account_row.id,
        'scope_kind', account_row.scope_kind,
        'workspace_id', account_row.workspace_id,
        'library_id', account_row.library_id,
        'provider_catalog_id', account_row.provider_catalog_id,
        'label', account_row.label,
        'base_url', account_row.base_url,
        'credential_state', account_row.credential_state,
        'has_api_key', account_row.api_key is not null,
        'created_at', account_row.created_at,
        'updated_at', account_row.updated_at
    )::text
    from public.ai_account account_row
    where id in (
        select account_id from public.ai_binding
        where scope_kind = 'instance'
           or workspace_id = (
               select workspace_id from public.catalog_library where id = :'library_id'::uuid
           )
           or library_id = :'library_id'::uuid
    )
    union all
    select 'ai_model_catalog', to_jsonb(model_row)::text
    from public.ai_model_catalog model_row
    where id in (
        select model_catalog_id from public.ai_binding
        where scope_kind = 'instance'
           or workspace_id = (
               select workspace_id from public.catalog_library where id = :'library_id'::uuid
           )
           or library_id = :'library_id'::uuid
    )
    union all
    select 'ai_provider_catalog', to_jsonb(provider_row)::text
    from public.ai_provider_catalog provider_row
    where id in (
        select provider_catalog_id from public.ai_binding
        where scope_kind = 'instance'
           or workspace_id = (
               select workspace_id from public.catalog_library where id = :'library_id'::uuid
           )
           or library_id = :'library_id'::uuid
    )
) logical_row
order by logical_row.row_kind, logical_row.row_identity;

select pg_catalog.format($query$
select
    'entity_vector_identity' as section,
    %L as relation_name,
    jsonb_build_array(
        vector_row.key, vector_row.vector_id, vector_row.workspace_id,
        vector_row.library_id, vector_row.entity_id,
        vector_row.embedding_model_key, vector_row.vector_kind,
        vector_row.dimensions, vector_row.embedding::text
    )::text as row_identity,
    case
        when catalog_library.id is not null
         and knowledge_entity.entity_id is not null
         and knowledge_entity.workspace_id = vector_row.workspace_id
         and knowledge_entity.library_id = vector_row.library_id
            then 'consistent'
        else 'inconsistent_or_orphan'
    end as consistency_role
from %s vector_row
left join public.knowledge_entity knowledge_entity
  on knowledge_entity.entity_id = vector_row.entity_id
left join public.catalog_library catalog_library
  on catalog_library.id = vector_row.library_id
 and catalog_library.workspace_id = vector_row.workspace_id
where vector_row.library_id = %L::uuid
order by row_identity, consistency_role;
$query$,
    relation.relname,
    pg_catalog.format('%I.%I', namespace.nspname, relation.relname),
    :'library_id'
)
from pg_catalog.pg_class relation
join pg_catalog.pg_namespace namespace on namespace.oid = relation.relnamespace
where namespace.nspname = 'public'
  and relation.relkind in ('r', 'p')
  and relation.relname ~ '^knowledge_entity_vector_d[1-9][0-9]*$'
order by relation.relname
\gexec
\endif

commit;
