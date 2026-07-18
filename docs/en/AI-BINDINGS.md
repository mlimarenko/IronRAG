# AI bindings

An *AI binding profile* links one operator-facing responsibility (understand a
query, build embeddings, answer a question, …) to an AI account and catalog
model. Prompt and sampling settings live inline on that binding. Every
LLM-touching stage in IronRAG resolves its physical purpose through one function:
`AiCatalogService::resolve_active_runtime_binding`.

The administration UI exposes exactly five required profiles and optional
`extract_text`. Runtime stages such as query embedding,
semantic reranking, and visual analysis call the canonical profile directly;
they are not separately configurable binding purposes.

This document describes that contract, the scope hierarchy, and what to think
about when you pick a provider/model for each profile.

## 1. Data model

A binding row in `ai_binding` captures the following pieces:

| Field | Source | Notes |
|---|---|---|
| `binding_purpose` | `ai_binding_purpose` enum | one of the internal runtime purposes below |
| `scope_kind` | `ai_scope_kind` enum | `instance` / `workspace` / `library` |
| `workspace_id`, `library_id` | nullable | populated according to `scope_kind` |
| `account_id` | `ai_account` | API credential + base URL + provider catalog |
| `model_catalog_id` | `ai_model_catalog` | typed model identity and capabilities |
| prompt/sampling columns | inline on `ai_binding` | system prompt, temperature, top_p, output override, extra parameters |
| `binding_state` | `ai_binding_state` enum | exactly `active`, `invalid`, or `disabled` |

The runtime resolves the **effective** binding by walking the scope ladder:

1. library-scoped active binding for `(library_id, purpose)`
2. workspace-scoped active binding for `(workspace_id, purpose)`
3. instance-scoped active binding for `(purpose)` only

The first match wins. If the canonical profile is not configured, the stage
fails loudly (no silent substitution of an unrelated provider or model).

## 2. Operator profiles and internal purposes

The normal configuration surface is deliberately small:

| Profile | Required | Canonical purpose | Contract |
|---|---:|---|---|
| Graph Extraction | yes | `extract_graph` | strict typed entity/relationship JSON |
| Embeddings | yes | `embed_chunk` | one embedding model for both indexed segments and query vectors |
| Query Understanding | yes | `query_compile` | typed `QueryIR` and semantic reranking through separate runtime calls |
| Answer Generation | yes | `query_answer` | grounded, citation-aware answer generation |
| AI Assistant | yes | `agent` | tool-calling UI/MCP host |
| Document Understanding | no | `extract_text` | complex text/OCR/visual understanding after deterministic parsers |

These six canonical purposes are the complete binding enum. Runtime events and
billing keep their own typed stage identity without creating extra model
profiles:

| Internal purpose | Stage that consumes it | Resolution contract |
|---|---|---|
| `extract_text` | ingest: complex text/OCR and visual analysis | canonical multimodal Document Understanding profile |
| `extract_graph` | ingest: graph builder | strict JSON tagging of entities/relationships per chunk |
| `embed_chunk` | ingest and query: index segments and embed questions | one canonical Embeddings profile and vector space |
| `query_compile` | query: build `QueryIR` and optionally rerank evidence | one Query Understanding profile; separate calls, schemas, budgets, and accounting |
| `query_answer` | query: grounded answer over retrieved bundle | citation-aware long-form generation; instruction following |
| `agent` | UI in-product agent + MCP `grounded_answer` host | tool-calling chat model with good instruction following |

Model eligibility comes from typed catalog capability/modality metadata. It is
never guessed from a provider name, model name, suffix, or a hand-maintained
natural-language dictionary.

`agent` has a stricter conjunction: the model must explicitly include the
`agent` catalog role and use the `chat` capability kind, while the provider
profile must declare both `chat` and `tools` as `supported`. `unknown` is not
treated as support. Upgrade migration 0012 materializes Agent eligibility and a
canonical Agent bootstrap entry from `query_answer` only under that typed
provider contract; pre-existing dedicated Agent configuration is preserved
without turning it into a runtime fallback.

When a provider's model-list response does not contain typed metadata, catalog
generation accepts an operator manifest through
`IRONRAG_AI_MODEL_CAPABILITIES_JSON_B64`. The decoded value is an object keyed
first by provider kind and then by the exact model identity; each entry declares
only `capabilityKind` (`chat` or `embedding`) and `modalityKind` (`text` or
`multimodal`). Duplicate or oversized manifests fail closed; invalid signatures
and discovered models without a typed declaration are skipped.

## 3. Wire-level prompt structure (matters for caching)

`build_structured_chat_request` produces a `ChatRequest` whose
`generate()` path serializes to this shape (see
`apps/api/src/integrations/llm/openai_compatible.rs`):

```jsonc
{
  "model": "<model_name>",
  "messages": [
    { "role": "system", "content": "<binding.system_prompt OR the built-in prompt for the purpose>" },
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
- `extra_parameters_json` from the binding and typed provider request policy is merged
  into the request body verbatim. **Do not put per-call dynamic values
  there** (user id, request id, timestamp, etc.). Anything dynamic will
  break the provider's prompt cache and you pay full latency on every
  call.

Billing normalizes cache counters by provider protocol. OpenAI-compatible
cached tokens are a subset of total input and are subtracted before the
ordinary-input charge is created. Anthropic's input, cache-creation, and
cache-read counters are disjoint and are added only for context-size price-tier
selection. The current price schema has no cache-write unit, so Anthropic
cache-creation tokens are explicitly approximated at the ordinary input rate;
cache reads remain a separate cached-input charge.

## 4. Choosing a model per profile

Latency, quality, and price tradeoffs are real and provider-specific.
What matters from IronRAG's side is the *shape* of the work:

### Query Understanding (`query_compile`)

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

Provider-backed semantic reranking always uses this same profile. Compilation
and reranking remain separate runtime calls with independent schemas,
deadlines, concurrency limits, traces, and usage accounting.

### `query_answer`

- Long-form generation with inline citations. Output dominates latency.
- Streaming helps; the UI assistant and the MCP grounded-answer tool
  both consume the streamed deltas.
- A higher-quality model usually pays for itself here because the
  user-visible answer is the product.

#### Semantic rerank rollout

Provider-backed semantic reranking is opt-in and defaults to
`IRONRAG_QUERY_SEMANTIC_RERANK_MODE=off`. Roll it out as `off` -> `shadow` ->
`active`. The existing `IRONRAG_QUERY_RERANK_ENABLED` switch is the master
gate; startup rejects `shadow` or `active` while that switch is false.

- `off` continues to use the deterministic lexical heuristic with the resolved
  standalone retrieval question and makes no provider call.
- `shadow` sends the resolved standalone retrieval question and bounded
  candidate excerpts to the active Query Understanding profile in one process-bounded,
  low-priority background task on a genuine result-cache miss. It never changes
  answer ordering and does not wait for the provider response on the query
  path. Normal result-cache hits stay fast and do not schedule or bill a second
  shadow sample; the persisted query-execution replay row and cache-hit log make
  that historical reuse explicit.
- `active` waits for a validated provider ranking. The configured timeout,
  with a hard ceiling of 3000 ms, is a decision budget that starts before
  binding lookup. Binding lookup and the durable reservation are not canceled
  mid-database operation, but both consume this budget; the provider receives
  only the remaining time. If the budget expires after reservation, the known
  reservation is terminalized and the provider is not called. Missing binding,
  timeout, provider failure, malformed response, or accounting failure uses the
  same deterministic lexical fallback. Mandatory completion/accounting after a
  provider response can add database overhead outside the decision deadline.

The provider sees candidate text and opaque numeric indices, never the internal
IronRAG entity, relationship, chunk, document, workspace, or library UUIDs. The
runtime accepts exactly one finite score in `[0, 1]` for every submitted index;
duplicates, omissions, extra fields, and out-of-range values are rejected.
Candidate count, per-candidate raw characters, and total raw query+candidate
characters are bounded by the five `IRONRAG_QUERY_SEMANTIC_RERANK_*` settings
and compile-time hard caps (32 candidates, 2400 raw characters per candidate,
32000 raw characters total). After UTF-8 encoding and JSON escaping, the full
user message has a separate hard 96 KiB cap; candidates are removed from the
tail until it fits. Provider scores control ordering only; original retrieval
scores remain intact.

### Embeddings (one profile for indexing and query vectors)

- This is one operator binding, not two independently selectable models.
  `embed_chunk` indexes chunks and the query path resolves that same profile to
  embed questions. There is no second query-embedding purpose.
- Every stored vector and query lookup uses a secret-free
  `embedding-profile:v1:<sha256>` execution-profile key. It fingerprints the
  resolved provider/model execution path and canonical request parameters;
  scope, binding row, requested purpose, and secret value are excluded. Vector
  lookup requires an exact profile-key match and one unambiguous dimension.
- `extraParametersJson.dimensions` overrides model-catalog `metadataJson.dimensions`.
  Both are validated as positive storage-safe integers during binding
  resolution; an invalid explicit value fails closed instead of falling back.
  A catalog-only dimension participates in the profile key but is not injected
  into the upstream request body.
- Equal dimensions do **not** make two embedding spaces compatible. Any profile
  change that produces a new key requires
  `ironrag-maintenance rebuild vector-plane --source-library <uuid>`, even when
  the old and new models return the same number of values. Pre-profile UUID-keyed
  vector rows also require this one-time rebuild after upgrade.
- If a library has active source material but no vectors for the exact active
  profile, retrieval fails with the rebuild action instead of silently returning
  lexical-only or empty vector results.
- A genuinely empty library is a healthy typed state. Query preflight skips the
  embedding provider and ANN lanes instead of demanding an impossible rebuild.
- Rebuild streams canonical chunks with a keyset cursor, writes vectors in
  provider batches, and reconciles each vector manifest once at the end. It
  does not retain the complete library in memory or recount the lane per row.
- Throughput matters at ingest scale; latency matters on the query path. Those
  are measurements for the same model, not a reason to permit incompatible
  vector spaces.

### `extract_graph`

- Strict JSON output per chunk; runs many times per document. Cost and
  throughput dominate over single-call latency.
- A smaller, fast structured-output model is usually fine.

### Document Understanding (`extract_text`)

- Deterministic file parsers and OCR run first where they are sufficient. The
  canonical multimodal Document Understanding model handles complex extraction
  and visual content that needs model reasoning.
- Visual analysis uses that same model. The binding is accepted only when the
  catalog declares a chat-capable multimodal model and the provider supports
  visual input. Without a suitable profile, deterministic extraction remains
  available and model-only analysis is unavailable.

### `agent`

- Tool-calling chat model used by the UI in-product agent and as the
  MCP host for `grounded_answer`. Needs to follow tool-call schemas
  exactly and pick the right tool with the right arguments.

## 5. Inspecting active bindings

Bindings live in Postgres. This query joins the binding, account, provider,
and model catalog tables and prints every active physical purpose:

```sql
SELECT
  b.scope_kind,
  b.workspace_id,
  b.library_id,
  b.binding_purpose,
  p.provider_kind,
  m.model_name,
  octet_length(coalesce(b.system_prompt,'')) AS sys_prompt_bytes,
  b.temperature,
  b.top_p,
  b.max_output_tokens_override
FROM ai_binding b
JOIN ai_account a          ON a.id = b.account_id
JOIN ai_provider_catalog p ON p.id = a.provider_catalog_id
JOIN ai_model_catalog m    ON m.id = b.model_catalog_id
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
FROM ai_binding b
CROSS JOIN library
JOIN ai_account a          ON a.id = b.account_id
JOIN ai_provider_catalog p ON p.id = a.provider_catalog_id
JOIN ai_model_catalog m    ON m.id = b.model_catalog_id
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
- **Embedding-space drift.** Changing the active embedding execution profile
  requires a vector rebuild whenever its profile key changes. This applies to
  equal-dimension models as well as dimension changes (for example `1024` →
  `3072`); a dimension match alone is never proof that coordinates are
  comparable.
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
  binding row does not already have a value. An operator-raised output
  budget (e.g. for graph extraction) is preserved across
  backend restarts and is never silently reverted to the catalog
  default.

## 7. Changing the active binding

1. Ensure model discovery or the operator capability manifest has produced an
   `ai_model_catalog` row with the required typed capability/modality.
2. Create or select an `ai_account` for the provider.
3. Use the administration UI or AI configuration API to save the logical
   profile at the desired scope. Model, prompt, sampling, output override, and
   extra parameters are stored inline in `ai_binding`; do not write the table
   directly.
4. Re-run the regression bench in `scripts/bench/agent_turn_p95.py`
   against the affected libraries before committing to the new binding.

See `docs/en/PIPELINE.md` for how the ingest stages chain bindings, and
`docs/en/BENCHMARKS.md` for the bench harness.
