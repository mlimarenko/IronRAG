alter table runtime_attempt_stage_accounting
    add column if not exists accounting_scope text not null default 'stage_rollup',
    add column if not exists call_sequence_no integer not null default 0;

alter table runtime_attempt_stage_accounting
    drop constraint if exists runtime_attempt_stage_accounting_stage_event_id_key;

create unique index if not exists idx_runtime_attempt_stage_accounting_event_scope_seq
    on runtime_attempt_stage_accounting(stage_event_id, accounting_scope, call_sequence_no);

create index if not exists idx_runtime_attempt_stage_accounting_project_scope
    on runtime_attempt_stage_accounting(project_id, accounting_scope, created_at desc);

alter table runtime_attempt_cost_summary
    add column if not exists settled_estimated_cost numeric(20,8),
    add column if not exists in_flight_estimated_cost numeric(20,8),
    add column if not exists in_flight_stage_count integer not null default 0,
    add column if not exists missing_stage_count integer not null default 0;
