ALTER TABLE ai_provider_credential
    ADD COLUMN IF NOT EXISTS base_url text;

ALTER TABLE ai_provider_credential
    ALTER COLUMN api_key DROP NOT NULL;

INSERT INTO ai_provider_catalog (
    id,
    provider_kind,
    display_name,
    api_style,
    lifecycle_state,
    default_base_url,
    capability_flags_json
)
VALUES (
    '00000000-0000-0000-0000-000000000104',
    'ollama',
    'Ollama',
    'openai_compatible',
    'active',
    'http://localhost:11434/v1',
    '{"chat": true, "embedding": true, "vision": true}'::jsonb
)
ON CONFLICT (provider_kind) DO UPDATE
SET
    display_name = EXCLUDED.display_name,
    lifecycle_state = EXCLUDED.lifecycle_state::ai_provider_lifecycle_state,
    default_base_url = EXCLUDED.default_base_url,
    capability_flags_json = EXCLUDED.capability_flags_json;

INSERT INTO ai_model_catalog (
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
VALUES
    ('00000000-0000-0000-0000-000000000241', '00000000-0000-0000-0000-000000000104', 'qwen3:0.6b', 'chat', 'text', null, null, 'active', '{"defaultRoles": ["extract_graph", "query_answer"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000242', '00000000-0000-0000-0000-000000000104', 'qwen3-embedding:0.6b', 'embedding', 'text', null, null, 'active', '{"defaultRoles": ["embed_chunk"], "seedSource": "provider_catalog"}'::jsonb),
    ('00000000-0000-0000-0000-000000000243', '00000000-0000-0000-0000-000000000104', 'qwen3-vl:2b', 'chat', 'multimodal', null, null, 'active', '{"defaultRoles": ["vision"], "seedSource": "provider_catalog"}'::jsonb)
ON CONFLICT (provider_catalog_id, model_name, capability_kind) DO NOTHING;
