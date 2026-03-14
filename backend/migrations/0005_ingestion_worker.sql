alter table ingestion_job
    add column if not exists updated_at timestamptz not null default now(),
    add column if not exists idempotency_key text,
    add column if not exists parent_job_id uuid references ingestion_job(id) on delete set null,
    add column if not exists attempt_count integer not null default 0,
    add column if not exists worker_id text,
    add column if not exists lease_expires_at timestamptz,
    add column if not exists heartbeat_at timestamptz,
    add column if not exists payload_json jsonb not null default '{}'::jsonb,
    add column if not exists result_json jsonb not null default '{}'::jsonb;

create unique index if not exists idx_ingestion_job_idempotency_key
    on ingestion_job(idempotency_key)
    where idempotency_key is not null;

create index if not exists idx_ingestion_job_queue_claim
    on ingestion_job(status, lease_expires_at, created_at asc);

create table if not exists ingestion_job_attempt (
    id uuid primary key,
    job_id uuid not null references ingestion_job(id) on delete cascade,
    attempt_no integer not null,
    worker_id text,
    status text not null,
    stage text not null,
    error_message text,
    started_at timestamptz not null default now(),
    finished_at timestamptz,
    created_at timestamptz not null default now(),
    unique(job_id, attempt_no)
);

create index if not exists idx_ingestion_job_attempt_job_created
    on ingestion_job_attempt(job_id, created_at desc);
