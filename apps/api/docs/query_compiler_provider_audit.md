# Provider Abstraction Audit for QueryCompiler

Reverse-engineering of existing LLM provider infrastructure — what `QueryCompiler` (NL → typed IR stage) can reuse, and what needs to be added.

## 1. Canonical LLM trait and entry point

**Trait:** `LlmGateway` in `apps/api/src/integrations/llm.rs:291-332`
- `async fn generate(&self, request: ChatRequest) -> Result<ChatResponse>` — base text-to-text
- `async fn generate_with_tools(&self, request: ToolUseRequest) -> Result<ToolUseResponse>` — OpenAI-compatible tool use
- `async fn generate_stream(...)` / `generate_with_tools_stream(...)` — streaming variants

**Implementation:** `UnifiedGateway` (`llm.rs:335-1179`) — single gateway for all providers (OpenAI, DeepSeek, Qwen, Ollama).

**Entry point for query_answer:**
- `answer_pipeline.rs:80` — `resolve_query_answer_provider_selection()` → `agent_loop::run_assistant_turn()` → `generate_with_tools()` via MCP dispatch (`agent_loop.rs:150-171`).

**Function calling / tool use:** Supported (OpenAI-compatible stack with `tool_choice="auto"`, tool_calls extraction).

**Structured output:** Supported at `ChatRequest` level via `response_format: Option<serde_json::Value>` (`llm.rs:42`), passthrough to OpenAI-compatible request (`openai_compatible.rs:145, 164, 191`).

**Path topology:** One `UnifiedGateway` for everything (text, tools, embedding, vision), each stage uses its own request type. For query_answer — tool-use path via assistant loop.

## 2. Structured output status per provider

**OpenAI:**
- `response_format: {"type":"json_schema", "json_schema":{"name":"...", "strict":true, "schema":{...}}}` in ChatRequest
- Test evidence: `llm.rs:1256-1263`
- Strict JSON Schema supported.

**DeepSeek:**
- OpenAI-compatible; `response_format` passthrough works
- Note `max_completion_tokens` vs `max_tokens` split (`openai_compatible.rs:756-758`, DeepSeek uses `max_tokens`)
- JSON mode works; strict schema may need fallback to `{"type":"json_object"}` or tool_choice hack.

**Qwen:**
- OpenAI-compatible; base_url = `https://dashscope-intl.aliyuncs.com/compatible-mode/v1` (`llm.rs:653-657`)
- JSON object mode likely works; strict schema needs verification.

**Ollama:**
- OpenAI-compatible, base_url from override; no API key required (`llm.rs:624-630`)
- Structured output depends on model; strict JSON Schema may not be supported by local models.

**Gaps:**
- No `call_structured<Req, Res>() -> Result<Res>` wrapper with automatic deserialization + schema enforcement
- No provider-specific fallback for providers without strict schema support
- `response_format` only in `ChatRequest`, not in `ToolUseRequest`

## 3. Provider profile resolution flow

**resolve_effective_provider_profile** (`services/ingest/runtime.rs:191-213`):
```
LibraryId
  → resolve_library_binding_selection(library_id, AiBindingPurpose::QueryAnswer)
  → EffectiveProviderProfile { indexing, embedding, answer, vision }
```

**selection_for_runtime_task_kind** (`domains/provider_profiles.rs:75-81`):
```
RuntimeTaskKind::QueryAnswer
  → AiBindingPurpose::for_runtime_task_kind()
  → AiBindingPurpose::QueryAnswer
  → EffectiveProviderProfile.selection_for_binding_purpose()
  → ProviderModelSelection { provider_kind, model_name }
```

**Scope hierarchy:** Library > Workspace > Instance (descend until binding found).
**Caching:** Per-request only; SQL query on each resolve call.
**User override:** via `ai_binding_assignment` table rows.

## 4. Requirements for QueryCompiler

**New enum variants:**
1. `RuntimeTaskKind::QueryCompile` in `domains/agent_runtime.rs:66-74` (+ `as_str`, `FromStr`)
2. `AiBindingPurpose::QueryCompile` in `domains/ai.rs:10-17` (+ `as_str`, `for_runtime_task_kind`)
3. Optional: `RuntimeStageKind::Compile` in `agent_runtime.rs:201-213` for separate observability stage

**SQL migrations (new file `0002_query_compile.sql`):**
- `ALTER TYPE ai_binding_purpose ADD VALUE IF NOT EXISTS 'query_compile';`
- `ALTER TYPE runtime_task_kind ADD VALUE IF NOT EXISTS 'query_compile';` (if such enum exists)
- Optional: `ALTER TYPE runtime_stage_kind ADD VALUE IF NOT EXISTS 'compile';`

**Bootstrap** (`services/ai_catalog_service/bootstrap.rs`):
- Mirror `QueryAnswer` binding seed with `AiBindingPurpose::QueryCompile`.
- New env vars: `IRONRAG_UI_BOOTSTRAP_QUERY_COMPILE_PROVIDER_KIND`, `_MODEL_NAME`.

**Reuse:**
- `ChatRequest` + `UnifiedGateway.generate()` (has `response_format` field already)
- `resolve_effective_provider_profile()` called with `AiBindingPurpose::QueryCompile`

**New:**
- Wrapper `call_query_compiler<Res: DeserializeOwned + JsonSchema>(prompt, system, params) -> Result<Res>` in `services/query/compiler.rs`:
  1. Resolve selection via `AiBindingPurpose::QueryCompile`
  2. Build `ChatRequest` with `response_format` (strict for OpenAI, json_object fallback for others)
  3. Call `UnifiedGateway.generate()`
  4. Deserialize output text into `Res`
  5. Return `Res` or validation error

**Provider-specific response_format builders** (`integrations/llm.rs`):
- `schema_for_openai(name, schema)` — strict json_schema
- `schema_for_openai_compat(name, schema)` — json_object fallback
- Per-provider dispatch in wrapper.

## 5. Observability

**Existing query_answer tracking:**
- `services/query/llm_context_debug.rs` — `LlmContextSnapshot` (execution_id, library_id, iterations)
- `RuntimeStageKind::Answer` (`agent_runtime.rs:206`), `RuntimeActionKind::ModelRequest` (`agent_runtime.rs:312`)

**QueryCompile integration:**
- Add `RuntimeStageKind::Compile` (parallel to `Answer`)
- Use existing `query_execution` as owner; compile becomes a substage
- Log to `runtime_stage`: `stage_kind="compile"`, `input_summary_json={prompt, schema}`, `output_summary_json={parsed_ir}`
- `LlmContextDebug` captures automatically if compile uses the same gateway path

## Action plan (10 steps)

1. Add `RuntimeTaskKind::QueryCompile` to `agent_runtime.rs` (enum, as_str, FromStr).
2. Add `AiBindingPurpose::QueryCompile` to `ai.rs` (enum, as_str, `for_runtime_task_kind`).
3. Add `RuntimeStageKind::Compile` to `agent_runtime.rs`.
4. Create `migrations/0002_query_compile.sql` with `ALTER TYPE` statements.
5. Update `bootstrap.rs` to seed QueryCompile binding (mirror QueryAnswer).
6. Create `services/query/compiler.rs` with `call_query_compiler<Res>()`:
   - Resolve selection via `AiBindingPurpose::QueryCompile`
   - Build ChatRequest with schema response_format
   - Call `UnifiedGateway.generate()`
   - Deserialize to `Res`
7. Implement provider-specific response_format builders in `integrations/llm.rs` (strict for OpenAI, json_object fallback).
8. QueryCompile integration test (mock provider, fixed schema, verify JSON parsing).
9. Update `ai_catalog_service/tests.rs` to include QueryCompile purpose.
10. Document entry point, provider support, fallback behaviour.

**Bottom line:** OpenAI-compatible infrastructure, `response_format` passthrough, and binding/profile resolution are already production-ready. Only additions needed: enum variants, SQL migration, bootstrap seed, and the `call_query_compiler<Res>()` DX wrapper. Real complexity is provider-specific fallback for non-OpenAI strict-schema support (esp. Ollama).
