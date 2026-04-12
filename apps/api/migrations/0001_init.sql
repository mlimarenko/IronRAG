-- IronRAG v0.2.0 consolidated schema
-- Postgres (control plane, operations), Redis (queues, session cache),
-- ArangoDB (knowledge graph, retrieval traces).

-- ============================================================================
-- Extensions
-- ============================================================================

create extension if not exists pgcrypto;

-- ============================================================================
-- Functions
-- ============================================================================

create or replace function uuidv7() returns uuid as $$
declare
    unix_ts_ms bigint;
    uuid_bytes bytea;
begin
    unix_ts_ms = (extract(epoch from clock_timestamp()) * 1000)::bigint;
    uuid_bytes = set_byte(
        set_byte(
            overlay(
                uuid_send(gen_random_uuid())
                placing substring(int8send(unix_ts_ms) from 3)
                from 1 for 6
            ),
            6, (get_byte(uuid_send(gen_random_uuid()), 6) & 15) | 112
        ),
        8, (get_byte(uuid_send(gen_random_uuid()), 8) & 63) | 128
    );
    return encode(uuid_bytes, 'hex')::uuid;
end
$$ language plpgsql volatile;

-- ============================================================================
-- Enums
-- ============================================================================

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
create type ai_scope_kind as enum ('instance', 'workspace', 'library');

create type surface_kind as enum ('ui', 'rest', 'mcp', 'worker', 'bootstrap', 'stream', 'internal');

create type content_document_state as enum ('active', 'deleted');
create type content_source_kind as enum ('upload', 'append', 'replace', 'edit', 'connector_sync', 'import', 'web_page');
create type content_mutation_operation_kind as enum (
    'upload',
    'append',
    'replace',
    'edit',
    'reprocess',
    'delete',
    'connector_sync',
    'web_capture'
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

create type ingest_job_kind as enum (
    'content_mutation',
    'connector_sync',
    'reindex',
    'reembed',
    'graph_refresh',
    'web_discovery',
    'web_materialize_page'
);
create type ingest_queue_state as enum ('queued', 'leased', 'completed', 'failed', 'canceled');
create type ingest_attempt_state as enum ('leased', 'running', 'succeeded', 'failed', 'abandoned', 'canceled');
create type ingest_stage_state as enum ('started', 'completed', 'failed', 'skipped');

create type extract_state as enum ('missing', 'processing', 'ready', 'failed');
create type web_ingest_mode as enum ('single_page', 'recursive_crawl');
create type web_boundary_policy as enum ('same_host', 'allow_external');
create type web_run_state as enum (
    'accepted',
    'discovering',
    'processing',
    'completed',
    'completed_partial',
    'failed',
    'canceled'
);
create type web_candidate_host_classification as enum ('same_host', 'external');
create type web_candidate_state as enum (
    'discovered',
    'eligible',
    'duplicate',
    'excluded',
    'blocked',
    'queued',
    'processing',
    'processed',
    'failed',
    'canceled'
);

-- Knowledge graph (entities, relations) lives in ArangoDB; enums below are for query, billing, and ops.

create type query_conversation_state as enum ('active', 'archived');
create type query_turn_kind as enum ('user', 'assistant', 'system', 'tool');

create type runtime_execution_owner_kind as enum (
    'query_execution',
    'graph_extraction_attempt',
    'structured_preparation',
    'technical_fact_extraction'
);

create type runtime_task_kind as enum (
    'query_plan',
    'query_rerank',
    'query_answer',
    'query_verify',
    'graph_extract',
    'structured_prepare',
    'technical_fact_extract'
);

create type runtime_lifecycle_state as enum (
    'accepted',
    'running',
    'completed',
    'recovered',
    'failed',
    'canceled'
);

create type runtime_stage_kind as enum (
    'plan',
    'retrieve',
    'rerank',
    'assemble_context',
    'answer',
    'verify',
    'extract_graph',
    'structured_prepare',
    'technical_fact_extract',
    'recovery',
    'persist'
);

create type runtime_stage_state as enum (
    'pending',
    'running',
    'completed',
    'recovered',
    'failed',
    'canceled'
);

create type runtime_action_kind as enum (
    'deterministic_step',
    'model_request',
    'tool_request',
    'tool_result',
    'recovery_attempt',
    'persistence_write'
);

create type runtime_action_state as enum (
    'pending',
    'running',
    'completed',
    'recovered',
    'failed',
    'canceled'
);

create type runtime_decision_target_kind as enum (
    'model_request',
    'tool_request',
    'tool_result',
    'stage_transition',
    'final_outcome'
);

create type runtime_decision_kind as enum ('allow', 'reject', 'terminate');

create type billing_owning_execution_kind as enum (
    'ingest_attempt',
    'query_execution',
    'graph_extraction_attempt',
    'binding_validation'
);
create type billing_call_state as enum ('started', 'completed', 'failed', 'canceled');
create type billing_unit as enum (
    'per_1m_input_tokens',
    'per_1m_output_tokens',
    'per_1m_cached_input_tokens'
);

create type ops_async_operation_status as enum ('accepted', 'processing', 'ready', 'failed', 'superseded', 'canceled');
create type ops_degraded_state as enum ('healthy', 'degraded', 'failed', 'rebuilding');
create type ops_warning_severity as enum ('info', 'warn', 'error');

create type audit_result_kind as enum ('succeeded', 'rejected', 'failed');

-- ============================================================================
-- Tables
-- ============================================================================

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
    extraction_prompt text,
    ai_summary text,
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
    price_variant_key text not null default 'default',
    request_input_tokens_min integer,
    request_input_tokens_max integer,
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
    workspace_id uuid references catalog_workspace(id) on delete cascade,
    provider_catalog_id uuid not null references ai_provider_catalog(id) on delete restrict,
    label text not null,
    api_key text,
    base_url text,
    credential_state ai_credential_state not null default 'active',
    scope_kind ai_scope_kind not null,
    library_id uuid,
    created_by_principal_id uuid references iam_principal(id) on delete set null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint ai_provider_credential_library_scope_fkey
        foreign key (library_id, workspace_id)
        references catalog_library(id, workspace_id)
        on delete cascade,
    constraint ai_provider_credential_scope_check
        check (
            (scope_kind = 'instance' and workspace_id is null and library_id is null)
            or (scope_kind = 'workspace' and workspace_id is not null and library_id is null)
            or (scope_kind = 'library' and workspace_id is not null and library_id is not null)
        )
);

create table ai_model_preset (
    id uuid primary key default uuidv7(),
    workspace_id uuid references catalog_workspace(id) on delete cascade,
    model_catalog_id uuid not null references ai_model_catalog(id) on delete restrict,
    preset_name text not null,
    system_prompt text,
    temperature double precision,
    top_p double precision,
    max_output_tokens_override integer,
    extra_parameters_json jsonb not null default '{}'::jsonb,
    scope_kind ai_scope_kind not null,
    library_id uuid,
    created_by_principal_id uuid references iam_principal(id) on delete set null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint ai_model_preset_library_scope_fkey
        foreign key (library_id, workspace_id)
        references catalog_library(id, workspace_id)
        on delete cascade,
    constraint ai_model_preset_scope_check
        check (
            (scope_kind = 'instance' and workspace_id is null and library_id is null)
            or (scope_kind = 'workspace' and workspace_id is not null and library_id is null)
            or (scope_kind = 'library' and workspace_id is not null and library_id is not null)
        )
);

create table ai_binding_assignment (
    id uuid primary key default uuidv7(),
    workspace_id uuid,
    library_id uuid,
    binding_purpose ai_binding_purpose not null,
    provider_credential_id uuid not null,
    model_preset_id uuid not null,
    binding_state ai_binding_state not null default 'active',
    scope_kind ai_scope_kind not null,
    updated_by_principal_id uuid references iam_principal(id) on delete set null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint ai_binding_assignment_library_scope_fkey
        foreign key (library_id, workspace_id)
        references catalog_library(id, workspace_id)
        on delete cascade,
    constraint ai_binding_assignment_provider_credential_fkey
        foreign key (provider_credential_id)
        references ai_provider_credential(id)
        on delete restrict,
    constraint ai_binding_assignment_model_preset_fkey
        foreign key (model_preset_id)
        references ai_model_preset(id)
        on delete restrict,
    constraint ai_binding_assignment_scope_check
        check (
            (scope_kind = 'instance' and workspace_id is null and library_id is null)
            or (scope_kind = 'workspace' and workspace_id is not null and library_id is null)
            or (scope_kind = 'library' and workspace_id is not null and library_id is not null)
        )
);

create table ai_binding_validation (
    id uuid primary key default uuidv7(),
    binding_id uuid not null references ai_binding_assignment(id) on delete cascade,
    validation_state ai_validation_state not null,
    checked_at timestamptz not null default now(),
    failure_code text,
    message text
);

create table billing_provider_call (
    id uuid primary key default uuidv7(),
    workspace_id uuid not null,
    library_id uuid not null,
    binding_id uuid references ai_binding_assignment(id) on delete set null,
    owning_execution_kind billing_owning_execution_kind not null,
    owning_execution_id uuid not null,
    provider_catalog_id uuid not null references ai_provider_catalog(id) on delete restrict,
    model_catalog_id uuid not null references ai_model_catalog(id) on delete restrict,
    call_kind text not null,
    started_at timestamptz not null default now(),
    completed_at timestamptz,
    call_state billing_call_state not null default 'started',
    runtime_execution_id uuid, -- FK added after runtime_execution table
    runtime_task_kind runtime_task_kind,
    foreign key (library_id, workspace_id)
        references catalog_library(id, workspace_id)
        on delete cascade,
    constraint chk_billing_provider_call_runtime_attribution
        check (
            (runtime_execution_id is null and runtime_task_kind is null)
            or (runtime_execution_id is not null and runtime_task_kind is not null)
        )
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

create table content_web_ingest_run (
    id uuid primary key default uuidv7(),
    mutation_id uuid not null unique references content_mutation(id) on delete cascade,
    async_operation_id uuid unique references ops_async_operation(id) on delete set null,
    workspace_id uuid not null references catalog_workspace(id) on delete cascade,
    library_id uuid not null references catalog_library(id) on delete cascade,
    mode web_ingest_mode not null,
    seed_url text not null,
    normalized_seed_url text not null,
    boundary_policy web_boundary_policy not null,
    max_depth integer not null,
    max_pages integer not null,
    run_state web_run_state not null default 'accepted',
    requested_by_principal_id uuid references iam_principal(id) on delete set null,
    requested_at timestamptz not null default now(),
    completed_at timestamptz,
    failure_code text,
    cancel_requested_at timestamptz,
    check (max_depth >= 0),
    check (max_pages >= 1)
);

create table content_web_discovered_page (
    id uuid primary key default uuidv7(),
    run_id uuid not null references content_web_ingest_run(id) on delete cascade,
    discovered_url text,
    normalized_url text not null,
    final_url text,
    canonical_url text,
    depth integer not null,
    referrer_candidate_id uuid references content_web_discovered_page(id) on delete set null,
    host_classification web_candidate_host_classification not null,
    candidate_state web_candidate_state not null default 'discovered',
    classification_reason text,
    content_type text,
    http_status integer,
    discovered_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    document_id uuid references content_document(id) on delete set null,
    result_revision_id uuid references content_revision(id) on delete set null,
    mutation_item_id uuid references content_mutation_item(id) on delete set null,
    snapshot_storage_key text,
    check (depth >= 0)
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
    provider_kind text,
    model_name text,
    prompt_tokens integer,
    completion_tokens integer,
    total_tokens integer,
    cached_tokens integer,
    estimated_cost numeric(18,8),
    currency_code text,
    elapsed_ms bigint,
    started_at timestamptz,
    recorded_at timestamptz not null default now(),
    unique (attempt_id, ordinal)
);

create table ingest_stage_provider_call (
    id uuid primary key default uuidv7(),
    stage_event_id uuid not null references ingest_stage_event(id) on delete cascade,
    call_sequence integer not null,
    provider_kind text not null,
    model_name text not null,
    prompt_tokens integer,
    completion_tokens integer,
    total_tokens integer,
    cached_tokens integer,
    estimated_cost numeric(18,8),
    currency_code text,
    elapsed_ms bigint,
    started_at timestamptz not null default now(),
    completed_at timestamptz,
    unique (stage_event_id, call_sequence)
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
    binding_id uuid references ai_binding_assignment(id) on delete set null,
    query_text text not null,
    failure_code text,
    runtime_execution_id uuid, -- FK added after runtime_execution table
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

create table runtime_execution (
    id uuid primary key default uuidv7(),
    owner_kind runtime_execution_owner_kind not null,
    owner_id uuid not null,
    task_kind runtime_task_kind not null,
    surface_kind surface_kind not null,
    contract_name text not null,
    contract_version text not null,
    lifecycle_state runtime_lifecycle_state not null,
    active_stage runtime_stage_kind,
    turn_budget integer not null,
    turn_count integer not null default 0,
    parallel_action_limit integer not null,
    failure_code text,
    failure_summary_redacted text,
    accepted_at timestamptz not null default now(),
    completed_at timestamptz
);

-- Add deferred FKs now that runtime_execution exists
alter table query_execution
    add constraint query_execution_runtime_execution_id_fkey
        foreign key (runtime_execution_id) references runtime_execution(id) on delete restrict;

alter table billing_provider_call
    add constraint billing_provider_call_runtime_execution_id_fkey
        foreign key (runtime_execution_id) references runtime_execution(id) on delete set null;

create table runtime_stage_record (
    id uuid primary key default uuidv7(),
    runtime_execution_id uuid not null references runtime_execution(id) on delete cascade,
    stage_kind runtime_stage_kind not null,
    stage_ordinal integer not null,
    attempt_no integer not null,
    stage_state runtime_stage_state not null,
    deterministic boolean not null default false,
    started_at timestamptz not null default now(),
    completed_at timestamptz,
    input_summary_json jsonb not null default '{}'::jsonb,
    output_summary_json jsonb not null default '{}'::jsonb,
    failure_code text,
    failure_summary_redacted text,
    unique (runtime_execution_id, stage_ordinal, attempt_no)
);

create table runtime_action_record (
    id uuid primary key default uuidv7(),
    runtime_execution_id uuid not null references runtime_execution(id) on delete cascade,
    stage_record_id uuid not null references runtime_stage_record(id) on delete cascade,
    action_kind runtime_action_kind not null,
    action_ordinal integer not null,
    action_state runtime_action_state not null,
    provider_binding_id uuid references ai_binding_assignment(id) on delete set null,
    tool_name text,
    usage_json jsonb,
    summary_json jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    unique (runtime_execution_id, action_ordinal)
);

create table runtime_policy_decision (
    id uuid primary key default uuidv7(),
    runtime_execution_id uuid not null references runtime_execution(id) on delete cascade,
    stage_record_id uuid references runtime_stage_record(id) on delete cascade,
    action_record_id uuid references runtime_action_record(id) on delete cascade,
    target_kind runtime_decision_target_kind not null,
    decision_kind runtime_decision_kind not null,
    reason_code text not null,
    reason_summary_redacted text not null,
    created_at timestamptz not null default now()
);

create table runtime_graph_extraction (
    id uuid primary key default uuidv7(),
    runtime_execution_id uuid not null references runtime_execution(id) on delete cascade,
    library_id uuid not null references catalog_library(id) on delete cascade,
    document_id uuid not null references content_document(id) on delete cascade,
    chunk_id uuid not null,
    provider_kind text not null,
    model_name text not null,
    extraction_version text not null,
    prompt_hash text not null,
    status text not null,
    raw_output_json jsonb not null,
    normalized_output_json jsonb not null,
    glean_pass_count integer not null default 0,
    error_message text,
    created_at timestamptz not null default now()
);

create table runtime_graph_extraction_recovery_attempt (
    id uuid primary key default uuidv7(),
    runtime_execution_id uuid not null references runtime_execution(id) on delete cascade,
    workspace_id uuid not null references catalog_workspace(id) on delete cascade,
    library_id uuid not null references catalog_library(id) on delete cascade,
    document_id uuid not null references content_document(id) on delete cascade,
    revision_id uuid references content_revision(id) on delete set null,
    ingestion_run_id uuid,
    attempt_no integer not null,
    chunk_id uuid,
    recovery_kind text not null,
    trigger_reason text not null,
    status text not null,
    raw_issue_summary text,
    recovered_summary text,
    started_at timestamptz not null default now(),
    finished_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create table runtime_graph_snapshot (
    library_id uuid primary key references catalog_library(id) on delete cascade,
    graph_status text not null,
    projection_version bigint not null,
    node_count integer not null default 0,
    edge_count integer not null default 0,
    provenance_coverage_percent double precision,
    last_built_at timestamptz,
    last_error_message text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create table runtime_graph_filtered_artifact (
    id uuid primary key default uuidv7(),
    library_id uuid not null references catalog_library(id) on delete cascade,
    ingestion_run_id uuid,
    revision_id uuid references content_revision(id) on delete set null,
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

create table runtime_graph_node (
    id uuid primary key default uuidv7(),
    library_id uuid not null references catalog_library(id) on delete cascade,
    canonical_key text not null,
    label text not null,
    node_type text not null,
    aliases_json jsonb not null default '[]'::jsonb,
    summary text,
    metadata_json jsonb not null default '{}'::jsonb,
    support_count integer not null default 0,
    community_id integer,
    community_level integer default 0,
    projection_version bigint not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (library_id, canonical_key, projection_version)
);

create table runtime_graph_edge (
    id uuid primary key default uuidv7(),
    library_id uuid not null references catalog_library(id) on delete cascade,
    from_node_id uuid not null references runtime_graph_node(id) on delete cascade,
    to_node_id uuid not null references runtime_graph_node(id) on delete cascade,
    relation_type text not null,
    canonical_key text not null,
    summary text,
    weight double precision,
    support_count integer not null default 0,
    metadata_json jsonb not null default '{}'::jsonb,
    projection_version bigint not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (library_id, canonical_key, projection_version)
);

create table runtime_graph_evidence (
    id uuid primary key default uuidv7(),
    library_id uuid not null references catalog_library(id) on delete cascade,
    evidence_identity_key text not null,
    target_kind text not null,
    target_id uuid not null,
    document_id uuid references content_document(id) on delete set null,
    revision_id uuid references content_revision(id) on delete set null,
    activated_by_attempt_id uuid,
    deactivated_by_mutation_id uuid,
    chunk_id uuid,
    source_file_name text,
    page_ref text,
    evidence_text text not null,
    confidence_score double precision,
    is_active boolean not null default true,
    created_at timestamptz not null default now(),
    unique (library_id, evidence_identity_key)
);

create table runtime_graph_canonical_summary (
    id uuid primary key default uuidv7(),
    workspace_id uuid not null references catalog_workspace(id) on delete cascade,
    library_id uuid not null references catalog_library(id) on delete cascade,
    target_kind text not null,
    target_id uuid not null,
    summary_text text not null,
    confidence_status text not null,
    support_count integer not null default 0,
    source_truth_version bigint not null,
    generated_from_mutation_id uuid,
    warning_text text,
    generated_at timestamptz not null default now(),
    superseded_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (library_id, target_kind, target_id, source_truth_version)
);

create table runtime_graph_community (
    id serial primary key,
    library_id uuid not null,
    community_id integer not null,
    level integer not null default 0,
    node_count integer not null default 0,
    edge_count integer not null default 0,
    summary text,
    top_entities text[] not null default '{}',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (library_id, community_id, level)
);

create table runtime_vector_target (
    id uuid primary key default uuidv7(),
    library_id uuid not null references catalog_library(id) on delete cascade,
    target_kind text not null,
    target_id uuid not null,
    provider_kind text not null,
    model_name text not null,
    dimensions integer,
    embedding_json jsonb not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (library_id, target_kind, target_id, provider_kind, model_name)
);

create table runtime_provider_profile (
    library_id uuid primary key references catalog_library(id) on delete cascade,
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

create table runtime_provider_validation_log (
    id uuid primary key default uuidv7(),
    library_id uuid references catalog_library(id) on delete set null,
    provider_kind text not null,
    model_name text not null,
    capability text not null,
    status text not null,
    error_message text,
    created_at timestamptz not null default now()
);

create table content_mutation_impact_scope (
    id uuid primary key default uuidv7(),
    workspace_id uuid not null references catalog_workspace(id) on delete cascade,
    library_id uuid not null references catalog_library(id) on delete cascade,
    document_id uuid not null references content_document(id) on delete cascade,
    mutation_id uuid not null references content_mutation(id) on delete cascade,
    mutation_kind text not null,
    source_revision_id uuid references content_revision(id) on delete set null,
    target_revision_id uuid references content_revision(id) on delete set null,
    scope_status text not null,
    confidence_status text not null,
    affected_node_ids_json jsonb not null default '[]'::jsonb,
    affected_relationship_ids_json jsonb not null default '[]'::jsonb,
    fallback_reason text,
    detected_at timestamptz not null default now(),
    completed_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create table query_chunk_reference (
    execution_id uuid not null references query_execution(id) on delete cascade,
    chunk_id uuid not null references content_chunk(id) on delete cascade,
    rank integer not null,
    score double precision not null,
    primary key (execution_id, chunk_id)
);

-- Graph-grounded retrieval traces: ArangoDB knowledge_retrieval_trace; Postgres stores chunk references above.

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
    document_summary text,
    head_updated_at timestamptz not null default now()
);

-- ============================================================================
-- Indexes
-- ============================================================================

create unique index idx_iam_api_token_secret_latest_active
    on iam_api_token_secret (token_principal_id)
    where revoked_at is null;

create unique index idx_ai_price_catalog_system_effective
    on ai_price_catalog (
        model_catalog_id,
        billing_unit,
        price_variant_key,
        coalesce(request_input_tokens_min, -1),
        coalesce(request_input_tokens_max, -1),
        effective_from
    )
    where catalog_scope = 'system';

create unique index idx_ai_price_catalog_workspace_override_effective
    on ai_price_catalog (
        model_catalog_id,
        billing_unit,
        price_variant_key,
        coalesce(request_input_tokens_min, -1),
        coalesce(request_input_tokens_max, -1),
        workspace_id,
        effective_from
    )
    where catalog_scope = 'workspace_override';

create index idx_ai_price_catalog_resolution
    on ai_price_catalog (
        model_catalog_id,
        billing_unit,
        price_variant_key,
        request_input_tokens_min,
        request_input_tokens_max,
        effective_from desc
    );

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

create index idx_ai_binding_assignment_library_purpose
    on ai_binding_assignment (library_id, binding_purpose, binding_state);

-- ai_provider_credential scope indexes
create unique index ai_provider_credential_instance_label_key
    on ai_provider_credential (provider_catalog_id, label)
    where scope_kind = 'instance';

create unique index ai_provider_credential_workspace_label_key
    on ai_provider_credential (workspace_id, provider_catalog_id, label)
    where scope_kind = 'workspace';

create unique index ai_provider_credential_library_label_key
    on ai_provider_credential (library_id, provider_catalog_id, label)
    where scope_kind = 'library';

create index ai_provider_credential_scope_idx
    on ai_provider_credential (scope_kind, workspace_id, library_id, provider_catalog_id);

-- ai_model_preset scope indexes
create unique index ai_model_preset_instance_name_key
    on ai_model_preset (model_catalog_id, preset_name)
    where scope_kind = 'instance';

create unique index ai_model_preset_workspace_name_key
    on ai_model_preset (workspace_id, model_catalog_id, preset_name)
    where scope_kind = 'workspace';

create unique index ai_model_preset_library_name_key
    on ai_model_preset (library_id, model_catalog_id, preset_name)
    where scope_kind = 'library';

create index ai_model_preset_scope_idx
    on ai_model_preset (scope_kind, workspace_id, library_id, model_catalog_id);

-- ai_binding_assignment scope indexes
create unique index ai_binding_assignment_instance_purpose_key
    on ai_binding_assignment (binding_purpose)
    where scope_kind = 'instance';

create unique index ai_binding_assignment_workspace_purpose_key
    on ai_binding_assignment (workspace_id, binding_purpose)
    where scope_kind = 'workspace';

create unique index ai_binding_assignment_library_purpose_key
    on ai_binding_assignment (library_id, binding_purpose)
    where scope_kind = 'library';

create index ai_binding_assignment_scope_idx
    on ai_binding_assignment (scope_kind, workspace_id, library_id, binding_purpose, binding_state);

create index idx_content_document_library_state
    on content_document (library_id, document_state);

create index idx_content_revision_document_created_at
    on content_revision (document_id, created_at desc);

create index idx_content_mutation_library_state
    on content_mutation (library_id, mutation_state, requested_at desc);

create unique index idx_content_web_discovered_page_run_normalized_url
    on content_web_discovered_page (run_id, normalized_url);

create index idx_content_web_discovered_page_run_canonical_url
    on content_web_discovered_page (run_id, canonical_url)
    where canonical_url is not null;

create index idx_content_web_ingest_run_library_requested
    on content_web_ingest_run (library_id, requested_at desc);

create index idx_content_web_ingest_run_library_state
    on content_web_ingest_run (library_id, run_state, requested_at desc);

create index idx_content_web_ingest_run_mutation
    on content_web_ingest_run (mutation_id);

create index idx_content_web_discovered_page_run_state
    on content_web_discovered_page (run_id, candidate_state, depth, discovered_at);

create index idx_content_web_discovered_page_document
    on content_web_discovered_page (document_id);

create index idx_ingest_job_library_queue
    on ingest_job (library_id, queue_state, priority, available_at);

create index idx_ingest_attempt_job_state
    on ingest_attempt (job_id, attempt_state, started_at desc);

create index idx_ingest_stage_event_attempt_ordinal
    on ingest_stage_event (attempt_id, ordinal);

create index idx_ingest_stage_provider_call_stage_event
    on ingest_stage_provider_call(stage_event_id);

create index idx_extract_chunk_result_attempt_state
    on extract_chunk_result (attempt_id, extract_state);

create index idx_query_conversation_library_updated_at
    on query_conversation (library_id, updated_at desc);

create index idx_query_execution_library_started_at
    on query_execution (library_id, started_at desc);

create unique index idx_query_execution_runtime_execution_id
    on query_execution (runtime_execution_id)
    where runtime_execution_id is not null;

create index idx_runtime_graph_extraction_runtime_execution_id
    on runtime_graph_extraction (runtime_execution_id);

create index idx_runtime_graph_extraction_recovery_attempt_runtime_execution_id
    on runtime_graph_extraction_recovery_attempt (runtime_execution_id, started_at asc);

create index idx_content_mutation_impact_scope_document_active
    on content_mutation_impact_scope (document_id, updated_at desc)
    where completed_at is null;

create index idx_content_mutation_impact_scope_library_active
    on content_mutation_impact_scope (library_id, updated_at desc)
    where completed_at is null;

create index idx_content_mutation_impact_scope_mutation
    on content_mutation_impact_scope (mutation_id);

create index idx_runtime_graph_filtered_artifact_library_created_at
    on runtime_graph_filtered_artifact (library_id, created_at desc);

create index idx_runtime_graph_node_library_projection
    on runtime_graph_node (library_id, projection_version, created_at asc);

create index idx_runtime_graph_edge_library_projection
    on runtime_graph_edge (library_id, projection_version, created_at asc);

create index idx_runtime_graph_edge_library_projection_nodes
    on runtime_graph_edge (library_id, projection_version, from_node_id, to_node_id);

create index idx_runtime_graph_evidence_library_target_active
    on runtime_graph_evidence (library_id, target_kind, target_id, is_active, created_at desc);

create index idx_runtime_graph_evidence_library_document_active
    on runtime_graph_evidence (library_id, document_id, is_active, created_at desc);

create index idx_runtime_graph_evidence_library_document_revision_active
    on runtime_graph_evidence (library_id, document_id, revision_id, is_active, created_at desc);

create index idx_runtime_graph_canonical_summary_library_active
    on runtime_graph_canonical_summary (library_id, generated_at desc)
    where superseded_at is null;

create index idx_runtime_graph_canonical_summary_target_active
    on runtime_graph_canonical_summary (library_id, target_kind, target_id, generated_at desc)
    where superseded_at is null;

create index idx_runtime_graph_community_library
    on runtime_graph_community(library_id);

create index idx_runtime_vector_target_library_kind_provider
    on runtime_vector_target (library_id, target_kind, provider_kind, model_name, updated_at desc);

create index idx_runtime_provider_validation_log_library_created_at
    on runtime_provider_validation_log (library_id, created_at desc);

create index idx_runtime_provider_validation_log_provider_created_at
    on runtime_provider_validation_log (provider_kind, model_name, capability, created_at desc);

create index idx_billing_provider_call_runtime_execution_id
    on billing_provider_call (runtime_execution_id)
    where runtime_execution_id is not null;

create index idx_runtime_execution_owner
    on runtime_execution (owner_kind, owner_id, accepted_at desc);

create index idx_runtime_stage_record_execution
    on runtime_stage_record (runtime_execution_id, stage_ordinal asc, attempt_no asc);

create index idx_runtime_action_record_execution
    on runtime_action_record (runtime_execution_id, action_ordinal asc);

create index idx_runtime_policy_decision_execution
    on runtime_policy_decision (runtime_execution_id, created_at asc);

create index idx_billing_provider_call_owner
    on billing_provider_call (owning_execution_kind, owning_execution_id);

create index idx_ops_async_operation_library_status
    on ops_async_operation (library_id, status, created_at desc);

create index idx_audit_event_actor_created_at
    on audit_event (actor_principal_id, created_at desc);

create index idx_audit_event_request_id
    on audit_event (request_id)
    where request_id is not null;

-- ============================================================================
-- Seed data
-- ============================================================================

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
    ),
    (
        '00000000-0000-0000-0000-000000000104',
        'ollama',
        'Ollama',
        'openai_compatible',
        'active',
        'http://localhost:11434/v1',
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
    ('00000000-0000-0000-0000-000000000201', '00000000-0000-0000-0000-000000000101', 'gpt-5.4-mini', 'chat', 'multimodal', null, null, 'active', '{"defaultRoles": ["extract_graph", "extract_text", "vision", "query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000202', '00000000-0000-0000-0000-000000000101', 'text-embedding-3-large', 'embedding', 'text', null, null, 'active', '{"defaultRoles": ["embed_chunk"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000203', '00000000-0000-0000-0000-000000000101', 'gpt-5.4', 'chat', 'multimodal', null, null, 'active', '{"defaultRoles": ["extract_graph", "query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000204', '00000000-0000-0000-0000-000000000102', 'deepseek-chat', 'chat', 'text', null, null, 'active', '{"defaultRoles": ["extract_graph", "query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000205', '00000000-0000-0000-0000-000000000103', 'qwen3-max', 'chat', 'text', null, null, 'active', '{"defaultRoles": ["extract_graph", "query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000206', '00000000-0000-0000-0000-000000000103', 'text-embedding-v4', 'embedding', 'text', null, null, 'active', '{"defaultRoles": ["embed_chunk"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000207', '00000000-0000-0000-0000-000000000103', 'qwen3.5-plus', 'chat', 'text', null, null, 'active', '{"defaultRoles": ["query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000208', '00000000-0000-0000-0000-000000000101', 'gpt-5.4-nano', 'chat', 'multimodal', null, null, 'active', '{"defaultRoles": ["extract_graph", "query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000209', '00000000-0000-0000-0000-000000000101', 'gpt-5.4-pro', 'chat', 'multimodal', null, null, 'active', '{"defaultRoles": ["query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000210', '00000000-0000-0000-0000-000000000101', 'gpt-5.3-chat-latest', 'chat', 'text', null, null, 'active', '{"defaultRoles": ["query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000211', '00000000-0000-0000-0000-000000000101', 'gpt-5.3-codex', 'chat', 'text', null, null, 'active', '{"defaultRoles": ["query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000212', '00000000-0000-0000-0000-000000000101', 'gpt-4.1', 'chat', 'multimodal', null, null, 'active', '{"defaultRoles": ["query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000213', '00000000-0000-0000-0000-000000000101', 'gpt-4.1-mini', 'chat', 'multimodal', null, null, 'active', '{"defaultRoles": ["extract_text", "vision", "query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000214', '00000000-0000-0000-0000-000000000101', 'gpt-4.1-nano', 'chat', 'multimodal', null, null, 'active', '{"defaultRoles": ["query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000215', '00000000-0000-0000-0000-000000000101', 'gpt-4o', 'chat', 'multimodal', null, null, 'active', '{"defaultRoles": ["extract_text", "vision", "query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000216', '00000000-0000-0000-0000-000000000101', 'gpt-4o-mini', 'chat', 'multimodal', null, null, 'active', '{"defaultRoles": ["extract_text", "vision", "query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000217', '00000000-0000-0000-0000-000000000101', 'o1', 'chat', 'text', null, null, 'active', '{"defaultRoles": ["query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000218', '00000000-0000-0000-0000-000000000101', 'o1-mini', 'chat', 'text', null, null, 'active', '{"defaultRoles": ["query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000219', '00000000-0000-0000-0000-000000000101', 'o3', 'chat', 'multimodal', null, null, 'active', '{"defaultRoles": ["query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000220', '00000000-0000-0000-0000-000000000101', 'o3-mini', 'chat', 'text', null, null, 'active', '{"defaultRoles": ["query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000221', '00000000-0000-0000-0000-000000000101', 'o3-pro', 'chat', 'text', null, null, 'active', '{"defaultRoles": ["query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000222', '00000000-0000-0000-0000-000000000101', 'o4-mini', 'chat', 'multimodal', null, null, 'active', '{"defaultRoles": ["extract_text", "vision", "query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000223', '00000000-0000-0000-0000-000000000101', 'text-embedding-3-small', 'embedding', 'text', null, null, 'active', '{"defaultRoles": ["embed_chunk"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000224', '00000000-0000-0000-0000-000000000102', 'deepseek-reasoner', 'chat', 'text', null, null, 'active', '{"defaultRoles": ["extract_graph", "query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000225', '00000000-0000-0000-0000-000000000103', 'qwen3-max-preview', 'chat', 'text', null, null, 'preview', '{"defaultRoles": ["extract_graph", "query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000226', '00000000-0000-0000-0000-000000000103', 'qwen-max', 'chat', 'text', null, null, 'active', '{"defaultRoles": ["query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000227', '00000000-0000-0000-0000-000000000103', 'qwen-max-latest', 'chat', 'text', null, null, 'active', '{"defaultRoles": ["query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000228', '00000000-0000-0000-0000-000000000103', 'qwen-plus', 'chat', 'text', null, null, 'active', '{"defaultRoles": ["extract_graph", "query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000229', '00000000-0000-0000-0000-000000000103', 'qwen-plus-latest', 'chat', 'text', null, null, 'active', '{"defaultRoles": ["extract_graph", "query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000230', '00000000-0000-0000-0000-000000000103', 'qwen-flash', 'chat', 'text', null, null, 'active', '{"defaultRoles": ["extract_graph", "query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000231', '00000000-0000-0000-0000-000000000103', 'qwen-vl-max', 'chat', 'multimodal', null, null, 'active', '{"defaultRoles": ["extract_text", "vision"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000232', '00000000-0000-0000-0000-000000000103', 'qwen-vl-max-latest', 'chat', 'multimodal', null, null, 'active', '{"defaultRoles": ["extract_text", "vision"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000233', '00000000-0000-0000-0000-000000000103', 'qwen-vl-plus', 'chat', 'multimodal', null, null, 'active', '{"defaultRoles": ["extract_text", "vision"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000234', '00000000-0000-0000-0000-000000000103', 'qwen-vl-plus-latest', 'chat', 'multimodal', null, null, 'active', '{"defaultRoles": ["extract_text", "vision"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000235', '00000000-0000-0000-0000-000000000103', 'qwen-vl-ocr', 'chat', 'multimodal', null, null, 'active', '{"defaultRoles": ["extract_text", "vision"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000236', '00000000-0000-0000-0000-000000000103', 'qwen-vl-ocr-latest', 'chat', 'multimodal', null, null, 'active', '{"defaultRoles": ["extract_text", "vision"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000237', '00000000-0000-0000-0000-000000000103', 'text-embedding-v3', 'embedding', 'text', null, null, 'active', '{"defaultRoles": ["embed_chunk"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000238', '00000000-0000-0000-0000-000000000103', 'qwen-turbo', 'chat', 'text', null, null, 'deprecated', '{"defaultRoles": ["query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000239', '00000000-0000-0000-0000-000000000103', 'qwen-turbo-latest', 'chat', 'text', null, null, 'deprecated', '{"defaultRoles": ["query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000240', '00000000-0000-0000-0000-000000000103', 'qwen3.5-flash', 'chat', 'text', null, null, 'active', '{"defaultRoles": ["extract_graph", "query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000241', '00000000-0000-0000-0000-000000000104', 'qwen3:0.6b', 'chat', 'text', null, null, 'active', '{"defaultRoles": ["extract_graph", "query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000242', '00000000-0000-0000-0000-000000000104', 'qwen3-embedding:0.6b', 'embedding', 'text', null, null, 'active', '{"defaultRoles": ["embed_chunk"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000243', '00000000-0000-0000-0000-000000000104', 'qwen3-vl:2b', 'chat', 'multimodal', null, null, 'active', '{"defaultRoles": ["vision"], "seedSource": "provider_catalog"}'::jsonb);

insert into ai_price_catalog (
    id,
    model_catalog_id,
    billing_unit,
    price_variant_key,
    request_input_tokens_min,
    request_input_tokens_max,
    unit_price,
    currency_code,
    effective_from,
    effective_to,
    catalog_scope,
    workspace_id
)
values
    ('00000000-0000-0000-0000-000000000301', '00000000-0000-0000-0000-000000000201', 'per_1m_input_tokens', 'default', null, null, 0.75, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000302', '00000000-0000-0000-0000-000000000201', 'per_1m_output_tokens', 'default', null, null, 4.50, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000303', '00000000-0000-0000-0000-000000000202', 'per_1m_input_tokens', 'default', null, null, 0.13, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000304', '00000000-0000-0000-0000-000000000203', 'per_1m_input_tokens', 'default', null, null, 2.50, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000305', '00000000-0000-0000-0000-000000000203', 'per_1m_output_tokens', 'default', null, null, 15.00, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000313', '00000000-0000-0000-0000-000000000203', 'per_1m_cached_input_tokens', 'default', null, null, 0.25, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000306', '00000000-0000-0000-0000-000000000204', 'per_1m_input_tokens', 'default', null, null, 0.27, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000307', '00000000-0000-0000-0000-000000000204', 'per_1m_output_tokens', 'default', null, null, 1.10, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000354', '00000000-0000-0000-0000-000000000204', 'per_1m_cached_input_tokens', 'default', null, null, 0.07, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000308', '00000000-0000-0000-0000-000000000205', 'per_1m_input_tokens', 'default', 0, 32000, 0.359, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000309', '00000000-0000-0000-0000-000000000205', 'per_1m_output_tokens', 'default', 0, 32000, 1.434, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000310', '00000000-0000-0000-0000-000000000206', 'per_1m_input_tokens', 'default', null, null, 0.07, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000311', '00000000-0000-0000-0000-000000000207', 'per_1m_input_tokens', 'default', 0, 128000, 0.115, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000312', '00000000-0000-0000-0000-000000000207', 'per_1m_output_tokens', 'default', 0, 128000, 0.688, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000314', '00000000-0000-0000-0000-000000000201', 'per_1m_cached_input_tokens', 'default', null, null, 0.075, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000315', '00000000-0000-0000-0000-000000000208', 'per_1m_input_tokens', 'default', null, null, 0.20, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000316', '00000000-0000-0000-0000-000000000208', 'per_1m_cached_input_tokens', 'default', null, null, 0.02, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000317', '00000000-0000-0000-0000-000000000208', 'per_1m_output_tokens', 'default', null, null, 1.25, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000318', '00000000-0000-0000-0000-000000000209', 'per_1m_input_tokens', 'default', null, null, 30.00, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000319', '00000000-0000-0000-0000-000000000209', 'per_1m_output_tokens', 'default', null, null, 180.00, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000320', '00000000-0000-0000-0000-000000000210', 'per_1m_input_tokens', 'default', null, null, 1.75, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000321', '00000000-0000-0000-0000-000000000210', 'per_1m_cached_input_tokens', 'default', null, null, 0.175, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000322', '00000000-0000-0000-0000-000000000210', 'per_1m_output_tokens', 'default', null, null, 14.00, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000323', '00000000-0000-0000-0000-000000000211', 'per_1m_input_tokens', 'default', null, null, 1.75, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000324', '00000000-0000-0000-0000-000000000211', 'per_1m_cached_input_tokens', 'default', null, null, 0.175, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000325', '00000000-0000-0000-0000-000000000211', 'per_1m_output_tokens', 'default', null, null, 14.00, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000326', '00000000-0000-0000-0000-000000000212', 'per_1m_input_tokens', 'default', null, null, 2.00, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000327', '00000000-0000-0000-0000-000000000212', 'per_1m_cached_input_tokens', 'default', null, null, 0.50, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000328', '00000000-0000-0000-0000-000000000212', 'per_1m_output_tokens', 'default', null, null, 8.00, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000329', '00000000-0000-0000-0000-000000000213', 'per_1m_input_tokens', 'default', null, null, 0.40, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000330', '00000000-0000-0000-0000-000000000213', 'per_1m_cached_input_tokens', 'default', null, null, 0.10, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000331', '00000000-0000-0000-0000-000000000213', 'per_1m_output_tokens', 'default', null, null, 1.60, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000332', '00000000-0000-0000-0000-000000000214', 'per_1m_input_tokens', 'default', null, null, 0.10, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000333', '00000000-0000-0000-0000-000000000214', 'per_1m_cached_input_tokens', 'default', null, null, 0.025, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000334', '00000000-0000-0000-0000-000000000214', 'per_1m_output_tokens', 'default', null, null, 0.40, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000335', '00000000-0000-0000-0000-000000000215', 'per_1m_input_tokens', 'default', null, null, 2.50, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000336', '00000000-0000-0000-0000-000000000215', 'per_1m_cached_input_tokens', 'default', null, null, 1.25, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000337', '00000000-0000-0000-0000-000000000215', 'per_1m_output_tokens', 'default', null, null, 10.00, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000338', '00000000-0000-0000-0000-000000000216', 'per_1m_input_tokens', 'default', null, null, 0.15, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000339', '00000000-0000-0000-0000-000000000216', 'per_1m_cached_input_tokens', 'default', null, null, 0.075, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000340', '00000000-0000-0000-0000-000000000216', 'per_1m_output_tokens', 'default', null, null, 0.60, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000341', '00000000-0000-0000-0000-000000000217', 'per_1m_input_tokens', 'default', null, null, 15.00, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000342', '00000000-0000-0000-0000-000000000217', 'per_1m_output_tokens', 'default', null, null, 60.00, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000343', '00000000-0000-0000-0000-000000000218', 'per_1m_input_tokens', 'default', null, null, 1.10, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000344', '00000000-0000-0000-0000-000000000218', 'per_1m_output_tokens', 'default', null, null, 4.40, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000345', '00000000-0000-0000-0000-000000000219', 'per_1m_input_tokens', 'default', null, null, 2.00, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000346', '00000000-0000-0000-0000-000000000219', 'per_1m_output_tokens', 'default', null, null, 8.00, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000347', '00000000-0000-0000-0000-000000000220', 'per_1m_input_tokens', 'default', null, null, 1.10, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000348', '00000000-0000-0000-0000-000000000220', 'per_1m_output_tokens', 'default', null, null, 4.40, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000349', '00000000-0000-0000-0000-000000000221', 'per_1m_input_tokens', 'default', null, null, 20.00, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000350', '00000000-0000-0000-0000-000000000221', 'per_1m_output_tokens', 'default', null, null, 80.00, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000351', '00000000-0000-0000-0000-000000000222', 'per_1m_input_tokens', 'default', null, null, 1.10, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000352', '00000000-0000-0000-0000-000000000222', 'per_1m_output_tokens', 'default', null, null, 4.40, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000353', '00000000-0000-0000-0000-000000000223', 'per_1m_input_tokens', 'default', null, null, 0.02, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000355', '00000000-0000-0000-0000-000000000224', 'per_1m_input_tokens', 'default', null, null, 0.55, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000356', '00000000-0000-0000-0000-000000000224', 'per_1m_cached_input_tokens', 'default', null, null, 0.14, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000357', '00000000-0000-0000-0000-000000000224', 'per_1m_output_tokens', 'default', null, null, 2.19, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000358', '00000000-0000-0000-0000-000000000205', 'per_1m_input_tokens', 'default', 32001, 128000, 0.574, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000359', '00000000-0000-0000-0000-000000000205', 'per_1m_output_tokens', 'default', 32001, 128000, 2.294, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000360', '00000000-0000-0000-0000-000000000205', 'per_1m_input_tokens', 'default', 128001, 256000, 1.004, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000361', '00000000-0000-0000-0000-000000000205', 'per_1m_output_tokens', 'default', 128001, 256000, 4.014, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000362', '00000000-0000-0000-0000-000000000225', 'per_1m_input_tokens', 'default', 0, 32000, 0.861, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000363', '00000000-0000-0000-0000-000000000225', 'per_1m_output_tokens', 'default', 0, 32000, 3.441, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000364', '00000000-0000-0000-0000-000000000225', 'per_1m_input_tokens', 'default', 32001, 128000, 1.434, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000365', '00000000-0000-0000-0000-000000000225', 'per_1m_output_tokens', 'default', 32001, 128000, 5.735, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000366', '00000000-0000-0000-0000-000000000225', 'per_1m_input_tokens', 'default', 128001, 256000, 2.151, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000367', '00000000-0000-0000-0000-000000000225', 'per_1m_output_tokens', 'default', 128001, 256000, 8.602, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000368', '00000000-0000-0000-0000-000000000226', 'per_1m_input_tokens', 'default', null, null, 0.345, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000369', '00000000-0000-0000-0000-000000000226', 'per_1m_output_tokens', 'default', null, null, 1.377, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000370', '00000000-0000-0000-0000-000000000227', 'per_1m_input_tokens', 'default', null, null, 0.345, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000371', '00000000-0000-0000-0000-000000000227', 'per_1m_output_tokens', 'default', null, null, 1.377, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000372', '00000000-0000-0000-0000-000000000207', 'per_1m_input_tokens', 'default', 128001, 256000, 0.287, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000373', '00000000-0000-0000-0000-000000000207', 'per_1m_output_tokens', 'default', 128001, 256000, 1.720, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000374', '00000000-0000-0000-0000-000000000207', 'per_1m_input_tokens', 'default', 256001, 1000000, 0.573, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000375', '00000000-0000-0000-0000-000000000207', 'per_1m_output_tokens', 'default', 256001, 1000000, 3.440, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000376', '00000000-0000-0000-0000-000000000228', 'per_1m_input_tokens', 'default', 0, 128000, 0.115, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000377', '00000000-0000-0000-0000-000000000228', 'per_1m_output_tokens', 'default', 0, 128000, 0.287, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000378', '00000000-0000-0000-0000-000000000228', 'per_1m_output_tokens', 'thinking', 0, 128000, 1.147, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000379', '00000000-0000-0000-0000-000000000228', 'per_1m_input_tokens', 'default', 128001, 256000, 0.345, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000380', '00000000-0000-0000-0000-000000000228', 'per_1m_output_tokens', 'default', 128001, 256000, 2.868, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000381', '00000000-0000-0000-0000-000000000228', 'per_1m_output_tokens', 'thinking', 128001, 256000, 3.441, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000382', '00000000-0000-0000-0000-000000000228', 'per_1m_input_tokens', 'default', 256001, 1000000, 0.689, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000383', '00000000-0000-0000-0000-000000000228', 'per_1m_output_tokens', 'default', 256001, 1000000, 6.881, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000384', '00000000-0000-0000-0000-000000000228', 'per_1m_output_tokens', 'thinking', 256001, 1000000, 9.175, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000385', '00000000-0000-0000-0000-000000000229', 'per_1m_input_tokens', 'default', 0, 128000, 0.115, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000386', '00000000-0000-0000-0000-000000000229', 'per_1m_output_tokens', 'default', 0, 128000, 0.287, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000387', '00000000-0000-0000-0000-000000000229', 'per_1m_output_tokens', 'thinking', 0, 128000, 1.147, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000388', '00000000-0000-0000-0000-000000000229', 'per_1m_input_tokens', 'default', 128001, 256000, 0.345, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000389', '00000000-0000-0000-0000-000000000229', 'per_1m_output_tokens', 'default', 128001, 256000, 2.868, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000390', '00000000-0000-0000-0000-000000000229', 'per_1m_output_tokens', 'thinking', 128001, 256000, 3.441, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000391', '00000000-0000-0000-0000-000000000229', 'per_1m_input_tokens', 'default', 256001, 1000000, 0.689, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000392', '00000000-0000-0000-0000-000000000229', 'per_1m_output_tokens', 'default', 256001, 1000000, 6.881, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000393', '00000000-0000-0000-0000-000000000229', 'per_1m_output_tokens', 'thinking', 256001, 1000000, 9.175, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000394', '00000000-0000-0000-0000-000000000230', 'per_1m_input_tokens', 'default', 0, 256000, 0.05, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000395', '00000000-0000-0000-0000-000000000230', 'per_1m_output_tokens', 'default', 0, 256000, 0.40, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000396', '00000000-0000-0000-0000-000000000230', 'per_1m_input_tokens', 'default', 256001, 1000000, 0.25, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000397', '00000000-0000-0000-0000-000000000230', 'per_1m_output_tokens', 'default', 256001, 1000000, 2.00, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000398', '00000000-0000-0000-0000-000000000231', 'per_1m_input_tokens', 'default', null, null, 0.23, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000399', '00000000-0000-0000-0000-000000000231', 'per_1m_output_tokens', 'default', null, null, 0.574, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000400', '00000000-0000-0000-0000-000000000232', 'per_1m_input_tokens', 'default', null, null, 0.23, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000401', '00000000-0000-0000-0000-000000000232', 'per_1m_output_tokens', 'default', null, null, 0.574, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000402', '00000000-0000-0000-0000-000000000233', 'per_1m_input_tokens', 'default', null, null, 0.115, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000403', '00000000-0000-0000-0000-000000000233', 'per_1m_output_tokens', 'default', null, null, 0.287, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000404', '00000000-0000-0000-0000-000000000234', 'per_1m_input_tokens', 'default', null, null, 0.115, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000405', '00000000-0000-0000-0000-000000000234', 'per_1m_output_tokens', 'default', null, null, 0.287, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000406', '00000000-0000-0000-0000-000000000235', 'per_1m_input_tokens', 'default', null, null, 0.043, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000407', '00000000-0000-0000-0000-000000000235', 'per_1m_output_tokens', 'default', null, null, 0.072, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000408', '00000000-0000-0000-0000-000000000236', 'per_1m_input_tokens', 'default', null, null, 0.043, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000409', '00000000-0000-0000-0000-000000000236', 'per_1m_output_tokens', 'default', null, null, 0.072, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000410', '00000000-0000-0000-0000-000000000237', 'per_1m_input_tokens', 'default', null, null, 0.07, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000411', '00000000-0000-0000-0000-000000000238', 'per_1m_input_tokens', 'default', null, null, 0.044, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000412', '00000000-0000-0000-0000-000000000238', 'per_1m_output_tokens', 'default', null, null, 0.087, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000413', '00000000-0000-0000-0000-000000000239', 'per_1m_input_tokens', 'default', null, null, 0.044, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000414', '00000000-0000-0000-0000-000000000239', 'per_1m_output_tokens', 'default', null, null, 0.087, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000415', '00000000-0000-0000-0000-000000000240', 'per_1m_input_tokens', 'default', 0, 256000, 0.05, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000416', '00000000-0000-0000-0000-000000000240', 'per_1m_output_tokens', 'default', 0, 256000, 0.40, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000417', '00000000-0000-0000-0000-000000000240', 'per_1m_input_tokens', 'default', 256001, 1000000, 0.25, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null),
    ('00000000-0000-0000-0000-000000000418', '00000000-0000-0000-0000-000000000240', 'per_1m_output_tokens', 'default', 256001, 1000000, 2.00, 'USD', timestamptz '2026-04-01 00:00:00+00', null, 'system', null);
