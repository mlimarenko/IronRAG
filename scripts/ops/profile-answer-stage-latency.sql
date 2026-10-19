-- Answer-stage wall-time decomposition for successful query work.
--
-- Usage (psql only):
--
--   psql "$DATABASE_URL" -X -v ON_ERROR_STOP=1 \
--     -v since='2026-08-04T00:00:00Z' \
--     -v until='2026-08-11T00:00:00Z' \
--     -f scripts/ops/profile-answer-stage-latency.sql
--
-- Optional: -v max_answer_stages=50000 (default 250000).
--
--
-- POPULATION AND BOUND
--
-- Population: one row per `answer` stage record whose `started_at` falls in
-- the half-open [since, until) window, whose state is `completed` or
-- `recovered`, and whose runtime execution is a `query_answer` task owned by
-- a `query_execution`. Everything downstream is keyed on that row set; no
-- later stage widens it.
--
-- Two bounds protect the run, and both are enforced before any expensive
-- work happens:
--
--  1. The window. `runtime_stage_record` has no index on `started_at` or
--     `stage_kind` (its only indexes are the `id` primary key at 4648 and
--     `(runtime_execution_id, stage_ordinal, attempt_no)` at 4656/5777), so
--     the driving access is one sequential scan of that table whatever the
--     window is. The window bounds everything after that scan, which is
--     where all the real cost lives. An operator who wants the scan itself
--     bounded needs this index, which this script deliberately does not add:
--
--       create index concurrently idx_runtime_stage_record_answer_started_at
--         on public.runtime_stage_record (started_at)
--         where stage_kind = 'answer';
--
--  2. `max_answer_stages`. The population is written to a temporary table
--     and counted before the report runs; over the cap the script ABORTS and
--     tells the operator to narrow the window. It never samples. A sampled
--     population would print p50/p95 figures that look authoritative and are
--     not, so the failure is loud instead.
--
-- Per stage the report reads the debug snapshot exactly once and reduces it
-- to scalars in place. `query_llm_context_snapshot.snapshot_json` holds the
-- whole provider wire payload — every `requestMessages` entry and every tool
-- `resultText` — so it is measured in hundreds of kilobytes per row. It must
-- never cross a CTE boundary (a materialized CTE detoasts and spools it) and
-- must never appear in a GROUP BY key (that hashes or sorts the whole
-- document). The `left join lateral` below aggregates it where it is read,
-- so the outer select has no GROUP BY to hang the blob on and only numbers
-- and short text travel onward.
--
-- The whole script runs in one repeatable-read transaction so the population,
-- the provider calls and the snapshots are all read from a single snapshot of
-- the database.
--
--
-- MEASUREMENT SEMANTICS
--
-- Recovered stages count as successful work. Failed and canceled stages do
-- not enter the latency distribution.
--
-- `billing_provider_call` records one logical provider call, not each HTTP
-- attempt. Its clipped wall time includes admission, provider retries and
-- backoff, and durable usage completion. Consequently this report can split
-- one logical call from several sequential logical calls, but cannot split a
-- slow first attempt from retry/backoff inside one call.
--
-- A provider call is attributed to every answer attempt of the same runtime
-- execution whose stage window contains the call's `started_at`. Retried
-- answer stages (same execution, higher `attempt_no`) with overlapping
-- windows therefore share a call. That is long-standing behaviour and is
-- preserved here; it is recorded so nobody reads a shared call as a bug.
--
-- For an agent turn, model calls belong to the parent runtime execution while
-- tool calls can spawn child executions. The debug snapshot records each
-- tool's wall time. Tools within one iteration may run concurrently, so their
-- critical-path contribution is the longest tool duration in that iteration;
-- those per-iteration maxima are summed because iterations are sequential.
-- Child provider calls remain inside that tool contribution; the child query
-- also appears as its own grounded execution-path row. The `p95_tail` row uses
-- conditional means for stages at or above p95, so its four attributed shares
-- add to 100%. Independent component percentiles would not be additive.
--
-- Fallback signals are deliberately separate, may overlap, and are lower
-- bounds because no general fallback-reason field exists. Two or more
-- `query_answer` calls identify fixed-evidence regeneration after the first
-- answer candidate did not finish the path. Revision call kinds identify
-- literal repair. `safe_fallback` is the persisted answer disposition.
-- Agent degraded completions, provider retries, and provider 429s are present
-- only when `snapshot_json.agentLoop` exists. The exact model-omission
-- `runtime_fallback` tool-call origin is not persisted and is not guessed.
--
--
-- SCHEMA EVIDENCE (apps/api/migrations/0001_init.sql)
--
--   billing_provider_call.id, owning_execution_kind                  1670, 1674
--   billing_provider_call.owning_execution_id, call_kind             1675, 1678
--   billing_provider_call.started_at, completed_at, call_state        1679-1681
--   billing_provider_call.runtime_execution_id, runtime_task_kind     1682-1683
--   billing_usage.provider_call_id, usage_kind, quantity              1694-1697
--   knowledge_context_bundle.bundle_id, candidate_summary             2454, 2466
--   query_execution.id, context_bundle_id, runtime_execution_id       3026, 3030, 3036
--   query_llm_context_snapshot.execution_id, snapshot_json           3079-3080
--   runtime_execution.id, owner_kind, owner_id, task_kind             3144-3147
--   runtime_stage_record.id, runtime_execution_id, stage_kind         3466-3468
--   runtime_stage_record.stage_state, started_at, completed_at        3471, 3473-3474
--
-- Indexes the joins rely on, so a future edit can tell a cheap lookup from a
-- scan:
--
--   knowledge_context_bundle_pkey (bundle_id)                              4272
--   query_llm_context_snapshot_pkey (execution_id)                         4472
--   idx_billing_provider_call_owner (owning_execution_kind, ..._id)        4930
--   idx_billing_usage_provider_call (provider_call_id)                     4951
--
-- Enum columns are compared against enum literals, never `::text`. The cast
-- is a CoerceViaIO the planner cannot see column statistics through, and four
-- of them in one predicate collapse the population estimate toward a single
-- row — which is what talks the planner into nested loops over the
-- unindexed intermediate results.
--
-- JSON evidence (not constrained by the SQL schema):
--   query_llm_context_snapshot.snapshot_json fields are serialized from
--   LlmContextSnapshot, LlmIterationDebug, ResponseToolCallDebug, and
--   AgentLoopMetadata in apps/api/src/services/query/llm_context_debug.rs.
--   knowledge_context_bundle.candidate_summary.answerDisposition is written
--   by attach_query_answer_outcome in
--   apps/api/src/services/query/execution/verification.rs.

\set ON_ERROR_STOP on
\pset border 2
\pset null 'n/a'

-- The window has no default: it is the bound, so an operator has to state it.
-- Without these guards an unset variable reaches the server as a literal `:`
-- and comes back as `syntax error at or near ":"`, which says nothing.
\if :{?since}
\else
do $$
begin
    raise exception 'missing -v since=<timestamptz>'
        using hint = 'The half-open [since, until) window is what bounds this '
                     'report, so there is no default for it.';
end
$$;
\endif

\if :{?until}
\else
do $$
begin
    raise exception 'missing -v until=<timestamptz>'
        using hint = 'The half-open [since, until) window is what bounds this '
                     'report, so there is no default for it.';
end
$$;
\endif

\if :{?max_answer_stages}
\else
\set max_answer_stages 250000
\endif

begin isolation level repeatable read;

set local ironrag.max_answer_stages = :'max_answer_stages';

-- The one sequential scan of `runtime_stage_record`. Narrowing happens here,
-- ahead of every join and every snapshot read. The result holds scalars only,
-- so a large window costs bytes proportional to the stage count and nothing
-- proportional to snapshot size. ANALYZE gives the planner measured counts for
-- it instead of the default guess a CTE would have carried.
create temporary table answer_stage on commit drop as
select
    stage.id as answer_stage_id,
    stage.runtime_execution_id,
    runtime.owner_id as query_execution_id,
    stage.started_at,
    stage.completed_at,
    greatest(
        extract(epoch from (stage.completed_at - stage.started_at)) * 1000.0,
        0.0
    )::double precision as answer_ms
from public.runtime_stage_record as stage
join public.runtime_execution as runtime
  on runtime.id = stage.runtime_execution_id
where stage.stage_kind = 'answer'
  and stage.stage_state in ('completed', 'recovered')
  and stage.started_at >= :'since'::timestamptz
  and stage.started_at < :'until'::timestamptz
  and stage.completed_at is not null
  and runtime.owner_kind = 'query_execution'
  and runtime.task_kind = 'query_answer';

analyze answer_stage;

do $$
declare
    population_count bigint;
    population_cap bigint := current_setting('ironrag.max_answer_stages')::bigint;
begin
    select count(*) into population_count from answer_stage;

    if population_count > population_cap then
        raise exception
            'answer-stage population is % rows, over the max_answer_stages cap of %',
            population_count, population_cap
        using hint =
            'Narrow -v since / -v until, or raise -v max_answer_stages deliberately. '
            'This report never samples, so it refuses rather than print '
            'percentiles from part of the window.';
    end if;

    raise notice 'answer-stage population: % rows (max_answer_stages %)',
        population_count, population_cap;
end
$$;

with answer_provider_call as (
    select
        stage.answer_stage_id,
        provider_call.id as provider_call_id,
        provider_call.call_kind,
        provider_call.call_state::text as call_state,
        greatest(
            extract(
                epoch from (
                    least(
                        coalesce(provider_call.completed_at, stage.completed_at),
                        stage.completed_at
                    ) - greatest(provider_call.started_at, stage.started_at)
                )
            ) * 1000.0,
            0.0
        )::double precision as stage_overlap_ms
    from answer_stage as stage
    join public.billing_provider_call as provider_call
      on provider_call.runtime_execution_id = stage.runtime_execution_id
     and provider_call.runtime_task_kind = 'query_answer'
     and provider_call.owning_execution_kind = 'query_execution'
     and provider_call.owning_execution_id = stage.query_execution_id
     and provider_call.started_at >= stage.started_at
     and provider_call.started_at <= stage.completed_at
),
provider_usage as (
    select
        answer_call.answer_stage_id,
        answer_call.provider_call_id,
        sum(usage.quantity) filter (
            where usage.usage_kind in (
                'prompt_tokens',
                'cached_input_tokens',
                'cache_creation_input_tokens'
            )
        )::double precision as input_tokens,
        sum(usage.quantity) filter (
            where usage.usage_kind = 'completion_tokens'
        )::double precision as output_tokens
    from answer_provider_call as answer_call
    left join public.billing_usage as usage
      on usage.provider_call_id = answer_call.provider_call_id
    group by answer_call.answer_stage_id, answer_call.provider_call_id
),
provider_rollup as (
    select
        stage.answer_stage_id,
        count(answer_call.provider_call_id) as provider_call_count,
        count(answer_call.provider_call_id) filter (
            where answer_call.call_state = 'completed'
        ) as completed_provider_call_count,
        count(answer_call.provider_call_id) filter (
            where answer_call.call_state in ('failed', 'canceled')
        ) as failed_or_canceled_provider_call_count,
        coalesce(sum(answer_call.stage_overlap_ms), 0.0) as provider_wall_ms,
        coalesce(max(answer_call.stage_overlap_ms), 0.0) as longest_provider_call_ms,
        count(answer_call.provider_call_id) filter (
            where answer_call.call_kind = 'query_answer'
        ) as answer_generation_call_count,
        count(answer_call.provider_call_id) filter (
            where answer_call.call_kind in (
                'query_answer_literal_revision',
                'query_answer_inventory_revision'
            )
        ) as answer_revision_call_count,
        coalesce(sum(provider_usage.input_tokens), 0.0) as input_tokens,
        coalesce(sum(provider_usage.output_tokens), 0.0) as output_tokens
    from answer_stage as stage
    left join answer_provider_call as answer_call
      on answer_call.answer_stage_id = stage.answer_stage_id
    left join provider_usage
      on provider_usage.answer_stage_id = answer_call.answer_stage_id
     and provider_usage.provider_call_id = answer_call.provider_call_id
    group by stage.answer_stage_id
),
-- The snapshot is opened, walked and reduced to scalars in one place. The
-- lateral does the aggregation, so this select needs no GROUP BY and
-- `snapshot_json` never becomes a group key or a tuplestore payload.
snapshot_rollup as (
    select
        stage.answer_stage_id,
        snapshot.execution_id is not null as has_snapshot,
        coalesce(
            jsonb_typeof(snapshot.snapshot_json -> 'agentLoop') = 'object',
            false
        ) as is_agent_loop,
        case
            when jsonb_typeof(snapshot.snapshot_json -> 'agentLoop') = 'object'
                then coalesce(iteration_rollup.max_iteration_number, 0)
            else coalesce(iteration_rollup.iteration_count, 0)::integer
        end as observed_iteration_count,
        coalesce(iteration_rollup.critical_path_tool_ms, 0.0) as critical_path_tool_ms,
        snapshot.snapshot_json -> 'agentLoop' ->> 'stoppedReason' as agent_stop_reason,
        coalesce(
            (snapshot.snapshot_json -> 'agentLoop' ->> 'toolCallCount')::bigint,
            0
        ) as agent_tool_call_count,
        coalesce(
            (snapshot.snapshot_json -> 'agentLoop' ->> 'provider429Count')::bigint,
            0
        ) as provider_429_count,
        coalesce(
            (snapshot.snapshot_json -> 'agentLoop' ->> 'providerRetryCount')::bigint,
            0
        ) as provider_retry_count,
        coalesce(
            (snapshot.snapshot_json -> 'agentLoop' ->> 'degradedCompletionCount')::bigint,
            0
        ) as degraded_completion_count
    from answer_stage as stage
    left join public.query_llm_context_snapshot as snapshot
      on snapshot.execution_id = stage.query_execution_id
    left join lateral (
        select
            count(*) as iteration_count,
            max(
                case
                    when jsonb_typeof(iteration.value -> 'iteration') = 'number'
                        then (iteration.value ->> 'iteration')::integer
                end
            ) as max_iteration_number,
            -- Tools inside one iteration may run concurrently, so the
            -- iteration contributes its longest tool; iterations are
            -- sequential, so those maxima add.
            coalesce(sum(tool_duration.critical_path_tool_ms), 0.0)
                as critical_path_tool_ms
        from jsonb_array_elements(
            case
                when jsonb_typeof(snapshot.snapshot_json -> 'iterations') = 'array'
                    then snapshot.snapshot_json -> 'iterations'
                else '[]'::jsonb
            end
        ) as iteration(value)
        left join lateral (
            select
                coalesce(max(
                    case
                        when jsonb_typeof(tool.value -> 'durationMs') = 'number'
                            then (tool.value ->> 'durationMs')::double precision
                        else 0.0
                    end
                ), 0.0) as critical_path_tool_ms
            from jsonb_array_elements(
                case
                    when jsonb_typeof(iteration.value -> 'responseToolCalls') = 'array'
                        then iteration.value -> 'responseToolCalls'
                    else '[]'::jsonb
                end
            ) as tool(value)
        ) as tool_duration on true
    ) as iteration_rollup on true
),
answer_outcome as (
    select
        stage.answer_stage_id,
        bundle.candidate_summary ->> 'answerDisposition' as answer_disposition
    from answer_stage as stage
    join public.query_execution as execution
      on execution.id = stage.query_execution_id
     and execution.runtime_execution_id = stage.runtime_execution_id
    left join public.knowledge_context_bundle as bundle
      on bundle.bundle_id = execution.context_bundle_id
),
uncapped_fact as (
    select
        stage.answer_stage_id,
        stage.answer_ms,
        provider.provider_call_count,
        provider.completed_provider_call_count,
        provider.failed_or_canceled_provider_call_count,
        provider.provider_wall_ms,
        provider.longest_provider_call_ms,
        provider.answer_generation_call_count,
        provider.answer_revision_call_count,
        provider.input_tokens,
        provider.output_tokens,
        snapshot.has_snapshot,
        snapshot.is_agent_loop,
        snapshot.observed_iteration_count,
        snapshot.agent_tool_call_count,
        snapshot.critical_path_tool_ms,
        snapshot.agent_stop_reason,
        snapshot.provider_429_count,
        snapshot.provider_retry_count,
        snapshot.degraded_completion_count,
        outcome.answer_disposition
    from answer_stage as stage
    join provider_rollup as provider
      on provider.answer_stage_id = stage.answer_stage_id
    join snapshot_rollup as snapshot
      on snapshot.answer_stage_id = stage.answer_stage_id
    left join answer_outcome as outcome
      on outcome.answer_stage_id = stage.answer_stage_id
),
fact as materialized (
    select
        uncapped.*,
        least(uncapped.provider_wall_ms, uncapped.answer_ms) as attributed_provider_wall_ms,
        least(
            uncapped.longest_provider_call_ms,
            least(uncapped.provider_wall_ms, uncapped.answer_ms)
        ) as attributed_longest_provider_call_ms,
        greatest(
            least(uncapped.provider_wall_ms, uncapped.answer_ms)
                - least(
                    uncapped.longest_provider_call_ms,
                    least(uncapped.provider_wall_ms, uncapped.answer_ms)
                ),
            0.0
        ) as attributed_additional_provider_calls_ms,
        least(
            uncapped.critical_path_tool_ms,
            greatest(uncapped.answer_ms - least(uncapped.provider_wall_ms, uncapped.answer_ms), 0.0)
        ) as attributed_agent_tool_ms,
        greatest(
            uncapped.answer_ms
                - least(uncapped.provider_wall_ms, uncapped.answer_ms)
                - least(
                    uncapped.critical_path_tool_ms,
                    greatest(
                        uncapped.answer_ms - least(uncapped.provider_wall_ms, uncapped.answer_ms),
                        0.0
                    )
                ),
            0.0
        ) as unattributed_ms
    from uncapped_fact as uncapped
),
threshold as (
    select
        percentile_cont(0.95) within group (order by fact.answer_ms) as answer_p95_ms
    from fact
),
cohort_member as (
    select 'overall' as section, 'all_answer_stages' as cohort, fact.*
    from fact

    union all

    select 'p95_tail', 'answer_p95_and_slower', fact.*
    from fact
    cross join threshold
    where fact.answer_ms >= threshold.answer_p95_ms

    union all

    select
        'execution_path',
        case when fact.is_agent_loop then 'agent_loop' else 'grounded_non_agent' end,
        fact.*
    from fact

    union all

    select
        'logical_provider_calls',
        case
            when fact.provider_call_count = 0 then 'zero'
            when fact.provider_call_count = 1 then 'one'
            else 'two_or_more_sequential'
        end,
        fact.*
    from fact

    union all

    select
        'agent_loop_iterations',
        case
            when fact.observed_iteration_count <= 1 then 'one'
            else 'more_than_one'
        end,
        fact.*
    from fact
    where fact.is_agent_loop

    union all

    select 'agent_stop_reason', coalesce(fact.agent_stop_reason, 'missing'), fact.*
    from fact
    where fact.is_agent_loop

    union all

    select 'fallback_signal', 'fixed_evidence_regeneration', fact.*
    from fact
    where fact.answer_generation_call_count >= 2

    union all

    select 'fallback_signal', 'literal_revision', fact.*
    from fact
    where fact.answer_revision_call_count > 0

    union all

    select 'fallback_signal', 'provider_failure_then_completion', fact.*
    from fact
    where fact.failed_or_canceled_provider_call_count > 0
      and fact.completed_provider_call_count > 0

    union all

    select 'fallback_signal', 'safe_fallback_outcome', fact.*
    from fact
    where fact.answer_disposition = 'safe_fallback'

    union all

    select 'fallback_signal', 'agent_degraded_completion', fact.*
    from fact
    where fact.degraded_completion_count > 0

    union all

    select 'fallback_signal', 'agent_provider_retry_recorded', fact.*
    from fact
    where fact.provider_retry_count > 0

    union all

    select 'fallback_signal', 'agent_provider_429_recorded', fact.*
    from fact
    where fact.provider_429_count > 0

    union all

    select
        'telemetry_coverage',
        case when fact.has_snapshot then 'snapshot_present' else 'snapshot_missing' end,
        fact.*
    from fact
),
population as (
    select
        count(*) as answer_stage_count,
        count(*) filter (where fact.is_agent_loop) as agent_stage_count
    from fact
),
report as (
    select
        member.section,
        member.cohort,
        count(*) as sample_count,
        round(
            count(*)::numeric * 100.0 / nullif(population.answer_stage_count, 0),
            2
        ) as share_of_all_answer_stages_pct,
        round(
            count(*)::numeric * 100.0
                / nullif(
                    case
                        when member.section in ('agent_loop_iterations', 'agent_stop_reason')
                            or (
                                member.section = 'fallback_signal'
                                and member.cohort like 'agent_%'
                            )
                            then population.agent_stage_count
                        else population.answer_stage_count
                    end,
                    0
                ),
            2
        ) as share_of_parent_pct,
        round(avg(member.answer_ms)::numeric, 2) as mean_answer_ms,
        round(
            (percentile_cont(0.50) within group (order by member.answer_ms))::numeric,
            2
        ) as answer_p50_ms,
        round(
            (percentile_cont(0.95) within group (order by member.answer_ms))::numeric,
            2
        ) as answer_p95_ms,
        round(avg(member.attributed_longest_provider_call_ms)::numeric, 2)
            as mean_longest_provider_call_ms,
        round(avg(member.attributed_additional_provider_calls_ms)::numeric, 2)
            as mean_additional_provider_calls_ms,
        round(avg(member.attributed_agent_tool_ms)::numeric, 2)
            as mean_agent_tool_critical_path_ms,
        round(avg(member.unattributed_ms)::numeric, 2) as mean_unattributed_ms,
        round(
            sum(member.attributed_longest_provider_call_ms)::numeric * 100.0
                / nullif(sum(member.answer_ms)::numeric, 0),
            2
        ) as longest_provider_share_pct,
        round(
            sum(member.attributed_additional_provider_calls_ms)::numeric * 100.0
                / nullif(sum(member.answer_ms)::numeric, 0),
            2
        ) as additional_provider_calls_share_pct,
        round(
            sum(member.attributed_agent_tool_ms)::numeric * 100.0
                / nullif(sum(member.answer_ms)::numeric, 0),
            2
        ) as agent_tool_critical_path_share_pct,
        round(
            sum(member.unattributed_ms)::numeric * 100.0
                / nullif(sum(member.answer_ms)::numeric, 0),
            2
        ) as unattributed_share_pct,
        round(
            (percentile_cont(0.50) within group (order by member.provider_call_count))::numeric,
            2
        ) as provider_calls_p50,
        round(
            (percentile_cont(0.95) within group (order by member.provider_call_count))::numeric,
            2
        ) as provider_calls_p95,
        round(
            (percentile_cont(0.95) within group (order by member.input_tokens))::numeric,
            2
        ) as input_tokens_p95,
        round(
            (percentile_cont(0.95) within group (order by member.output_tokens))::numeric,
            2
        ) as output_tokens_p95,
        round(
            (percentile_cont(0.95) within group (
                order by member.observed_iteration_count
            ))::numeric,
            2
        ) as observed_iterations_p95,
        round(
            (percentile_cont(0.95) within group (
                order by member.agent_tool_call_count
            ))::numeric,
            2
        ) as agent_tool_calls_p95
    from cohort_member as member
    cross join population
    group by
        member.section,
        member.cohort,
        population.answer_stage_count,
        population.agent_stage_count
)
select
    report.section,
    report.cohort,
    report.sample_count,
    report.share_of_all_answer_stages_pct,
    report.share_of_parent_pct,
    report.mean_answer_ms,
    report.answer_p50_ms,
    report.answer_p95_ms,
    report.mean_longest_provider_call_ms,
    report.mean_additional_provider_calls_ms,
    report.mean_agent_tool_critical_path_ms,
    report.mean_unattributed_ms,
    report.longest_provider_share_pct,
    report.additional_provider_calls_share_pct,
    report.agent_tool_critical_path_share_pct,
    report.unattributed_share_pct,
    report.provider_calls_p50,
    report.provider_calls_p95,
    report.input_tokens_p95,
    report.output_tokens_p95,
    report.observed_iterations_p95,
    report.agent_tool_calls_p95
from report
order by
    case report.section
        when 'overall' then 1
        when 'p95_tail' then 2
        when 'execution_path' then 3
        when 'logical_provider_calls' then 4
        when 'agent_loop_iterations' then 5
        when 'agent_stop_reason' then 6
        when 'fallback_signal' then 7
        when 'telemetry_coverage' then 8
        else 9
    end,
    report.cohort;

commit;
