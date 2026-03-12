create table if not exists api_token (
    id uuid primary key,
    workspace_id uuid references workspace(id) on delete cascade,
    token_kind text not null,
    label text not null,
    token_hash text not null unique,
    scope_json jsonb not null default '[]'::jsonb,
    status text not null default 'active',
    last_used_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index if not exists idx_api_token_workspace_id on api_token(workspace_id);
create index if not exists idx_api_token_status on api_token(status);
