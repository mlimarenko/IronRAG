# Running IronRAG with Ollama

This guide documents the local Ollama integration: which models work for
which binding purpose, how PostgreSQL vector storage interacts with the
embedding dimension you pick, operational gotchas, and an example profile set
for a 12 GB consumer GPU.

## Why Ollama

Ollama exposes an OpenAI-compatible API at `http://<host>:11434/v1`, so
IronRAG can talk to it through the same `openai_compatible` provider
adapter that handles cloud OpenAI/DeepSeek/OpenRouter. There is no
Ollama-specific code path in IronRAG; everything described here is
configuration.

Pick Ollama for any binding purpose where you want the inference cost
to stay on local hardware: ingest stages (`embed_chunk`,
`extract_graph`, and optional `extract_text`) are the obvious wins because
they run once per revision and the latency is hidden by the worker
queue. Keep `query_answer` on a frontier cloud model if you can — that
stage runs on every user turn and answer quality is what they perceive.

The configuration contract remains exactly five required profiles:
`extract_graph`, `embed_chunk`, `query_compile`, `query_answer`, and `agent`.
The multimodal `extract_text` profile and the `query_rerank` profile for
provider-backed semantic rerank are optional regardless of which profiles run
locally.

## Provider registration

The Ollama provider catalog row ships with the IronRAG bootstrap. Its base URL
is required and may point at a private network; an API key is not required.
Create an AI account with the URL reachable from the backend container, then
use its OpenAI-compatible `/v1/models` endpoint for model discovery.

The bootstrap catalog has the stable embedding entries `nomic-embed-text`
(768 dimensions) and `mxbai-embed-large` (1024 dimensions). Chat models are
operator-supplied because Ollama serves the models pulled on that host. Do not
depend on a fixed provider-catalog UUID: the catalog source defines no such
deployment-independent identifier. A multimodal model for document
understanding must be configured for the optional `extract_text` binding; there
is no separate document-analysis binding alias.

## Example profile set (12 GB VRAM, single GPU)

Example profile set:

| Purpose       | Model                  | Latency | Quality                                | VRAM |
|---------------|------------------------|---------|----------------------------------------|------|
| `embed_chunk` | `qwen3-embedding:0.6b` | 59 ms   | 1024-dim, code-aware                   | 1 GB |
| `extract_graph` | `llama3.1:8b`        | 3.1 s   | JSON_OK, 11 entities / 8 relations     | 5.5 GB |
| `extract_text` | `qwen3-vl:4b`         | n/a     | multimodal chat (kept for PDF OCR)     | 3.3 GB |
| `query_answer` | cloud model           | —       | unchanged                              | 0    |

Models with known limitations:

- **`qwen3:4b` / `qwen3:8b`** — emit ~800 tokens of `<thinking>…</thinking>`
  preamble before any structured output. Ollama does not currently
  respect the `/no_think` directive over the OpenAI-compatible API.
  Result is empty JSON every time. Skip until Ollama supports the
  thinking-budget flag for this family.
- **`phi4-mini`** — fast (~2 s) and valid JSON, but only 5 entities
  versus llama3.1's 11 on the same prompt. Use it if you need raw speed
  more than recall.
- **`gemma3:4b`** — high cold-start latency (~66 s on the first call,
  ~3 s warm), wraps JSON in a markdown fence that the parser strips
  but other extractors might not. Workable, not better than llama3.1.

## VRAM budget and model swapping

12 GB cannot hold all three picks simultaneously (1 + 5.5 + 3.3 = 9.8 GB
of weights plus context-window buffers). Ollama unloads idle models
after `OLLAMA_KEEP_ALIVE` (5 min by default), which is fine for
ingest's sequential stages: embed runs first, then graph extract,
then optional visual analysis through Document Understanding. Each stage warm-loads its model on the first
chunk and serves the rest of the batch from VRAM.

If you see a stage stall for ~5–10 s every 5 minutes that is the model
reload. Set `OLLAMA_KEEP_ALIVE` in the Ollama deployment configuration to
suppress it under steady load.

## Context window and idle timeout

Ollama's default `num_ctx` for chat models is **4096 tokens**. Markdown
files like long READMEs or design docs blow past that and end up
truncated mid-chunk; graph extraction then misses entities that fell
off the end. Set `num_ctx` on the relevant binding:

```json
{
  "temperature": 0.0,
  "extraParametersJson": { "num_ctx": 8192 }
}
```

8192 is enough for IronRAG's default chunk size and any
reasonable system prompt. Embedding models are unaffected (they don't
use chat context); set `num_ctx: 2048` for them just for cleanliness.

The other knob is IronRAG's per-revision idle timeout. The default
`runtime_graph_extract_idle_timeout_seconds = 300` is calibrated for
cloud throughput. With local llama3.1:8b at ~3 s per chunk, a 100-chunk
markdown file needs ~5 minutes of continuous progress just to fit
inside the timeout — any GPU contention or model swap pushes it over.
Bump to **1800 s** for local-Ollama runs:

```bash
# .env — read automatically by docker compose
IRONRAG_RUNTIME_GRAPH_EXTRACT_IDLE_TIMEOUT_SECONDS=1800
```

Restart `backend` *and* `worker` together, then restart or recreate
`frontend` as well. The frontend nginx upstream resolves `backend` at
startup, so a backend container recreate can leave `/v1/*` proxying to a
stale Docker IP until nginx restarts.

## Vector dimensions: per library

PostgreSQL stores chunk and entity embeddings in per-`(library, dim)`
pgvector relations tracked by a vector manifest. A deployment can hold
libraries with different active embedding dimensions at the same time.

What this means in practice:

- The active `embed_chunk` profile for a library determines that library's
  embedding dimension and coordinate space for both stored and query vectors.
- Switching one library from a 3072-dimensional embedding model to a
  1024-dimensional Ollama embedding model does not require the whole
  deployment to use 1024 dimensions.
- Existing vector material for the affected library still has to be
  rebuilt before retrieval uses the new embedding model.

### Running the rebuild

The binary ships inside the backend image:

```bash
docker exec ironrag-backend-1 \
  ironrag-maintenance rebuild vector-plane --source-library <library-uuid>
```

Pick the library whose active binding should drive the rebuild. The
rebuild updates vector material for the affected `(library, dim)` lane:

```
vector-plane rebuild completed
  library_id=…  previous_dimensions=Some(3072)  target_dimensions=1024
  libraries_rebuilt=1
  chunk_embeddings_rebuilt=820  graph_node_embeddings_rebuilt=0
```

## Failure modes specific to local Ollama

| Symptom | Cause | Fix |
|---|---|---|
| `ProviderUnavailable: failed to resolve chunk embedding dimensions for <uuid>` during ingest | The embedding model returned vectors whose profile does not match the active library vector lane | Check the library's `embed_chunk` profile, then run `ironrag-maintenance rebuild vector-plane --source-library` for the affected library. |
| `graph extraction idle timeout: no chunk completed for revision … within 300s` | Local LLM is slower than the timeout assumes | Bump `IRONRAG_RUNTIME_GRAPH_EXTRACT_IDLE_TIMEOUT_SECONDS` in the docker-compose env. Worker restart required. |
| qwen3:* extract bindings return empty JSON | Model emits 800 tokens of `<thinking>` before content; OpenAI-compatible API does not honor `/no_think` | Pick a non-thinking model (llama3.1:8b, phi4-mini, gemma3:4b). |
| First call after 5 min idle is ~10× slower | `OLLAMA_KEEP_ALIVE=5m` evicted the model from VRAM | Increase `OLLAMA_KEEP_ALIVE` in Ollama's compose file, or accept the warmup as part of cold-start latency. |
| Document Understanding times out on small images | `qwen3-vl:4b` ships with `num_ctx=4096`; some PDF page images encode to longer prompts | Set `num_ctx: 8192` in the `extract_text` binding's `extraParametersJson`. |
| Backend health says OK at port 19000 but every `/v1/*` returns 404 | Frontend nginx is still pointing at a stale backend Docker IP after a `--force-recreate backend` | Recreate the frontend too — backend and frontend must be recreated together. |

## Recommended Ollama runtime tuning

In the Ollama deployment configuration:

```yaml
environment:
  OLLAMA_HOST: 0.0.0.0:11434
  OLLAMA_KEEP_ALIVE: 30m     # avoid mid-batch evictions
  OLLAMA_NUM_PARALLEL: 2      # tune to your VRAM/GPU
  OLLAMA_MAX_LOADED_MODELS: 2 # >1 only if VRAM allows; otherwise let it swap
  NVIDIA_VISIBLE_DEVICES: all
  NVIDIA_DRIVER_CAPABILITIES: compute,utility
```

`OLLAMA_MAX_LOADED_MODELS=2` lets you keep embedding + LLM warm
simultaneously when there is headroom. On a 12 GB card with the profile set
above this is borderline — `qwen3-embedding:0.6b` (1 GB) plus
`llama3.1:8b` (5.5 GB) fits if no other process is competing, but a
parallel multimodal extraction call will evict one of them.
