-- Include paused ingest jobs in the canonical operator queue index only after
-- the 0010 enum value has been committed. PostgreSQL rejects using a freshly
-- added enum value in the same migration transaction that introduces it.

drop index if exists idx_ingest_job_active_queue_rank;

create index idx_ingest_job_active_queue_rank
    on ingest_job (queue_state, queue_rank, priority, available_at, queued_at, id)
    where queue_state in ('queued', 'leased', 'paused');
