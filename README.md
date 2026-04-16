



# IronRAG

Production-grade knowledge memory for AI agents and teams



[English docs](./docs/en/README.md) • [Документация](./docs/ru/README.md) • [MCP](./docs/en/MCP.md) • [CLI](./docs/en/CLI.md) • [IAM](./docs/en/IAM.md)

---

IronRAG turns documents, code, PDFs, spreadsheets, and web pages into a structured knowledge base with a typed knowledge graph. AI agents query it over MCP; humans use the built-in UI. One self-hosted system -- your data stays on your infrastructure.

### Why IronRAG

- **Knowledge graph, not just vectors.** Entities, typed relationships, evidence chains, and document links -- agents reason over structure, not noisy similarity hits.
- **MCP server out of the box.** 21 tools for search, document reading, graph traversal, and web ingestion. Connect Claude, Cursor, VS Code, or any MCP client in one line.
- **Any provider.** OpenAI, DeepSeek, Qwen, or Ollama for fully local inference. Mix freely -- DeepSeek for reasoning, OpenAI for embeddings, Ollama for air-gapped environments.
- **Cost tracking.** Per-document extraction cost and per-query execution cost. Workspace-level price overrides.
- **Fine-grained IAM.** Scoped tokens at system, workspace, or library level. Permission groups control who reads, writes, or connects agents.
- **Code-aware.** 15-language AST parsing via tree-sitter. Config parsers for JSON, YAML, TOML. Technical fact extraction for endpoints, env vars, error codes.
- **Scales.** Tested on 5000+ documents, 25k+ graph nodes, 100k+ edges. Batched DB operations, streaming exports, memory-aware worker throttling.
- **Full backup/restore.** One-click tar.zst archive. Selective export (data only or with source files). Restore to the same or different deployment.

## Quick start

```bash
# One-line install (Docker required)
curl -fsSL https://raw.githubusercontent.com/mlimarenko/IronRAG/master/install.sh | bash
```

Or from source:

```bash
git clone https://github.com/mlimarenko/IronRAG.git
cd IronRAG/ironrag
cp .env.example .env          # add IRONRAG_OPENAI_API_KEY=sk-...
docker compose up -d
```

Open [http://127.0.0.1:19000](http://127.0.0.1:19000), create an admin account, upload a document, ask a question.

For fully local inference without cloud providers -- configure Ollama in Admin > AI.

### Other deployment options

```bash
# With S3-compatible storage (bundled s4core)
docker compose -f docker-compose-s4.yml up -d

# Local source build for development
docker compose -f docker-compose-local.yml up --build -d
```

Helm (Kubernetes):

```bash
helm upgrade --install ironrag charts/ironrag \
  --namespace ironrag --create-namespace \
  --set-string app.providerSecrets.openaiApiKey="${OPENAI_API_KEY}" \
  --wait --timeout 20m
```

## How it works

```
Documents ──> Parsing ──> Chunking ──> Embedding ──> Vector Index
                 |                        |
                 └──> Graph Extraction ──> Knowledge Graph
                         (LLM)              (ArangoDB)
                                               |
Query ──> Hybrid Search ──> Graph Traversal ──> Context Assembly ──> LLM Answer
           (BM25 + Vector)                        |
                                          Verification ──> Grounded Response
```

## Tech stack


| Layer           | Technology                                       |
| --------------- | ------------------------------------------------ |
| Backend         | Rust, Axum, tokio, SQLx                          |
| Frontend        | React, Vite, TypeScript, Tailwind, shadcn/ui     |
| Graph rendering | Sigma.js + Graphology (WebGL, Web Worker layout) |
| Knowledge graph | ArangoDB                                         |
| Document store  | PostgreSQL                                       |
| Job queue       | Redis                                            |
| Code parsing    | tree-sitter (15 languages)                       |
| Deployment      | Docker Compose, Helm                             |


## MCP tools

21 tools out of the box. Create a token in **Admin > Access**, copy the snippet from **Admin > MCP**.


| Category  | Tools                                                                                                           |
| --------- | --------------------------------------------------------------------------------------------------------------- |
| Documents | `search_documents`, `read_document`, `list_documents`, `upload_documents`, `update_document`, `delete_document` |
| Graph     | `search_entities`, `get_graph_topology`, `list_relations`                                                       |
| Web crawl | `submit_web_ingest_run`, `get_web_ingest_run`, `cancel_web_ingest_run`                                          |
| Q&A       | `ask` (grounded answer in a single call)                                                                        |
| Discovery | `list_workspaces`, `list_libraries`                                                                             |


## Documentation


|          | English                           | Russian                           |
| -------- | --------------------------------- | --------------------------------- |
| Overview | [README](./docs/en/README.md)     | [README](./docs/ru/README.md)     |
| Pipeline | [PIPELINE](./docs/en/PIPELINE.md) | [PIPELINE](./docs/ru/PIPELINE.md) |
| MCP      | [MCP](./docs/en/MCP.md)           | [MCP](./docs/ru/MCP.md)           |
| IAM      | [IAM](./docs/en/IAM.md)           | [IAM](./docs/ru/IAM.md)           |
| CLI      | [CLI](./docs/en/CLI.md)           | [CLI](./docs/ru/CLI.md)           |


## Star History



## License

[MIT](./LICENSE)