-- Ingest-stage latency and total time to ready for one reference document.
--
-- Usage (psql only):
--
--   psql "$DATABASE_URL" -X -v ON_ERROR_STOP=1 \
--     -v document_id='00000000-0000-0000-0000-000000000001' \
--     -f scripts/ops/profile-ingest-stage-latency.sql
--
-- One latency sample is one completed content-mutation job. Repeated stage
-- events for retries are summed within that job before percentiles are taken.
-- `time_to_ready` starts at queue admission and ends only when the job is
-- completed and either text or graph has a readiness timestamp, matching the
-- backend's current document-activity rule.
--
-- Schema evidence (apps/api/migrations/0001_init.sql):
--   ingest_attempt.id, job_id                                      2193-2194
--   ingest_job.id, knowledge_document_id, knowledge_revision_id    2218, 2224-2225
--   ingest_job.job_kind, queue_state                               2226-2227
--   ingest_job.queued_at, completed_at                             2230, 2232
--   ingest_stage_event.attempt_id, stage_name, stage_state         2254-2256
--   ingest_stage_event.ordinal, elapsed_ms                         2257, 2268
--   knowledge_revision.revision_id, document_id                    2750, 2753
--   knowledge_revision.text_readable_at, graph_ready_at            2770, 2772

\set ON_ERROR_STOP on
\pset border 2
\pset null 'n/a'

with ready_job as (
    select
        job.id as job_id,
        job.queued_at,
        greatest(
            job.completed_at,
            case
                when revision.text_readable_at is null then revision.graph_ready_at
                when revision.graph_ready_at is null then revision.text_readable_at
                else least(revision.text_readable_at, revision.graph_ready_at)
            end
        ) as ready_at
    from public.ingest_job as job
    join public.knowledge_revision as revision
      on revision.revision_id = job.knowledge_revision_id
     and revision.document_id = job.knowledge_document_id
    where job.knowledge_document_id = :'document_id'::uuid
      and job.job_kind::text = 'content_mutation'
      and job.queue_state::text = 'completed'
      and job.completed_at is not null
      and (
          revision.text_readable_at is not null
          or revision.graph_ready_at is not null
      )
),
stage_measurement as (
    select
        ready.job_id,
        event.stage_name as stage,
        min(event.ordinal) as display_order,
        sum(greatest(event.elapsed_ms, 0))::double precision as latency_ms
    from ready_job as ready
    join public.ingest_attempt as attempt
      on attempt.job_id = ready.job_id
    join public.ingest_stage_event as event
      on event.attempt_id = attempt.id
    where event.elapsed_ms is not null
      and event.stage_state::text in ('completed', 'failed')
    group by ready.job_id, event.stage_name
),
measurement as (
    select
        stage.stage,
        stage.display_order,
        stage.latency_ms
    from stage_measurement as stage

    union all

    select
        'time_to_ready' as stage,
        2147483647 as display_order,
        greatest(
            extract(epoch from (ready.ready_at - ready.queued_at)) * 1000.0,
            0.0
        )::double precision as latency_ms
    from ready_job as ready
)
select
    measurement.stage,
    round(
        (percentile_cont(0.50) within group (order by measurement.latency_ms))::numeric,
        2
    ) as p50_ms,
    round(
        (percentile_cont(0.95) within group (order by measurement.latency_ms))::numeric,
        2
    ) as p95_ms,
    count(measurement.latency_ms) as sample_count
from measurement
group by measurement.stage
order by min(measurement.display_order), measurement.stage;
