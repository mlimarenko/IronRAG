-- Weekly runtime-stage latency profile for successful query work.
--
-- Usage (psql only):
--
--   psql "$DATABASE_URL" -X -v ON_ERROR_STOP=1 \
--     -v since='2026-08-04T00:00:00Z' \
--     -v until='2026-08-11T00:00:00Z' \
--     -f scripts/ops/profile-runtime-stage-latency.sql
--
-- The half-open window is applied to the time each stage began. Recovered
-- stages count as successful work. Failed and canceled stages do not enter
-- the latency distribution.
--
-- Schema evidence (apps/api/migrations/0001_init.sql):
--   runtime_stage_kind values                                      616-628
--   runtime_stage_record.stage_kind                               3468
--   runtime_stage_record.stage_state                              3471
--   runtime_stage_record.started_at                               3473
--   runtime_stage_record.completed_at                             3474
--
-- The migration's runtime_stage_kind values do not include `compile`, so its
-- row will show no samples. Keeping that row makes the missing measurement
-- visible and avoids treating the distinct `plan` stage as compiler time.

\set ON_ERROR_STOP on
\pset border 2
\pset null 'n/a'

with requested_stage(stage, stored_stage, display_order) as (
    values
        ('compile', 'compile', 1),
        ('retrieve', 'retrieve', 2),
        ('rerank', 'rerank', 3),
        ('answer', 'answer', 4),
        ('verification', 'verify', 5)
),
measurement as (
    select
        record.stage_kind::text as stored_stage,
        greatest(
            extract(epoch from (record.completed_at - record.started_at)) * 1000.0,
            0.0
        )::double precision as latency_ms
    from public.runtime_stage_record as record
    where record.started_at >= :'since'::timestamptz
      and record.started_at < :'until'::timestamptz
      and record.completed_at is not null
      and record.stage_state::text in ('completed', 'recovered')
)
select
    requested.stage,
    round(
        (percentile_cont(0.50) within group (order by measurement.latency_ms))::numeric,
        2
    ) as p50_ms,
    round(
        (percentile_cont(0.95) within group (order by measurement.latency_ms))::numeric,
        2
    ) as p95_ms,
    count(measurement.latency_ms) as sample_count
from requested_stage as requested
left join measurement
  on measurement.stored_stage = requested.stored_stage
group by requested.stage, requested.display_order
order by requested.display_order;
