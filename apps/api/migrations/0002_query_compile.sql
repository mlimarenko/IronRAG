-- Adds `query_compile` as a canonical AI binding purpose and provisions the
-- persistent tier of the QueryCompiler cache.
--
-- QueryCompiler is the NL→IR stage: it turns a user question into a typed
-- QueryIR (AST) that replaces the ~450 hardcoded keyword classifiers scattered
-- through the query pipeline. Operators pick its provider/model via the same
-- binding system as every other stage (no model name is hardcoded in code).
--
-- Cache tiers: Redis is the hot tier (24h TTL); the table below captures the
-- same keyed compilations so operators can audit "what IR did we derive for
-- this question" offline and so a cold Redis never stalls prod on the LLM
-- call. Rows are automatically invalidated by `schema_version` when the IR
-- schema changes incompatibly (see `QUERY_IR_SCHEMA_VERSION`).

alter type ai_binding_purpose add value if not exists 'query_compile';

create table query_ir_cache (
    library_id uuid not null references catalog_library(id) on delete cascade,
    question_hash text not null,
    schema_version smallint not null default 1,
    query_ir_json jsonb not null,
    provider_kind text,
    model_name text,
    usage_json jsonb not null default '{}'::jsonb,
    compiled_at timestamptz not null default now(),
    primary key (library_id, question_hash)
);

create index idx_query_ir_cache_compiled_at on query_ir_cache (compiled_at desc);
