use super::*;

pub async fn list_libraries(
    postgres: &PgPool,
    workspace_id: Option<Uuid>,
) -> Result<Vec<CatalogLibraryRow>, sqlx::Error> {
    match workspace_id {
        Some(workspace_id) => {
            sqlx::query_as::<_, CatalogLibraryRow>(
                "select id, workspace_id, slug, display_name, description, extraction_prompt, web_ingest_policy, recognition_policy, retrieval_config, lifecycle_state::text as lifecycle_state, include_document_hint_in_mcp_answers, chunking_template, created_at, updated_at
                 from catalog_library
                 where workspace_id = $1
                 order by created_at desc",
            )
            .bind(workspace_id)
            .fetch_all(postgres)
            .await
        }
        None => {
            sqlx::query_as::<_, CatalogLibraryRow>(
                "select id, workspace_id, slug, display_name, description, extraction_prompt, web_ingest_policy, recognition_policy, retrieval_config, lifecycle_state::text as lifecycle_state, include_document_hint_in_mcp_answers, chunking_template, created_at, updated_at
                 from catalog_library
                 order by created_at desc",
            )
            .fetch_all(postgres)
            .await
        }
    }
}

pub async fn search_library_page(
    postgres: &PgPool,
    query: CatalogLibraryPageQuery<'_>,
) -> Result<CatalogLibraryPageRows, sqlx::Error> {
    let visible_library_ids = query.visible_library_ids.map(<[Uuid]>::to_vec);
    let total_count = sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from catalog_library
         where workspace_id = $1
           and (
               $2::text is null
               or position(lower($2) in lower(display_name)) > 0
               or position(lower($2) in lower(slug)) > 0
           )
           and ($3::uuid[] is null or id = any($3))",
    )
    .bind(query.workspace_id)
    .bind(query.search)
    .bind(visible_library_ids.as_deref())
    .fetch_one(postgres)
    .await?;

    let items = sqlx::query_as::<_, CatalogLibraryRow>(
        "select id, workspace_id, slug, display_name, description, extraction_prompt,
                web_ingest_policy, recognition_policy, retrieval_config,
                lifecycle_state::text as lifecycle_state,
                include_document_hint_in_mcp_answers, chunking_template, created_at, updated_at
         from catalog_library
         where workspace_id = $1
           and (
               $2::text is null
               or position(lower($2) in lower(display_name)) > 0
               or position(lower($2) in lower(slug)) > 0
           )
           and ($3::uuid[] is null or id = any($3))
         order by (id = $4) desc, updated_at desc, id desc
         limit $5 offset $6",
    )
    .bind(query.workspace_id)
    .bind(query.search)
    .bind(visible_library_ids.as_deref())
    .bind(query.pinned_library_id)
    .bind(i64::from(query.limit))
    .bind(i64::from(query.offset))
    .fetch_all(postgres)
    .await?;

    Ok(CatalogLibraryPageRows { items, total_count })
}

const ADMIN_LIBRARY_FILTER_CTES_TEMPLATE: &str = "with library_candidates as materialized (
    select
        library.id,
        library.slug,
        library.display_name,
        library.lifecycle_state::text as lifecycle_state,
        library.include_document_hint_in_mcp_answers,
        workspace.id as workspace_id,
        workspace.slug as workspace_slug,
        workspace.display_name as workspace_display_name,
        readiness.missing_binding_purposes,
        cardinality(readiness.missing_binding_purposes) = 0 as ingestion_ready
    from catalog_library library
    join catalog_workspace workspace on workspace.id = library.workspace_id
    cross join lateral (
        select coalesce(
            array_agg(required.binding_purpose order by required.ordinality),
            array[]::text[]
        ) as missing_binding_purposes
        from unnest($1::text[]) with ordinality
            as required(binding_purpose, ordinality)
        where not exists (
            select 1
            from ai_binding binding
            where binding.binding_purpose = required.binding_purpose::ai_binding_purpose
              and binding.binding_state = 'active'
              and (
                  (binding.scope_kind = 'library' and binding.library_id = library.id)
                  or (
                      binding.scope_kind = 'workspace'
                      and binding.workspace_id = library.workspace_id
                  )
                  or binding.scope_kind = 'instance'
              )
        )
    ) readiness
), billing_usage as materialized (
    select
        execution_cost.library_id,
        count(distinct execution_cost.knowledge_document_id)
            filter (where execution_cost.knowledge_document_id is not null)::bigint
            as document_count,
        coalesce(sum(execution_cost.provider_call_count), 0)::bigint
            as provider_call_count,
        coalesce(sum(execution_cost.unpriced_provider_call_count), 0)::bigint
            as unpriced_provider_call_count,
        coalesce(sum(execution_cost.total_cost), 0) as total_cost,
        count(distinct execution_cost.currency_code)::bigint as currency_count,
        coalesce(min(execution_cost.currency_code), $3) as currency_code
    from billing_execution_cost execution_cost
    group by execution_cost.library_id
), cost_health as materialized (
    select
        rollup_state.library_id,
        true as cost_blocked
    from billing_execution_cost_rollup_state rollup_state
    where rollup_state.applied_generation < rollup_state.dirty_generation
       or rollup_state.terminal_error_code is not null
    group by rollup_state.library_id
), enriched as materialized (
    select
        candidate.*,
        coalesce(billing_usage.document_count, 0) as document_count,
        coalesce(billing_usage.provider_call_count, 0) as provider_call_count,
        coalesce(billing_usage.unpriced_provider_call_count, 0) as unpriced_provider_call_count,
        coalesce(billing_usage.total_cost, 0) as total_cost,
        coalesce(billing_usage.currency_count, 0) as currency_count,
        coalesce(billing_usage.currency_code, $3) as currency_code,
        coalesce(cost_health.cost_blocked, false) as cost_blocked,
        case
            when coalesce(cost_health.cost_blocked, false)
                 or coalesce(billing_usage.currency_count, 0) > 1
            then null
            else coalesce(billing_usage.document_count, 0)
        end as document_count_value,
        case
            when coalesce(cost_health.cost_blocked, false)
                 or coalesce(billing_usage.currency_count, 0) > 1
            then null
            else coalesce(billing_usage.provider_call_count, 0)
        end as provider_call_count_value,
        case
            when coalesce(cost_health.cost_blocked, false)
                  or coalesce(billing_usage.currency_count, 0) > 1
                  or (coalesce(billing_usage.unpriced_provider_call_count, 0) > 0
                      and coalesce(billing_usage.unpriced_provider_call_count, 0)
                          >= coalesce(billing_usage.provider_call_count, 0))
            then null
            else coalesce(billing_usage.total_cost, 0)
        end as total_cost_value,
        case
            when coalesce(cost_health.cost_blocked, false)
                 or coalesce(billing_usage.currency_count, 0) > 1
            then null
             when coalesce(billing_usage.unpriced_provider_call_count, 0) > 0
              and coalesce(billing_usage.unpriced_provider_call_count, 0)
                  >= coalesce(billing_usage.provider_call_count, 0)
             then 'unknown'
             when coalesce(billing_usage.unpriced_provider_call_count, 0) > 0
             then 'partial'
            else 'complete'
        end as coverage_state,
        (
            select count(*)::bigint
            from ingest_job job
            where job.library_id = candidate.id
              and job.queue_state = 'queued'
        ) as queued_job_count
    from library_candidates candidate
    left join billing_usage on billing_usage.library_id = candidate.id
    left join cost_health on cost_health.library_id = candidate.id
), filtered as materialized (
    select *
    from enriched
    where (
        (
             $2::text is null and $28::text is null
              and cardinality($29::bigint[]) = 0 and jsonb_array_length($30::jsonb) = 0
             and not $31::bool
             and cardinality($32::bool[]) = 0 and cardinality($33::text[]) = 0
        )
        or ($2::text is not null and (
            position(lower($2) in lower(display_name)) > 0
            or position(lower($2) in lower(slug)) > 0
            or position(lower($2) in lower(workspace_display_name)) > 0
            or position(lower($2) in lower(workspace_slug)) > 0
        ))
         or ($28::text is not null and (
             position(lower($28) in lower(display_name)) > 0
             or position(lower($28) in lower(slug)) > 0
             or position(lower($28) in lower(workspace_display_name)) > 0
             or position(lower($28) in lower(workspace_slug)) > 0
         ))
         or (cardinality($29::bigint[]) > 0 and (
             document_count_value = any($29)
             or queued_job_count = any($29)
             or provider_call_count_value = any($29)
         ))
          or {cost_search}
         or ($31::bool and total_cost_value is null)
         or (cardinality($32::bool[]) > 0 and ingestion_ready = any($32))
         or (cardinality($33::text[]) > 0 and lifecycle_state = any($33))
    )
      and (not $4::bool or workspace_display_name = any($5::text[]) or ($6::bool and workspace_display_name is null))
      and (not $7::bool or lifecycle_state = any($8::text[]) or ($9::bool and lifecycle_state is null))
      and (not $10::bool or ingestion_ready = any($11::bool[]) or ($12::bool and ingestion_ready is null))
      and (not $13::bool or display_name = any($14::text[]) or ($15::bool and display_name is null))
      and (not $16::bool or document_count_value = any($17::bigint[]) or ($18::bool and document_count_value is null))
      and (not $19::bool or queued_job_count = any($20::bigint[]) or ($21::bool and queued_job_count is null))
       and (not $22::bool or exists (
             select 1 from jsonb_to_recordset($23::jsonb) as selected(
                 amount numeric, \"currencyCode\" text, coverage text, \"unpricedCallCount\" bigint
             ) where currency_code = selected.\"currencyCode\"
               and round(total_cost_value, 2) = selected.amount
               and coverage_state = selected.coverage
               and unpriced_provider_call_count = selected.\"unpricedCallCount\"
           ) or ($24::bool and total_cost_value is null))
       and (not $25::bool or provider_call_count_value = any($26::bigint[]) or ($27::bool and provider_call_count_value is null))
)";

/// Only repository-owned projections and parameter numbers enter this SQL
/// fragment. Search values remain bound JSON, shared by global and facet search.
fn cost_search_sql(parameter: u8, columns: [&str; 4]) -> String {
    let [amount, currency, coverage, unpriced] = columns;
    format!(
        "exists (select 1 from jsonb_to_recordset(${parameter}::jsonb) as term(\
            amount numeric, \"currencyCode\" text, coverage text, \"unpricedCallCount\" bigint\
        ) where {amount} is not null \
          and (term.amount is null or {amount} = term.amount)\
          and (term.\"currencyCode\" is null or {currency} = term.\"currencyCode\")\
          and (term.coverage is null or {coverage} = term.coverage)\
          and (term.\"unpricedCallCount\" is null or {unpriced} = term.\"unpricedCallCount\"))"
    )
}

fn admin_library_filter_ctes() -> String {
    ADMIN_LIBRARY_FILTER_CTES_TEMPLATE.replace(
        "{cost_search}",
        &cost_search_sql(
            30,
            [
                "round(total_cost_value, 2)",
                "currency_code",
                "coverage_state",
                "unpriced_provider_call_count",
            ],
        ),
    )
}

/// Bind the fixed `$1..$33` scope of `ADMIN_LIBRARY_FILTER_CTES` in order. Every
/// consumer — page, facet counts and facet values — binds the same 33 values so
/// the shared CTE cannot be read with a different scope. `$28..$33` are the
/// typed generic search hints; the caller's own facet/list parameters follow.
macro_rules! bind_admin_library_scope {
    ($query:expr, $purposes:expr, $scope:expr, $filters:expr) => {{
        $query
            .bind($purposes)
            .bind($scope.search)
            .bind(DEFAULT_ADMIN_LIBRARY_CURRENCY_CODE)
            .bind($filters.workspace.present)
            .bind($filters.workspace.values.as_slice())
            .bind($filters.workspace.include_null)
            .bind($filters.lifecycle.present)
            .bind($filters.lifecycle.values.as_slice())
            .bind($filters.lifecycle.include_null)
            .bind($filters.readiness.present)
            .bind($filters.readiness.values.as_slice())
            .bind($filters.readiness.include_null)
            .bind($filters.library.present)
            .bind($filters.library.values.as_slice())
            .bind($filters.library.include_null)
            .bind($filters.documents.present)
            .bind($filters.documents.values.as_slice())
            .bind($filters.documents.include_null)
            .bind($filters.queue.present)
            .bind($filters.queue.values.as_slice())
            .bind($filters.queue.include_null)
            .bind($filters.cost.present)
            .bind(sqlx::types::Json(&$filters.cost.values))
            .bind($filters.cost.include_null)
            .bind($filters.calls.present)
            .bind($filters.calls.values.as_slice())
            .bind($filters.calls.include_null)
            .bind($scope.normalized_search)
            .bind($scope.numbers)
            .bind(sqlx::types::Json($scope.costs))
            .bind($scope.unavailable)
            .bind($scope.readiness)
            .bind($scope.lifecycle)
    }};
}

pub async fn list_admin_library_page(
    postgres: &PgPool,
    query: AdminLibraryPageQuery<'_>,
) -> Result<(Vec<AdminLibraryPageRow>, AdminLibraryFacetsRow), sqlx::Error> {
    let order = query.order.sql();
    let scope_sql = admin_library_filter_ctes();
    let item_sql = format!(
        "{scope_sql}
         select
            id,
            slug,
            display_name,
            lifecycle_state,
            include_document_hint_in_mcp_answers,
            workspace_id,
            workspace_slug,
            workspace_display_name,
            missing_binding_purposes,
            queued_job_count,
            cost_blocked,
            currency_count,
            total_cost,
            currency_code,
            document_count,
            provider_call_count,
            unpriced_provider_call_count,
            coverage_state
         from filtered
         order by {order}
          limit $34 offset $35"
    );
    let items = bind_admin_library_scope!(
        sqlx::query_as::<_, AdminLibraryPageRow>(sqlx::AssertSqlSafe(item_sql)),
        query.required_binding_purposes,
        query.scope,
        query.filters
    )
    .bind(i64::from(query.limit))
    .bind(i64::from(query.offset))
    .fetch_all(postgres);

    let facet_sql = format!(
        "{scope_sql}, facet_counts as (
            select
                count(*)::bigint as total_count,
                count(distinct workspace_id)::bigint as workspace_count,
                count(*) filter (where ingestion_ready)::bigint as ready_count,
                count(*) filter (where lifecycle_state = 'active')::bigint as active_count
            from filtered
         ), aggregate_cost as (
            select
                exists (
                    select 1
                    from billing_execution_cost_rollup_state rollup_state
                    join filtered on filtered.id = rollup_state.library_id
                    where rollup_state.applied_generation < rollup_state.dirty_generation
                       or rollup_state.terminal_error_code is not null
                ) as cost_blocked,
                count(distinct execution_cost.currency_code)::bigint as currency_count,
                coalesce(sum(execution_cost.total_cost), 0) as total_cost,
                coalesce(min(execution_cost.currency_code), $3) as currency_code,
                count(distinct execution_cost.knowledge_document_id)
                    filter (where execution_cost.knowledge_document_id is not null)::bigint
                    as document_count,
                coalesce(sum(execution_cost.provider_call_count), 0)::bigint
                    as provider_call_count,
                coalesce(sum(execution_cost.unpriced_provider_call_count), 0)::bigint
                    as unpriced_provider_call_count
            from filtered
            left join billing_execution_cost execution_cost
              on execution_cost.library_id = filtered.id
         )
         select facet_counts.*, aggregate_cost.*
         from facet_counts
         cross join aggregate_cost"
    );
    let facets = bind_admin_library_scope!(
        sqlx::query_as::<_, AdminLibraryFacetsRow>(sqlx::AssertSqlSafe(facet_sql)),
        query.required_binding_purposes,
        query.scope,
        query.filters
    )
    .fetch_one(postgres);

    tokio::try_join!(items, facets)
}

#[derive(Default)]
struct AdminLibraryFacetCursorBindings<'a> {
    text: Option<&'a str>,
    number: Option<i64>,
    decimal: Option<Decimal>,
    flag: Option<bool>,
    currency: Option<&'a str>,
    coverage: Option<&'a str>,
    unpriced_call_count: Option<i64>,
}

impl<'a> AdminLibraryFacetCursorBindings<'a> {
    fn from_cursor(cursor: Option<&'a AdminLibraryFacetCursorValue>) -> Self {
        match cursor {
            None => Self::default(),
            Some(AdminLibraryFacetCursorValue::Text(value)) => {
                Self { text: Some(value.as_str()), ..Self::default() }
            }
            Some(AdminLibraryFacetCursorValue::Number(value)) => {
                Self { number: Some(*value), ..Self::default() }
            }
            Some(AdminLibraryFacetCursorValue::Flag(value)) => {
                Self { flag: Some(*value), ..Self::default() }
            }
            Some(AdminLibraryFacetCursorValue::Cost {
                amount,
                currency,
                coverage,
                unpriced_call_count,
            }) => Self {
                decimal: Some(*amount),
                currency: Some(currency.as_str()),
                coverage: Some(coverage.as_str()),
                unpriced_call_count: Some(*unpriced_call_count),
                ..Self::default()
            },
        }
    }
}

/// One bounded, self-excluding page of distinct values for a facet column. The
/// selected column's own filter is dropped, while every other search/filter
/// predicate and the fixed `$1..$33` parameter layout stay identical to the
/// page query, so a facet count and the page it opens cannot disagree.
pub async fn list_admin_library_facet_page(
    postgres: &PgPool,
    query: AdminLibraryFacetQuery<'_>,
) -> Result<AdminLibraryFacetPage, sqlx::Error> {
    let value_sql = admin_library_facet_value_sql(query.column);
    let extra_sql = admin_library_facet_extra_sql(query.column);
    let cursor_sql = admin_library_facet_cursor_sql(query.column);
    let order_column = admin_library_facet_order_sql(query.column);
    let scope_sql = admin_library_filter_ctes();
    let facet_cost_search =
        cost_search_sql(44, ["value_decimal", "value_currency", "value_coverage", "value_number"]);
    let facet_sql = format!(
        "{scope_sql}, facet_rows as materialized (
            select
                {value_sql},
                {extra_sql},
                count(*)::bigint as facet_count
            from filtered
            group by 1, 2, 3, 4, 5, 6
         )
         select
             value_text, value_number, value_decimal, value_flag,
             value_currency, value_coverage, facet_count
         from facet_rows
         where (
               ($34::text is null and jsonb_array_length($44::jsonb) = 0
                and cardinality($45::bigint[]) = 0 and not $46::bool
                and cardinality($47::bool[]) = 0 and cardinality($48::text[]) = 0)
             or position(
                  lower($34) in lower(concat_ws(' ',
                     value_text, value_number::text, value_decimal::text, value_flag::text,
                     value_coverage, value_currency
                 ))
              ) > 0
              or {facet_cost_search}
              or value_number = any($45::bigint[])
              or ($46::bool and value_text is null and value_number is null
                  and value_decimal is null and value_flag is null)
              or value_flag = any($47::bool[])
              or value_text = any($48::text[])
         )
           and {cursor_sql}
         order by {order_column} asc nulls last
          limit $43"
    );
    let cursor = AdminLibraryFacetCursorBindings::from_cursor(query.cursor);
    let cursor_present = query.cursor.is_some();
    // The repository owns self-exclusion: the selected column's own filter never
    // narrows its own value list, so a count and the page it opens agree.
    let filters = query.filters.clone().without(query.column);
    let rows = bind_admin_library_scope!(
        sqlx::query_as::<_, AdminLibraryFacetRow>(sqlx::AssertSqlSafe(facet_sql)),
        query.required_binding_purposes,
        query.scope,
        &filters
    )
    .bind(query.facet_search)
    .bind(cursor.text)
    .bind(cursor.number)
    .bind(cursor.decimal)
    .bind(cursor.flag)
    .bind(cursor.currency)
    .bind(cursor.coverage)
    .bind(cursor.unpriced_call_count)
    .bind(cursor_present)
    .bind(i64::from(query.limit) + 1)
    .bind(sqlx::types::Json(query.facet_hints.costs))
    .bind(query.facet_hints.numbers)
    .bind(query.facet_hints.unavailable)
    .bind(query.facet_hints.readiness)
    .bind(query.facet_hints.lifecycle)
    .fetch_all(postgres)
    .await?;
    let has_more = rows.len() > query.limit as usize;
    let rows = if has_more { rows.into_iter().take(query.limit as usize).collect() } else { rows };
    Ok(AdminLibraryFacetPage { rows, has_more })
}

pub async fn list_library_identities(
    postgres: &PgPool,
) -> Result<Vec<CatalogLibraryIdentityRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogLibraryIdentityRow>("select id, workspace_id from catalog_library")
        .fetch_all(postgres)
        .await
}

pub async fn list_active_library_ids(postgres: &PgPool) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar::<_, Uuid>(
        "select id
         from catalog_library
         where lifecycle_state = 'active'::catalog_library_lifecycle_state
         order by id",
    )
    .fetch_all(postgres)
    .await
}

pub(super) const QUERY_AUTHORIZED_LIBRARY_CANDIDATES_SQL: &str = "select id
     from catalog_library
     where lifecycle_state = 'active'::catalog_library_lifecycle_state
       and (
           $1::boolean
           or workspace_id = any($2::uuid[])
           or id = any($3::uuid[])
       )
     order by id
     limit 2";

/// Return at most two active library ids covered by an already-authorized
/// global/workspace/library scope.
///
/// Two rows are sufficient to distinguish the only valid inference case
/// (exactly one candidate) from both zero and ambiguous candidates. Keeping
/// the projection to `id` prevents catalog prompts and configuration from
/// crossing tenant boundaries merely to make that decision.
pub async fn list_query_authorized_library_candidates(
    postgres: &PgPool,
    all_libraries: bool,
    workspace_ids: &[Uuid],
    library_ids: &[Uuid],
) -> Result<Vec<Uuid>, sqlx::Error> {
    if !all_libraries && workspace_ids.is_empty() && library_ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_scalar::<_, Uuid>(QUERY_AUTHORIZED_LIBRARY_CANDIDATES_SQL)
        .bind(all_libraries)
        .bind(workspace_ids)
        .bind(library_ids)
        .fetch_all(postgres)
        .await
}

pub async fn get_library_by_id(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<Option<CatalogLibraryRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogLibraryRow>(
        "select id, workspace_id, slug, display_name, description, extraction_prompt, web_ingest_policy, recognition_policy, retrieval_config, lifecycle_state::text as lifecycle_state, include_document_hint_in_mcp_answers, chunking_template, created_at, updated_at
         from catalog_library
         where id = $1",
    )
    .bind(library_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_library_by_workspace_and_slug(
    postgres: &PgPool,
    workspace_id: Uuid,
    slug: &str,
) -> Result<Option<CatalogLibraryRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogLibraryRow>(
        "select id, workspace_id, slug, display_name, description, extraction_prompt, web_ingest_policy, recognition_policy, retrieval_config, lifecycle_state::text as lifecycle_state, include_document_hint_in_mcp_answers, chunking_template, created_at, updated_at
         from catalog_library
         where workspace_id = $1 and slug = $2",
    )
    .bind(workspace_id)
    .bind(slug)
    .fetch_optional(postgres)
    .await
}
