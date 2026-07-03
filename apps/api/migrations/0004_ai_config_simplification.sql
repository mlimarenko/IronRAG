-- 0004_ai_config_simplification.sql
--
-- AI configuration simplification (Variant A, per .omc/autopilot/ai-config-simplification.md).
--
-- Merges `ai_model_preset` params INTO the binding row (params live inline).
-- Renames tables to match operator mental model:
--   ai_provider_credential  → ai_account         (rename; "OpenAI account")
--   ai_binding_assignment    → ai_binding         (rename; short + matches UI)
-- Renames FK column:
--   ai_binding.provider_credential_id → ai_binding.account_id
-- Renames enum type:
--   ai_credential_state → ai_account_state
-- Drops `ai_model_preset` (all bound data denormalized into ai_binding).
--
-- Read-only catalogs are NOT changed: ai_provider_catalog, ai_model_catalog,
-- ai_price_catalog stay as-is (they are seeded reference data).
--
-- The other tables that reference the binding table
--   (ai_binding_validation, billing_provider_call, query_execution,
--    runtime_action_record)
-- have their FKs re-pointed at ai_binding.
--
-- RENAME-FIRST + fully idempotent: the tables/enum are renamed up front with
-- `if exists` guards, then every column/backfill/constraint step operates on
-- the final names behind `if exists` / `if not exists` / existence-guarded DO
-- blocks. Re-running the whole file on an already-migrated database is a clean
-- no-op (per repo policy in ironrag/CLAUDE.md → Migration policy).

-- ============================================================================
-- 1. Rename tables + enum (idempotent: no-op once already renamed).
-- ============================================================================

alter table if exists public.ai_provider_credential rename to ai_account;
alter table if exists public.ai_binding_assignment rename to ai_binding;

do $rename_state_enum$
begin
    if exists (select 1 from pg_type where typname = 'ai_credential_state')
       and not exists (select 1 from pg_type where typname = 'ai_account_state') then
        alter type public.ai_credential_state rename to ai_account_state;
    end if;
end
$rename_state_enum$;

-- Rename ai_account indexes/constraint to shed the old table prefix.
alter index if exists ai_provider_credential_instance_label_key
    rename to ai_account_instance_label_key;
alter index if exists ai_provider_credential_workspace_label_key
    rename to ai_account_workspace_label_key;
alter index if exists ai_provider_credential_library_label_key
    rename to ai_account_library_label_key;
alter index if exists ai_provider_credential_scope_idx
    rename to ai_account_scope_idx;

do $rename_ck_credential$
begin
    if to_regclass('public.ai_account') is not null and exists (
        select 1 from pg_constraint
        where conrelid = 'public.ai_account'::regclass
          and conname = 'ai_provider_credential_scope_check'
    ) then
        alter table public.ai_account
            rename constraint ai_provider_credential_scope_check
                        to ai_account_scope_check;
    end if;
end
$rename_ck_credential$;

-- Rename ai_binding indexes/constraint to shed the old table prefix.
alter index if exists ai_binding_assignment_instance_purpose_key
    rename to ai_binding_instance_purpose_key;
alter index if exists ai_binding_assignment_workspace_purpose_key
    rename to ai_binding_workspace_purpose_key;
alter index if exists ai_binding_assignment_library_purpose_key
    rename to ai_binding_library_purpose_key;
alter index if exists ai_binding_assignment_scope_idx
    rename to ai_binding_scope_idx;
alter index if exists idx_ai_binding_assignment_library_purpose
    rename to idx_ai_binding_library_purpose;

do $rename_ck_binding$
begin
    if to_regclass('public.ai_binding') is not null and exists (
        select 1 from pg_constraint
        where conrelid = 'public.ai_binding'::regclass
          and conname = 'ai_binding_assignment_scope_check'
    ) then
        alter table public.ai_binding
            rename constraint ai_binding_assignment_scope_check
                        to ai_binding_scope_check;
    end if;
end
$rename_ck_binding$;

-- ============================================================================
-- 2. Rename the FK column provider_credential_id → account_id on ai_binding.
-- ============================================================================

do $rename_account_col$
begin
    if exists (
        select 1 from information_schema.columns
        where table_schema = 'public'
          and table_name = 'ai_binding'
          and column_name = 'provider_credential_id'
    ) then
        alter table public.ai_binding rename column provider_credential_id to account_id;
    end if;
end
$rename_account_col$;

-- ============================================================================
-- 3. Add inline preset columns to ai_binding (nullable at first; tightened to
--    NOT NULL after backfill).
-- ============================================================================

alter table if exists public.ai_binding
    add column if not exists model_catalog_id uuid,
    add column if not exists system_prompt text,
    add column if not exists temperature double precision,
    add column if not exists top_p double precision,
    add column if not exists max_output_tokens_override integer,
    add column if not exists extra_parameters_json jsonb;

-- ============================================================================
-- 4. Backfill inline params from ai_model_preset (only while both the source
--    table and the old model_preset_id FK column still exist).
-- ============================================================================

do $backfill$
begin
    if exists (
        select 1 from information_schema.tables
        where table_schema = 'public' and table_name = 'ai_model_preset'
    ) and exists (
        select 1 from information_schema.columns
        where table_schema = 'public'
          and table_name = 'ai_binding'
          and column_name = 'model_preset_id'
    ) then
        update public.ai_binding b
        set model_catalog_id = coalesce(b.model_catalog_id, mp.model_catalog_id),
            system_prompt = coalesce(b.system_prompt, mp.system_prompt),
            temperature = coalesce(b.temperature, mp.temperature),
            top_p = coalesce(b.top_p, mp.top_p),
            max_output_tokens_override = coalesce(
                b.max_output_tokens_override,
                mp.max_output_tokens_override
            ),
            extra_parameters_json = coalesce(
                b.extra_parameters_json,
                mp.extra_parameters_json
            )
        from public.ai_model_preset mp
        where b.model_preset_id = mp.id;
    end if;
end
$backfill$;

-- extra_parameters_json mirrors the preset contract: default '{}', NOT NULL.
alter table if exists public.ai_binding
    alter column extra_parameters_json set default '{}'::jsonb;

update public.ai_binding
set extra_parameters_json = '{}'::jsonb
where extra_parameters_json is null;

alter table if exists public.ai_binding
    alter column extra_parameters_json set not null;

-- model_catalog_id becomes NOT NULL once every binding is backfilled (vacuously
-- true on an empty table, so a fresh install with no bindings still passes).
do $enforce_model$
begin
    if to_regclass('public.ai_binding') is not null
       and not exists (select 1 from public.ai_binding where model_catalog_id is null) then
        alter table public.ai_binding alter column model_catalog_id set not null;
    end if;
end
$enforce_model$;

-- ============================================================================
-- 5. Rewire ai_binding constraints to the final shape.
-- ============================================================================

-- model_catalog_id → ai_model_catalog(id).
alter table if exists public.ai_binding
    drop constraint if exists ai_binding_model_catalog_fkey;
alter table if exists public.ai_binding
    add constraint ai_binding_model_catalog_fkey
        foreign key (model_catalog_id)
        references public.ai_model_catalog(id)
        on delete restrict;

-- account_id → ai_account(id) (replaces the old provider_credential FK).
alter table if exists public.ai_binding
    drop constraint if exists ai_binding_assignment_provider_credential_fkey;
alter table if exists public.ai_binding
    drop constraint if exists ai_binding_account_fkey;
alter table if exists public.ai_binding
    add constraint ai_binding_account_fkey
        foreign key (account_id)
        references public.ai_account(id)
        on delete restrict;

-- ============================================================================
-- 6. Drop model_preset_id column + ai_model_preset table (data now inline).
-- ============================================================================

alter table if exists public.ai_binding
    drop constraint if exists ai_binding_assignment_model_preset_fkey;
alter table if exists public.ai_binding
    drop column if exists model_preset_id;

drop index if exists ai_model_preset_instance_name_key;
drop index if exists ai_model_preset_workspace_name_key;
drop index if exists ai_model_preset_library_name_key;
drop index if exists ai_model_preset_scope_idx;

drop table if exists public.ai_model_preset;

-- ============================================================================
-- 7. Re-point every FK that referenced the binding table at ai_binding(id),
--    recreating them so the constraint names read `_binding_` cleanly.
-- ============================================================================

-- ai_binding_validation.binding_id
alter table if exists public.ai_binding_validation
    drop constraint if exists ai_binding_validation_binding_id_fkey;
alter table if exists public.ai_binding_validation
    add constraint ai_binding_validation_binding_id_fkey
        foreign key (binding_id)
        references public.ai_binding(id)
        on delete cascade;

-- billing_provider_call.binding_id
alter table if exists public.billing_provider_call
    drop constraint if exists billing_provider_call_binding_id_fkey;
alter table if exists public.billing_provider_call
    add constraint billing_provider_call_binding_id_fkey
        foreign key (binding_id)
        references public.ai_binding(id)
        on delete set null;

-- query_execution.binding_id
alter table if exists public.query_execution
    drop constraint if exists query_execution_binding_id_fkey;
alter table if exists public.query_execution
    add constraint query_execution_binding_id_fkey
        foreign key (binding_id)
        references public.ai_binding(id)
        on delete set null;

-- runtime_action_record.provider_binding_id
alter table if exists public.runtime_action_record
    drop constraint if exists runtime_action_record_provider_binding_id_fkey;
alter table if exists public.runtime_action_record
    add constraint runtime_action_record_provider_binding_id_fkey
        foreign key (provider_binding_id)
        references public.ai_binding(id)
        on delete set null;
