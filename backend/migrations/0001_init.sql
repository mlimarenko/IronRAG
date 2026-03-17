create extension if not exists vector;
create extension if not exists pgcrypto;

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
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    idempotency_key text,
    parent_job_id uuid references ingestion_job(id) on delete set null,
    attempt_count integer not null default 0,
    worker_id text,
    lease_expires_at timestamptz,
    heartbeat_at timestamptz,
    payload_json jsonb not null default '{}'::jsonb,
    result_json jsonb not null default '{}'::jsonb
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

create table if not exists chat_session (
    id uuid primary key,
    workspace_id uuid not null references workspace(id) on delete cascade,
    project_id uuid not null references project(id) on delete cascade,
    title text not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create table if not exists retrieval_run (
    id uuid primary key,
    project_id uuid not null references project(id) on delete cascade,
    query_text text not null,
    model_profile_id uuid references model_profile(id) on delete set null,
    top_k integer not null default 8,
    response_text text,
    debug_json jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    session_id uuid references chat_session(id) on delete set null
);

create table if not exists chat_message (
    id uuid primary key,
    session_id uuid not null references chat_session(id) on delete cascade,
    project_id uuid not null references project(id) on delete cascade,
    role text not null,
    content text not null,
    retrieval_run_id uuid references retrieval_run(id) on delete set null,
    created_at timestamptz not null default now()
);

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

create table if not exists ui_user (
    id uuid primary key,
    login text not null,
    email text not null,
    display_name text not null,
    role_label text not null,
    password_hash text not null,
    preferred_locale text not null default 'ru',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create table if not exists ui_session (
    id uuid primary key,
    user_id uuid not null references ui_user(id) on delete cascade,
    active_workspace_id uuid references workspace(id) on delete set null,
    active_project_id uuid references project(id) on delete set null,
    locale text not null default 'ru',
    expires_at timestamptz not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now()
);

create table if not exists workspace_member (
    workspace_id uuid not null references workspace(id) on delete cascade,
    user_id uuid not null references ui_user(id) on delete cascade,
    role_label text not null,
    created_at timestamptz not null default now(),
    primary key (workspace_id, user_id)
);

create table if not exists project_access_grant (
    project_id uuid not null references project(id) on delete cascade,
    user_id uuid not null references ui_user(id) on delete cascade,
    access_level text not null,
    created_at timestamptz not null default now(),
    primary key (project_id, user_id)
);

create table if not exists api_token (
    id uuid primary key,
    workspace_id uuid references workspace(id) on delete cascade,
    token_kind text not null,
    label text not null,
    token_hash text not null unique,
    token_preview text,
    scope_json jsonb not null default '[]'::jsonb,
    status text not null default 'active',
    last_used_at timestamptz,
    expires_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create table if not exists runtime_ingestion_run (
    id uuid primary key,
    project_id uuid not null references project(id) on delete cascade,
    document_id uuid references document(id) on delete set null,
    upload_batch_id uuid,
    track_id text not null unique,
    file_name text not null,
    file_type text not null,
    mime_type text,
    file_size_bytes bigint,
    status text not null,
    current_stage text not null,
    progress_percent integer,
    provider_profile_snapshot_json jsonb not null default '{}'::jsonb,
    latest_error_message text,
    current_attempt_no integer not null default 1,
    started_at timestamptz,
    finished_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create table if not exists runtime_ingestion_stage_event (
    id uuid primary key,
    ingestion_run_id uuid not null references runtime_ingestion_run(id) on delete cascade,
    attempt_no integer not null,
    stage text not null,
    status text not null,
    message text,
    metadata_json jsonb not null default '{}'::jsonb,
    started_at timestamptz not null default now(),
    finished_at timestamptz,
    created_at timestamptz not null default now()
);

create table if not exists runtime_extracted_content (
    id uuid primary key,
    ingestion_run_id uuid not null unique references runtime_ingestion_run(id) on delete cascade,
    document_id uuid references document(id) on delete set null,
    extraction_kind text not null,
    content_text text,
    page_count integer,
    char_count integer,
    extraction_warnings_json jsonb not null default '[]'::jsonb,
    source_map_json jsonb not null default '{}'::jsonb,
    provider_kind text,
    model_name text,
    extraction_version text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create table if not exists runtime_graph_snapshot (
    project_id uuid primary key references project(id) on delete cascade,
    graph_status text not null,
    projection_version bigint not null default 0,
    node_count integer not null default 0,
    edge_count integer not null default 0,
    provenance_coverage_percent double precision,
    last_built_at timestamptz,
    last_error_message text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create table if not exists runtime_graph_node (
    id uuid primary key,
    project_id uuid not null references project(id) on delete cascade,
    canonical_key text not null,
    label text not null,
    node_type text not null,
    aliases_json jsonb not null default '[]'::jsonb,
    summary text,
    metadata_json jsonb not null default '{}'::jsonb,
    support_count integer not null default 0,
    projection_version bigint not null default 0,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique(project_id, canonical_key, projection_version)
);

create table if not exists runtime_graph_edge (
    id uuid primary key,
    project_id uuid not null references project(id) on delete cascade,
    from_node_id uuid not null references runtime_graph_node(id) on delete cascade,
    to_node_id uuid not null references runtime_graph_node(id) on delete cascade,
    relation_type text not null,
    canonical_key text not null,
    summary text,
    weight double precision,
    support_count integer not null default 0,
    metadata_json jsonb not null default '{}'::jsonb,
    projection_version bigint not null default 0,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique(project_id, canonical_key, projection_version)
);

create table if not exists runtime_graph_evidence (
    id uuid primary key,
    project_id uuid not null references project(id) on delete cascade,
    target_kind text not null,
    target_id uuid not null,
    document_id uuid references document(id) on delete cascade,
    chunk_id uuid references chunk(id) on delete cascade,
    source_file_name text,
    page_ref text,
    evidence_text text not null,
    confidence_score double precision,
    is_active boolean not null default true,
    created_at timestamptz not null default now()
);

create table if not exists runtime_provider_profile (
    project_id uuid primary key references project(id) on delete cascade,
    indexing_provider_kind text not null,
    indexing_model_name text not null,
    embedding_provider_kind text not null,
    embedding_model_name text not null,
    answer_provider_kind text not null,
    answer_model_name text not null,
    vision_provider_kind text not null,
    vision_model_name text not null,
    last_validated_at timestamptz,
    last_validation_status text,
    last_validation_error text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create table if not exists runtime_provider_validation_log (
    id uuid primary key,
    project_id uuid references project(id) on delete cascade,
    provider_kind text not null,
    model_name text not null,
    capability text not null,
    status text not null,
    error_message text,
    created_at timestamptz not null default now()
);

create table if not exists runtime_query_execution (
    id uuid primary key,
    project_id uuid not null references project(id) on delete cascade,
    mode text not null,
    question text not null,
    status text not null,
    answer_text text,
    grounding_status text not null,
    provider_kind text not null,
    model_name text not null,
    debug_json jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    finished_at timestamptz
);

create table if not exists runtime_query_reference (
    id uuid primary key,
    query_execution_id uuid not null references runtime_query_execution(id) on delete cascade,
    reference_kind text not null,
    reference_id uuid not null,
    excerpt text,
    rank integer not null,
    score double precision,
    metadata_json jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now()
);

create table if not exists runtime_vector_target (
    id uuid primary key,
    project_id uuid not null references project(id) on delete cascade,
    target_kind text not null,
    target_id uuid not null,
    provider_kind text not null,
    model_name text not null,
    dimensions integer,
    embedding_json jsonb not null default '[]'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique(project_id, target_kind, target_id, provider_kind, model_name)
);

create table if not exists runtime_graph_extraction (
    id uuid primary key,
    project_id uuid not null references project(id) on delete cascade,
    document_id uuid not null references document(id) on delete cascade,
    chunk_id uuid not null references chunk(id) on delete cascade,
    provider_kind text not null,
    model_name text not null,
    extraction_version text not null,
    prompt_hash text not null,
    status text not null,
    raw_output_json jsonb not null default '{}'::jsonb,
    normalized_output_json jsonb not null default '{}'::jsonb,
    glean_pass_count integer not null default 1,
    error_message text,
    created_at timestamptz not null default now()
);

create index if not exists idx_project_workspace_id on project(workspace_id);
create index if not exists idx_provider_account_workspace_id on provider_account(workspace_id);
create index if not exists idx_model_profile_workspace_id on model_profile(workspace_id);
create index if not exists idx_source_project_id on source(project_id);
create unique index if not exists idx_ingestion_job_idempotency_key
    on ingestion_job(idempotency_key)
    where idempotency_key is not null;
create index if not exists idx_ingestion_job_project_status_created
    on ingestion_job(project_id, status, created_at desc);
create index if not exists idx_ingestion_job_queue_claim
    on ingestion_job(status, lease_expires_at, created_at asc);
create index if not exists idx_document_project_id on document(project_id);
create index if not exists idx_chunk_project_id on chunk(project_id);
create index if not exists idx_chunk_project_embedding_cosine
    on chunk using hnsw (embedding vector_cosine_ops);
create index if not exists idx_entity_project_id on entity(project_id);
create index if not exists idx_relation_project_id on relation(project_id);
create index if not exists idx_chat_session_project_updated
    on chat_session(project_id, updated_at desc, created_at desc);
create index if not exists idx_retrieval_run_project_created
    on retrieval_run(project_id, created_at desc);
create index if not exists idx_retrieval_run_session_created
    on retrieval_run(session_id, created_at desc);
create index if not exists idx_chat_message_session_created
    on chat_message(session_id, created_at asc, id asc);
create index if not exists idx_usage_event_workspace_project_created
    on usage_event(workspace_id, project_id, created_at desc);
create index if not exists idx_cost_ledger_workspace_project_created
    on cost_ledger(workspace_id, project_id, created_at desc);
create index if not exists idx_ingestion_job_attempt_job_created
    on ingestion_job_attempt(job_id, created_at desc);
create unique index if not exists idx_ui_user_login_lower on ui_user(lower(login));
create unique index if not exists idx_ui_user_email_lower on ui_user(lower(email));
create index if not exists idx_ui_session_user_expires on ui_session(user_id, expires_at desc);
create index if not exists idx_workspace_member_user on workspace_member(user_id, workspace_id);
create index if not exists idx_project_access_user on project_access_grant(user_id, project_id);
create index if not exists idx_api_token_workspace_id on api_token(workspace_id);
create index if not exists idx_api_token_status on api_token(status);
create index if not exists idx_api_token_expires_at
    on api_token (expires_at)
    where expires_at is not null;
create index if not exists idx_runtime_ingestion_run_project_created
    on runtime_ingestion_run(project_id, created_at desc);
create index if not exists idx_runtime_ingestion_run_project_status
    on runtime_ingestion_run(project_id, status, updated_at desc);
create index if not exists idx_runtime_stage_event_run_created
    on runtime_ingestion_stage_event(ingestion_run_id, created_at asc);
create index if not exists idx_runtime_graph_node_project_type
    on runtime_graph_node(project_id, node_type, updated_at desc);
create index if not exists idx_runtime_graph_edge_project_relation
    on runtime_graph_edge(project_id, relation_type, updated_at desc);
create index if not exists idx_runtime_graph_evidence_target
    on runtime_graph_evidence(project_id, target_kind, target_id, is_active);
create index if not exists idx_runtime_provider_validation_project_created
    on runtime_provider_validation_log(project_id, created_at desc);
create index if not exists idx_runtime_query_execution_project_created
    on runtime_query_execution(project_id, created_at desc);
create index if not exists idx_runtime_query_reference_execution_rank
    on runtime_query_reference(query_execution_id, rank asc);
create index if not exists idx_runtime_graph_extraction_project_document
    on runtime_graph_extraction(project_id, document_id, created_at desc);
create index if not exists idx_runtime_graph_extraction_chunk_created
    on runtime_graph_extraction(chunk_id, created_at desc);

alter table document
    add column if not exists current_revision_id uuid,
    add column if not exists active_status text not null default 'ready',
    add column if not exists active_mutation_kind text,
    add column if not exists active_mutation_status text,
    add column if not exists deleted_at timestamptz;

create table if not exists document_revision (
    id uuid primary key,
    document_id uuid not null references document(id) on delete cascade,
    revision_no integer not null,
    revision_kind text not null,
    parent_revision_id uuid references document_revision(id) on delete set null,
    source_file_name text not null,
    mime_type text,
    file_size_bytes bigint,
    appended_text_excerpt text,
    content_hash text,
    status text not null default 'pending',
    accepted_at timestamptz not null default now(),
    activated_at timestamptz,
    superseded_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique(document_id, revision_no)
);

create index if not exists idx_document_revision_document_revision_no
    on document_revision(document_id, revision_no desc);
create index if not exists idx_document_revision_document_status
    on document_revision(document_id, status, accepted_at desc);

insert into document_revision (
    id,
    document_id,
    revision_no,
    revision_kind,
    source_file_name,
    mime_type,
    content_hash,
    status,
    accepted_at,
    activated_at,
    created_at,
    updated_at
)
select
    gen_random_uuid(),
    d.id,
    1,
    'initial_upload',
    coalesce(nullif(d.title, ''), d.external_key),
    d.mime_type,
    d.checksum,
    'active',
    d.created_at,
    d.created_at,
    d.created_at,
    d.updated_at
from document as d
where not exists (
    select 1
    from document_revision as revision
    where revision.document_id = d.id
);

update document as d
set current_revision_id = revision.id
from document_revision as revision
where revision.document_id = d.id
  and revision.revision_no = 1
  and d.current_revision_id is null;

alter table runtime_ingestion_run
    add column if not exists revision_id uuid references document_revision(id) on delete set null,
    add column if not exists attempt_kind text not null default 'initial_upload',
    add column if not exists queue_started_at timestamptz not null default now(),
    add column if not exists queue_elapsed_ms bigint,
    add column if not exists total_elapsed_ms bigint;

update runtime_ingestion_run
set queue_started_at = coalesce(queue_started_at, created_at)
where queue_started_at is null;

create table if not exists document_mutation_workflow (
    id uuid primary key,
    document_id uuid not null references document(id) on delete cascade,
    target_revision_id uuid references document_revision(id) on delete set null,
    mutation_kind text not null,
    status text not null,
    stale_guard_revision_no integer,
    requested_by text,
    accepted_at timestamptz not null default now(),
    finished_at timestamptz,
    error_message text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create unique index if not exists idx_document_mutation_workflow_active
    on document_mutation_workflow(document_id)
    where status in ('accepted', 'reconciling');

alter table runtime_ingestion_stage_event
    add column if not exists provider_kind text,
    add column if not exists model_name text,
    add column if not exists elapsed_ms bigint;

update runtime_ingestion_stage_event
set elapsed_ms = greatest(
    0,
    floor(extract(epoch from (finished_at - started_at)) * 1000)::bigint
)
where finished_at is not null
  and elapsed_ms is null;

create table if not exists runtime_attempt_stage_accounting (
    id uuid primary key,
    ingestion_run_id uuid not null references runtime_ingestion_run(id) on delete cascade,
    stage_event_id uuid not null unique references runtime_ingestion_stage_event(id) on delete cascade,
    stage text not null,
    workspace_id uuid references workspace(id) on delete cascade,
    project_id uuid references project(id) on delete cascade,
    provider_kind text,
    model_name text,
    capability text not null,
    billing_unit text not null,
    usage_event_id uuid references usage_event(id) on delete set null,
    cost_ledger_id uuid references cost_ledger(id) on delete set null,
    pricing_catalog_entry_id uuid,
    pricing_status text not null,
    estimated_cost numeric(20,8),
    currency text,
    token_usage_json jsonb not null default '{}'::jsonb,
    pricing_snapshot_json jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now()
);

create index if not exists idx_runtime_attempt_stage_accounting_run
    on runtime_attempt_stage_accounting(ingestion_run_id, created_at asc);
create index if not exists idx_runtime_attempt_stage_accounting_pricing
    on runtime_attempt_stage_accounting(pricing_status, provider_kind, model_name);

create table if not exists runtime_attempt_cost_summary (
    ingestion_run_id uuid primary key references runtime_ingestion_run(id) on delete cascade,
    total_estimated_cost numeric(20,8),
    currency text,
    priced_stage_count integer not null default 0,
    unpriced_stage_count integer not null default 0,
    accounting_status text not null default 'unpriced',
    computed_at timestamptz not null default now()
);

alter table runtime_graph_evidence
    add column if not exists revision_id uuid references document_revision(id) on delete set null,
    add column if not exists activated_by_attempt_id uuid references runtime_ingestion_run(id) on delete set null,
    add column if not exists deactivated_by_mutation_id uuid references document_mutation_workflow(id) on delete set null;

create table if not exists model_pricing_catalog (
    id uuid primary key,
    workspace_id uuid references workspace(id) on delete cascade,
    provider_kind text not null,
    model_name text not null,
    capability text not null,
    billing_unit text not null,
    input_price numeric(20,8),
    output_price numeric(20,8),
    currency text not null default 'USD',
    status text not null default 'active',
    source_kind text not null default 'manual',
    note text,
    effective_from timestamptz not null,
    effective_to timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique(workspace_id, provider_kind, model_name, capability, billing_unit, effective_from)
);

create index if not exists idx_model_pricing_catalog_lookup
    on model_pricing_catalog(workspace_id, provider_kind, model_name, capability, billing_unit, effective_from desc);
create index if not exists idx_model_pricing_catalog_status
    on model_pricing_catalog(status, provider_kind, model_name);

create table if not exists query_intent_cache_entry (
    id uuid primary key,
    workspace_id uuid not null references workspace(id) on delete cascade,
    project_id uuid not null references project(id) on delete cascade,
    normalized_question_hash text not null,
    explicit_mode text not null,
    planned_mode text not null,
    high_level_keywords_json jsonb not null default '[]'::jsonb,
    low_level_keywords_json jsonb not null default '[]'::jsonb,
    intent_summary text,
    source_truth_version bigint not null,
    status text not null default 'fresh',
    created_at timestamptz not null default now(),
    last_used_at timestamptz not null default now(),
    expires_at timestamptz not null
);

create unique index if not exists idx_query_intent_cache_entry_unique
    on query_intent_cache_entry(project_id, normalized_question_hash, explicit_mode, source_truth_version);

create index if not exists idx_query_intent_cache_entry_lookup
    on query_intent_cache_entry(project_id, explicit_mode, source_truth_version, status, last_used_at desc);

create index if not exists idx_query_intent_cache_entry_expiry
    on query_intent_cache_entry(project_id, expires_at asc);
