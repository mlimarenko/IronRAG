create table if not exists source (
    id uuid primary key,
    project_id uuid not null references project(id) on delete cascade,
    source_kind text not null,
    label text not null,
    config_json jsonb not null default '{}'::jsonb,
    status text not null default 'active',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create table if not exists ingestion_job (
    id uuid primary key,
    project_id uuid not null references project(id) on delete cascade,
    source_id uuid references source(id) on delete set null,
    trigger_kind text not null,
    status text not null,
    stage text not null,
    requested_by text,
    error_message text,
    started_at timestamptz,
    finished_at timestamptz,
    created_at timestamptz not null default now()
);

create table if not exists document (
    id uuid primary key,
    project_id uuid not null references project(id) on delete cascade,
    source_id uuid references source(id) on delete set null,
    external_key text not null,
    title text,
    mime_type text,
    checksum text,
    metadata_json jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create table if not exists chunk (
    id uuid primary key,
    document_id uuid not null references document(id) on delete cascade,
    project_id uuid not null references project(id) on delete cascade,
    ordinal integer not null,
    content text not null,
    token_count integer,
    embedding vector(1536),
    metadata_json jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now()
);

create table if not exists chunk_embedding (
    chunk_id uuid primary key references chunk(id) on delete cascade,
    project_id uuid not null references project(id) on delete cascade,
    provider_kind text not null,
    model_name text not null,
    dimensions integer not null,
    embedding_json jsonb not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create table if not exists entity (
    id uuid primary key,
    project_id uuid not null references project(id) on delete cascade,
    canonical_name text not null,
    entity_type text,
    metadata_json jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now()
);

create table if not exists relation (
    id uuid primary key,
    project_id uuid not null references project(id) on delete cascade,
    from_entity_id uuid not null references entity(id) on delete cascade,
    to_entity_id uuid not null references entity(id) on delete cascade,
    relation_type text not null,
    weight double precision,
    provenance_json jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now()
);

create table if not exists retrieval_run (
    id uuid primary key,
    project_id uuid not null references project(id) on delete cascade,
    query_text text not null,
    model_profile_id uuid references model_profile(id) on delete set null,
    top_k integer not null default 8,
    response_text text,
    debug_json jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now()
);

create index if not exists idx_source_project_id on source(project_id);
create index if not exists idx_ingestion_job_project_status_created on ingestion_job(project_id, status, created_at desc);
create index if not exists idx_document_project_id on document(project_id);
create index if not exists idx_chunk_project_id on chunk(project_id);
create index if not exists idx_chunk_project_embedding_cosine on chunk using hnsw (embedding vector_cosine_ops);
create index if not exists idx_entity_project_id on entity(project_id);
create index if not exists idx_relation_project_id on relation(project_id);
create index if not exists idx_retrieval_run_project_created on retrieval_run(project_id, created_at desc);
