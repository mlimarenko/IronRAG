alter type billing_owning_execution_kind add value if not exists 'graph_extraction_attempt';

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

create type runtime_surface_kind as enum ('rest', 'stream', 'mcp', 'worker', 'internal');

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

create table runtime_execution (
    id uuid primary key default uuidv7(),
    owner_kind runtime_execution_owner_kind not null,
    owner_id uuid not null,
    task_kind runtime_task_kind not null,
    surface_kind runtime_surface_kind not null,
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
    provider_binding_id uuid references ai_library_model_binding(id) on delete set null,
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
    project_id uuid not null,
    document_id uuid not null,
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
    workspace_id uuid not null,
    project_id uuid not null,
    document_id uuid not null,
    revision_id uuid,
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
    project_id uuid primary key,
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
    project_id uuid not null,
    ingestion_run_id uuid,
    revision_id uuid,
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
    project_id uuid not null,
    canonical_key text not null,
    label text not null,
    node_type text not null,
    aliases_json jsonb not null default '[]'::jsonb,
    summary text,
    metadata_json jsonb not null default '{}'::jsonb,
    support_count integer not null default 0,
    projection_version bigint not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (project_id, canonical_key, projection_version)
);

create table runtime_graph_edge (
    id uuid primary key default uuidv7(),
    project_id uuid not null,
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
    unique (project_id, canonical_key, projection_version)
);

create table runtime_graph_evidence (
    id uuid primary key default uuidv7(),
    project_id uuid not null,
    evidence_identity_key text not null,
    target_kind text not null,
    target_id uuid not null,
    document_id uuid,
    revision_id uuid,
    activated_by_attempt_id uuid,
    deactivated_by_mutation_id uuid,
    chunk_id uuid,
    source_file_name text,
    page_ref text,
    evidence_text text not null,
    confidence_score double precision,
    is_active boolean not null default true,
    created_at timestamptz not null default now(),
    unique (project_id, evidence_identity_key)
);

create table runtime_graph_canonical_summary (
    id uuid primary key default uuidv7(),
    workspace_id uuid not null,
    project_id uuid not null,
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
    unique (project_id, target_kind, target_id, source_truth_version)
);

create table runtime_vector_target (
    id uuid primary key default uuidv7(),
    project_id uuid not null,
    target_kind text not null,
    target_id uuid not null,
    provider_kind text not null,
    model_name text not null,
    dimensions integer,
    embedding_json jsonb not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (project_id, target_kind, target_id, provider_kind, model_name)
);

create table runtime_provider_profile (
    project_id uuid primary key,
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
    project_id uuid,
    provider_kind text not null,
    model_name text not null,
    capability text not null,
    status text not null,
    error_message text,
    created_at timestamptz not null default now()
);

alter table query_execution
    add column runtime_execution_id uuid references runtime_execution(id) on delete restrict;

alter table billing_provider_call
    add column runtime_execution_id uuid references runtime_execution(id) on delete set null,
    add column runtime_task_kind runtime_task_kind,
    add constraint chk_billing_provider_call_runtime_attribution
        check (
            (runtime_execution_id is null and runtime_task_kind is null)
            or (runtime_execution_id is not null and runtime_task_kind is not null)
        );

create unique index idx_query_execution_runtime_execution_id
    on query_execution (runtime_execution_id)
    where runtime_execution_id is not null;

create unique index idx_runtime_graph_extraction_runtime_execution_id
    on runtime_graph_extraction (runtime_execution_id);

create index idx_runtime_graph_extraction_recovery_attempt_runtime_execution_id
    on runtime_graph_extraction_recovery_attempt (runtime_execution_id, started_at asc);

create index idx_runtime_graph_filtered_artifact_project_created_at
    on runtime_graph_filtered_artifact (project_id, created_at desc);

create index idx_runtime_graph_node_project_projection
    on runtime_graph_node (project_id, projection_version, created_at asc);

create index idx_runtime_graph_edge_project_projection
    on runtime_graph_edge (project_id, projection_version, created_at asc);

create index idx_runtime_graph_edge_project_projection_nodes
    on runtime_graph_edge (project_id, projection_version, from_node_id, to_node_id);

create index idx_runtime_graph_evidence_project_target_active
    on runtime_graph_evidence (project_id, target_kind, target_id, is_active, created_at desc);

create index idx_runtime_graph_evidence_project_document_active
    on runtime_graph_evidence (project_id, document_id, is_active, created_at desc);

create index idx_runtime_graph_evidence_project_document_revision_active
    on runtime_graph_evidence (project_id, document_id, revision_id, is_active, created_at desc);

create index idx_runtime_graph_canonical_summary_project_active
    on runtime_graph_canonical_summary (project_id, generated_at desc)
    where superseded_at is null;

create index idx_runtime_graph_canonical_summary_target_active
    on runtime_graph_canonical_summary (project_id, target_kind, target_id, generated_at desc)
    where superseded_at is null;

create index idx_runtime_vector_target_project_kind_provider
    on runtime_vector_target (project_id, target_kind, provider_kind, model_name, updated_at desc);

create index idx_runtime_provider_validation_log_project_created_at
    on runtime_provider_validation_log (project_id, created_at desc);

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
