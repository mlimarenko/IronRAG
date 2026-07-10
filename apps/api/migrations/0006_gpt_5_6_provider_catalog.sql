-- GPT-5.6 catalog rows verified against each provider's live model feed.
-- OpenAI and GPTunneL expose Sol, Terra, and Luna. OpenRouter and RouterAI
-- additionally expose provider-side `-pro` aliases.

with gpt_56_models (
    id,
    provider_kind,
    model_name
) as (
    values
        ('aa630221-2195-59f0-ad5d-a597f23336cd'::uuid, 'openai', 'gpt-5.6-luna'),
        ('8ca296a2-eb2f-5e47-a32a-bddbe771c07c'::uuid, 'openai', 'gpt-5.6-sol'),
        ('82d9a255-a383-559e-ab8d-6fc61e19d2e2'::uuid, 'openai', 'gpt-5.6-terra'),
        ('2212d057-e2f0-56fa-a1a8-64355d0f600c'::uuid, 'gptunnel', 'gpt-5.6-luna'),
        ('092ae448-a961-50ef-856b-be7fc6732a7a'::uuid, 'gptunnel', 'gpt-5.6-sol'),
        ('3db9ea8d-0722-5b06-a0d3-de4bf29b1fc5'::uuid, 'gptunnel', 'gpt-5.6-terra'),
        ('0755d00c-3098-57a1-b9df-65053528ede6'::uuid, 'openrouter', 'openai/gpt-5.6-luna'),
        ('6af1ba41-ef39-5287-844e-7ffa5dd6372b'::uuid, 'openrouter', 'openai/gpt-5.6-luna-pro'),
        ('04dd20bf-306a-544b-8faf-e3fb4e08263d'::uuid, 'openrouter', 'openai/gpt-5.6-sol'),
        ('da315e58-fc70-5aa6-9367-608ddede57bd'::uuid, 'openrouter', 'openai/gpt-5.6-sol-pro'),
        ('9be938df-6ca3-5cd8-a222-75c9bf9fab6f'::uuid, 'openrouter', 'openai/gpt-5.6-terra'),
        ('65e8b70a-e955-5d26-922a-abb9e6a942f0'::uuid, 'openrouter', 'openai/gpt-5.6-terra-pro'),
        ('630bc8df-13d0-5e85-9e89-76ea0368ecaf'::uuid, 'routerai', 'openai/gpt-5.6-luna'),
        ('5e87dbbc-b8e3-5d9d-ba59-70c74e361294'::uuid, 'routerai', 'openai/gpt-5.6-luna-pro'),
        ('8520db9c-f12c-597f-b3f7-65dc8bf62405'::uuid, 'routerai', 'openai/gpt-5.6-sol'),
        ('e15aab0e-ad4f-5484-bdcf-39b0092394c0'::uuid, 'routerai', 'openai/gpt-5.6-sol-pro'),
        ('82cdc332-2285-5390-ac0d-dff377358303'::uuid, 'routerai', 'openai/gpt-5.6-terra'),
        ('3283203c-a1ea-5a93-b134-61fe913ed942'::uuid, 'routerai', 'openai/gpt-5.6-terra-pro')
)
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
select
    model.id,
    provider.id,
    model.model_name,
    'chat'::ai_model_capability_kind,
    'multimodal'::ai_model_modality_kind,
    1050000,
    128000,
    'active'::ai_model_lifecycle_state,
    '{"defaultRoles":["extract_text","extract_graph","query_compile","query_answer","vision","agent"],"seedSource":"provider_catalog"}'::jsonb
from gpt_56_models model
join ai_provider_catalog provider on provider.provider_kind = model.provider_kind
on conflict (provider_catalog_id, model_name, capability_kind) do update set
    modality_kind = excluded.modality_kind,
    context_window = excluded.context_window,
    max_output_tokens = excluded.max_output_tokens,
    lifecycle_state = excluded.lifecycle_state,
    metadata_json = excluded.metadata_json;

-- Canonical price catalog uses USD. OpenAI/OpenRouter/RouterAI prices are the
-- providers' published USD rates. GPTunneL publishes RUB rates; these values
-- use the catalog's established 95 RUB/USD normalization.
with gpt_56_prices (
    provider_kind,
    model_name,
    input_price,
    output_price
) as (
    values
        ('openai', 'gpt-5.6-luna', 1.00000000::numeric, 6.00000000::numeric),
        ('openai', 'gpt-5.6-sol', 5.00000000::numeric, 30.00000000::numeric),
        ('openai', 'gpt-5.6-terra', 2.50000000::numeric, 15.00000000::numeric),
        ('gptunnel', 'gpt-5.6-luna', 2.10526316::numeric, 12.63157895::numeric),
        ('gptunnel', 'gpt-5.6-sol', 10.52631579::numeric, 63.15789474::numeric),
        ('gptunnel', 'gpt-5.6-terra', 5.26315789::numeric, 31.57894737::numeric),
        ('openrouter', 'openai/gpt-5.6-luna', 1.00000000::numeric, 6.00000000::numeric),
        ('openrouter', 'openai/gpt-5.6-luna-pro', 1.00000000::numeric, 6.00000000::numeric),
        ('openrouter', 'openai/gpt-5.6-sol', 5.00000000::numeric, 30.00000000::numeric),
        ('openrouter', 'openai/gpt-5.6-sol-pro', 5.00000000::numeric, 30.00000000::numeric),
        ('openrouter', 'openai/gpt-5.6-terra', 2.50000000::numeric, 15.00000000::numeric),
        ('openrouter', 'openai/gpt-5.6-terra-pro', 2.50000000::numeric, 15.00000000::numeric),
        ('routerai', 'openai/gpt-5.6-luna', 1.00000000::numeric, 6.00000000::numeric),
        ('routerai', 'openai/gpt-5.6-luna-pro', 1.00000000::numeric, 6.00000000::numeric),
        ('routerai', 'openai/gpt-5.6-sol', 5.00000000::numeric, 30.00000000::numeric),
        ('routerai', 'openai/gpt-5.6-sol-pro', 5.00000000::numeric, 30.00000000::numeric),
        ('routerai', 'openai/gpt-5.6-terra', 2.50000000::numeric, 15.00000000::numeric),
        ('routerai', 'openai/gpt-5.6-terra-pro', 2.50000000::numeric, 15.00000000::numeric)
), expanded_prices as (
    select
        provider_kind,
        model_name,
        'per_1m_input_tokens'::billing_unit as billing_unit,
        input_price as unit_price
    from gpt_56_prices
    union all
    select
        provider_kind,
        model_name,
        'per_1m_output_tokens'::billing_unit,
        output_price
    from gpt_56_prices
)
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
select
    md5(
        'ironrag:price:' || price.provider_kind || ':' || price.model_name || ':' || price.billing_unit
    )::uuid,
    model.id,
    price.billing_unit,
    'default',
    null,
    null,
    price.unit_price,
    'USD',
    '2026-07-09T00:00:00Z'::timestamptz,
    null,
    'system'::ai_price_catalog_scope,
    null
from expanded_prices price
join ai_provider_catalog provider on provider.provider_kind = price.provider_kind
join ai_model_catalog model
  on model.provider_catalog_id = provider.id
 and model.model_name = price.model_name
 and model.capability_kind = 'chat'
on conflict do nothing;

-- Make the verified family the provider bootstrap default while retaining the
-- existing embedding pair. Graph extraction stays on the smaller 5.4 Nano;
-- Luna handles the other non-agent chat purposes and Sol handles agent tools.
with provider_defaults (
    provider_kind,
    graph_model,
    luna_model,
    sol_model
) as (
    values
        ('openai', 'gpt-5.4-nano', 'gpt-5.6-luna', 'gpt-5.6-sol'),
        ('gptunnel', 'gpt-5.4-nano', 'gpt-5.6-luna', 'gpt-5.6-sol'),
        ('openrouter', 'openai/gpt-5.4-nano', 'openai/gpt-5.6-luna', 'openai/gpt-5.6-sol'),
        ('routerai', 'openai/gpt-5.4-nano', 'openai/gpt-5.6-luna', 'openai/gpt-5.6-sol')
)
update ai_provider_catalog provider
set capability_flags_json = jsonb_set(
    provider.capability_flags_json,
    '{bootstrapPresets}',
    (
        select coalesce(
            jsonb_agg(
                case preset ->> 'purpose'
                    when 'extract_graph' then preset || jsonb_build_object('modelName', defaults.graph_model)
                    when 'extract_text' then preset || jsonb_build_object('modelName', defaults.luna_model)
                    when 'query_compile' then preset || jsonb_build_object('modelName', defaults.luna_model)
                    when 'query_answer' then preset || jsonb_build_object('modelName', defaults.luna_model)
                    when 'vision' then preset || jsonb_build_object('modelName', defaults.luna_model)
                    else preset
                end
                order by ordinal
            ) filter (where preset ->> 'purpose' <> 'agent'),
            '[]'::jsonb
        ) || jsonb_build_array(
            jsonb_build_object(
                'purpose', 'agent',
                'modelName', defaults.sol_model,
                'maxOutputTokensOverride', 65536,
                'extraParametersJson', '{"reasoning_effort":"none"}'::jsonb
            )
        )
        from jsonb_array_elements(provider.capability_flags_json -> 'bootstrapPresets')
            with ordinality as presets(preset, ordinal)
    ),
    true
)
from provider_defaults defaults
where provider.provider_kind = defaults.provider_kind;
