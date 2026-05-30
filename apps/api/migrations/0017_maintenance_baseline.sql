-- Canonical baseline for the scheduled maintenance pipeline.
-- One file accumulates schema additions until the next public release
-- (per the one-migration-per-release policy). Every statement is idempotent
-- so partial re-application after a crash or operator-side checksum reset
-- converges on the same final shape.

-- ============================================================================
-- maintenance_job_run — durable lease per (class, scope)
-- ============================================================================

do $$
begin
    if not exists (select 1 from pg_type where typname = 'maintenance_run_state') then
        create type maintenance_run_state as enum (
            'pending',
            'leased',
            'completed',
            'failed',
            'dead_letter'
        );
    end if;
end$$;

-- One row per (class, scope) carries the full lifecycle for that maintenance unit.
-- The scheduler picks rows whose state='pending' AND next_due_at <= now(),
-- atomically flips state to 'leased', and runs the sweeper.
-- Heartbeat is refreshed inside the run; a separate reaper returns rows
-- whose heartbeat went stale back to 'pending'.
-- Run history (per-run metrics, durations) lives in Prometheus/Loki — this
-- table tracks current state, not history, on purpose.
create table if not exists maintenance_job_run (
    id                    uuid primary key default uuidv7(),
    class                 text not null,
    scope_kind            text not null check (scope_kind in ('instance', 'library')),
    scope_id              uuid,
    owner_node            text,
    state                 maintenance_run_state not null default 'pending',
    cursor_json           jsonb not null default '{}'::jsonb,
    attempts              integer not null default 0,
    last_started_at       timestamptz,
    heartbeat_at          timestamptz,
    last_completed_at     timestamptz,
    rows_removed_total    bigint not null default 0,
    bytes_reclaimed_total bigint not null default 0,
    error_code            text,
    error_text            text,
    next_due_at           timestamptz not null default now(),
    created_at            timestamptz not null default now(),
    updated_at            timestamptz not null default now(),
    check (
        (scope_kind = 'instance' and scope_id is null)
        or (scope_kind = 'library' and scope_id is not null)
    )
);

-- Exactly one row per (class, scope). scope_id null sentinel keeps the unique
-- index usable for both 'instance' (null) and 'library' (uuid) scopes.
create unique index if not exists maintenance_job_run_scope_key
    on maintenance_job_run (
        class,
        scope_kind,
        coalesce(scope_id, '00000000-0000-0000-0000-000000000000'::uuid)
    );

-- Hot path: scheduler tick looks for next-due pending row in a class.
create index if not exists maintenance_job_run_due_idx
    on maintenance_job_run (class, next_due_at)
    where state = 'pending';

-- Reaper looks for leased rows with stale heartbeat.
create index if not exists maintenance_job_run_heartbeat_idx
    on maintenance_job_run (heartbeat_at)
    where state = 'leased';

-- Dead-letter dashboard query.
create index if not exists maintenance_job_run_dead_letter_idx
    on maintenance_job_run (class, updated_at)
    where state = 'dead_letter';

-- ============================================================================
-- content_document_head — null-head recovery retry state
-- ============================================================================
-- Recovery for null-head documents (failed ingest) is bounded:
-- the ingest worker increments recovery_attempts_count on each retry,
-- stamps last_recovery_error_code so we can rate-limit "same error 24 h",
-- and sets dead_letter_at once max_attempts is exhausted — at which point
-- the doc is excluded from automatic recovery and only an operator action
-- can clear the dead-letter mark.
-- These live on content_document_head (canonical doc state) rather than
-- being derived from ingest_attempt history because the attempts table is
-- itself a retention candidate and counting attempts via JOIN on a
-- retention-managed table is brittle.

alter table content_document_head
    add column if not exists recovery_attempts_count integer not null default 0;

alter table content_document_head
    add column if not exists last_recovery_error_code text;

alter table content_document_head
    add column if not exists last_recovery_attempt_at timestamptz;

alter table content_document_head
    add column if not exists dead_letter_at timestamptz;

-- Partial index for the ingest worker recovery scan:
-- null-head docs that are not yet dead-lettered, ordered by oldest attempt
-- (or never attempted) so we always make progress.
create index if not exists idx_content_document_head_recovery_candidates
    on content_document_head (last_recovery_attempt_at nulls first)
    where readable_revision_id is null
      and active_revision_id is null
      and dead_letter_at is null;

-- ============================================================================
-- Retention indexes — recorded_at / completed_at on history tables
-- ============================================================================
-- The `retention.stage-events` and `retention.attempts` sweepers issue
-- batched DELETEs filtered by the table's timestamp column. Without the
-- index below the predicate would compile to a sequential scan, taking
-- an AccessExclusiveLock for the duration on a 2 M+ row table and
-- blocking ingest writes. Pre-create the index here so the retention
-- sweepers can rely on it.

create index if not exists idx_ingest_stage_event_recorded_at
    on ingest_stage_event (recorded_at);

-- ============================================================================
-- AI runtime presets — explicit output budgets for chat/tool calls
-- ============================================================================
-- Chat-compatible routers can preflight credit usage against the model's full
-- default output window when max_tokens is omitted. Runtime presets therefore
-- carry explicit product-level output budgets. Operators can still override
-- these per preset; existing explicit values are preserved.

with purpose_output_caps(purpose, max_tokens) as (
    values
        ('extract_text', 2048),
        ('extract_graph', 3072),
        ('query_compile', 512),
        ('query_answer', 1024),
        ('agent', 1024),
        ('vision', 2048)
),
updated_provider_flags as (
    select
        provider.id,
        jsonb_set(
            provider.capability_flags_json,
            '{bootstrapPresets}',
            (
                select jsonb_agg(
                    case
                        when purpose_output_caps.max_tokens is not null
                             and not (preset.value ? 'maxOutputTokensOverride')
                            then preset.value || jsonb_build_object(
                                'maxOutputTokensOverride',
                                purpose_output_caps.max_tokens
                            )
                        else preset.value
                    end
                    order by preset.ordinality
                )
                from jsonb_array_elements(provider.capability_flags_json -> 'bootstrapPresets')
                    with ordinality as preset(value, ordinality)
                left join purpose_output_caps
                    on purpose_output_caps.purpose = preset.value ->> 'purpose'
            ),
            true
        ) as capability_flags_json
    from ai_provider_catalog provider
    where jsonb_typeof(provider.capability_flags_json -> 'bootstrapPresets') = 'array'
)
update ai_provider_catalog provider
set capability_flags_json = updated_provider_flags.capability_flags_json
from updated_provider_flags
where provider.id = updated_provider_flags.id
  and provider.capability_flags_json is distinct from updated_provider_flags.capability_flags_json;

with purpose_output_caps(purpose, max_tokens) as (
    values
        ('extract_text'::ai_binding_purpose, 2048),
        ('extract_graph'::ai_binding_purpose, 3072),
        ('query_compile'::ai_binding_purpose, 512),
        ('query_answer'::ai_binding_purpose, 1024),
        ('agent'::ai_binding_purpose, 1024),
        ('vision'::ai_binding_purpose, 2048)
),
preset_caps as (
    select
        binding.model_preset_id,
        max(purpose_output_caps.max_tokens) as max_tokens
    from ai_binding_assignment binding
    join purpose_output_caps
        on purpose_output_caps.purpose = binding.binding_purpose
    group by binding.model_preset_id
)
update ai_model_preset preset
set max_output_tokens_override = preset_caps.max_tokens
from preset_caps
where preset.id = preset_caps.model_preset_id
  and preset.max_output_tokens_override is null;

-- ============================================================================
-- runtime_graph_evidence — GIN expression index on tokenized evidence_text
-- ============================================================================
--
-- The grounded_answer pipeline issues `to_tsvector('simple', evidence_text)
-- @@ to_tsquery('simple', ?)` against this table on every retrieve pass to
-- pull graph-evidence chunks that mention the query terms. Without an
-- expression index on the tsvector projection, Postgres recomputes
-- to_tsvector for every row → sequential scan over the evidence table.
-- On a populated production library this lane alone burned 4-7 s per
-- retrieve pass (observed `text_search_elapsed_ms=4519` in the
-- `retrieval.graph_evidence_breakdown` stage), and the UI agent loop
-- can fan out up to 8 such queries per turn.
--
-- A GIN index on the same expression turns the scan into an index probe.
-- The expression must match the call site byte-for-byte (analyzer
-- 'simple', same column, no extra casts) or Postgres will fall back
-- to seq scan.
create index if not exists runtime_graph_evidence_text_gin
    on runtime_graph_evidence
    using gin (to_tsvector('simple'::regconfig, evidence_text));
