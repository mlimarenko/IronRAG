-- 0007: canonical Agent binding wiring + chat-model role normalization
--
-- Migration 0006 added the `agent` value to `ai_binding_purpose` so the
-- in-process MCP-agent loop under the UI assistant could resolve a
-- dedicated binding. The remaining wiring (admin UI surface, bootstrap
-- preset/binding seeding, chat-model role allow-lists) was incomplete:
--
--   * `ai_provider_catalog.capability_flags_json.bootstrapPresets[]`
--     declared no entry for `agent`, so bootstrap could not synthesize
--     a preset for it. Backend now derives the Agent profile from the
--     QueryAnswer profile in code; no JSONB rewrite needed here.
--
--   * `ai_model_catalog.metadata_json.defaultRoles` for chat models was
--     frozen with the older 4-purpose chat list (no `extract_text` for
--     deepseek-* on the Deepseek provider, no `agent` for any chat
--     model). Recompute the array from the canonical text-chat /
--     multimodal-chat capability lists.
--
--   * Existing libraries/workspaces/instance scopes have an active
--     `query_answer` binding but no `agent` binding, which makes the
--     UI-assistant 409 with "no active 'agent' binding configured".
--     Backfill an Agent preset+binding cloned from the active
--     QueryAnswer one. Idempotent on conflict.

-- Recompute defaultRoles for every text-chat model so the canonical
-- chat purposes (extract_text, extract_graph, query_compile,
-- query_answer, agent) become selectable in the admin UI for chat
-- models that were seeded before purposes expanded.
update ai_model_catalog
set metadata_json = jsonb_set(
    metadata_json,
    '{defaultRoles}',
    to_jsonb(array['extract_text','extract_graph','query_compile','query_answer','agent']),
    true
)
where capability_kind = 'chat'
  and modality_kind = 'text';

-- Recompute defaultRoles for multimodal chat models (chat + vision).
update ai_model_catalog
set metadata_json = jsonb_set(
    metadata_json,
    '{defaultRoles}',
    to_jsonb(array['extract_text','extract_graph','query_compile','query_answer','vision','agent']),
    true
)
where capability_kind = 'chat'
  and modality_kind = 'multimodal';

-- Backfill instance-scope Agent presets from existing QueryAnswer
-- presets so canonical bootstrap can wire the binding.
insert into ai_model_preset (
    id,
    workspace_id,
    library_id,
    scope_kind,
    model_catalog_id,
    preset_name,
    system_prompt,
    temperature,
    top_p,
    max_output_tokens_override,
    extra_parameters_json
)
select
    uuidv7(),
    answer_preset.workspace_id,
    answer_preset.library_id,
    answer_preset.scope_kind,
    answer_preset.model_catalog_id,
    -- canonical_runtime_preset_name builds "<provider> Agent · <model>"
    provider.display_name || ' Agent · ' || model.model_name,
    answer_preset.system_prompt,
    answer_preset.temperature,
    answer_preset.top_p,
    answer_preset.max_output_tokens_override,
    answer_preset.extra_parameters_json
from ai_binding_assignment answer_binding
join ai_model_preset answer_preset
    on answer_preset.id = answer_binding.model_preset_id
join ai_model_catalog model
    on model.id = answer_preset.model_catalog_id
join ai_provider_catalog provider
    on provider.id = model.provider_catalog_id
where answer_binding.binding_purpose = 'query_answer'
  and answer_binding.binding_state = 'active'
  and not exists (
    select 1
    from ai_model_preset existing
    where existing.scope_kind = answer_preset.scope_kind
      and existing.workspace_id is not distinct from answer_preset.workspace_id
      and existing.library_id is not distinct from answer_preset.library_id
      and existing.model_catalog_id = answer_preset.model_catalog_id
      and existing.preset_name = provider.display_name || ' Agent · ' || model.model_name
  )
on conflict do nothing;

-- Backfill Agent bindings cloned from active QueryAnswer bindings,
-- pointing at the Agent preset just inserted (or pre-existing).
insert into ai_binding_assignment (
    id,
    workspace_id,
    library_id,
    scope_kind,
    binding_purpose,
    provider_credential_id,
    model_preset_id,
    binding_state
)
select
    uuidv7(),
    answer_binding.workspace_id,
    answer_binding.library_id,
    answer_binding.scope_kind,
    'agent'::ai_binding_purpose,
    answer_binding.provider_credential_id,
    agent_preset.id,
    'active'::ai_binding_state
from ai_binding_assignment answer_binding
join ai_model_preset answer_preset
    on answer_preset.id = answer_binding.model_preset_id
join ai_model_catalog model
    on model.id = answer_preset.model_catalog_id
join ai_provider_catalog provider
    on provider.id = model.provider_catalog_id
join ai_model_preset agent_preset
    on agent_preset.scope_kind = answer_preset.scope_kind
    and agent_preset.workspace_id is not distinct from answer_preset.workspace_id
    and agent_preset.library_id is not distinct from answer_preset.library_id
    and agent_preset.model_catalog_id = answer_preset.model_catalog_id
    and agent_preset.preset_name = provider.display_name || ' Agent · ' || model.model_name
where answer_binding.binding_purpose = 'query_answer'
  and answer_binding.binding_state = 'active'
  and not exists (
    select 1
    from ai_binding_assignment existing
    where existing.binding_purpose = 'agent'
      and existing.scope_kind = answer_binding.scope_kind
      and existing.workspace_id is not distinct from answer_binding.workspace_id
      and existing.library_id is not distinct from answer_binding.library_id
  )
on conflict do nothing;
