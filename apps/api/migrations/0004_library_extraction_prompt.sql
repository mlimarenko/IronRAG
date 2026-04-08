ALTER TABLE catalog_library ADD COLUMN IF NOT EXISTS extraction_prompt text;

-- Register Ollama as a known provider for local model support.
-- Any OpenAI-compatible endpoint works when base_url is configured.
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
    'Ollama (Local)',
    'openai_compatible',
    'active',
    'http://localhost:11434/v1',
    '{"chat": true, "embedding": true, "vision": false}'::jsonb
)
ON CONFLICT (provider_kind) DO NOTHING;
