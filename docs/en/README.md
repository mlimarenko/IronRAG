<p align="center">
  <img src="../assets/ironrag-logo.svg" height="64" alt="IronRAG" />
</p>

<h1 align="center">IronRAG</h1>

<p align="center">
  Production-grade knowledge memory for AI agents and teams.<br/>
  Upload documents. Build a knowledge graph. Ask questions. Ship agents.
</p>

<p align="center">
  <img src="../assets/readme-flow.gif" width="720" alt="IronRAG pipeline" />
</p>

---

## What is IronRAG?

IronRAG turns your documents, code, PDFs, spreadsheets, and web pages into a structured knowledge base that AI agents and humans can query instantly. It is a self-hosted, open-source system that runs on your infrastructure and keeps your data under your control.

Unlike simple vector databases, IronRAG builds a **knowledge graph** from your content: entities, relationships, evidence chains, and document links. Agents that connect to IronRAG don't just search text -- they reason over structured knowledge.

## Why IronRAG?

**For AI engineers building production agents:**

- **MCP server out of the box.** Connect Claude, Cursor, VS Code, or any MCP-compatible agent in one line. 21 tools covering search, document reading, graph traversal, and web ingestion -- all permission-gated per token.
- **Structured memory, not just embeddings.** The knowledge graph captures entities, typed relationships, and evidence with support ranking. Agents get grounded context, not noisy similarity hits.
- **Multi-provider flexibility.** Use OpenAI, DeepSeek, Qwen, or **Ollama for fully local inference** -- no cloud dependency required. Mix providers freely: DeepSeek for reasoning, OpenAI for embeddings, Ollama for privacy-sensitive workloads.
- **Cost tracking per query and document.** Every LLM call is metered. See per-document extraction cost and per-query execution cost in the dashboard. Set workspace-level price overrides.

**For teams managing knowledge:**

- **Upload anything.** PDF, DOCX, PPTX, XLSX, CSV, Markdown, HTML, source code (15 languages with AST parsing), images (via vision models), and web pages (single-page or recursive crawl).
- **Knowledge graph visualization.** Interactive WebGL graph with 60fps rendering at 25k+ nodes. Entity types, sub-types, relationship exploration, drag, zoom, filter by type.
- **Grounded answers with sources.** Every answer cites specific document sections. Verification guardrails reject unsupported claims.
- **Full backup and restore.** One-click tar.zst archive export with selective inclusion. Restore to the same or different deployment. Designed for GitLab-style backup workflows.

**For ops teams running production:**

- **Fine-grained IAM.** Scoped tokens at system, workspace, or library level. Permission groups control who can read, write, admin, or connect agents.
- **Scales with your data.** Tested on libraries with 5000+ documents, 25k+ graph nodes, 82k+ edges. Batched database operations, streaming exports, connection pool tuning, and memory-aware worker throttling.
- **Observable.** Prometheus metrics, structured tracing, audit log with surface/result filters, per-document pipeline stage timings.
- **Single Docker Compose.** Postgres, ArangoDB, Redis, backend, worker, frontend -- all in one `docker compose up -d`. Helm chart available for Kubernetes.

## How it works

```
Documents ──> Parsing ──> Chunking ──> Embedding ──> Vector Index
                 │                        │
                 └──> Graph Extraction ──> Knowledge Graph
                         (LLM)               (ArangoDB)
                                                │
Query ──> Hybrid Search ──> Graph Traversal ──> Context Assembly ──> LLM Answer
            (BM25 + Vector)                        │
                                           Verification ──> Grounded Response
```

1. **Upload** a document (API, UI, MCP, or web crawl).
2. **Parse** into structured blocks (headings, paragraphs, tables, code, images).
3. **Extract** entities and relationships via LLM -- builds the knowledge graph.
4. **Embed** chunks for vector similarity search.
5. **Query** combines vector search, BM25 lexical search, and graph traversal.
6. **Answer** is generated from assembled context and verified against source evidence.

## Tech stack

| Layer | Technology |
|-------|-----------|
| Backend | Rust, Axum, tokio |
| Frontend | React, Vite, TypeScript, Tailwind, shadcn/ui |
| Graph rendering | Sigma.js, Graphology (WebGL, Web Worker layout) |
| Document store | PostgreSQL |
| Knowledge graph | ArangoDB |
| Job coordination | Redis |
| Code parsing | tree-sitter (15 languages) |
| Backup format | tar.zst (streaming, chunked NDJSON) |

## Quick start

```bash
git clone https://github.com/mlimarenko/IronRAG.git
cd IronRAG/ironrag
cp .env.example .env
# Add your API key: IRONRAG_OPENAI_API_KEY=sk-...
docker compose up -d
```

Open [http://127.0.0.1:19000](http://127.0.0.1:19000), create an admin account, upload a document, and ask a question.

For local-only inference without any cloud provider, configure Ollama bindings in the Admin panel.

## Documentation

| Topic | Link |
|-------|------|
| Ingestion pipeline | [PIPELINE.md](./PIPELINE.md) |
| MCP integration | [MCP.md](./MCP.md) |
| IAM & tokens | [IAM.md](./IAM.md) |
| CLI reference | [CLI.md](./CLI.md) |
| Frontend architecture | [FRONTEND.md](./FRONTEND.md) |
| Benchmarks | [BENCHMARKS.md](./BENCHMARKS.md) |

## Helm install

```bash
helm upgrade --install ironrag charts/ironrag \
  --namespace ironrag --create-namespace \
  --set-string app.providerSecrets.openaiApiKey="${OPENAI_API_KEY}" \
  --wait --timeout 20m
```

## License

[MIT](../../LICENSE)
