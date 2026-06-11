# AI bindings

An *AI binding* links a runtime purpose (compile a query, embed a chunk,
answer a question, …) to the provider + model + preset that should serve
it. Every LLM-touching stage in IronRAG resolves its binding through one
function: `AiCatalogService::resolve_active_runtime_binding`.

This document describes the contract, the available purposes, the scope
hierarchy, and what to think about when you pick provider/model for each
purpose.

## 1. Data model

A binding row in `ai_binding_assignment` joins six pieces:

| Field | Source | Notes |
|---|---|---|
| `binding_purpose` | `ai_binding_purpose` enum | one of the 8 purposes below |
| `scope_kind` | `ai_scope_kind` enum | `instance` / `workspace` / `library` |
| `workspace_id`, `library_id` | nullable | populated according to `scope_kind` |
| `provider_credential_id` | `ai_provider_credential` | api key + base URL + provider catalog |
| `model_preset_id` | `ai_model_preset` | model + temperature + top_p + (optional) system prompt |
| `binding_state` | `ai_binding_state` enum | `active`, `inactive`, … |

The runtime resolves the **effective** binding by walking the scope ladder:

1. library-scoped active binding for `(library_id, purpose)`
2. workspace-scoped active binding for `(workspace_id, purpose)`
3. instance-scoped active binding for `(purpose)` only

The first match wins. If nothing is configured, the stage fails loudly
(no silent fallback to a default provider).

## 2. The eight purposes

| Purpose | Stage that consumes it | What the model must do |
|---|---|---|
| `extract_text` | ingest: text/code/image OCR fallback | structured extraction of plain text from messy sources |
| `extract_graph` | ingest: graph builder | strict JSON tagging of entities/relationships per chunk |
| `embed_chunk` | ingest: vector indexer | embeddings (not chat); dimension must match the per-library shard |
| `query_compile` | query: turn NL question into `QueryIR` | strict JSON output against a fixed schema; low temperature |
| `query_retrieve` | query: embed the question for vector search | embeddings (not chat); must share the same model and dimension as `embed_chunk` for the library |
| `query_answer` | query: grounded answer over retrieved bundle | citation-aware long-form generation; instruction following |
| `vision` | ingest: image-to-text on visual chunks | multi-modal model with image input |
| `agent` | UI in-product agent + MCP `grounded_answer` host | tool-calling chat model with good instruction following |

## 3. Wire-level prompt structure (matters for caching)

`build_structured_chat_request` produces a `ChatRequest` whose
`generate()` path serializes to this shape (see
`apps/api/src/integrations/llm/openai_compatible.rs`):

```jsonc
{
  "model": "<model_name>",
  "messages": [
    { "role": "system", "content": "<preset.system_prompt OR the built-in prompt for the purpose>" },
    { "role": "user",   "content": "<purpose-specific user prompt>" }
  ],
  "temperature": ...,
  "top_p": ...,
  "response_format": { "type": "json_schema", "json_schema": { ... } },
  "max_completion_tokens": ...
}
```

The static system prompt is **always** the first message, followed by the
variable user payload. That is the layout OpenAI's automatic prompt
caching expects: the longest constant prefix sits at the front of the
serialized body, so identical prefixes hash to the same cache key.

Implications:

- The `response_format` JSON schema is part of the request body and
  therefore part of the cache key. If you change the schema, you bust
  every cached prefix on the provider side.
- `temperature`, `top_p`, `max_completion_tokens`, and the resolved
  `model_name` are also in the request body. Avoid jittering them per
  call.
- `extra_parameters_json` on the credential and the preset is merged
  into the request body verbatim. **Do not put per-call dynamic values
  there** (user id, request id, timestamp, etc.). Anything dynamic will
  break the provider's prompt cache and you pay full latency on every
  call.

## 4. Choosing a model per purpose

Latency, quality, and price tradeoffs are real and provider-specific.
What matters from IronRAG's side is the *shape* of the work:

### `query_compile`

- Hot path on every grounded answer; cold p95 of the answer pipeline is
  dominated by this single call when the IR cache misses.
- Input is small (built-in system prompt + question ~< 1 KB +
  JSON schema; ~1.5–10K tokens depending on the prompt).
- Output is strict JSON, typically 200–500 tokens.
- The system prompt and the schema are static; expect a high cache-hit
  ratio on the provider side as long as nothing dynamic is in
  `extra_parameters_json`.
- Optimize for **time-to-first-token** more than for raw token throughput;
  a smaller/faster model with strict structured-output support is
  usually the right pick.
- Quality matters: a bad IR breaks retrieval scope/focus. Always A/B
  candidate models against a stable set of golden questions before
  switching the active binding.

### `query_answer`

- Long-form generation with inline citations. Output dominates latency.
- Streaming helps; the UI assistant and the MCP grounded-answer tool
  both consume the streamed deltas.
- A higher-quality model usually pays for itself here because the
  user-visible answer is the product.

### `embed_chunk` / `query_retrieve`

- Neither is a chat model. Both are embedding models; `embed_chunk` indexes
  chunks at ingest time, `query_retrieve` embeds the question at query time.
  They must share the same model and dimension for a given library — the
  runtime enforces this and fails loudly if they disagree.
- Dimension is part of the per-library contract; vector material is stored
  in per-`(library, dim)` pgvector relations.
- Switching to a model with a different embedding dimension requires a vector
  rebuild pass (`ironrag-maintenance rebuild vector-plane --source-library`).
- Latency matters at ingest scale for `embed_chunk`; for `query_retrieve` it
  is on the critical query path.

### `extract_graph`

- Strict JSON output per chunk; runs many times per document. Cost and
  throughput dominate over single-call latency.
- A smaller, fast structured-output model is usually fine.

### `vision` / `extract_text`

- The active provider's vision capability decides whether image chunks
  go through Docling OCR or through the LLM vision route. Without an
  active `vision` binding, image chunks degrade to text-extraction
  only.

### `agent`

- Tool-calling chat model used by the UI in-product agent and as the
  MCP host for `grounded_answer`. Needs to follow tool-call schemas
  exactly and pick the right tool with the right arguments.

## 5. Inspecting active bindings

Bindings live in Postgres. This query joins all six tables and prints
each active binding with its provider, model, and preset:

```sql
SELECT
  b.scope_kind,
  b.workspace_id,
  b.library_id,
  b.binding_purpose,
  p.provider_kind,
  m.model_name,
  octet_length(coalesce(mp.system_prompt,'')) AS sys_prompt_bytes,
  mp.temperature,
  mp.top_p,
  mp.max_output_tokens_override
FROM ai_binding_assignment b
JOIN ai_provider_credential pc ON pc.id = b.provider_credential_id
JOIN ai_provider_catalog p     ON p.id  = pc.provider_catalog_id
JOIN ai_model_preset mp        ON mp.id = b.model_preset_id
JOIN ai_model_catalog m        ON m.id  = mp.model_catalog_id
WHERE b.binding_state = 'active'
ORDER BY b.binding_purpose, b.scope_kind;
```

To resolve the effective binding for one library + one purpose (mirrors
the runtime resolver):

```sql
WITH library AS (
  SELECT id AS library_id, workspace_id FROM catalog_library WHERE id = $1
)
SELECT b.scope_kind, p.provider_kind, m.model_name
FROM ai_binding_assignment b, library
JOIN ai_provider_credential pc ON pc.id = b.provider_credential_id
JOIN ai_provider_catalog p     ON p.id  = pc.provider_catalog_id
JOIN ai_model_preset mp        ON mp.id = b.model_preset_id
JOIN ai_model_catalog m        ON m.id  = mp.model_catalog_id
WHERE b.binding_state = 'active'
  AND b.binding_purpose = $2
  AND (
        (b.scope_kind = 'library'   AND b.library_id   = library.library_id)
     OR (b.scope_kind = 'workspace' AND b.workspace_id = library.workspace_id)
     OR (b.scope_kind = 'instance')
  )
ORDER BY CASE b.scope_kind
           WHEN 'library'   THEN 1
           WHEN 'workspace' THEN 2
           WHEN 'instance'  THEN 3
         END
LIMIT 1;
```

## 6. Common pitfalls

- **No active binding → loud failure.** Stages refuse to run with a
  silent default. If a `query_compile` binding is missing, the
  grounded-answer pipeline returns `409/422` with `QueryCompile binding
  is not configured`. Fix the binding, do not introduce a fallback.
- **Embedding dimension drift.** Changing the `embed_chunk` model
  between two models with different dimensions (e.g. `1024` →
  `3072`) requires a vector-migrate pass and re-indexing. The per-dim
  pgvector relations do the routing; old vector material stays around
  until migrated.
- **Per-call dynamic `extra_parameters_json`.** Anything that varies
  per request (user ids, timestamps, request ids) goes into the
  request body and busts every provider-side prompt cache.
- **System prompt edits.** Editing the built-in `QUERY_COMPILER_SYSTEM_PROMPT`
  invalidates the prompt cache for every binding that uses the built-in
  prompt. Treat this constant as part of the schema and version changes
  through `QUERY_IR_SCHEMA_VERSION` in the IR cache key. Graph-extraction
  prompts live in the graph service source and do not have a separate
  named compile-time constant.
- **Mixing `instance`, `workspace`, and `library` scopes.** A library
  override hides the workspace fallback completely. If you set a
  `library`-scoped `query_compile` binding, the workspace and instance
  ones are not consulted for that library. Use scope only when you
  actually need a per-library or per-workspace override.
- **Operator-set `max_output_tokens_override` persists across restarts.**
  The startup seed fills `max_output_tokens_override` only when the
  preset row does not already have a value. An operator-raised output
  budget (e.g. for a graph-extraction preset) is preserved across
  backend restarts and is never silently reverted to the catalog
  default.

## 7. Changing the active binding

1. Add the model to `ai_model_catalog` if it is not already there.
2. Create a model preset in `ai_model_preset` (temperature, top_p,
   optional system prompt override, `max_output_tokens_override`).
3. Create a provider credential in `ai_provider_credential` for the
   chosen provider catalog row.
4. Insert an `ai_binding_assignment` row with the desired
   `scope_kind`, `binding_purpose`, `provider_credential_id`,
   `model_preset_id`, and `binding_state = 'active'`. The unique index
   on `(scope, purpose)` enforces single-active per scope.
5. Re-run the regression bench in `scripts/bench/agent_turn_p95.py`
   against the affected libraries before committing to the new binding.

See `docs/en/PIPELINE.md` for how the ingest stages chain bindings, and
`docs/en/BENCHMARKS.md` for the bench harness.
