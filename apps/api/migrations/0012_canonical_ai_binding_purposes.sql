-- Collapse obsolete AI binding profiles into the six executable purposes.
-- Canonical rows win when both canonical and obsolete rows exist at one scope.

-- Historical pre-release restores could bypass constraint triggers and leave a
-- library creator id whose principal no longer exists. AI binding mutations
-- advance the affected library generation, which makes PostgreSQL recheck that
-- otherwise dormant FK and abort this migration. Restore the declared
-- `ON DELETE SET NULL` state before touching bindings.
update public.catalog_library library
set created_by_principal_id = null
where library.created_by_principal_id is not null
  and not exists (
      select 1
      from public.iam_principal principal
      where principal.id = library.created_by_principal_id
  );

create temporary table canonical_ai_binding_redirect (
    obsolete_binding_id uuid primary key,
    canonical_binding_id uuid not null
);

insert into canonical_ai_binding_redirect (obsolete_binding_id, canonical_binding_id)
select obsolete.id, canonical.id
from public.ai_binding obsolete
join public.ai_binding canonical
  on canonical.scope_kind = obsolete.scope_kind
 and canonical.workspace_id is not distinct from obsolete.workspace_id
 and canonical.library_id is not distinct from obsolete.library_id
 and canonical.binding_purpose::text = case obsolete.binding_purpose::text
        when 'query_retrieve' then 'embed_chunk'
        when 'rerank' then 'query_compile'
        when 'vision' then 'extract_text'
     end
where obsolete.binding_purpose::text in ('query_retrieve', 'rerank', 'vision')
on conflict (obsolete_binding_id) do nothing;

update public.ai_binding_validation validation
set binding_id = redirect.canonical_binding_id
from canonical_ai_binding_redirect redirect
where validation.binding_id = redirect.obsolete_binding_id;

update public.billing_provider_call call
set binding_id = redirect.canonical_binding_id
from canonical_ai_binding_redirect redirect
where call.binding_id = redirect.obsolete_binding_id;

update public.query_execution execution
set binding_id = redirect.canonical_binding_id
from canonical_ai_binding_redirect redirect
where execution.binding_id = redirect.obsolete_binding_id;

update public.runtime_action_record action
set provider_binding_id = redirect.canonical_binding_id
from canonical_ai_binding_redirect redirect
where action.provider_binding_id = redirect.obsolete_binding_id;

delete from public.ai_binding obsolete
using canonical_ai_binding_redirect redirect
where obsolete.id = redirect.obsolete_binding_id;

drop table canonical_ai_binding_redirect;

-- `utility` never represented an executable runtime profile. There is no
-- canonical model to merge it into, so remove these stale bindings before the
-- enum is rebuilt. Dependent validation rows cascade; nullable runtime and
-- billing references are cleared by their foreign-key contracts.
delete from public.ai_binding
where binding_purpose::text = 'utility';

update public.ai_binding
set binding_purpose = (
    case binding_purpose::text
        when 'query_retrieve' then 'embed_chunk'
        when 'rerank' then 'query_compile'
        when 'vision' then 'extract_text'
    end
)::public.ai_binding_purpose
where binding_purpose::text in ('query_retrieve', 'rerank', 'vision');

-- Billing call kinds describe operations, not binding profiles. Rename the
-- historical retrieval label so new and retained rows use the same explicit
-- query-embedding operation vocabulary.
update public.billing_provider_call
set call_kind = 'query_embedding'
where call_kind = 'query_retrieve';

-- Normalize catalog roles structurally. A multimodal model previously tagged
-- only for `vision`, for example, becomes an `extract_text` model rather than
-- losing its document-understanding capability.
--
-- Some historical catalog rows contain only non-executable roles such as
-- `utility` or private extension labels. Preserve those rows for billing and
-- preset foreign keys, but make them explicitly non-selectable: an empty
-- canonical role array is valid only as the disabled historical state created
-- by this migration. Public create/update APIs still require at least one
-- canonical purpose.
alter table public.ai_model_catalog
    drop constraint if exists ai_model_catalog_default_roles_check;
alter table public.ai_model_catalog
    add constraint ai_model_catalog_default_roles_check check (
        jsonb_typeof(metadata_json -> 'defaultRoles') is not distinct from 'array'
        and (
            jsonb_array_length(metadata_json -> 'defaultRoles') > 0
            or lifecycle_state = 'disabled'::public.ai_model_lifecycle_state
        )
    );

with expanded_roles as (
    select
        model.id,
        role.ordinality,
        case role.value #>> '{}'
            when 'query_retrieve' then to_jsonb('embed_chunk'::text)
            when 'rerank' then to_jsonb('query_compile'::text)
            when 'vision' then to_jsonb('extract_text'::text)
            when 'extract_text' then role.value
            when 'extract_graph' then role.value
            when 'embed_chunk' then role.value
            when 'query_compile' then role.value
            when 'query_answer' then role.value
            when 'agent' then role.value
            else null
        end as normalized_role
    from public.ai_model_catalog model
    cross join lateral jsonb_array_elements(
        case
            when jsonb_typeof(model.metadata_json -> 'defaultRoles') = 'array'
                then model.metadata_json -> 'defaultRoles'
            else '[]'::jsonb
        end
    )
        with ordinality as role(value, ordinality)
), deduplicated_roles as (
    select id, normalized_role, min(ordinality) as first_ordinality
    from expanded_roles
    where normalized_role is not null
    group by id, normalized_role
), assembled_roles as (
    select
        role_models.id,
        coalesce(
            jsonb_agg(roles.normalized_role order by roles.first_ordinality)
                filter (where roles.normalized_role is not null),
            '[]'::jsonb
        ) as roles
    from (select distinct id from expanded_roles) role_models
    left join deduplicated_roles roles on roles.id = role_models.id
    group by role_models.id
)
update public.ai_model_catalog model
set metadata_json = jsonb_set(model.metadata_json, '{defaultRoles}', assembled.roles, false),
    lifecycle_state = case
        when jsonb_array_length(assembled.roles) = 0
            then 'disabled'::public.ai_model_lifecycle_state
        else model.lifecycle_state
    end
from assembled_roles assembled
where model.id = assembled.id;

-- Normalize only fully typed provider bootstrap presets. Canonical entries
-- take precedence over obsolete duplicates for the same resulting purpose,
-- but an invalid canonical entry must never shadow a later valid entry.
with preset_providers as (
    select provider.id
    from public.ai_provider_catalog provider
    where jsonb_typeof(provider.capability_flags_json -> 'bootstrapPresets') = 'array'
), expanded_presets as (
    select
        provider.id,
        preset.value,
        preset.ordinality,
        coalesce(
            jsonb_typeof(preset.value) = 'object'
            and jsonb_typeof(preset.value -> 'purpose') = 'string'
            and preset.value ->> 'purpose' in (
                'query_retrieve',
                'rerank',
                'vision',
                'extract_text',
                'extract_graph',
                'embed_chunk',
                'query_compile',
                'query_answer',
                'agent'
            )
            and jsonb_typeof(preset.value -> 'modelName') = 'string'
            and preset.value ->> 'modelName' ~ '[^[:space:]]'
            and case
                when not (preset.value ? 'temperature') then true
                when jsonb_typeof(preset.value -> 'temperature') = 'null' then true
                when jsonb_typeof(preset.value -> 'temperature') = 'number' then
                    abs((preset.value ->> 'temperature')::numeric) <=
                        1.7976931348623157e308::numeric
                else false
            end
            and case
                when not (preset.value ? 'topP') then true
                when jsonb_typeof(preset.value -> 'topP') = 'null' then true
                when jsonb_typeof(preset.value -> 'topP') = 'number' then
                    abs((preset.value ->> 'topP')::numeric) <=
                        1.7976931348623157e308::numeric
                else false
            end
            and case
                when not (preset.value ? 'maxOutputTokensOverride') then true
                when jsonb_typeof(preset.value -> 'maxOutputTokensOverride') = 'null' then true
                when jsonb_typeof(preset.value -> 'maxOutputTokensOverride') = 'number' then
                    preset.value ->> 'maxOutputTokensOverride' ~ '^-?[0-9]+$'
                    and (preset.value ->> 'maxOutputTokensOverride')::numeric
                        between -2147483648 and 2147483647
                else false
            end
            and case
                when not (preset.value ? 'systemPrompt') then true
                else jsonb_typeof(preset.value -> 'systemPrompt') in ('string', 'null')
            end
            and case
                when not (preset.value ? 'extraParametersJson') then true
                else jsonb_typeof(preset.value -> 'extraParametersJson') = 'object'
            end,
            false
        ) as valid_preset,
        case preset.value ->> 'purpose'
            when 'query_retrieve' then 'embed_chunk'
            when 'rerank' then 'query_compile'
            when 'vision' then 'extract_text'
            else preset.value ->> 'purpose'
        end as normalized_purpose
    from public.ai_provider_catalog provider
    cross join lateral jsonb_array_elements(
        case
            when jsonb_typeof(provider.capability_flags_json -> 'bootstrapPresets') = 'array'
                then provider.capability_flags_json -> 'bootstrapPresets'
            else '[]'::jsonb
        end
    ) with ordinality as preset(value, ordinality)
), valid_presets as (
    select id, value, ordinality, normalized_purpose
    from expanded_presets
    where valid_preset
), ranked_presets as (
    select
        id,
        value,
        ordinality,
        normalized_purpose,
        row_number() over (
            partition by id, normalized_purpose
            order by ((value ->> 'purpose') = normalized_purpose) desc, ordinality
        ) as precedence
    from valid_presets
), assembled_presets as (
    select
        preset_providers.id,
        coalesce(
            jsonb_agg(
                jsonb_set(
                    ranked_presets.value,
                    '{purpose}',
                    to_jsonb(ranked_presets.normalized_purpose),
                    true
                )
                order by ranked_presets.ordinality
            ) filter (where ranked_presets.precedence = 1),
            '[]'::jsonb
        ) as presets
    from preset_providers
    left join ranked_presets on ranked_presets.id = preset_providers.id
    group by preset_providers.id
)
update public.ai_provider_catalog provider
set capability_flags_json = jsonb_set(
    provider.capability_flags_json,
    '{bootstrapPresets}',
    assembled.presets,
    false
)
from assembled_presets assembled
where provider.id = assembled.id;

-- A present bootstrapPresets field is a typed array. Historical malformed
-- scalar/object values carry no executable preset and normalize to empty.
update public.ai_provider_catalog provider
set capability_flags_json = jsonb_set(
    provider.capability_flags_json,
    '{bootstrapPresets}',
    '[]'::jsonb,
    false
)
where provider.capability_flags_json ? 'bootstrapPresets'
  and jsonb_typeof(provider.capability_flags_json -> 'bootstrapPresets') is distinct from 'array';

-- A historical catalog seed stamped Agent onto every chat model. Retire only
-- that provenance-marked inference: operator-created roles, existing Agent
-- bindings, and explicit dedicated Agent presets remain untouched. For a
-- provider without an Agent preset, the canonical query_answer preset target
-- is retained only when the typed profile explicitly supports both chat and
-- tools. Missing or unknown capabilities therefore fail closed.
with legacy_seeded_agent_models as (
    select model.id
    from public.ai_model_catalog model
    join public.ai_provider_catalog provider on provider.id = model.provider_catalog_id
    where model.metadata_json ->> 'seedSource' = 'provider_catalog'
      and jsonb_typeof(model.metadata_json -> 'defaultRoles') = 'array'
      and (model.metadata_json -> 'defaultRoles') @> '["agent"]'::jsonb
      and not exists (
          select 1
          from public.ai_binding binding
          where binding.model_catalog_id = model.id
            and binding.binding_purpose::text = 'agent'
      )
      and not exists (
          select 1
          from jsonb_array_elements(
              case
                  when jsonb_typeof(provider.capability_flags_json -> 'bootstrapPresets') = 'array'
                      then provider.capability_flags_json -> 'bootstrapPresets'
                  else '[]'::jsonb
              end
          ) preset
          where preset ->> 'purpose' = 'agent'
            and preset ->> 'modelName' = model.model_name
      )
      and not coalesce(
          provider.capability_flags_json #>> '{capabilities,chat}' = 'supported'
          and provider.capability_flags_json #>> '{capabilities,tools}' = 'supported'
          and not exists (
              select 1
              from jsonb_array_elements(
                  case
                      when jsonb_typeof(provider.capability_flags_json -> 'bootstrapPresets') = 'array'
                          then provider.capability_flags_json -> 'bootstrapPresets'
                      else '[]'::jsonb
                  end
              ) preset
              where preset ->> 'purpose' = 'agent'
          )
          and exists (
              select 1
              from jsonb_array_elements(
                  case
                      when jsonb_typeof(provider.capability_flags_json -> 'bootstrapPresets') = 'array'
                          then provider.capability_flags_json -> 'bootstrapPresets'
                      else '[]'::jsonb
                  end
              ) preset
              where preset ->> 'purpose' = 'query_answer'
                and preset ->> 'modelName' = model.model_name
          ),
          false
      )
), pruned_agent_roles as (
    select
        model.id,
        coalesce(
            jsonb_agg(role.value order by role.ordinality)
                filter (where role.value #>> '{}' <> 'agent'),
            '[]'::jsonb
        ) as roles
    from public.ai_model_catalog model
    join legacy_seeded_agent_models legacy on legacy.id = model.id
    cross join lateral jsonb_array_elements(model.metadata_json -> 'defaultRoles')
        with ordinality as role(value, ordinality)
    group by model.id
)
update public.ai_model_catalog model
set metadata_json = jsonb_set(model.metadata_json, '{defaultRoles}', pruned.roles, false),
    lifecycle_state = case
        when jsonb_array_length(pruned.roles) = 0
            then 'disabled'::public.ai_model_lifecycle_state
        else model.lifecycle_state
    end
from pruned_agent_roles pruned
where model.id = pruned.id;

-- Materialize Agent eligibility once for the single canonical query_answer
-- preset target only when no dedicated Agent preset exists and the typed
-- provider profile explicitly supports chat plus tools. This is persisted
-- migration state, not a runtime alias: callers still resolve Agent directly.
update public.ai_model_catalog model
set metadata_json = jsonb_set(
    model.metadata_json,
    '{defaultRoles}',
    (model.metadata_json -> 'defaultRoles') || jsonb_build_array('agent'::text),
    false
)
from public.ai_provider_catalog provider
where provider.id = model.provider_catalog_id
  and provider.capability_flags_json #>> '{capabilities,chat}' = 'supported'
  and provider.capability_flags_json #>> '{capabilities,tools}' = 'supported'
  and model.capability_kind = 'chat'::public.ai_model_capability_kind
  and jsonb_typeof(model.metadata_json -> 'defaultRoles') = 'array'
  and (model.metadata_json -> 'defaultRoles') @> '["query_answer"]'::jsonb
  and not ((model.metadata_json -> 'defaultRoles') @> '["agent"]'::jsonb)
  and not exists (
      select 1
      from jsonb_array_elements(provider.capability_flags_json -> 'bootstrapPresets') preset
      where preset ->> 'purpose' = 'agent'
  )
  and exists (
      select 1
      from jsonb_array_elements(provider.capability_flags_json -> 'bootstrapPresets') preset
      where preset ->> 'purpose' = 'query_answer'
        and preset ->> 'modelName' = model.model_name
  );

-- Agent is an independent required runtime purpose. For an upgraded catalog
-- that has no dedicated Agent preset, materialize a separate persisted preset
-- from its valid canonical query_answer preset. Existing Agent presets always
-- win and runtime never substitutes query_answer when Agent is missing.
with materialized_agent_presets as (
    select
        provider.id,
        jsonb_set(
            preset.value,
            '{purpose}',
            to_jsonb('agent'::text),
            false
        ) as preset
    from public.ai_provider_catalog provider
    cross join lateral jsonb_array_elements(
        case
            when jsonb_typeof(provider.capability_flags_json -> 'bootstrapPresets') = 'array'
                then provider.capability_flags_json -> 'bootstrapPresets'
            else '[]'::jsonb
        end
    ) preset(value)
    join public.ai_model_catalog model
      on model.provider_catalog_id = provider.id
     and model.model_name = preset.value ->> 'modelName'
     and model.capability_kind = 'chat'::public.ai_model_capability_kind
     and jsonb_typeof(model.metadata_json -> 'defaultRoles') = 'array'
     and (model.metadata_json -> 'defaultRoles') @> '["agent"]'::jsonb
    where jsonb_typeof(preset.value) = 'object'
      and preset.value ->> 'purpose' = 'query_answer'
      and jsonb_typeof(preset.value -> 'modelName') = 'string'
      and btrim(preset.value ->> 'modelName') <> ''
      and provider.capability_flags_json #>> '{capabilities,chat}' = 'supported'
      and provider.capability_flags_json #>> '{capabilities,tools}' = 'supported'
      and not exists (
          select 1
          from jsonb_array_elements(provider.capability_flags_json -> 'bootstrapPresets') existing
          where existing ->> 'purpose' = 'agent'
      )
)
update public.ai_provider_catalog provider
set capability_flags_json = jsonb_set(
    provider.capability_flags_json,
    '{bootstrapPresets}',
    (provider.capability_flags_json -> 'bootstrapPresets')
        || jsonb_build_array(materialized.preset),
    false
)
from materialized_agent_presets materialized
where provider.id = materialized.id;

-- PostgreSQL enums cannot drop individual values, so rebuild the type after
-- every persisted value and JSON catalog reference has been normalized.
do $rebuild_ai_binding_purpose$
begin
    if exists (
        select 1
        from pg_enum enum_value
        join pg_type enum_type on enum_type.oid = enum_value.enumtypid
        join pg_namespace namespace on namespace.oid = enum_type.typnamespace
        where namespace.nspname = 'public'
          and enum_type.typname = 'ai_binding_purpose'
          and enum_value.enumlabel not in (
              'extract_text',
              'extract_graph',
              'embed_chunk',
              'query_compile',
              'query_answer',
              'agent'
          )
    ) then
        alter type public.ai_binding_purpose rename to ai_binding_purpose_with_obsolete_profiles;
        create type public.ai_binding_purpose as enum (
            'extract_text',
            'extract_graph',
            'embed_chunk',
            'query_compile',
            'query_answer',
            'agent'
        );
        alter table public.ai_binding
            alter column binding_purpose type public.ai_binding_purpose
            using binding_purpose::text::public.ai_binding_purpose;
        drop type public.ai_binding_purpose_with_obsolete_profiles;
    end if;
end
$rebuild_ai_binding_purpose$;

do $assert_canonical_ai_binding_purposes$
begin
    if exists (
        select 1
        from public.ai_binding
        where binding_purpose::text in ('query_retrieve', 'rerank', 'vision', 'utility')
    ) then
        raise exception 'obsolete AI binding rows remain after canonicalization';
    end if;

    if exists (
        select 1
        from public.ai_model_catalog model
        cross join lateral jsonb_array_elements_text(
            case
                when jsonb_typeof(model.metadata_json -> 'defaultRoles') = 'array'
                    then model.metadata_json -> 'defaultRoles'
                else '[]'::jsonb
            end
        ) role
        where coalesce(
            role not in (
                'extract_text',
                'extract_graph',
                'embed_chunk',
                'query_compile',
                'query_answer',
                'agent'
            ),
            true
        )
    ) then
        raise exception 'obsolete AI model roles remain after canonicalization';
    end if;

    if exists (
        select 1
        from public.ai_model_catalog model
        where jsonb_typeof(model.metadata_json -> 'defaultRoles') is distinct from 'array'
           or (
               jsonb_array_length(model.metadata_json -> 'defaultRoles') = 0
               and model.lifecycle_state <> 'disabled'::public.ai_model_lifecycle_state
           )
    ) then
        raise exception 'AI models without canonical roles must be disabled';
    end if;

    if exists (
        select 1
        from public.ai_model_catalog model
        join public.ai_provider_catalog provider on provider.id = model.provider_catalog_id
        where model.metadata_json ->> 'seedSource' = 'provider_catalog'
          and (model.metadata_json -> 'defaultRoles') @> '["agent"]'::jsonb
          and not exists (
              select 1
              from public.ai_binding binding
              where binding.model_catalog_id = model.id
                and binding.binding_purpose::text = 'agent'
          )
          and not exists (
              select 1
              from jsonb_array_elements(
                  case
                      when jsonb_typeof(provider.capability_flags_json -> 'bootstrapPresets') = 'array'
                          then provider.capability_flags_json -> 'bootstrapPresets'
                      else '[]'::jsonb
                  end
              ) preset
              where preset ->> 'purpose' = 'agent'
                and preset ->> 'modelName' = model.model_name
          )
    ) then
        raise exception 'catalog-seeded Agent roles require an explicit Agent preset or binding';
    end if;

    if exists (
        select 1
        from public.ai_provider_catalog provider
        where provider.capability_flags_json ? 'bootstrapPresets'
          and jsonb_typeof(provider.capability_flags_json -> 'bootstrapPresets')
              is distinct from 'array'
    ) or exists (
        select 1
        from public.ai_provider_catalog provider
        cross join lateral jsonb_array_elements(
            case
                when jsonb_typeof(provider.capability_flags_json -> 'bootstrapPresets') = 'array'
                    then provider.capability_flags_json -> 'bootstrapPresets'
                else '[]'::jsonb
            end
        ) preset
        where jsonb_typeof(preset) is distinct from 'object'
           or jsonb_typeof(preset -> 'purpose') is distinct from 'string'
           or preset ->> 'purpose' not in (
               'extract_text',
               'extract_graph',
               'embed_chunk',
               'query_compile',
               'query_answer',
               'agent'
           )
           or jsonb_typeof(preset -> 'modelName') is distinct from 'string'
           or preset ->> 'modelName' !~ '[^[:space:]]'
           or case
               when not (preset ? 'temperature') then false
               when jsonb_typeof(preset -> 'temperature') = 'null' then false
               when jsonb_typeof(preset -> 'temperature') = 'number' then
                   abs((preset ->> 'temperature')::numeric) >
                       1.7976931348623157e308::numeric
               else true
           end
           or case
               when not (preset ? 'topP') then false
               when jsonb_typeof(preset -> 'topP') = 'null' then false
               when jsonb_typeof(preset -> 'topP') = 'number' then
                   abs((preset ->> 'topP')::numeric) > 1.7976931348623157e308::numeric
               else true
           end
           or case
               when not (preset ? 'maxOutputTokensOverride') then false
               when jsonb_typeof(preset -> 'maxOutputTokensOverride') = 'null' then false
               when jsonb_typeof(preset -> 'maxOutputTokensOverride') = 'number' then
                   preset ->> 'maxOutputTokensOverride' !~ '^-?[0-9]+$'
                   or (preset ->> 'maxOutputTokensOverride')::numeric
                       not between -2147483648 and 2147483647
               else true
           end
           or case
               when not (preset ? 'systemPrompt') then false
               else jsonb_typeof(preset -> 'systemPrompt') not in ('string', 'null')
           end
           or case
               when not (preset ? 'extraParametersJson') then false
               else jsonb_typeof(preset -> 'extraParametersJson') is distinct from 'object'
           end
    ) then
        raise exception 'invalid or noncanonical AI bootstrap presets remain after canonicalization';
    end if;

    if exists (
        select 1
        from public.ai_provider_catalog provider
        cross join lateral jsonb_array_elements(
            case
                when jsonb_typeof(provider.capability_flags_json -> 'bootstrapPresets') = 'array'
                    then provider.capability_flags_json -> 'bootstrapPresets'
                else '[]'::jsonb
            end
        ) preset
        group by provider.id, preset ->> 'purpose'
        having count(*) > 1
    ) then
        raise exception 'duplicate AI bootstrap preset purposes remain after canonicalization';
    end if;

    if exists (
        select 1
        from public.billing_provider_call
        where call_kind = 'query_retrieve'
    ) then
        raise exception 'obsolete query embedding billing call kinds remain after canonicalization';
    end if;

    if (
        select array_agg(enum_value.enumlabel::text order by enum_value.enumsortorder)
        from pg_enum enum_value
        join pg_type enum_type on enum_type.oid = enum_value.enumtypid
        join pg_namespace namespace on namespace.oid = enum_type.typnamespace
        where namespace.nspname = 'public'
          and enum_type.typname = 'ai_binding_purpose'
    ) is distinct from array[
        'extract_text',
        'extract_graph',
        'embed_chunk',
        'query_compile',
        'query_answer',
        'agent'
    ] then
        raise exception 'AI binding purpose enum does not match the canonical six-purpose contract';
    end if;
end
$assert_canonical_ai_binding_purposes$;

create index if not exists knowledge_chunk_active_library_rebuild_cursor_index
    on public.knowledge_chunk (library_id, chunk_index, chunk_id)
    where chunk_state = 'ready'
      and raptor_level is null;
