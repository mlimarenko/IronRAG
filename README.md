<p align="center">
  <img src="./docs/assets/ironrag-logo.svg" alt="IronRAG logo" width="180">
</p>

<p align="center">
  <img src="./docs/assets/readme-flow.gif" alt="RustRAG demo: dashboard, documents, grounded assistant, and graph exploration" width="840">
</p>

<h1 align="center">RustRAG</h1>
<p align="center">One-click knowledge system for documents, internal bots, and AI agents</p>

<p align="center">
  <a href="https://github.com/mlimarenko/RustRAG/stargazers"><img src="https://img.shields.io/github/stars/mlimarenko/RustRAG?style=flat-square" alt="Stars"></a>
  <a href="https://github.com/mlimarenko/RustRAG/releases"><img src="https://img.shields.io/github/v/release/mlimarenko/RustRAG?style=flat-square" alt="Release"></a>
  <a href="https://hub.docker.com/r/pipingspace/rustrag-backend"><img src="https://img.shields.io/docker/pulls/pipingspace/rustrag-backend?style=flat-square" alt="Docker Pulls"></a>
  <a href="./LICENSE"><img src="https://img.shields.io/github/license/mlimarenko/RustRAG?style=flat-square" alt="License"></a>
</p>

<p align="center">
  <a href="./README-RU.md">README-RU</a> &bull;
  <a href="./MCP.md">MCP</a> &bull;
  <a href="./MCP-RU.md">MCP-RU</a>
</p>

---

Load files, links, and images into one knowledge base, turn them into searchable text, embeddings, and graph relations, then expose the same memory in the operator UI and over MCP.

## Architecture

One public port on **web**. The frontend container serves the **React + Vite** SPA and proxies `/v1/*` to the **Rust / Axum** API. The same backend image also runs as the **worker** and the one-shot **startup authority** responsible for migrations, Arango bootstrap, and storage initialization. `s4core` is optional and is only used by the S3 compose/Helm profiles.

```text
                         ┌─────────────────────────┐
                         │    web (SPA + /v1)      │
                         └───────────┬─────────────┘
               ┌─────────────────────┴─────────────────────┐
        GET /* (SPA)                                 /v1/* (API + MCP)
      ┌────────▼─────────┐                       ┌─────────▼──────────┐
      │    frontend      │                       │      backend       │
      │  React + Vite    │                       │   Rust / Axum      │
      └──────────────────┘                       └─────────┬──────────┘
                         ┌─────────────────────────────────┼────────────────────┐
                  ┌──────▼──────┐                  ┌───────▼───────┐    ┌───────▼───────┐
                  │  ArangoDB   │                  │   Postgres    │    │    Redis      │
                  │ graph+vector│                  │ IAM + control │    │ worker queue  │
                  └─────────────┘                  └───────────────┘    └───────┬───────┘
                                           ┌────────────────────────────┴───────┬───────────────┐
                                           │                                    │               │
                                     ┌─────▼─────┐                        ┌─────▼─────┐   ┌────▼────┐
                                     │  startup  │                        │  worker   │   │ s4core  │
                                     └───────────┘                        └───────────┘   └─────────┘
```

## Pipeline

```text
upload / URL → extract text → structured blocks → boilerplate filter
  → semantic chunking (2800 chars, 10% overlap, heading-aware)
  → embed chunks → graph extraction (v6: 10 entity types, 88 relation types)
  → entity resolution (alias/acronym merge) → document summary
  → quality scoring → hybrid index (BM25 + vector) → UI + MCP + API
```

## Deploy

Prerequisite: Docker with Compose v2, or Kubernetes with Helm.

```bash
# Install without cloning
curl -fsSL https://raw.githubusercontent.com/mlimarenko/RustRAG/master/install.sh | bash

# Or from a cloned repo
cp .env.example .env
docker compose up -d
```

Compose profiles:

- `docker-compose.yml` — bundled Postgres/Redis/ArangoDB + filesystem storage
- `docker-compose-s4.yml` — bundled Postgres/Redis/ArangoDB + bundled `s4core` + S3 storage
- `docker-compose-local.yml` — source build for local development

Examples:

```bash
docker compose up -d
docker compose -f docker-compose-s4.yml up -d
docker compose -f docker-compose-local.yml up --build -d
```

Default UI URL: [http://127.0.0.1:19000](http://127.0.0.1:19000)

Helm:

```bash
OPENAI_API_KEY=... \
helm upgrade --install rustrag charts/rustrag \
  --namespace rustrag \
  --create-namespace \
  --values charts/rustrag/values/examples/bundled-s3.yaml \
  --set-string app.frontendOrigin=https://rustrag.example.com \
  --set-string app.providerSecrets.openaiApiKey="${OPENAI_API_KEY}" \
  --wait \
  --wait-for-jobs \
  --timeout 20m
```

External dependencies:

```bash
helm upgrade --install rustrag charts/rustrag \
  --namespace rustrag \
  --create-namespace \
  --values charts/rustrag/values/examples/external-services.yaml
```

Chart profiles:

- `bundled-s3.yaml` — bundled Postgres/Redis/ArangoDB + bundled `s4core`
- `external-services.yaml` — external Postgres/Redis/ArangoDB/S3
- `filesystem-single-node.yaml` — single-node filesystem mode only

Minikube is used only for local chart validation. It is not a deployment profile.

Full runtime reference: [apps/api/.env.example](./apps/api/.env.example)

## Features

- **Document ingestion** -- text, code (50+ extensions), PDF, DOCX, PPTX, HTML, images, and web links with boilerplate filtering and quality scoring
- **Typed knowledge graph** -- 10 universal entity types (person, organization, location, event, artifact, natural, process, concept, attribute, entity), 88 relation types, entity resolution, and document summaries
- **Hybrid search** -- BM25 + vector cosine via Reciprocal Rank Fusion, field-weighted scoring (heading matches boosted 1.5x)
- **Grounded assistant** -- built-in chat UI with answer verification and evidence panel
- **21 MCP tools** -- Q&A (`ask`), search, read, upload, graph exploration, web crawl, and admin
- **Smart chunking** -- 2800-char semantic chunks with 10% overlap, heading-aware splitting, code-aware boundaries, boilerplate detection
- **Access control** -- API tokens, grants, library scoping, and ready-made MCP client snippets
- **Spending tracking** -- per-document and per-library cost visibility
- **Model selection** -- configurable providers and models per pipeline stage

## MCP

21 tools out of the box. Create a token in **Admin > Access**, attach grants, copy the snippet from **Admin > MCP**.

| Category | Tools |
|----------|-------|
| **Q&A** | `ask` -- grounded question answering |
| **Documents** | `search_documents`, `read_document`, `list_documents`, `upload_documents`, `update_document`, `delete_document` |
| **Graph** | `search_entities`, `get_graph_topology`, `list_relations` |
| **Web Crawl** | `submit_web_ingest_run`, `get_web_ingest_run`, `cancel_web_ingest_run` |
| **Discovery** | `list_workspaces`, `list_libraries` |

Search and read responses default to `includeReferences=false` to minimize token usage. Full guide: [MCP.md](./MCP.md)

## Tech Stack

| Layer | Technology |
|-------|-----------|
| API + Worker | Rust, Axum, SQLx |
| Frontend | React, Vite, Tailwind, shadcn/ui |
| Graph + Vector | ArangoDB 3.12 |
| Control Plane | PostgreSQL 18 |
| Worker Queue | Redis 8 |
| Edge / SPA | nginx 1.28 inside `web` |
| Deployment | Helm, Docker Compose, Ansible |

## Configuration

All variables use `RUSTRAG_*` prefix. Key files:

| File | Purpose |
|------|---------|
| `.env.example` | Compose variables |
| `apps/api/.env.example` | Full runtime config reference |
| `apps/api/src/app/config.rs` | Built-in defaults |

## Benchmarks

Two golden datasets: Wikipedia corpus (30 questions) and code corpus (20 questions across Go/TS/Python/Rust/Terraform/React/K8s/Docker).

```bash
export RUSTRAG_SESSION_COOKIE="..."
export RUSTRAG_BENCHMARK_WORKSPACE_ID="workspace-uuid"
make benchmark-grounded-seed   # upload corpus
make benchmark-grounded-all    # run QA matrix
make benchmark-golden          # golden dataset
```

## Roadmap

### 0.2.0 -- Quality & Performance (done)

- [x] Hybrid search (BM25 + vector RRF fusion)
- [x] Graph extraction v6 (few-shot, 10 entity types, 88 relation types)
- [x] Semantic chunking (2800 chars, overlap, heading-aware, code-aware)
- [x] Boilerplate detection, quality scoring, entity resolution
- [x] 21 MCP tools including `ask` and graph navigation
- [x] Typed entity coloring and edge labels in graph UI
- [x] Parallel graph extraction (up to 8 concurrent chunks)
- [ ] SSE streaming for query answers
- [ ] Conversation context in multi-turn queries
- [ ] Incremental re-processing (diff-aware ingest)
- [ ] Export/import libraries
- [x] Ollama/local model support
  Verified live with Ollama `qwen3:4b`; stale-model detection verified against missing `qwen3:0.6b`.
- [ ] Confluence, Notion, Google Drive connectors

## Star History

<p align="center">
  <a href="https://star-history.com/#mlimarenko/RustRAG&Date">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=mlimarenko/RustRAG&type=Date&theme=dark" />
      <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=mlimarenko/RustRAG&type=Date" />
      <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=mlimarenko/RustRAG&type=Date" width="700" />
    </picture>
  </a>
</p>

## Contributing

PRs welcome. Prefer the one canonical path over compatibility layers.

## License

[MIT](./LICENSE)
