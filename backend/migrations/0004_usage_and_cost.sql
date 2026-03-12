create table if not exists usage_event (
    id uuid primary key,
    workspace_id uuid references workspace(id) on delete cascade,
    project_id uuid references project(id) on delete cascade,
    provider_account_id uuid references provider_account(id) on delete set null,
    model_profile_id uuid references model_profile(id) on delete set null,
    usage_kind text not null,
    prompt_tokens integer,
    completion_tokens integer,
    total_tokens integer,
    raw_usage_json jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now()
);

create table if not exists cost_ledger (
    id uuid primary key,
    workspace_id uuid references workspace(id) on delete cascade,
    project_id uuid references project(id) on delete cascade,
    usage_event_id uuid not null references usage_event(id) on delete cascade,
    provider_kind text not null,
    model_name text not null,
    currency text not null default 'USD',
    estimated_cost numeric(18,8) not null default 0,
    pricing_snapshot_json jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now()
);

create index if not exists idx_usage_event_workspace_project_created on usage_event(workspace_id, project_id, created_at desc);
create index if not exists idx_cost_ledger_workspace_project_created on cost_ledger(workspace_id, project_id, created_at desc);
