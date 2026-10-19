-- Provider cost by query turn and by ingested document, split by the billing
-- stage label stored in billing_provider_call.call_kind.
--
-- Usage (psql only):
--
--   psql "$DATABASE_URL" -X -v ON_ERROR_STOP=1 \
--     -v since='2026-08-04T00:00:00Z' \
--     -v until='2026-08-11T00:00:00Z' \
--     -f scripts/ops/profile-billing-stage-costs.sql
--
-- The half-open window is applied to provider-call start time. Costs remain
-- separated by currency. An `unpriced` row is deliberately visible when a
-- call has no charge, rather than silently treating missing pricing as paid
-- zero-cost work.
--
-- Schema evidence (apps/api/migrations/0001_init.sql):
--   billing_charge.id, usage_id, currency_code, total_price          1613-1617
--   billing_execution_cost.owning_execution_kind, owning_execution_id 1628-1629
--   billing_execution_cost.knowledge_document_id                    1637
--   billing_provider_call.id                                        1670
--   billing_provider_call.owning_execution_kind                     1674
--   billing_provider_call.owning_execution_id                       1675
--   billing_provider_call.call_kind, started_at                     1678-1679
--   billing_usage.id, provider_call_id                              1693-1694
--   ingest_attempt.id, job_id                                      2193-2194
--   ingest_job.id, knowledge_document_id                           2218, 2224
--   query_execution.id, request_turn_id, response_turn_id          3026, 3031-3032
--   runtime_graph_extraction.id, document_id                       3325, 3328

\set ON_ERROR_STOP on
\pset border 2
\pset null 'n/a'

with provider_call_cost as (
    select
        provider_call.id as provider_call_id,
        provider_call.owning_execution_kind::text as execution_kind,
        provider_call.owning_execution_id,
        provider_call.call_kind as stage,
        charge.currency_code,
        coalesce(sum(charge.total_price), 0::numeric) as total_cost,
        count(distinct usage.id) filter (where charge.id is not null)
            as charged_usage_count,
        count(distinct usage.id) filter (where charge.id is null)
            as unpriced_usage_count,
        bool_or(charge.id is not null) as has_priced_usage
    from public.billing_provider_call as provider_call
    left join public.billing_usage as usage
      on usage.provider_call_id = provider_call.id
    left join public.billing_charge as charge
      on charge.usage_id = usage.id
    where provider_call.started_at >= :'since'::timestamptz
      and provider_call.started_at < :'until'::timestamptz
      and provider_call.owning_execution_kind::text in (
          'query_execution',
          'ingest_attempt',
          'graph_extraction_attempt'
      )
    group by
        provider_call.id,
        provider_call.owning_execution_kind,
        provider_call.owning_execution_id,
        provider_call.call_kind,
        charge.currency_code
),
attributed_cost as (
    select
        case
            when call_cost.execution_kind = 'query_execution'
             and coalesce(query.response_turn_id, query.request_turn_id) is not null
                then 'turn'
            when call_cost.execution_kind = 'query_execution'
                then 'query_execution_without_turn'
            when call_cost.execution_kind = 'ingest_attempt'
             and coalesce(execution_cost.knowledge_document_id, job.knowledge_document_id)
                 is not null
                then 'ingest_document'
            when call_cost.execution_kind = 'graph_extraction_attempt'
             and coalesce(execution_cost.knowledge_document_id, graph.document_id) is not null
                then 'ingest_document'
            else 'ingest_without_document'
        end as cost_scope,
        coalesce(
            case
                when call_cost.execution_kind = 'query_execution'
                    then coalesce(query.response_turn_id, query.request_turn_id)
                when call_cost.execution_kind = 'ingest_attempt'
                    then coalesce(
                        execution_cost.knowledge_document_id,
                        job.knowledge_document_id
                    )
                when call_cost.execution_kind = 'graph_extraction_attempt'
                    then coalesce(execution_cost.knowledge_document_id, graph.document_id)
                else null
            end,
            call_cost.owning_execution_id
        ) as subject_id,
        call_cost.provider_call_id,
        call_cost.stage,
        call_cost.currency_code,
        call_cost.total_cost,
        call_cost.charged_usage_count,
        call_cost.unpriced_usage_count,
        call_cost.has_priced_usage
    from provider_call_cost as call_cost
    left join public.billing_execution_cost as execution_cost
      on execution_cost.owning_execution_kind::text = call_cost.execution_kind
     and execution_cost.owning_execution_id = call_cost.owning_execution_id
    left join public.query_execution as query
      on call_cost.execution_kind = 'query_execution'
     and query.id = call_cost.owning_execution_id
    left join public.ingest_attempt as attempt
      on call_cost.execution_kind = 'ingest_attempt'
     and attempt.id = call_cost.owning_execution_id
    left join public.ingest_job as job
      on job.id = attempt.job_id
    left join public.runtime_graph_extraction as graph
      on call_cost.execution_kind = 'graph_extraction_attempt'
     and graph.id = call_cost.owning_execution_id
)
select
    attributed.cost_scope,
    attributed.subject_id,
    attributed.stage,
    coalesce(attributed.currency_code, 'unpriced') as currency_code,
    round(sum(attributed.total_cost), 8) as total_cost,
    count(distinct attributed.provider_call_id) as provider_call_count,
    sum(attributed.charged_usage_count) as charged_usage_count,
    sum(attributed.unpriced_usage_count) as unpriced_usage_count,
    count(distinct attributed.provider_call_id)
        filter (where not attributed.has_priced_usage) as unpriced_provider_call_count
from attributed_cost as attributed
group by
    attributed.cost_scope,
    attributed.subject_id,
    attributed.stage,
    coalesce(attributed.currency_code, 'unpriced')
order by
    attributed.cost_scope,
    attributed.subject_id,
    attributed.stage,
    currency_code;
