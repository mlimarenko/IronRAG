create extension if not exists vector;

create table if not exists workspace (
    id uuid primary key,
    slug text not null unique,
    name text not null,
    status text not null default 'active',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create table if not exists project (
    id uuid primary key,
    workspace_id uuid not null references workspace(id) on delete cascade,
    slug text not null,
    name text not null,
    description text,
    default_model_profile_id uuid,
    default_embedding_profile_id uuid,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (workspace_id, slug)
);

create table if not exists provider_account (
    id uuid primary key,
    workspace_id uuid not null references workspace(id) on delete cascade,
    provider_kind text not null,
    label text not null,
    api_base_url text,
    encrypted_secret jsonb,
    status text not null default 'active',
    metadata_json jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create table if not exists model_profile (
    id uuid primary key,
    workspace_id uuid not null references workspace(id) on delete cascade,
    provider_account_id uuid not null references provider_account(id) on delete restrict,
    profile_kind text not null,
    model_name text not null,
    temperature double precision,
    max_output_tokens integer,
    json_config jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index if not exists idx_project_workspace_id on project(workspace_id);
create index if not exists idx_provider_account_workspace_id on provider_account(workspace_id);
create index if not exists idx_model_profile_workspace_id on model_profile(workspace_id);
