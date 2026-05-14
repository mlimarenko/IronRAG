-- Canonical operator-controlled ingest queue ordering.
--
-- `priority` keeps its coarse scheduling meaning, while `queue_rank` is the
-- explicit mutable order shown and changed from the administration queue view.

alter table ingest_job
    add column if not exists queue_rank bigint;

alter type ingest_queue_state add value if not exists 'paused';

update ingest_job
set queue_rank = (extract(epoch from queued_at) * 1000000)::bigint
where queue_rank is null;

alter table ingest_job
    alter column queue_rank set not null,
    alter column queue_rank set default ((extract(epoch from clock_timestamp()) * 1000000)::bigint);

drop index if exists idx_ingest_job_active_queue_rank;

create index idx_ingest_job_active_queue_rank
    on ingest_job (queue_state, queue_rank, priority, available_at, queued_at, id)
    where queue_state in ('queued', 'leased', 'paused');

-- Canonical OpenRouter 3072-dimension embedding baseline.
--
-- OpenRouter exposes more embedding models, but most return dimensions that do
-- not match the current vector index. Seed only models that have been verified
-- to produce 3072-dimensional vectors through the IronRAG request shape.

with openrouter_embedding_models (
    id,
    model_name
) as (
    values
        (
            'c4ab7bf4-9d0c-5270-9f8b-2c5604d4b744'::uuid,
            'openai/text-embedding-3-large'
        ),
        (
            '617154cb-2ef1-5f77-ae2c-487adcd4fac8'::uuid,
            'qwen/qwen3-embedding-8b'
        )
)
insert into ai_model_catalog (
    id,
    provider_catalog_id,
    model_name,
    capability_kind,
    modality_kind,
    lifecycle_state,
    metadata_json
)
select
    openrouter_embedding_models.id,
    provider.id,
    openrouter_embedding_models.model_name,
    'embedding'::ai_model_capability_kind,
    'text'::ai_model_modality_kind,
    'active'::ai_model_lifecycle_state,
    '{"defaultRoles":["embed_chunk","query_retrieve"],"seedSource":"provider_catalog"}'::jsonb
from ai_provider_catalog provider
join openrouter_embedding_models on true
where provider.provider_kind = 'openrouter'
on conflict (provider_catalog_id, model_name, capability_kind) do update set
    modality_kind = excluded.modality_kind,
    lifecycle_state = excluded.lifecycle_state,
    metadata_json = excluded.metadata_json;

with openrouter_embedding_presets (
    model_name,
    preset_name,
    extra_parameters_json
) as (
    values
        (
            'openai/text-embedding-3-large',
            'OpenRouter Embed Chunk · openai/text-embedding-3-large',
            '{}'::jsonb
        ),
        (
            'openai/text-embedding-3-large',
            'OpenRouter Query Retrieve · openai/text-embedding-3-large',
            '{}'::jsonb
        ),
        (
            'qwen/qwen3-embedding-8b',
            'OpenRouter Embed Chunk · qwen/qwen3-embedding-8b',
            '{"dimensions":3072}'::jsonb
        ),
        (
            'qwen/qwen3-embedding-8b',
            'OpenRouter Query Retrieve · qwen/qwen3-embedding-8b',
            '{"dimensions":3072}'::jsonb
        )
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
    model.id,
    openrouter_embedding_presets.preset_name,
    null,
    null,
    openrouter_embedding_presets.extra_parameters_json
from ai_model_catalog model
join ai_provider_catalog provider on provider.id = model.provider_catalog_id
join openrouter_embedding_presets
  on openrouter_embedding_presets.model_name = model.model_name
where provider.provider_kind = 'openrouter'
  and model.capability_kind = 'embedding'
on conflict do nothing;
