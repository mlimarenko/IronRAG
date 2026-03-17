alter table runtime_ingestion_run
    add column if not exists last_activity_at timestamptz,
    add column if not exists last_heartbeat_at timestamptz,
    add column if not exists activity_status text not null default 'queued';

update runtime_ingestion_run
set last_activity_at = coalesce(last_activity_at, finished_at, started_at, updated_at, created_at),
    last_heartbeat_at = coalesce(last_heartbeat_at, updated_at, created_at),
    activity_status = case
        when status in ('ready', 'ready_no_graph') then 'ready'
        when status = 'failed' then 'failed'
        when status = 'processing' then 'active'
        else 'queued'
    end
where last_activity_at is null
   or last_heartbeat_at is null
   or activity_status = 'queued';

create table if not exists runtime_document_contribution_summary (
    document_id uuid primary key references document(id) on delete cascade,
    revision_id uuid references document_revision(id) on delete cascade,
    ingestion_run_id uuid references runtime_ingestion_run(id) on delete set null,
    latest_attempt_no integer not null default 1,
    chunk_count integer,
    admitted_graph_node_count integer not null default 0,
    admitted_graph_edge_count integer not null default 0,
    filtered_graph_edge_count integer not null default 0,
    filtered_artifact_count integer not null default 0,
    computed_at timestamptz not null default now()
);

create index if not exists idx_runtime_document_contribution_summary_revision
    on runtime_document_contribution_summary(revision_id, computed_at desc);

create table if not exists runtime_graph_filtered_artifact (
    id uuid primary key,
    project_id uuid not null references project(id) on delete cascade,
    ingestion_run_id uuid references runtime_ingestion_run(id) on delete set null,
    revision_id uuid references document_revision(id) on delete set null,
    target_kind text not null,
    candidate_key text not null,
    source_node_key text,
    target_node_key text,
    relation_type text,
    filter_reason text not null,
    summary text,
    metadata_json jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now()
);

create index if not exists idx_runtime_graph_filtered_artifact_project_reason
    on runtime_graph_filtered_artifact(project_id, filter_reason, created_at desc);
create index if not exists idx_runtime_graph_filtered_artifact_revision
    on runtime_graph_filtered_artifact(revision_id, created_at desc);

create index if not exists idx_runtime_ingestion_run_project_activity
    on runtime_ingestion_run(project_id, activity_status, last_activity_at desc);
create index if not exists idx_runtime_ingestion_run_project_heartbeat
    on runtime_ingestion_run(project_id, last_heartbeat_at desc);

create index if not exists idx_ingestion_job_running_worker_lease
    on ingestion_job(worker_id, lease_expires_at, updated_at desc)
    where status = 'running';
