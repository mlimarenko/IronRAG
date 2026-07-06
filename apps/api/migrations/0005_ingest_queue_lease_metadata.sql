alter table public.ingest_job
    add column if not exists queue_leased_at timestamp with time zone,
    add column if not exists queue_lease_token text,
    add column if not exists queue_lease_owner text;

create index if not exists idx_ingest_job_queue_lease_recovery
    on public.ingest_job (queue_state, queue_leased_at, queued_at, id)
    where queue_state = 'leased'::public.ingest_queue_state;

update public.ingest_job job
set queue_leased_at = coalesce(
        job.queue_leased_at,
        nullif((
            select greatest(
                coalesce(max(attempt.heartbeat_at), '-infinity'::timestamp with time zone),
                coalesce(max(attempt.started_at), '-infinity'::timestamp with time zone)
            )
            from public.ingest_attempt attempt
            where attempt.job_id = job.id
        ), '-infinity'::timestamp with time zone),
        job.queued_at
    ),
    queue_lease_token = coalesce(
        job.queue_lease_token,
        'legacy-' || job.id::text
    ),
    queue_lease_owner = coalesce(job.queue_lease_owner, 'legacy-migration')
where job.queue_state = 'leased'::public.ingest_queue_state
  and (
      job.queue_leased_at is null
      or job.queue_lease_token is null
      or job.queue_lease_owner is null
  );
