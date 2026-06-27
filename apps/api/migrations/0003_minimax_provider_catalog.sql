-- MiniMax provider catalog seed.
--
-- Sources verified 2026-06-26:
-- - https://platform.minimax.io/docs/api-reference/text-openai-api
-- - https://platform.minimax.io/docs/api-reference/models/openai/list-models
-- - https://platform.minimax.io/docs/guides/quickstart-preparation
-- - https://platform.minimax.io/docs/api-reference/pricing
--
-- MiniMax pay-as-you-go API keys and Token Plan subscription keys are both
-- bearer credentials for the OpenAI-compatible base URL below. The current
-- billing_unit enum supports input, output, and prompt-cache read pricing;
-- MiniMax prompt-cache write pricing is intentionally not inserted until the
-- billing model has a distinct write unit.

with provider_profiles (
    id,
    provider_kind,
    display_name,
    api_style,
    lifecycle_state,
    default_base_url,
    capability_flags_json
) as (
    values
    (
        '00000000-0000-0000-0000-000000000108'::uuid,
        'minimax',
        'MiniMax',
        'openai_compatible'::ai_provider_api_style,
        'active'::ai_provider_lifecycle_state,
        'https://api.minimax.io/v1',
        $json${
            "runtime": {"kind":"openai_compatible","authScheme":"bearer","tokenLimitParameter":"max_completion_tokens","structuredOutput":"prompt_only_json_object","chatPath":"/chat/completions","embeddingsPath":null,"modelsPath":"/models"},
            "credentials": {"apiKeyRequired":true,"baseUrlRequired":false,"baseUrlMode":"fixed","validationMode":"model_list"},
            "baseUrl": {"allowOverride":false,"requireHttps":true,"allowPrivateNetwork":false,"trimSuffixes":[]},
            "modelDiscovery": {"mode":"credential","paths":[{"capabilityKind":"chat","path":"/models"}]},
            "capabilities": {"chat":"supported","embeddings":"unsupported","vision":"supported","streaming":"supported","tools":"supported","modelDiscovery":"supported"},
            "bootstrapPresets": [
                {"purpose":"extract_text","modelName":"MiniMax-M3","extraParametersJson":{"thinking":{"type":"disabled"}}},
                {"purpose":"extract_graph","modelName":"MiniMax-M3","extraParametersJson":{"thinking":{"type":"disabled"}}},
                {"purpose":"query_compile","modelName":"MiniMax-M3","extraParametersJson":{"thinking":{"type":"disabled"}}},
                {"purpose":"query_answer","modelName":"MiniMax-M3","extraParametersJson":{"thinking":{"type":"disabled"}}},
                {"purpose":"vision","modelName":"MiniMax-M3","extraParametersJson":{"thinking":{"type":"disabled"}}}
            ],
            "uiHints": {"credentialFields":["api_key"],"baseUrlEditable":false}
        }$json$::jsonb
    )
)
insert into ai_provider_catalog (
    id,
    provider_kind,
    display_name,
    api_style,
    lifecycle_state,
    default_base_url,
    capability_flags_json
)
select
    id,
    provider_kind,
    display_name,
    api_style,
    lifecycle_state,
    default_base_url,
    capability_flags_json
from provider_profiles
on conflict (provider_kind) do update set
    display_name          = excluded.display_name,
    api_style             = excluded.api_style,
    lifecycle_state       = excluded.lifecycle_state,
    default_base_url      = excluded.default_base_url,
    capability_flags_json = excluded.capability_flags_json;

with model_seed (
    id,
    provider_kind,
    model_name,
    modality_kind,
    context_window,
    default_roles
) as (
    values
        ('1f24d331-23b5-5d9a-b6fc-b9ee915708e0'::uuid, 'minimax', 'MiniMax-M3', 'multimodal'::ai_model_modality_kind, 1000000, array['extract_text','extract_graph','query_compile','query_answer','vision','agent']),
        ('df576de2-41b8-5b67-92d2-30f18917aa2f'::uuid, 'minimax', 'MiniMax-M2.7', 'text'::ai_model_modality_kind, 204800, array['query_answer']),
        ('07dff77e-6115-5906-bafa-73d837acf9b7'::uuid, 'minimax', 'MiniMax-M2.7-highspeed', 'text'::ai_model_modality_kind, 204800, array['query_answer']),
        ('578f9396-4b2a-5100-94de-3ca1dd6b31c7'::uuid, 'minimax', 'MiniMax-M2.5', 'text'::ai_model_modality_kind, 204800, array['query_answer']),
        ('54af7b6d-385a-5570-a2d0-e085dd211dc6'::uuid, 'minimax', 'MiniMax-M2.5-highspeed', 'text'::ai_model_modality_kind, 204800, array['query_answer']),
        ('34ebc273-7d93-53b5-b3c3-29d3fb39130a'::uuid, 'minimax', 'MiniMax-M2.1', 'text'::ai_model_modality_kind, 204800, array['query_answer']),
        ('eeb27df1-9793-53ff-89ab-98e9cb27314b'::uuid, 'minimax', 'MiniMax-M2.1-highspeed', 'text'::ai_model_modality_kind, 204800, array['query_answer']),
        ('31d9841e-4d0e-5bae-a16a-9fc854cc1bc1'::uuid, 'minimax', 'MiniMax-M2', 'text'::ai_model_modality_kind, 204800, array['query_answer'])
),
catalog_seed as (
    select
        model_seed.id,
        provider.id as provider_catalog_id,
        model_seed.model_name,
        model_seed.modality_kind,
        model_seed.context_window,
        jsonb_build_object(
            'defaultRoles', to_jsonb(model_seed.default_roles),
            'seedSource', 'provider_catalog'
        ) as metadata_json
    from model_seed
    join ai_provider_catalog provider on provider.provider_kind = model_seed.provider_kind
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
    id,
    provider_catalog_id,
    model_name,
    'chat'::ai_model_capability_kind,
    modality_kind,
    context_window,
    null,
    'active'::ai_model_lifecycle_state,
    metadata_json
from catalog_seed
on conflict (provider_catalog_id, model_name, capability_kind) do update set
    modality_kind    = excluded.modality_kind,
    context_window   = excluded.context_window,
    max_output_tokens = excluded.max_output_tokens,
    lifecycle_state  = excluded.lifecycle_state,
    metadata_json    = excluded.metadata_json;

with role_titles(role_name, role_title) as (
    values
        ('extract_text', 'Extract Text'),
        ('extract_graph', 'Extract Graph'),
        ('query_compile', 'Query Compile'),
        ('query_answer', 'Query Answer'),
        ('vision', 'Vision')
),
eligible_models as (
    select
        provider.display_name as provider_display_name,
        model.id as model_catalog_id,
        model.model_name,
        jsonb_array_elements_text(model.metadata_json -> 'defaultRoles') as role_name
    from ai_model_catalog model
    join ai_provider_catalog provider on provider.id = model.provider_catalog_id
    where provider.provider_kind = 'minimax'
      and model.lifecycle_state = 'active'
),
preset_seed as (
    select
        eligible.model_catalog_id,
        eligible.provider_display_name || ' ' || role_titles.role_title || ' · ' || eligible.model_name as preset_name,
        case when eligible.model_name = 'MiniMax-M3'
            then '{"thinking":{"type":"disabled"}}'::jsonb
            else '{}'::jsonb
        end as extra_parameters_json
    from eligible_models eligible
    join role_titles on role_titles.role_name = eligible.role_name
)
insert into ai_model_preset (
    scope_kind,
    workspace_id,
    library_id,
    model_catalog_id,
    preset_name,
    temperature,
    top_p,
    extra_parameters_json
)
select
    'instance'::ai_scope_kind,
    null,
    null,
    model_catalog_id,
    preset_name,
    0.3,
    0.9,
    extra_parameters_json
from preset_seed
on conflict do nothing;

with price_seed (
    price_id,
    provider_kind,
    model_name,
    billing_unit,
    price_variant_key,
    request_input_tokens_min,
    request_input_tokens_max,
    unit_price,
    currency_code
) as (
    values
        ('f879c991-9cdd-5d5a-b58e-7de44a0e6d8c'::uuid, 'minimax', 'MiniMax-M3', 'per_1m_input_tokens'::billing_unit, 'default', null::integer, 512000::integer, 0.30::numeric, 'USD'),
        ('2d4b2138-6a5d-53f5-a9dd-a756dad596c0'::uuid, 'minimax', 'MiniMax-M3', 'per_1m_input_tokens'::billing_unit, 'default', 512001::integer, null::integer, 0.60::numeric, 'USD'),
        ('0881c06a-2f09-570f-b2c9-b4a250736f95'::uuid, 'minimax', 'MiniMax-M3', 'per_1m_output_tokens'::billing_unit, 'default', null::integer, 512000::integer, 1.20::numeric, 'USD'),
        ('b2116642-0d55-519d-9f09-494ba6e3f016'::uuid, 'minimax', 'MiniMax-M3', 'per_1m_output_tokens'::billing_unit, 'default', 512001::integer, null::integer, 2.40::numeric, 'USD'),
        ('d3d39b79-f54e-53f5-9a2c-403e158db4f5'::uuid, 'minimax', 'MiniMax-M3', 'per_1m_cached_input_tokens'::billing_unit, 'default', null::integer, 512000::integer, 0.06::numeric, 'USD'),
        ('ed414558-aa0f-5350-97f6-090478a31f85'::uuid, 'minimax', 'MiniMax-M3', 'per_1m_cached_input_tokens'::billing_unit, 'default', 512001::integer, null::integer, 0.12::numeric, 'USD'),
        ('34ff9244-30a2-50fb-8ad3-98e30029d5fe'::uuid, 'minimax', 'MiniMax-M2.7', 'per_1m_input_tokens'::billing_unit, 'default', null::integer, null::integer, 0.30::numeric, 'USD'),
        ('907838c6-cb85-5170-b93d-b949235955eb'::uuid, 'minimax', 'MiniMax-M2.7', 'per_1m_output_tokens'::billing_unit, 'default', null::integer, null::integer, 1.20::numeric, 'USD'),
        ('9e4ae521-5fe5-569f-814e-64e133fc7439'::uuid, 'minimax', 'MiniMax-M2.7', 'per_1m_cached_input_tokens'::billing_unit, 'default', null::integer, null::integer, 0.06::numeric, 'USD'),
        ('786f4efa-0fc6-59b2-83a7-b0ae07360296'::uuid, 'minimax', 'MiniMax-M2.7-highspeed', 'per_1m_input_tokens'::billing_unit, 'default', null::integer, null::integer, 0.60::numeric, 'USD'),
        ('b8466150-3543-58a3-a939-e69baf0f1850'::uuid, 'minimax', 'MiniMax-M2.7-highspeed', 'per_1m_output_tokens'::billing_unit, 'default', null::integer, null::integer, 2.40::numeric, 'USD'),
        ('f8e48e83-7893-5b6d-9144-511b630d6a80'::uuid, 'minimax', 'MiniMax-M2.7-highspeed', 'per_1m_cached_input_tokens'::billing_unit, 'default', null::integer, null::integer, 0.06::numeric, 'USD'),
        ('c6e74db1-c529-5c65-815b-fb7f9ad49703'::uuid, 'minimax', 'MiniMax-M2.5', 'per_1m_input_tokens'::billing_unit, 'default', null::integer, null::integer, 0.30::numeric, 'USD'),
        ('d9b9aedf-adeb-5ab2-a4b2-682e40720338'::uuid, 'minimax', 'MiniMax-M2.5', 'per_1m_output_tokens'::billing_unit, 'default', null::integer, null::integer, 1.20::numeric, 'USD'),
        ('27ec3976-09c1-589d-a2d3-a2b0fd231d28'::uuid, 'minimax', 'MiniMax-M2.5', 'per_1m_cached_input_tokens'::billing_unit, 'default', null::integer, null::integer, 0.03::numeric, 'USD'),
        ('d74a197a-f1dc-58e1-929b-42452f6e18fc'::uuid, 'minimax', 'MiniMax-M2.5-highspeed', 'per_1m_input_tokens'::billing_unit, 'default', null::integer, null::integer, 0.60::numeric, 'USD'),
        ('8f12fbf0-29c5-50f0-bcec-8344f14210b8'::uuid, 'minimax', 'MiniMax-M2.5-highspeed', 'per_1m_output_tokens'::billing_unit, 'default', null::integer, null::integer, 2.40::numeric, 'USD'),
        ('a74a7f0e-5582-53d7-8b92-479202a9c219'::uuid, 'minimax', 'MiniMax-M2.5-highspeed', 'per_1m_cached_input_tokens'::billing_unit, 'default', null::integer, null::integer, 0.03::numeric, 'USD'),
        ('061b6f53-f472-59d4-b198-a126865c3123'::uuid, 'minimax', 'MiniMax-M2.1', 'per_1m_input_tokens'::billing_unit, 'default', null::integer, null::integer, 0.30::numeric, 'USD'),
        ('587762c6-2ff8-557d-a6fc-ee087d029861'::uuid, 'minimax', 'MiniMax-M2.1', 'per_1m_output_tokens'::billing_unit, 'default', null::integer, null::integer, 1.20::numeric, 'USD'),
        ('190bb7b6-72c7-5fa8-a593-aca58f7521f4'::uuid, 'minimax', 'MiniMax-M2.1', 'per_1m_cached_input_tokens'::billing_unit, 'default', null::integer, null::integer, 0.03::numeric, 'USD'),
        ('53cee408-273b-58e4-be22-07782f43dd38'::uuid, 'minimax', 'MiniMax-M2.1-highspeed', 'per_1m_input_tokens'::billing_unit, 'default', null::integer, null::integer, 0.60::numeric, 'USD'),
        ('f449b3df-8fbb-50ef-9b56-66961920c788'::uuid, 'minimax', 'MiniMax-M2.1-highspeed', 'per_1m_output_tokens'::billing_unit, 'default', null::integer, null::integer, 2.40::numeric, 'USD'),
        ('74d0db27-d939-5d61-9af1-72a66df4df2b'::uuid, 'minimax', 'MiniMax-M2.1-highspeed', 'per_1m_cached_input_tokens'::billing_unit, 'default', null::integer, null::integer, 0.03::numeric, 'USD'),
        ('24bdd40e-e4b8-525c-92c0-7bf57760352d'::uuid, 'minimax', 'MiniMax-M2', 'per_1m_input_tokens'::billing_unit, 'default', null::integer, null::integer, 0.30::numeric, 'USD'),
        ('99922c65-a522-56b0-bbe4-8a4657508f9b'::uuid, 'minimax', 'MiniMax-M2', 'per_1m_output_tokens'::billing_unit, 'default', null::integer, null::integer, 1.20::numeric, 'USD'),
        ('28c58ae4-bf01-54b4-ba3e-7a943d784ac1'::uuid, 'minimax', 'MiniMax-M2', 'per_1m_cached_input_tokens'::billing_unit, 'default', null::integer, null::integer, 0.03::numeric, 'USD')
),
resolved_price_seed as (
    select
        price_seed.price_id,
        model.id as model_catalog_id,
        price_seed.billing_unit,
        price_seed.price_variant_key,
        price_seed.request_input_tokens_min,
        price_seed.request_input_tokens_max,
        price_seed.unit_price,
        price_seed.currency_code
    from price_seed
    join ai_provider_catalog provider on provider.provider_kind = price_seed.provider_kind
    join ai_model_catalog model
      on model.provider_catalog_id = provider.id
     and model.model_name = price_seed.model_name
     and model.capability_kind = 'chat'::ai_model_capability_kind
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
    price_id,
    model_catalog_id,
    billing_unit,
    price_variant_key,
    request_input_tokens_min,
    request_input_tokens_max,
    unit_price,
    currency_code,
    timestamptz '2026-06-26 00:00:00+00',
    null,
    'system'::ai_price_catalog_scope,
    null
from resolved_price_seed
on conflict do nothing;
