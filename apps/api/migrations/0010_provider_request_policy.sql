-- Provider request compatibility is explicit catalog data, never runtime
-- inference from provider/model names. Resolve rows by the catalog natural key
-- `(provider_kind, model_name, capability_kind)`: discovery and historical
-- seeds deliberately preserve an existing row id on conflict.

-- Historical pre-release restores could bypass constraint triggers and leave a
-- library creator id whose principal no longer exists. The catalog updates
-- below advance generations for libraries bound to changed models, which can
-- surface that dormant FK before the later canonical-binding migration runs.
-- Restore the declared `ON DELETE SET NULL` state first.
update public.catalog_library library
set created_by_principal_id = null
where library.created_by_principal_id is not null
  and not exists (
      select 1
      from public.iam_principal principal
      where principal.id = library.created_by_principal_id
  );

with model_request_policies (provider_kind, model_name, request_policy) as (
    values
        -- GPT-5.6 catalog family: provider-default sampling and a documented
        -- tool output ceiling. Tool choice remains required-capable.
        ('openai', 'gpt-5.6-luna', '{"sampling":"omit","toolChoice":"required_capable","defaultToolMaxOutputTokens":65536}'::jsonb),
        ('openai', 'gpt-5.6-sol', '{"sampling":"omit","toolChoice":"required_capable","defaultToolMaxOutputTokens":65536}'::jsonb),
        ('openai', 'gpt-5.6-terra', '{"sampling":"omit","toolChoice":"required_capable","defaultToolMaxOutputTokens":65536}'::jsonb),
        ('gptunnel', 'gpt-5.6-luna', '{"sampling":"omit","toolChoice":"required_capable","defaultToolMaxOutputTokens":65536}'::jsonb),
        ('gptunnel', 'gpt-5.6-sol', '{"sampling":"omit","toolChoice":"required_capable","defaultToolMaxOutputTokens":65536}'::jsonb),
        ('gptunnel', 'gpt-5.6-terra', '{"sampling":"omit","toolChoice":"required_capable","defaultToolMaxOutputTokens":65536}'::jsonb),
        ('openrouter', 'openai/gpt-5.6-luna', '{"sampling":"omit","toolChoice":"required_capable","defaultToolMaxOutputTokens":65536}'::jsonb),
        ('openrouter', 'openai/gpt-5.6-luna-pro', '{"sampling":"omit","toolChoice":"required_capable","defaultToolMaxOutputTokens":65536}'::jsonb),
        ('openrouter', 'openai/gpt-5.6-sol', '{"sampling":"omit","toolChoice":"required_capable","defaultToolMaxOutputTokens":65536}'::jsonb),
        ('openrouter', 'openai/gpt-5.6-sol-pro', '{"sampling":"omit","toolChoice":"required_capable","defaultToolMaxOutputTokens":65536}'::jsonb),
        ('openrouter', 'openai/gpt-5.6-terra', '{"sampling":"omit","toolChoice":"required_capable","defaultToolMaxOutputTokens":65536}'::jsonb),
        ('openrouter', 'openai/gpt-5.6-terra-pro', '{"sampling":"omit","toolChoice":"required_capable","defaultToolMaxOutputTokens":65536}'::jsonb),
        ('routerai', 'openai/gpt-5.6-luna', '{"sampling":"omit","toolChoice":"required_capable","defaultToolMaxOutputTokens":65536}'::jsonb),
        ('routerai', 'openai/gpt-5.6-luna-pro', '{"sampling":"omit","toolChoice":"required_capable","defaultToolMaxOutputTokens":65536}'::jsonb),
        ('routerai', 'openai/gpt-5.6-sol', '{"sampling":"omit","toolChoice":"required_capable","defaultToolMaxOutputTokens":65536}'::jsonb),
        ('routerai', 'openai/gpt-5.6-sol-pro', '{"sampling":"omit","toolChoice":"required_capable","defaultToolMaxOutputTokens":65536}'::jsonb),
        ('routerai', 'openai/gpt-5.6-terra', '{"sampling":"omit","toolChoice":"required_capable","defaultToolMaxOutputTokens":65536}'::jsonb),
        ('routerai', 'openai/gpt-5.6-terra-pro', '{"sampling":"omit","toolChoice":"required_capable","defaultToolMaxOutputTokens":65536}'::jsonb),

        -- OpenAI-hosted GPT-5.5 rows use provider-default sampling and only
        -- automatic tool routing.
        ('openai', 'gpt-5.5', '{"sampling":"omit","toolChoice":"auto_only"}'::jsonb),
        ('openai', 'gpt-5.5-2026-04-23', '{"sampling":"omit","toolChoice":"auto_only"}'::jsonb),
        ('openai', 'gpt-5.5-pro', '{"sampling":"omit","toolChoice":"auto_only"}'::jsonb),
        ('openai', 'gpt-5.5-pro-2026-04-23', '{"sampling":"omit","toolChoice":"auto_only"}'::jsonb),

        -- Provider-verified automatic-only tool routing. Every compatibility
        -- row is enumerated so catalog changes require an explicit review.
        ('deepseek', 'deepseek-reasoner', '{"sampling":"forward","toolChoice":"auto_only"}'::jsonb),
        ('deepseek', 'deepseek-v4-flash', '{"sampling":"forward","toolChoice":"auto_only"}'::jsonb),
        ('deepseek', 'deepseek-v4-pro', '{"sampling":"forward","toolChoice":"auto_only"}'::jsonb),
        ('gptunnel', 'o1', '{"sampling":"forward","toolChoice":"auto_only"}'::jsonb),
        ('gptunnel', 'o1-preview', '{"sampling":"forward","toolChoice":"auto_only"}'::jsonb),
        ('gptunnel', 'o3', '{"sampling":"forward","toolChoice":"auto_only"}'::jsonb),
        ('gptunnel', 'o3-mini', '{"sampling":"forward","toolChoice":"auto_only"}'::jsonb),
        ('gptunnel', 'o4-mini', '{"sampling":"forward","toolChoice":"auto_only"}'::jsonb),
        ('openai', 'o1', '{"sampling":"forward","toolChoice":"auto_only"}'::jsonb),
        ('openai', 'o1-2024-12-17', '{"sampling":"forward","toolChoice":"auto_only"}'::jsonb),
        ('openai', 'o1-mini', '{"sampling":"forward","toolChoice":"auto_only"}'::jsonb),
        ('openai', 'o1-pro', '{"sampling":"forward","toolChoice":"auto_only"}'::jsonb),
        ('openai', 'o1-pro-2025-03-19', '{"sampling":"forward","toolChoice":"auto_only"}'::jsonb),
        ('openai', 'o3', '{"sampling":"forward","toolChoice":"auto_only"}'::jsonb),
        ('openai', 'o3-2025-04-16', '{"sampling":"forward","toolChoice":"auto_only"}'::jsonb),
        ('openai', 'o3-deep-research', '{"sampling":"forward","toolChoice":"auto_only"}'::jsonb),
        ('openai', 'o3-deep-research-2025-06-26', '{"sampling":"forward","toolChoice":"auto_only"}'::jsonb),
        ('openai', 'o3-mini', '{"sampling":"forward","toolChoice":"auto_only"}'::jsonb),
        ('openai', 'o3-mini-2025-01-31', '{"sampling":"forward","toolChoice":"auto_only"}'::jsonb),
        ('openai', 'o3-pro', '{"sampling":"forward","toolChoice":"auto_only"}'::jsonb),
        ('openai', 'o3-pro-2025-06-10', '{"sampling":"forward","toolChoice":"auto_only"}'::jsonb),
        ('openai', 'o4-mini', '{"sampling":"forward","toolChoice":"auto_only"}'::jsonb),
        ('openai', 'o4-mini-2025-04-16', '{"sampling":"forward","toolChoice":"auto_only"}'::jsonb),
        ('openai', 'o4-mini-deep-research', '{"sampling":"forward","toolChoice":"auto_only"}'::jsonb),
        ('openai', 'o4-mini-deep-research-2025-06-26', '{"sampling":"forward","toolChoice":"auto_only"}'::jsonb)
)
update ai_model_catalog model
set metadata_json = jsonb_set(
    model.metadata_json,
    '{requestPolicy}',
    policy.request_policy,
    true
)
from model_request_policies policy
join ai_provider_catalog provider
  on provider.provider_kind = policy.provider_kind
where model.provider_catalog_id = provider.id
  and model.model_name = policy.model_name
  and model.capability_kind = 'chat'::ai_model_capability_kind;

-- Qwen's tool endpoint requires thinking to be disabled for these bootstrap
-- bindings. Persist the protocol option in catalog/binding metadata. Existing
-- operator values remain authoritative because the right-hand JSON object wins.
update ai_provider_catalog provider
set capability_flags_json = jsonb_set(
    provider.capability_flags_json,
    '{bootstrapPresets}',
    (
        select jsonb_agg(
            case
                when preset ->> 'purpose' in ('query_answer', 'agent') then
                    jsonb_set(
                        preset,
                        '{extraParametersJson}',
                        '{"enable_thinking":false}'::jsonb ||
                            case
                                when jsonb_typeof(preset -> 'extraParametersJson') = 'object'
                                    then preset -> 'extraParametersJson'
                                else '{}'::jsonb
                            end,
                        true
                    )
                else preset
            end
            order by ordinal
        )
        from jsonb_array_elements(provider.capability_flags_json -> 'bootstrapPresets')
            with ordinality as presets(preset, ordinal)
    ),
    true
)
where provider.provider_kind = 'qwen'
  and jsonb_typeof(provider.capability_flags_json -> 'bootstrapPresets') = 'array';

update ai_binding binding
set extra_parameters_json = '{"enable_thinking":false}'::jsonb ||
    case
        when jsonb_typeof(binding.extra_parameters_json) = 'object'
            then binding.extra_parameters_json
        else '{}'::jsonb
    end
from ai_account account
join ai_provider_catalog provider on provider.id = account.provider_catalog_id
where binding.account_id = account.id
  and binding.binding_purpose = 'agent'::ai_binding_purpose
  and provider.provider_kind = 'qwen';
