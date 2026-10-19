# AI bindings

An _AI binding profile_ links one operator-facing responsibility (understand a
query, build embeddings, answer a question, …) to an AI account and catalog
model. Prompt and sampling settings live inline on that binding. Every
LLM work in IronRAG resolves through the selected binding profile.

The administration UI exposes exactly five required profiles plus the optional
`extract_text`, `query_rerank` and `graph_entity_type` profiles. Runtime stages
such as query embedding and visual analysis call the designated profile
directly; they are not separately configurable binding purposes. Semantic
rerank and graph entity typing are the exceptions: each resolves its own
binding (`query_rerank`, `graph_entity_type`). Semantic rerank degrades to the
deterministic lexical fallback when its profile is unbound; ingest resolves
`graph_entity_type` after entity candidates are detected and before node
identity and edges are materialized.

This document describes that contract, the scope hierarchy, and what to think
about when you pick a provider/model for each profile.

## 1. Data model

A binding row in `ai_binding` captures the following pieces:

| Field                        | Source                    | Notes                                                                |
| ---------------------------- | ------------------------- | -------------------------------------------------------------------- |
| `binding_purpose`            | `ai_binding_purpose` enum | one of the internal runtime purposes below                           |
| `scope_kind`                 | `ai_scope_kind` enum      | `instance` / `workspace` / `library`                                 |
| `workspace_id`, `library_id` | nullable                  | populated according to `scope_kind`                                  |
| `account_id`                 | `ai_account`              | API credential + base URL + provider catalog                         |
| `model_catalog_id`           | `ai_model_catalog`        | typed model identity and capabilities                                |
| prompt/sampling columns      | inline on `ai_binding`    | system prompt, temperature, top_p, output override, extra parameters |
| `binding_state`              | `ai_binding_state` enum   | exactly `active`, `invalid`, or `disabled`                           |

The runtime resolves the **effective** binding by walking the scope ladder:

1. library-scoped active binding for `(library_id, purpose)`
2. workspace-scoped active binding for `(workspace_id, purpose)`
3. instance-scoped active binding for `(purpose)` only

The first match wins. If the designated profile is not configured, the stage
fails loudly (no silent substitution of an unrelated provider or model).

## 2. Operator profiles

The normal configuration surface is deliberately small:

| Profile                | Required | Binding purpose     | Contract                                                                                                                                           |
| ---------------------- | -------: | ------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| Graph Extraction       |      yes | `extract_graph`     | strict typed entity/relationship JSON                                                                                                              |
| Embeddings             |      yes | `embed_chunk`       | one embedding model for both indexed segments and query vectors                                                                                    |
| Query Understanding    |      yes | `query_compile`     | typed `QueryIR`, HyDE expansion, and query consolidation                                                                                           |
| Semantic Rerank        |       no | `query_rerank`      | provider-backed semantic reranking of retrieved candidates, bound separately from query compilation                                                |
| Graph Entity Typing    |       no | `graph_entity_type` | typed classification of extracted entities, resolved before node identity and edges; served by a decision provider's endpoint or a fast chat model |
| Answer Generation      |      yes | `query_answer`      | grounded, citation-aware answer generation                                                                                                         |
| AI Assistant           |      yes | `agent`             | tool-calling UI/MCP host                                                                                                                           |
| Document Understanding |       no | `extract_text`      | complex text/OCR/visual understanding after deterministic parsers                                                                                  |

These eight binding purposes are the complete binding enum. Runtime events and
billing keep their own typed stage identity without creating extra model
profiles:

| Internal purpose    | Stage that consumes it                                                   | Resolution contract                                                                                                                                |
| ------------------- | ------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| `extract_text`      | ingest: complex text/OCR and visual analysis                             | designated multimodal Document Understanding profile                                                                                               |
| `extract_graph`     | ingest: graph builder                                                    | strict JSON tagging of entities/relationships per chunk                                                                                            |
| `graph_entity_type` | ingest: graph entity typing                                              | typed classification of extracted entities into one closed entity type; the classified type forms the node identity key and the materialized edges |
| `embed_chunk`       | ingest and query: index segments and embed questions                     | one shared Embeddings profile and vector space                                                                                                     |
| `query_compile`     | query: compile the typed `QueryIR`, expand HyDE, and consolidate queries | one Query Understanding profile                                                                                                                    |
| `query_rerank`      | query: provider-backed semantic rerank of retrieved candidates           | one Semantic Rerank profile; an unbound purpose degrades to the deterministic lexical fallback, never to `query_compile`                           |
| `query_answer`      | query: grounded answer over retrieved bundle                             | citation-aware long-form generation; instruction following                                                                                         |
| `agent`             | UI in-product agent + MCP `grounded_answer` host                         | tool-calling chat model with good instruction following                                                                                            |

Model eligibility comes from typed catalog capability/modality metadata. It is
never guessed from a provider name, model name, suffix, or a hand-maintained
natural-language dictionary.

`agent` has a stricter conjunction: the model must explicitly include the
`agent` catalog role and use the `chat` capability kind, while the provider
profile must declare both `chat` and `tools` as `supported`. The fresh
baseline creates an Agent binding from `query_answer` only under that typed
provider contract; an explicitly configured Agent binding remains unchanged.

When a provider's model-list response does not contain typed metadata, catalog
generation accepts an operator manifest through
`IRONRAG_AI_MODEL_CAPABILITIES_JSON_B64`. The decoded value is an object keyed
first by provider kind and then by the exact model identity; each entry declares
only `capabilityKind` (`chat` or `embedding`) and `modalityKind` (`text` or
`multimodal`). Duplicate or oversized manifests fail closed; invalid signatures
and discovered models without a typed declaration are skipped.

### Decision providers (TypeSafe)

A provider whose profile declares `capabilities.decision: supported` is a typed
decision endpoint (TypeSafe System One), not a chat vendor. It must declare
every generative capability (`chat`, `embeddings`, `vision`, `tools`,
`streaming`) `unsupported`, and every model-discovery path must be
`capabilityKind: decision`. Such a provider serves only the two decision
purposes, `graph_entity_type` and `query_rerank`; its models may allow only
those purposes. A generative-purpose binding against it is refused even when
the caller asks for `disabled`.

Both decision purposes ship: `query_rerank` since IRG-187 evaluates the
shortlist through the SystemOne `Score` primitive, and `graph_entity_type`
since IRG-185 classifies extracted entity candidates through the `Choice`
primitive before node identity and edges are computed. Active bindings for
either purpose are accepted. The seed ships TypeSafe Jev's `jev-latest` model
for inspection. An ordinary chat provider is unaffected: a fast chat model
whose catalog row allows `graph_entity_type` or `query_rerank` stays bindable
active and serves the same stage.

A TypeSafe account is validated against the provider's `GET /v1/models`
OpenAI-shaped listing (`credentials.validationMode: model_list` on its
profile), the same path every other seeded provider uses.

## 3. Wire-level prompt structure (matters for caching)

An OpenAI-compatible request has this shape:

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
selection. Anthropic cache-creation tokens use the model's cache-write input
rate; cache reads use its cached-input rate.

## 4. Choosing a model per profile

Latency, quality, and price tradeoffs are real and provider-specific.
What matters from IronRAG's side is the _shape_ of the work:

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

### Semantic Rerank (`query_rerank`)

Provider-backed semantic reranking resolves the `query_rerank` binding, so it
can be bound, budgeted, and switched to a fast non-reasoning model without
touching the typed-query compiler. There is no alias onto `query_compile`: an
unbound `query_rerank` purpose degrades the stage to the deterministic lexical
fallback. Compilation and reranking remain separate runtime calls with
independent schemas, deadlines, concurrency limits, traces, and usage
accounting.

#### Semantic rerank rollout

Provider-backed semantic reranking is opt-in and defaults to
`IRONRAG_QUERY_SEMANTIC_RERANK_MODE=off`. Roll it out as `off` -> `shadow` ->
`active`. The existing `IRONRAG_QUERY_RERANK_ENABLED` switch is the master
gate; startup rejects `shadow` or `active` while that switch is false.

- `off` continues to use the deterministic lexical heuristic with the resolved
  standalone retrieval question and makes no provider call.
- `shadow` sends the resolved standalone retrieval question and bounded
  candidate excerpts to the active `query_rerank` profile in one process-bounded,
  low-priority background task on a genuine result-cache miss. It never changes
  answer ordering and does not wait for the provider response on the query
  path. Normal result-cache hits stay fast and do not schedule or bill a second
  shadow sample; the persisted query-execution replay row and cache-hit log make
  that historical reuse explicit.
- `active` applies only a validated provider ranking and is bounded by a
  separate turn wait budget. The configured timeout is the provider decision
  budget that starts before binding lookup; it must not exceed the compile-time
  1000 ms turn wait (`SEMANTIC_RERANK_TURN_WAIT_MS`), otherwise an active rerank
  could never call the provider: such a configuration is rejected at startup
  (fail closed, message names `query_semantic_rerank_timeout_ms`). The default
  timeout equals that same ceiling. The answer turn never waits longer than the
  turn wait. When the provider budget does not fit the turn wait, the provider
  call is never started and the deterministic lexical fallback is used
  immediately; when the call is started and the turn wait expires, the same
  fallback applies. Every degradation is recorded as the structured warning
  `semantic rerank degraded to the deterministic lexical fallback` with a
  `reason` field and a running `degradations_total`, and increments the exported
  `ironrag.semantic_rerank.degradations` counter under the same bounded `reason`
  label (`provider_budget_exceeds_turn_wait`, `turn_wait_exceeded`,
  `missing_binding`, `provider_failure`, `invalid_response`, `timed_out`), so a
  permanent fallback is visible to an operator without reading logs. Binding
  lookup and the durable reservation are not canceled mid-database operation,
  but both consume the provider budget; the provider receives only the remaining
  time. If the budget expires after reservation, the known reservation is
  terminalized and the provider is not called. Missing binding, timeout,
  provider failure, malformed response, or accounting failure uses the same
  deterministic lexical fallback. Mandatory completion/accounting after a
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

### `query_answer`

- Long-form generation with inline citations. Output dominates latency.
- Streaming helps; the UI assistant and the MCP grounded-answer tool
  both consume the streamed deltas.
- A higher-quality model usually pays for itself here because the
  user-visible answer is the product.

### Embeddings (one profile for indexing and query vectors)

- This is one operator binding, not two independently selectable models.
  `embed_chunk` indexes chunks and the query path resolves that same profile to
  embed questions. There is no second query-embedding purpose.
- Every stored vector and query lookup uses a secret-free
  `embedding-profile:v1:<sha256>` execution-profile key. It fingerprints the
  resolved provider/model execution path and normalized request parameters;
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
   the old and new models return the same number of values.
- If a library has active source material but no vectors for the exact active
  profile, retrieval fails with the rebuild action instead of silently returning
  lexical-only or empty vector results.
- A genuinely empty library is a healthy typed state. Query preflight skips the
  embedding provider and ANN lanes instead of demanding an impossible rebuild.
- Rebuild streams stored chunks with a keyset cursor, writes vectors in
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
  designated multimodal Document Understanding model handles complex extraction
  and visual content that needs model reasoning.
- Visual analysis uses that same model. The binding is accepted only when the
  catalog declares a chat-capable multimodal model and the provider supports
  visual input. Without a suitable profile, deterministic extraction remains
  available and model-only analysis is unavailable.

### `agent`

- Tool-calling chat model used by the UI in-product agent and as the
  MCP host for `grounded_answer`. Needs to follow tool-call schemas
  exactly and pick the right tool with the right arguments.

### Reasoning mode (per binding)

A binding may carry a typed reasoning selection beside its raw
`extraParametersJson`. It is stored under the canonical key
`extraParametersJson.reasoningMode` as `{"version": 1, "mode": "none"}`; an
absent key is the honest **unset** state. The administration UI edits it with
the **Reasoning mode** selector, which appears for every purpose except
Embeddings. The selector owns that key and the key is never edited through the
additional-parameters JSON, so there is one author of the typed selection.

**Unset is neither "Off" nor the vendor's original default.** Leaving the
selector on _Default (provider policy)_ preserves the existing automatic policy and raw parameters: the
provider's automatic `ProviderReasoningPolicy` (`Forward`, or
`DisableWithNone`) and every raw parameter still apply exactly as before. The
UI promises no particular vendor default for that state.

Selecting **Off** is allowed only for the exact provider/model pair whose model
catalog row confirms it at `metadataJson.reasoningCapability`:

- `mode` is `none`;
- `wire` is one of the documented wire controls `reasoning_effort_none` or
  `thinking_disabled`;
- `evidence` is non-blank and names the first-party source that confirmed this
  exact pair.

Nothing is inferred from the provider profile, the provider's `chat`
capability, or the model name, family or suffix. A reserved server projection
(`_providerModelReasoning`) or a client-supplied request policy never grants a
capability. Unconfirmed or unreadable metadata is explained read-only and no
mode can be selected. A saved selection that is unreadable (wrong version or
type, unknown mode) or not confirmed for the bound model is shown honestly,
and only an explicit **Unset** clears it — it is never rewritten to `none`.
Switching account or model invalidates the previous reasoning decision, even
when the new pair also confirms Off. Save stays blocked until the operator
selects **Confirm for this pair** or **Unset**; an unconfirmed pair can only
be reset. Returning to the original model does not restore the old decision.
Blocking explanations automatically open **Model and parameters**, so the
disabled Save button never hides its resolution inside a closed disclosure.

When **Off** is selected, the server emits exactly one wire control and clears
the other owned switch before the request leaves, so two reasoning controls are
never sent together:

| Confirmed wire          | Emitted provider parameter       |
| ----------------------- | -------------------------------- |
| `reasoning_effort_none` | `reasoning_effort: "none"`       |
| `thinking_disabled`     | `thinking: {"type": "disabled"}` |

The declared control may appear in the raw JSON only with its exact value; the
other owned switch is a conflict even when it also disables reasoning. The
dialog names the conflicting parameter and can remove just that key on request,
without dropping unrelated JSON. A `reasoningMode` typed into the raw JSON is
rejected until it is removed.

The vocabulary is currently closed to `none`, and only these two wire controls
are confirmed by first-party documentation:

- `reasoning_effort`: OpenAI documents `none` (among other efforts) for Chat
  Completions — <https://developers.openai.com/api/docs/guides/reasoning>.
- `thinking` and `reasoning_effort`: DeepSeek's Chat Completions reference
  documents `thinking.type` `enabled`/`disabled` and `reasoning_effort`
  `none`/`low`/`high`/`max` for `deepseek-flash` and `deepseek-v4-pro` —
  <https://api-docs.deepseek.com/api/create-chat-completion>.

That documentation confirms the wire vocabulary, not acceptance by any
deployed pair. An operator still confirms each exact model in its catalog row;
the models this build seeds remain unconfirmed until then.

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

1. Create or select an `ai_account` for the provider.
2. Choose the model. The binding dialog lists the `ai_model_catalog` rows for
   that provider together with whatever the account's own key reaches right
   now, read live from the provider through
   `GET /v1/ai/accounts/{account_id}/available-models`. Choosing a model that
   has no catalog row yet creates one on save. OpenRouter and RouterAI list
   their models with per-token prices, so the catalog seeds a curated priced
   set from them — OpenRouter in US dollars, RouterAI in roubles. GPTunnel
   publishes no authless price list, so it seeds none and its models come from
   the account's own listing.
3. Use the administration UI or AI configuration API to save the logical
   profile at the desired scope. Model, prompt, sampling, output override, and
   extra parameters are stored inline in `ai_binding`; do not write the table
   directly.

A row created this way carries only what the provider published — context
length, output ceiling, input modalities, and rates. Where the provider
publishes no rates the row records zero, and the dialog says so before you
save; set the real rates in the catalog if you want spend counted for that
model. Nothing is inferred from the model id.

A provider also declares which purposes none of its seeded models cover, and of
which kind. `provider_cannot` is a capability gap — Anthropic exposes no
embeddings endpoint, so an embedding binding needs a second provider.
`operator_supplies` means the provider serves the purpose and the model is
yours to choose from the live listing above; GPTunnel, LiteLLM, Ollama and
Qwen are in this group.

## 8. Catalog providers

| Provider                  | Chat | Vision | Embedding | Notes                                                                                                          |
| ------------------------- | ---- | ------ | --------- | -------------------------------------------------------------------------------------------------------------- |
| **OpenAI**                | ✅   | ✅     | ✅        | Direct API                                                                                                     |
| **DeepSeek**              | ✅   | ✅     | —         | Very low cost; V4.1-Flash reads images; no embeddings                                                          |
| **Qwen / DashScope-intl** | ✅   | ✅     | ✅        | Cost close to DeepSeek; seeds priced chat/vision; the embedding model is operator-supplied                     |
| **GPTunnel**              | ✅   | ✅     | ✅        | Router: publishes no authless price list, so it seeds no models — add them from your account                   |
| **OpenRouter**            | ✅   | ✅     | ✅        | Router: seeds a curated priced set, quoted in USD                                                              |
| **RouterAI**              | ✅   | ✅     | ✅        | Router: seeds the same chat set quoted in RUB                                                                  |
| **MiniMax**               | ✅   | ✅     | —         | Direct API or Token Plan subscription key                                                                      |
| **Ollama**                | ✅   | ✅     | ✅        | Fully local, air-gapped, GPU optional — see [OLLAMA.md](./OLLAMA.md)                                           |
| **LiteLLM**               | ✅   | ✅     | ✅        | Self-hosted OpenAI-compatible gateway: add as an OpenAI-compatible account with your LiteLLM base URL          |
| **TypeSafe Jev**          | —    | —      | —         | Typed decision endpoint: serves only `graph_entity_type` / `query_rerank`, both active since IRG-187 / IRG-185 |

**Admin → AI → Bindings** presents the eight purposes in runtime order. Configure
only the purposes used by each enabled workflow. Bindings are scoped to
instance, workspace, or library.

The raster-image engine named `vision` uses the active multimodal `extract_text`
binding. Switch a library to the `docling` engine to OCR images locally with no
visual-model calls. Selecting `vision` without a compatible `extract_text`
binding fails loudly.

Bootstrap provider keys from `.env`: [INSTALL.md](./INSTALL.md).

## 9. Reasoning controls: source and vendor audit

Audit date: **2026-10-04**. This matrix covers the 12 seeded providers in
[`catalog_seed.rs`](../../apps/api/src/app/catalog_seed.rs), audited at
`b3092cc3` and unchanged at `17e553fe`. It publishes the accepted IRG-166 rev4
audit, not a live account/model compatibility test.

The code declares `requestPolicy.reasoning = disable_with_none` for OpenAI and
DeepSeek. This provider-level policy is not proof that every model accepts
`none`. Other OpenAI-compatible profiles use `forward`; Anthropic and Gemini
use native transports without a modeled reasoning control. `forward` does not
mean that the vendor lacks reasoning controls. TypeSafe is a decision endpoint,
not a generative reasoning provider in this configuration.

| Provider | Catalog policy / implementation | Vendor-documented control and source |
|---|---|---|
| OpenAI | `disable_with_none`; model restrictions still apply | [`reasoning.effort` / `reasoning_effort`](https://developers.openai.com/api/docs/guides/reasoning); allowed values depend on the model. GPT-6 Astra rejects `none`; GPT-6.1 Sol supports neither `none` nor `minimal` and defaults to `medium`. |
| Anthropic | Native Messages transport; control not modeled | [`output_config.effort`](https://platform.claude.com/docs/en/build-with-claude/effort): `low`, `medium`, `high`, `xhigh`, `max`, depending on the model. `thinking.budget_tokens` is a separate control. |
| Gemini | Native `generateContent`; control not modeled | [`generationConfig.thinkingConfig`](https://ai.google.dev/api/generate-content): integer `thinkingBudget` or `thinkingLevel` (`THINKING_LEVEL_UNSPECIFIED`, `MINIMAL`, `LOW`, `MEDIUM`, `HIGH`). Level support is model-specific. |
| MiniMax | OpenAI-compatible `forward`; control not modeled | [`thinking.type` / `reasoning_effort`](https://platform.minimax.io/docs/api-reference/text-openai-api). Effort levels apply to M3.1-Flash-Preview, default to `max`, and reject `none`; M3.1-Flash and M2.x cannot disable thinking. |
| DeepSeek | `disable_with_none`; endpoint control modeled | [`thinking.type` / `reasoning_effort`](https://api-docs.deepseek.com/api/create-chat-completion): `enabled`/`disabled`, or `none`, `low`, `high`, `max`; effort defaults to `high`. |
| OpenRouter | OpenAI-compatible `forward`; control not modeled | [`reasoning`](https://openrouter.ai/docs/guides/best-practices/reasoning-tokens) normalizes effort/token budgets. Consult per-model metadata; `mandatory=true` rejects `none`, and dynamic routers omit the field. |
| RouterAI | OpenAI-compatible `forward`; unconfirmed | [Vendor site](https://routerai.ru/); primary reasoning-control documentation was not obtained. OpenAI compatibility does not establish model support. |
| GPTunnel | OpenAI-compatible `forward`; unconfirmed | [Vendor site](https://gptunnel.ru/); no primary reasoning-control content was obtained from its JS shell. No models are seeded. |
| Ollama | OpenAI-compatible `forward`; control not modeled | [`reasoning_effort` / `reasoning.effort`](https://docs.ollama.com/api/openai-compatibility); accepted names/defaults depend on model metadata. Boolean-only thinking models map `none` to `false`. For GPT-OSS, no thinking output is not proof of zero reasoning compute or billing. Seeded models are embeddings only. |
| Qwen | OpenAI-compatible `forward`; control not modeled | [`enable_thinking`](https://www.alibabacloud.com/help/en/model-studio/deep-thinking) is a non-OpenAI parameter passed through `extra_body`; `reasoning_effort` applies only to Omni. |
| LiteLLM | OpenAI-compatible `forward`; control not modeled | [`reasoning_effort` / native `thinking`](https://docs.litellm.ai/docs/reasoning_content), SDK/Proxy v1.63.0+. Support depends on the routed model; Anthropic tool continuations require `thinking_blocks`. |
| TypeSafe | Configured decision endpoint; generative reasoning unsupported | [Configured endpoint](https://api.typesafe.ai); no public primary generative-control documentation obtained. The application's decision-only contract does not establish the capabilities of every vendor product. |

The audit distinguishes two provider policies modeled in code, seven documented
control families not modeled there, and three unconfirmed vendor controls.
Native transport support, an endpoint policy and model-specific compatibility
are different claims; none supplies a missing typed binding validation.

Model and transport qualifications from the accepted audit:

- Opus 5.5 defaults to `medium` and rejects `thinking.type=disabled` at every
  effort level. Opus 5 defaults to `high` and rejects it only at `xhigh`/`max`.
- Gemini's snake-case `thinking_level` belongs to the Interactions API, not the
  configured `generateContent` transport. Gemini 3+ level controls cannot be
  assumed valid on older models.
- Astra/Sol Chat Completions do not support function calling. Reasoning effort
  compatibility alone does not make a model eligible for the Agent profile.
- An unset control is not the literal value `none`: request construction must
  apply the binding/provider policy, and explicit `none` must respect the model's
  restrictions. This audit does not assert that the selector or typed wire
  validation is implemented; those remain separate product work.

No provider calls or active-binding changes were made for this documentation.
