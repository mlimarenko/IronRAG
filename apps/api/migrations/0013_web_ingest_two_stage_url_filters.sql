alter table content_web_ingest_run
    add column if not exists crawl_allow_patterns jsonb not null default '[]'::jsonb,
    add column if not exists crawl_block_patterns jsonb not null default '[]'::jsonb,
    add column if not exists materialization_allow_patterns jsonb not null default '[]'::jsonb,
    add column if not exists materialization_block_patterns jsonb not null default '[]'::jsonb;

do $$
begin
    if exists (
        select 1
        from information_schema.columns
        where table_name = 'content_web_ingest_run'
          and column_name = 'url_filter_mode'
    ) and exists (
        select 1
        from information_schema.columns
        where table_name = 'content_web_ingest_run'
          and column_name = 'url_patterns'
    ) then
        execute $sql$
            update content_web_ingest_run
            set
                crawl_block_patterns = case
                    when url_filter_mode = 'blocklist'::web_url_filter_mode
                         and jsonb_typeof(url_patterns) = 'array'
                        then url_patterns
                    else crawl_block_patterns
                end,
                materialization_allow_patterns = case
                    when url_filter_mode = 'allowlist'::web_url_filter_mode
                         and jsonb_typeof(url_patterns) = 'array'
                        then url_patterns
                    else materialization_allow_patterns
                end,
                materialization_block_patterns = case
                    when url_filter_mode = 'blocklist'::web_url_filter_mode
                         and jsonb_typeof(url_patterns) = 'array'
                        then url_patterns
                    else materialization_block_patterns
                end
            where crawl_allow_patterns = '[]'::jsonb
              and crawl_block_patterns = '[]'::jsonb
              and materialization_allow_patterns = '[]'::jsonb
              and materialization_block_patterns = '[]'::jsonb
        $sql$;
    end if;
end;
$$;

alter table catalog_library
    alter column web_ingest_policy set default '{
        "crawlFilter": {
            "allowPatterns": [],
            "blockPatterns": [
                {"kind": "path_prefix", "value": "/aboutconfluencepage.action"},
                {"kind": "path_prefix", "value": "/collector/pages.action"},
                {"kind": "path_prefix", "value": "/dashboard/configurerssfeed.action"},
                {"kind": "path_prefix", "value": "/exportword"},
                {"kind": "path_prefix", "value": "/forgotuserpassword.action"},
                {"kind": "path_prefix", "value": "/label/"},
                {"kind": "path_prefix", "value": "/labels/"},
                {"kind": "path_prefix", "value": "/login.action"},
                {"kind": "path_prefix", "value": "/pages/diffpages.action"},
                {"kind": "path_prefix", "value": "/pages/diffpagesbyversion.action"},
                {"kind": "path_prefix", "value": "/pages/listundefinedpages.action"},
                {"kind": "path_prefix", "value": "/pages/reorderpages.action"},
                {"kind": "path_prefix", "value": "/pages/viewinfo.action"},
                {"kind": "path_prefix", "value": "/pages/viewpageattachments.action"},
                {"kind": "path_prefix", "value": "/pages/viewpreviousversions.action"},
                {"kind": "path_prefix", "value": "/pages/worddav/"},
                {"kind": "path_prefix", "value": "/plugins/recently-updated/"},
                {"kind": "path_prefix", "value": "/plugins/viewsource/viewpagesrc.action"},
                {"kind": "path_prefix", "value": "/dashboard.action"},
                {"kind": "path_prefix", "value": "/spacedirectory/view.action"},
                {"kind": "path_prefix", "value": "/spaces/flyingpdf/pdfpageexport.action"},
                {"kind": "path_prefix", "value": "/spaces/listattachmentsforspace.action"},
                {"kind": "path_prefix", "value": "/spaces/listrssfeeds.action"},
                {"kind": "path_prefix", "value": "/spaces/viewspacesummary.action"},
                {"kind": "glob", "value": "*/display/~*"},
                {"kind": "glob", "value": "*fileExtension=*"},
                {"kind": "glob", "value": "*labels=*"},
                {"kind": "glob", "value": "*metadataLink=*"},
                {"kind": "glob", "value": "*navigatingVersions=*"},
                {"kind": "glob", "value": "*openId=*"},
                {"kind": "glob", "value": "*originalVersion=*"},
                {"kind": "glob", "value": "*os_destination=*"},
                {"kind": "glob", "value": "*permissionViolation=*"},
                {"kind": "glob", "value": "*permissionviolation=*"},
                {"kind": "glob", "value": "*revisedVersion=*"},
                {"kind": "glob", "value": "*selectedPageVersions=*"},
                {"kind": "glob", "value": "*sortBy=*"}
            ]
        },
        "materializationFilter": {
            "allowPatterns": [],
            "blockPatterns": []
        }
    }'::jsonb;

with default_patterns as (
    select '[
        {"kind": "path_prefix", "value": "/aboutconfluencepage.action"},
        {"kind": "path_prefix", "value": "/collector/pages.action"},
        {"kind": "path_prefix", "value": "/dashboard/configurerssfeed.action"},
        {"kind": "path_prefix", "value": "/exportword"},
        {"kind": "path_prefix", "value": "/forgotuserpassword.action"},
        {"kind": "path_prefix", "value": "/label/"},
        {"kind": "path_prefix", "value": "/labels/"},
        {"kind": "path_prefix", "value": "/login.action"},
        {"kind": "path_prefix", "value": "/pages/diffpages.action"},
        {"kind": "path_prefix", "value": "/pages/diffpagesbyversion.action"},
        {"kind": "path_prefix", "value": "/pages/listundefinedpages.action"},
        {"kind": "path_prefix", "value": "/pages/reorderpages.action"},
        {"kind": "path_prefix", "value": "/pages/viewinfo.action"},
        {"kind": "path_prefix", "value": "/pages/viewpageattachments.action"},
        {"kind": "path_prefix", "value": "/pages/viewpreviousversions.action"},
        {"kind": "path_prefix", "value": "/pages/worddav/"},
        {"kind": "path_prefix", "value": "/plugins/recently-updated/"},
        {"kind": "path_prefix", "value": "/plugins/viewsource/viewpagesrc.action"},
        {"kind": "path_prefix", "value": "/dashboard.action"},
        {"kind": "path_prefix", "value": "/spacedirectory/view.action"},
        {"kind": "path_prefix", "value": "/spaces/flyingpdf/pdfpageexport.action"},
        {"kind": "path_prefix", "value": "/spaces/listattachmentsforspace.action"},
        {"kind": "path_prefix", "value": "/spaces/listrssfeeds.action"},
        {"kind": "path_prefix", "value": "/spaces/viewspacesummary.action"},
        {"kind": "glob", "value": "*/display/~*"},
        {"kind": "glob", "value": "*fileExtension=*"},
        {"kind": "glob", "value": "*labels=*"},
        {"kind": "glob", "value": "*metadataLink=*"},
        {"kind": "glob", "value": "*navigatingVersions=*"},
        {"kind": "glob", "value": "*openId=*"},
        {"kind": "glob", "value": "*originalVersion=*"},
        {"kind": "glob", "value": "*os_destination=*"},
        {"kind": "glob", "value": "*permissionViolation=*"},
        {"kind": "glob", "value": "*permissionviolation=*"},
        {"kind": "glob", "value": "*revisedVersion=*"},
        {"kind": "glob", "value": "*selectedPageVersions=*"},
        {"kind": "glob", "value": "*sortBy=*"}
    ]'::jsonb as patterns
),
normalized_policy as (
    select
        catalog_library.id,
        case
            when jsonb_typeof(web_ingest_policy #> '{crawlFilter,allowPatterns}') = 'array'
                then web_ingest_policy #> '{crawlFilter,allowPatterns}'
            else '[]'::jsonb
        end as crawl_allow_patterns,
        case
            when jsonb_typeof(web_ingest_policy #> '{crawlFilter,blockPatterns}') = 'array'
                then web_ingest_policy #> '{crawlFilter,blockPatterns}'
            when web_ingest_policy #>> '{urlFilter,mode}' = 'blocklist'
                 and jsonb_typeof(web_ingest_policy #> '{urlFilter,patterns}') = 'array'
                then (
                    select coalesce(jsonb_agg(distinct pattern), '[]'::jsonb)
                    from jsonb_array_elements(
                        default_patterns.patterns || (web_ingest_policy #> '{urlFilter,patterns}')
                    ) as pattern
                )
            when coalesce(web_ingest_policy, '{}'::jsonb) ? 'urlFilter'
                then default_patterns.patterns
            else default_patterns.patterns
        end as crawl_block_patterns,
        case
            when jsonb_typeof(web_ingest_policy #> '{materializationFilter,allowPatterns}') = 'array'
                then web_ingest_policy #> '{materializationFilter,allowPatterns}'
            when web_ingest_policy #>> '{urlFilter,mode}' = 'allowlist'
                 and jsonb_typeof(web_ingest_policy #> '{urlFilter,patterns}') = 'array'
                then web_ingest_policy #> '{urlFilter,patterns}'
            else '[]'::jsonb
        end as materialization_allow_patterns,
        case
            when jsonb_typeof(web_ingest_policy #> '{materializationFilter,blockPatterns}') = 'array'
                then web_ingest_policy #> '{materializationFilter,blockPatterns}'
            when web_ingest_policy #>> '{urlFilter,mode}' = 'blocklist'
                 and jsonb_typeof(web_ingest_policy #> '{urlFilter,patterns}') = 'array'
                then web_ingest_policy #> '{urlFilter,patterns}'
            else '[]'::jsonb
        end as materialization_block_patterns
    from catalog_library
    cross join default_patterns
)
update catalog_library target_library
set web_ingest_policy = jsonb_build_object(
    'crawlFilter',
    jsonb_build_object(
        'allowPatterns', normalized_policy.crawl_allow_patterns,
        'blockPatterns', normalized_policy.crawl_block_patterns
    ),
    'materializationFilter',
    jsonb_build_object(
        'allowPatterns', normalized_policy.materialization_allow_patterns,
        'blockPatterns', normalized_policy.materialization_block_patterns
    )
)
from normalized_policy
where normalized_policy.id = target_library.id
  and (
      coalesce(target_library.web_ingest_policy, '{}'::jsonb) ? 'urlFilter'
      or not (coalesce(target_library.web_ingest_policy, '{}'::jsonb) ? 'crawlFilter')
      or not (coalesce(target_library.web_ingest_policy, '{}'::jsonb) ? 'materializationFilter')
  );

alter table content_web_ingest_run
    drop constraint if exists content_web_ingest_run_url_patterns_array_check,
    drop column if exists url_filter_mode,
    drop column if exists url_patterns;

drop type if exists web_url_filter_mode;

do $$
begin
    if not exists (
        select 1
        from pg_constraint
        where conname = 'content_web_ingest_run_crawl_allow_patterns_array_check'
    ) then
        alter table content_web_ingest_run
            add constraint content_web_ingest_run_crawl_allow_patterns_array_check
            check (jsonb_typeof(crawl_allow_patterns) = 'array');
    end if;

    if not exists (
        select 1
        from pg_constraint
        where conname = 'content_web_ingest_run_crawl_block_patterns_array_check'
    ) then
        alter table content_web_ingest_run
            add constraint content_web_ingest_run_crawl_block_patterns_array_check
            check (jsonb_typeof(crawl_block_patterns) = 'array');
    end if;

    if not exists (
        select 1
        from pg_constraint
        where conname = 'content_web_ingest_run_materialization_allow_patterns_array_check'
    ) then
        alter table content_web_ingest_run
            add constraint content_web_ingest_run_materialization_allow_patterns_array_check
            check (jsonb_typeof(materialization_allow_patterns) = 'array');
    end if;

    if not exists (
        select 1
        from pg_constraint
        where conname = 'content_web_ingest_run_materialization_block_patterns_array_check'
    ) then
        alter table content_web_ingest_run
            add constraint content_web_ingest_run_materialization_block_patterns_array_check
            check (jsonb_typeof(materialization_block_patterns) = 'array');
    end if;
end;
$$;
