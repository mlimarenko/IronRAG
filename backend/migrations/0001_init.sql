-- Canonical RustRAG baseline schema (fresh stack):
-- Postgres (control-plane + operations), Redis (queue/session cache), ArangoDB (knowledge plane).
-- This migration intentionally avoids legacy graph-store bootstrap/repair routines.
create extension if not exists pgcrypto;

create type catalog_workspace_lifecycle_state as enum ('active', 'archived');
create type catalog_library_lifecycle_state as enum ('active', 'archived');
create type catalog_connector_kind as enum ('generic', 'filesystem', 'github', 's3', 'web');
create type catalog_connector_sync_mode as enum ('manual', 'scheduled', 'webhook');

create type iam_principal_kind as enum ('user', 'api_token', 'worker', 'bootstrap');
create type iam_principal_status as enum ('active', 'disabled', 'revoked');
create type iam_api_token_status as enum ('active', 'disabled', 'revoked', 'expired');
create type iam_membership_state as enum ('active', 'invited', 'suspended', 'ended');
create type iam_grant_resource_kind as enum (
    'system',
    'workspace',
    'library',
    'document',
    'query_session',
    'async_operation',
    'connector',
    'provider_credential',
    'library_binding'
);
create type iam_permission_kind as enum (
    'workspace_admin',
    'workspace_read',
    'library_read',
    'library_write',
    'document_read',
    'document_write',
    'connector_admin',
    'credential_admin',
    'binding_admin',
    'query_run',
    'ops_read',
    'audit_read',
    'iam_admin'
);

create type ai_provider_api_style as enum ('openai_compatible');
create type ai_provider_lifecycle_state as enum ('active', 'preview', 'deprecated', 'disabled');
create type ai_model_capability_kind as enum ('chat', 'embedding');
create type ai_model_modality_kind as enum ('text', 'multimodal');
create type ai_model_lifecycle_state as enum ('active', 'preview', 'deprecated', 'disabled');
create type ai_price_catalog_scope as enum ('system', 'workspace_override');
create type ai_credential_state as enum ('active', 'invalid', 'revoked');
create type ai_binding_purpose as enum (
    'extract_text',
    'extract_graph',
    'embed_chunk',
    'query_retrieve',
    'query_answer',
    'vision'
);
create type ai_binding_state as enum ('active', 'invalid', 'disabled');
create type ai_validation_state as enum ('pending', 'succeeded', 'failed');

create type surface_kind as enum ('ui', 'rest', 'mcp', 'worker', 'bootstrap');

create type content_document_state as enum ('active', 'deleted');
create type content_source_kind as enum ('upload', 'append', 'replace', 'connector_sync', 'import');
create type content_mutation_operation_kind as enum (
    'upload',
    'append',
    'replace',
    'reprocess',
    'delete',
    'connector_sync'
);
create type content_mutation_state as enum (
    'accepted',
    'running',
    'applied',
    'failed',
    'conflicted',
    'canceled'
);
create type content_mutation_item_state as enum ('pending', 'applied', 'failed', 'conflicted', 'skipped');

create type ingest_job_kind as enum ('content_mutation', 'connector_sync', 'reindex', 'reembed', 'graph_refresh');
create type ingest_queue_state as enum ('queued', 'leased', 'completed', 'failed', 'canceled');
create type ingest_attempt_state as enum ('leased', 'running', 'succeeded', 'failed', 'abandoned', 'canceled');
create type ingest_stage_state as enum ('started', 'completed', 'failed', 'skipped');

create type extract_state as enum ('missing', 'processing', 'ready', 'failed');

-- Legacy graph projection types removed: canonical graph truth is in ArangoDB.

create type query_conversation_state as enum ('active', 'archived');
create type query_turn_kind as enum ('user', 'assistant', 'system', 'tool');
create type query_execution_state as enum (
    'planned',
    'retrieving',
    'answering',
    'completed',
    'failed',
    'canceled'
);

create type billing_owning_execution_kind as enum ('ingest_attempt', 'query_execution', 'binding_validation');
create type billing_call_state as enum ('started', 'completed', 'failed', 'canceled');
create type billing_unit as enum ('per_1m_input_tokens', 'per_1m_output_tokens');

create type ops_async_operation_status as enum ('accepted', 'processing', 'ready', 'failed', 'superseded', 'canceled');
create type ops_degraded_state as enum ('healthy', 'degraded', 'failed', 'rebuilding');
create type ops_warning_severity as enum ('info', 'warn', 'error');

create type audit_result_kind as enum ('succeeded', 'rejected', 'failed');

create table iam_principal (
    id uuid primary key default uuidv7(),
    principal_kind iam_principal_kind not null,
    display_label text not null,
    status iam_principal_status not null default 'active',
    parent_principal_id uuid references iam_principal(id) on delete set null,
    created_at timestamptz not null default now(),
    disabled_at timestamptz
);

create table catalog_workspace (
    id uuid primary key default uuidv7(),
    slug text not null unique,
    display_name text not null,
    lifecycle_state catalog_workspace_lifecycle_state not null default 'active',
    created_by_principal_id uuid references iam_principal(id) on delete set null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    check (slug ~ '^[a-z0-9]+(?:[-_][a-z0-9]+)*$')
);

create table catalog_library (
    id uuid primary key default uuidv7(),
    workspace_id uuid not null references catalog_workspace(id) on delete cascade,
    slug text not null,
    display_name text not null,
    description text,
    lifecycle_state catalog_library_lifecycle_state not null default 'active',
    source_truth_version bigint not null default 1,
    created_by_principal_id uuid references iam_principal(id) on delete set null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (workspace_id, slug),
    unique (id, workspace_id),
    check (slug ~ '^[a-z0-9]+(?:[-_][a-z0-9]+)*$')
);

create table ai_provider_catalog (
    id uuid primary key,
    provider_kind text not null unique,
    display_name text not null,
    api_style ai_provider_api_style not null,
    lifecycle_state ai_provider_lifecycle_state not null default 'active',
    default_base_url text,
    capability_flags_json jsonb not null default '{}'::jsonb
);

create table ai_model_catalog (
    id uuid primary key,
    provider_catalog_id uuid not null references ai_provider_catalog(id) on delete cascade,
    model_name text not null,
    capability_kind ai_model_capability_kind not null,
    modality_kind ai_model_modality_kind not null,
    context_window integer,
    max_output_tokens integer,
    lifecycle_state ai_model_lifecycle_state not null default 'active',
    metadata_json jsonb not null default '{}'::jsonb,
    unique (provider_catalog_id, model_name, capability_kind)
);

create table ai_price_catalog (
    id uuid primary key,
    model_catalog_id uuid not null references ai_model_catalog(id) on delete cascade,
    billing_unit billing_unit not null,
    unit_price numeric(18,8) not null,
    currency_code text not null,
    effective_from timestamptz not null,
    effective_to timestamptz,
    catalog_scope ai_price_catalog_scope not null,
    workspace_id uuid references catalog_workspace(id) on delete cascade,
    check (effective_to is null or effective_to > effective_from),
    check (
        (catalog_scope = 'system' and workspace_id is null)
        or (catalog_scope = 'workspace_override' and workspace_id is not null)
    )
);

create table iam_user (
    principal_id uuid primary key references iam_principal(id) on delete cascade,
    login text not null unique,
    email text not null unique,
    display_name text not null,
    password_hash text not null,
    auth_provider_kind text not null default 'password',
    external_subject text
);

create table iam_session (
    id uuid primary key default uuidv7(),
    principal_id uuid not null references iam_principal(id) on delete cascade,
    session_secret_hash text not null,
    issued_at timestamptz not null default now(),
    expires_at timestamptz not null,
    revoked_at timestamptz,
    last_seen_at timestamptz not null default now()
);

create table iam_api_token (
    principal_id uuid primary key references iam_principal(id) on delete cascade,
    workspace_id uuid references catalog_workspace(id) on delete cascade,
    label text not null,
    token_prefix text not null,
    status iam_api_token_status not null default 'active',
    expires_at timestamptz,
    revoked_at timestamptz,
    issued_by_principal_id uuid references iam_principal(id) on delete set null,
    last_used_at timestamptz
);

create table iam_api_token_secret (
    token_principal_id uuid not null references iam_api_token(principal_id) on delete cascade,
    secret_version integer not null,
    secret_hash text not null,
    issued_at timestamptz not null default now(),
    revoked_at timestamptz,
    primary key (token_principal_id, secret_version)
);

create table iam_workspace_membership (
    workspace_id uuid not null references catalog_workspace(id) on delete cascade,
    principal_id uuid not null references iam_principal(id) on delete cascade,
    membership_state iam_membership_state not null,
    joined_at timestamptz not null default now(),
    ended_at timestamptz,
    primary key (workspace_id, principal_id)
);

create table iam_grant (
    id uuid primary key default uuidv7(),
    principal_id uuid not null references iam_principal(id) on delete cascade,
    resource_kind iam_grant_resource_kind not null,
    resource_id uuid not null,
    permission_kind iam_permission_kind not null,
    granted_by_principal_id uuid references iam_principal(id) on delete set null,
    granted_at timestamptz not null default now(),
    expires_at timestamptz,
    unique (principal_id, resource_kind, resource_id, permission_kind)
);

create table catalog_library_connector (
    id uuid primary key default uuidv7(),
    workspace_id uuid not null,
    library_id uuid not null,
    connector_kind catalog_connector_kind not null,
    display_name text not null,
    configuration_json jsonb not null default '{}'::jsonb,
    sync_mode catalog_connector_sync_mode not null default 'manual',
    last_sync_requested_at timestamptz,
    created_by_principal_id uuid references iam_principal(id) on delete set null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    foreign key (library_id, workspace_id)
        references catalog_library(id, workspace_id)
        on delete cascade
);

create table ai_provider_credential (
    id uuid primary key default uuidv7(),
    workspace_id uuid not null references catalog_workspace(id) on delete cascade,
    provider_catalog_id uuid not null references ai_provider_catalog(id) on delete restrict,
    label text not null,
    api_key text not null,
    credential_state ai_credential_state not null default 'active',
    created_by_principal_id uuid references iam_principal(id) on delete set null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (workspace_id, provider_catalog_id, label),
    unique (id, workspace_id)
);

create table ai_model_preset (
    id uuid primary key default uuidv7(),
    workspace_id uuid not null references catalog_workspace(id) on delete cascade,
    model_catalog_id uuid not null references ai_model_catalog(id) on delete restrict,
    preset_name text not null,
    system_prompt text,
    temperature double precision,
    top_p double precision,
    max_output_tokens_override integer,
    extra_parameters_json jsonb not null default '{}'::jsonb,
    created_by_principal_id uuid references iam_principal(id) on delete set null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (workspace_id, model_catalog_id, preset_name),
    unique (id, workspace_id)
);

create table ai_library_model_binding (
    id uuid primary key default uuidv7(),
    workspace_id uuid not null,
    library_id uuid not null,
    binding_purpose ai_binding_purpose not null,
    provider_credential_id uuid not null,
    model_preset_id uuid not null,
    binding_state ai_binding_state not null default 'active',
    updated_by_principal_id uuid references iam_principal(id) on delete set null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (library_id, binding_purpose),
    foreign key (library_id, workspace_id)
        references catalog_library(id, workspace_id)
        on delete cascade,
    foreign key (provider_credential_id, workspace_id)
        references ai_provider_credential(id, workspace_id)
        on delete restrict,
    foreign key (model_preset_id, workspace_id)
        references ai_model_preset(id, workspace_id)
        on delete restrict
);

create table ai_binding_validation (
    id uuid primary key default uuidv7(),
    binding_id uuid not null references ai_library_model_binding(id) on delete cascade,
    validation_state ai_validation_state not null,
    checked_at timestamptz not null default now(),
    failure_code text,
    message text
);

create table billing_provider_call (
    id uuid primary key default uuidv7(),
    workspace_id uuid not null,
    library_id uuid not null,
    binding_id uuid references ai_library_model_binding(id) on delete set null,
    owning_execution_kind billing_owning_execution_kind not null,
    owning_execution_id uuid not null,
    provider_catalog_id uuid not null references ai_provider_catalog(id) on delete restrict,
    model_catalog_id uuid not null references ai_model_catalog(id) on delete restrict,
    call_kind text not null,
    started_at timestamptz not null default now(),
    completed_at timestamptz,
    call_state billing_call_state not null default 'started',
    foreign key (library_id, workspace_id)
        references catalog_library(id, workspace_id)
        on delete cascade
);

create table billing_usage (
    id uuid primary key default uuidv7(),
    provider_call_id uuid not null references billing_provider_call(id) on delete cascade,
    usage_kind text not null,
    billing_unit billing_unit not null,
    quantity numeric(18,6) not null,
    observed_at timestamptz not null default now()
);

create table billing_charge (
    id uuid primary key default uuidv7(),
    usage_id uuid not null references billing_usage(id) on delete cascade,
    price_catalog_id uuid not null references ai_price_catalog(id) on delete restrict,
    currency_code text not null,
    unit_price numeric(18,8) not null,
    total_price numeric(18,8) not null,
    priced_at timestamptz not null default now()
);

create table billing_execution_cost (
    id uuid primary key default uuidv7(),
    owning_execution_kind billing_owning_execution_kind not null,
    owning_execution_id uuid not null,
    total_cost numeric(18,8) not null default 0,
    currency_code text not null,
    provider_call_count integer not null default 0,
    updated_at timestamptz not null default now(),
    unique (owning_execution_kind, owning_execution_id)
);

create table content_document (
    id uuid primary key default uuidv7(),
    workspace_id uuid not null,
    library_id uuid not null,
    external_key text not null,
    document_state content_document_state not null default 'active',
    created_by_principal_id uuid references iam_principal(id) on delete set null,
    created_at timestamptz not null default now(),
    deleted_at timestamptz,
    unique (library_id, external_key),
    unique (id, workspace_id, library_id),
    foreign key (library_id, workspace_id)
        references catalog_library(id, workspace_id)
        on delete cascade
);

create table content_revision (
    id uuid primary key default uuidv7(),
    document_id uuid not null,
    workspace_id uuid not null,
    library_id uuid not null,
    revision_number integer not null,
    parent_revision_id uuid references content_revision(id) on delete set null,
    content_source_kind content_source_kind not null,
    checksum text not null,
    mime_type text not null,
    byte_size bigint not null,
    title text,
    language_code text,
    source_uri text,
    storage_key text,
    created_by_principal_id uuid references iam_principal(id) on delete set null,
    created_at timestamptz not null default now(),
    unique (document_id, revision_number),
    unique (id, workspace_id, library_id),
    foreign key (document_id, workspace_id, library_id)
        references content_document(id, workspace_id, library_id)
        on delete cascade
);

create table content_chunk (
    id uuid primary key default uuidv7(),
    revision_id uuid not null references content_revision(id) on delete cascade,
    chunk_index integer not null,
    start_offset integer not null,
    end_offset integer not null,
    token_count integer,
    normalized_text text not null,
    text_checksum text not null,
    unique (revision_id, chunk_index),
    check (start_offset >= 0),
    check (end_offset >= start_offset)
);

create table content_mutation (
    id uuid primary key default uuidv7(),
    workspace_id uuid not null,
    library_id uuid not null,
    operation_kind content_mutation_operation_kind not null,
    requested_by_principal_id uuid references iam_principal(id) on delete set null,
    request_surface surface_kind not null,
    idempotency_key text,
    source_identity text,
    mutation_state content_mutation_state not null default 'accepted',
    requested_at timestamptz not null default now(),
    completed_at timestamptz,
    failure_code text,
    conflict_code text,
    foreign key (library_id, workspace_id)
        references catalog_library(id, workspace_id)
        on delete cascade
);

create table content_mutation_item (
    id uuid primary key default uuidv7(),
    mutation_id uuid not null references content_mutation(id) on delete cascade,
    document_id uuid references content_document(id) on delete set null,
    base_revision_id uuid references content_revision(id) on delete set null,
    result_revision_id uuid references content_revision(id) on delete set null,
    item_state content_mutation_item_state not null default 'pending',
    message text
);

create table ops_async_operation (
    id uuid primary key default uuidv7(),
    workspace_id uuid not null,
    library_id uuid not null,
    operation_kind text not null,
    surface_kind surface_kind not null,
    requested_by_principal_id uuid references iam_principal(id) on delete set null,
    status ops_async_operation_status not null default 'accepted',
    subject_kind text not null,
    subject_id uuid,
    created_at timestamptz not null default now(),
    completed_at timestamptz,
    failure_code text,
    foreign key (library_id, workspace_id)
        references catalog_library(id, workspace_id)
        on delete cascade
);

create table ingest_job (
    id uuid primary key default uuidv7(),
    workspace_id uuid not null,
    library_id uuid not null,
    mutation_id uuid references content_mutation(id) on delete set null,
    connector_id uuid references catalog_library_connector(id) on delete set null,
    async_operation_id uuid references ops_async_operation(id) on delete set null,
    knowledge_document_id uuid,
    knowledge_revision_id uuid,
    job_kind ingest_job_kind not null,
    queue_state ingest_queue_state not null default 'queued',
    priority integer not null default 100,
    dedupe_key text,
    queued_at timestamptz not null default now(),
    available_at timestamptz not null default now(),
    completed_at timestamptz,
    foreign key (library_id, workspace_id)
        references catalog_library(id, workspace_id)
        on delete cascade
);

create table ingest_attempt (
    id uuid primary key default uuidv7(),
    job_id uuid not null references ingest_job(id) on delete cascade,
    attempt_number integer not null,
    worker_principal_id uuid references iam_principal(id) on delete set null,
    lease_token text,
    knowledge_generation_id uuid,
    attempt_state ingest_attempt_state not null,
    current_stage text,
    started_at timestamptz not null default now(),
    heartbeat_at timestamptz,
    finished_at timestamptz,
    failure_class text,
    failure_code text,
    retryable boolean not null default false,
    unique (job_id, attempt_number)
);

create table ingest_stage_event (
    id uuid primary key default uuidv7(),
    attempt_id uuid not null references ingest_attempt(id) on delete cascade,
    stage_name text not null,
    stage_state ingest_stage_state not null,
    ordinal integer not null,
    message text,
    details_json jsonb not null default '{}'::jsonb,
    recorded_at timestamptz not null default now(),
    unique (attempt_id, ordinal)
);

create table extract_content (
    revision_id uuid primary key references content_revision(id) on delete cascade,
    attempt_id uuid references ingest_attempt(id) on delete set null,
    extract_state extract_state not null default 'missing',
    normalized_text text,
    text_checksum text,
    warning_count integer not null default 0,
    updated_at timestamptz not null default now()
);

create table extract_chunk_result (
    id uuid primary key default uuidv7(),
    chunk_id uuid not null references content_chunk(id) on delete cascade,
    attempt_id uuid not null references ingest_attempt(id) on delete cascade,
    extract_state extract_state not null,
    provider_call_id uuid references billing_provider_call(id) on delete set null,
    started_at timestamptz not null default now(),
    finished_at timestamptz,
    failure_code text,
    unique (chunk_id, attempt_id)
);

create table extract_node_candidate (
    id uuid primary key default uuidv7(),
    chunk_result_id uuid not null references extract_chunk_result(id) on delete cascade,
    canonical_key text not null,
    node_kind text not null,
    display_label text not null,
    summary text
);

create table extract_edge_candidate (
    id uuid primary key default uuidv7(),
    chunk_result_id uuid not null references extract_chunk_result(id) on delete cascade,
    canonical_key text not null,
    edge_kind text not null,
    from_canonical_key text not null,
    to_canonical_key text not null,
    summary text
);

create table extract_resume_cursor (
    attempt_id uuid primary key references ingest_attempt(id) on delete cascade,
    last_completed_chunk_index integer not null default -1,
    replay_count integer not null default 0,
    downgrade_level integer not null default 0,
    updated_at timestamptz not null default now()
);

-- Legacy graph projection tables removed: canonical graph truth is in ArangoDB
-- (knowledge_entity, knowledge_relation, knowledge_evidence, etc.).

create table query_conversation (
    id uuid primary key default uuidv7(),
    workspace_id uuid not null,
    library_id uuid not null,
    created_by_principal_id uuid references iam_principal(id) on delete set null,
    title text,
    conversation_state query_conversation_state not null default 'active',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (id, workspace_id, library_id),
    foreign key (library_id, workspace_id)
        references catalog_library(id, workspace_id)
        on delete cascade
);

create table query_execution (
    id uuid primary key default uuidv7(),
    workspace_id uuid not null,
    library_id uuid not null,
    conversation_id uuid not null references query_conversation(id) on delete cascade,
    context_bundle_id uuid not null,
    request_turn_id uuid,
    response_turn_id uuid,
    binding_id uuid references ai_library_model_binding(id) on delete set null,
    execution_state query_execution_state not null default 'planned',
    query_text text not null,
    failure_code text,
    started_at timestamptz not null default now(),
    completed_at timestamptz,
    foreign key (library_id, workspace_id)
        references catalog_library(id, workspace_id)
        on delete cascade
);

create table query_turn (
    id uuid primary key default uuidv7(),
    conversation_id uuid not null references query_conversation(id) on delete cascade,
    turn_index integer not null,
    turn_kind query_turn_kind not null,
    author_principal_id uuid references iam_principal(id) on delete set null,
    content_text text not null,
    execution_id uuid references query_execution(id) on delete set null,
    created_at timestamptz not null default now(),
    unique (conversation_id, turn_index)
);

alter table query_execution
    add constraint query_execution_request_turn_id_fkey
        foreign key (request_turn_id) references query_turn(id) on delete set null,
    add constraint query_execution_response_turn_id_fkey
        foreign key (response_turn_id) references query_turn(id) on delete set null;

create table query_chunk_reference (
    execution_id uuid not null references query_execution(id) on delete cascade,
    chunk_id uuid not null references content_chunk(id) on delete cascade,
    rank integer not null,
    score double precision not null,
    primary key (execution_id, chunk_id)
);

-- Legacy query_graph_node_reference and query_graph_edge_reference removed:
-- graph-grounded query references use ArangoDB knowledge_retrieval_trace.

create table ops_library_state (
    library_id uuid primary key references catalog_library(id) on delete cascade,
    queue_depth integer not null default 0,
    running_attempts integer not null default 0,
    readable_document_count integer not null default 0,
    failed_document_count integer not null default 0,
    degraded_state ops_degraded_state not null default 'healthy',
    last_recomputed_at timestamptz not null default now()
);

create table ops_library_warning (
    id uuid primary key default uuidv7(),
    library_id uuid not null references catalog_library(id) on delete cascade,
    warning_kind text not null,
    severity ops_warning_severity not null,
    message text not null,
    source_operation_id uuid references ops_async_operation(id) on delete set null,
    created_at timestamptz not null default now(),
    resolved_at timestamptz
);

create table audit_event (
    id uuid primary key default uuidv7(),
    actor_principal_id uuid references iam_principal(id) on delete set null,
    surface_kind surface_kind not null,
    action_kind text not null,
    request_id text,
    trace_id text,
    result_kind audit_result_kind not null,
    created_at timestamptz not null default now(),
    redacted_message text,
    internal_message text
);

create table audit_event_subject (
    audit_event_id uuid not null references audit_event(id) on delete cascade,
    subject_kind text not null,
    subject_id uuid not null,
    workspace_id uuid references catalog_workspace(id) on delete set null,
    library_id uuid references catalog_library(id) on delete set null,
    document_id uuid references content_document(id) on delete set null,
    primary key (audit_event_id, subject_kind, subject_id)
);

create table content_document_head (
    document_id uuid primary key references content_document(id) on delete cascade,
    active_revision_id uuid references content_revision(id) on delete set null,
    readable_revision_id uuid references content_revision(id) on delete set null,
    latest_mutation_id uuid references content_mutation(id) on delete set null,
    latest_successful_attempt_id uuid references ingest_attempt(id) on delete set null,
    head_updated_at timestamptz not null default now()
);

create unique index idx_iam_api_token_secret_latest_active
    on iam_api_token_secret (token_principal_id)
    where revoked_at is null;

create unique index idx_ai_price_catalog_system_effective
    on ai_price_catalog (model_catalog_id, billing_unit, effective_from)
    where catalog_scope = 'system';

create unique index idx_ai_price_catalog_workspace_override_effective
    on ai_price_catalog (model_catalog_id, billing_unit, workspace_id, effective_from)
    where catalog_scope = 'workspace_override';

create unique index idx_content_mutation_idempotency
    on content_mutation (requested_by_principal_id, request_surface, idempotency_key)
    where idempotency_key is not null;

create unique index idx_ingest_job_dedupe_key
    on ingest_job (library_id, dedupe_key)
    where dedupe_key is not null;

create index idx_catalog_library_workspace_lifecycle
    on catalog_library (workspace_id, lifecycle_state);

create index idx_catalog_library_connector_library_sync_mode
    on catalog_library_connector (library_id, sync_mode, last_sync_requested_at);

create index idx_iam_grant_principal_resource
    on iam_grant (principal_id, resource_kind, resource_id);

create index idx_ai_library_model_binding_library_purpose
    on ai_library_model_binding (library_id, binding_purpose, binding_state);

create index idx_content_document_library_state
    on content_document (library_id, document_state);

create index idx_content_revision_document_created_at
    on content_revision (document_id, created_at desc);

create index idx_content_mutation_library_state
    on content_mutation (library_id, mutation_state, requested_at desc);

create index idx_ingest_job_library_queue
    on ingest_job (library_id, queue_state, priority, available_at);

create index idx_ingest_attempt_job_state
    on ingest_attempt (job_id, attempt_state, started_at desc);

create index idx_ingest_stage_event_attempt_ordinal
    on ingest_stage_event (attempt_id, ordinal);

create index idx_extract_chunk_result_attempt_state
    on extract_chunk_result (attempt_id, extract_state);

-- Legacy graph projection indexes removed (ArangoDB canonical).

create index idx_query_conversation_library_updated_at
    on query_conversation (library_id, updated_at desc);

create index idx_query_execution_library_state
    on query_execution (library_id, execution_state, started_at desc);

create index idx_billing_provider_call_owner
    on billing_provider_call (owning_execution_kind, owning_execution_id);

create index idx_ops_async_operation_library_status
    on ops_async_operation (library_id, status, created_at desc);

create index idx_audit_event_actor_created_at
    on audit_event (actor_principal_id, created_at desc);

create index idx_audit_event_request_id
    on audit_event (request_id)
    where request_id is not null;

insert into ai_provider_catalog (
    id,
    provider_kind,
    display_name,
    api_style,
    lifecycle_state,
    default_base_url,
    capability_flags_json
)
values
    (
        '00000000-0000-0000-0000-000000000101',
        'openai',
        'OpenAI',
        'openai_compatible',
        'active',
        'https://api.openai.com/v1',
        '{"chat": true, "embedding": true, "vision": true}'::jsonb
    ),
    (
        '00000000-0000-0000-0000-000000000102',
        'deepseek',
        'DeepSeek',
        'openai_compatible',
        'active',
        'https://api.deepseek.com/v1',
        '{"chat": true, "embedding": false, "vision": false}'::jsonb
    ),
    (
        '00000000-0000-0000-0000-000000000103',
        'qwen',
        'Qwen',
        'openai_compatible',
        'active',
        'https://dashscope-intl.aliyuncs.com/compatible-mode/v1',
        '{"chat": true, "embedding": true, "vision": true}'::jsonb
    );

insert into ai_model_catalog (
    id,
    provider_catalog_id,
    model_name,
    capability_kind,
    modality_kind,
    context_window,
    max_output_tokens,
    lifecycle_state,
    metadata_json
)
values
    (
        '00000000-0000-0000-0000-000000000201',
        '00000000-0000-0000-0000-000000000101',
        'gpt-5-mini',
        'chat',
        'multimodal',
        null,
        null,
        'active',
        '{"defaultRoles": ["extract_text", "vision"], "seedSource": "provider_catalog"}'::jsonb
    ),
    (
        '00000000-0000-0000-0000-000000000202',
        '00000000-0000-0000-0000-000000000101',
        'text-embedding-3-large',
        'embedding',
        'text',
        null,
        null,
        'active',
        '{"defaultRoles": ["embed_chunk"], "seedSource": "provider_catalog"}'::jsonb
    ),
    (
        '00000000-0000-0000-0000-000000000203',
        '00000000-0000-0000-0000-000000000101',
        'gpt-5.4',
        'chat',
        'multimodal',
        null,
        null,
        'active',
        '{"defaultRoles": ["query_answer"], "seedSource": "provider_catalog"}'::jsonb
    ),
    (
        '00000000-0000-0000-0000-000000000204',
        '00000000-0000-0000-0000-000000000102',
        'deepseek-chat',
        'chat',
        'text',
        null,
        null,
        'active',
        '{"defaultRoles": ["extract_graph"], "seedSource": "provider_catalog"}'::jsonb
    ),
    (
        '00000000-0000-0000-0000-000000000205',
        '00000000-0000-0000-0000-000000000103',
        'qwen3-max',
        'chat',
        'text',
        null,
        null,
        'active',
        '{"defaultRoles": ["query_answer"], "seedSource": "provider_catalog"}'::jsonb
    ),
    (
        '00000000-0000-0000-0000-000000000206',
        '00000000-0000-0000-0000-000000000103',
        'text-embedding-v4',
        'embedding',
        'text',
        null,
        null,
        'active',
        '{"defaultRoles": ["embed_chunk"], "seedSource": "provider_catalog"}'::jsonb
    ),
    (
        '00000000-0000-0000-0000-000000000207',
        '00000000-0000-0000-0000-000000000103',
        'qwen3.5-plus',
        'chat',
        'multimodal',
        null,
        null,
        'active',
        '{"defaultRoles": ["vision"], "seedSource": "provider_catalog"}'::jsonb
    );

insert into ai_price_catalog (
    id,
    model_catalog_id,
    billing_unit,
    unit_price,
    currency_code,
    effective_from,
    effective_to,
    catalog_scope,
    workspace_id
)
values
    (
        '00000000-0000-0000-0000-000000000301',
        '00000000-0000-0000-0000-000000000201',
        'per_1m_input_tokens',
        0.25,
        'USD',
        timestamptz '2026-03-20 00:00:00+00',
        null,
        'system',
        null
    ),
    (
        '00000000-0000-0000-0000-000000000302',
        '00000000-0000-0000-0000-000000000201',
        'per_1m_output_tokens',
        2.00,
        'USD',
        timestamptz '2026-03-20 00:00:00+00',
        null,
        'system',
        null
    ),
    (
        '00000000-0000-0000-0000-000000000303',
        '00000000-0000-0000-0000-000000000202',
        'per_1m_input_tokens',
        0.13,
        'USD',
        timestamptz '2026-03-20 00:00:00+00',
        null,
        'system',
        null
    ),
    (
        '00000000-0000-0000-0000-000000000304',
        '00000000-0000-0000-0000-000000000203',
        'per_1m_input_tokens',
        2.50,
        'USD',
        timestamptz '2026-03-20 00:00:00+00',
        null,
        'system',
        null
    ),
    (
        '00000000-0000-0000-0000-000000000305',
        '00000000-0000-0000-0000-000000000203',
        'per_1m_output_tokens',
        15.00,
        'USD',
        timestamptz '2026-03-20 00:00:00+00',
        null,
        'system',
        null
    ),
    (
        '00000000-0000-0000-0000-000000000306',
        '00000000-0000-0000-0000-000000000204',
        'per_1m_input_tokens',
        0.28,
        'USD',
        timestamptz '2026-03-20 00:00:00+00',
        null,
        'system',
        null
    ),
    (
        '00000000-0000-0000-0000-000000000307',
        '00000000-0000-0000-0000-000000000204',
        'per_1m_output_tokens',
        0.42,
        'USD',
        timestamptz '2026-03-20 00:00:00+00',
        null,
        'system',
        null
    ),
    (
        '00000000-0000-0000-0000-000000000308',
        '00000000-0000-0000-0000-000000000205',
        'per_1m_input_tokens',
        1.20,
        'USD',
        timestamptz '2026-03-20 00:00:00+00',
        null,
        'system',
        null
    ),
    (
        '00000000-0000-0000-0000-000000000309',
        '00000000-0000-0000-0000-000000000205',
        'per_1m_output_tokens',
        6.00,
        'USD',
        timestamptz '2026-03-20 00:00:00+00',
        null,
        'system',
        null
    ),
    (
        '00000000-0000-0000-0000-000000000310',
        '00000000-0000-0000-0000-000000000206',
        'per_1m_input_tokens',
        0.07,
        'USD',
        timestamptz '2026-03-20 00:00:00+00',
        null,
        'system',
        null
    ),
    (
        '00000000-0000-0000-0000-000000000311',
        '00000000-0000-0000-0000-000000000207',
        'per_1m_input_tokens',
        0.40,
        'USD',
        timestamptz '2026-03-20 00:00:00+00',
        null,
        'system',
        null
    ),
    (
        '00000000-0000-0000-0000-000000000312',
        '00000000-0000-0000-0000-000000000207',
        'per_1m_output_tokens',
        2.40,
        'USD',
        timestamptz '2026-03-20 00:00:00+00',
        null,
        'system',
        null
    );
