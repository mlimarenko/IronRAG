<p align="center">
  <img src="./docs/assets/ironrag-logo.svg" alt="IronRAG logo" width="180">
</p>

<p align="center">
  <img src="./docs/assets/readme-flow.gif" alt="IronRAG demo: dashboard, documents, grounded assistant, and graph exploration">
</p>

<h1 align="center">IronRAG</h1>
<p align="center">One-click knowledge system for documents, internal bots, and AI agents</p>

<p align="center">
  <a href="https://github.com/mlimarenko/IronRAG/stargazers"><img src="https://img.shields.io/github/stars/mlimarenko/IronRAG?style=flat-square" alt="Stars"></a>
  <a href="https://github.com/mlimarenko/IronRAG/releases"><img src="https://img.shields.io/github/v/release/mlimarenko/IronRAG?style=flat-square" alt="Release"></a>
  <a href="https://hub.docker.com/r/pipingspace/ironrag-backend"><img src="https://img.shields.io/docker/pulls/pipingspace/ironrag-backend?style=flat-square" alt="Docker Pulls"></a>
  <a href="./LICENSE"><img src="https://img.shields.io/github/license/mlimarenko/IronRAG?style=flat-square" alt="License"></a>
</p>

<p align="center">
  <a href="./README-RU.md">README-RU</a> &bull;
  <a href="./docs/en/MCP.md">MCP</a> &bull;
  <a href="./docs/en/CLI.md">CLI</a> &bull;
  <a href="./docs/en/IAM.md">IAM</a>
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

### Table Pipeline

- **One canonical path** -- `csv`, `xls`, `xlsx`, `xlsb`, `ods`, and table blocks extracted from `docx`/`pdf` are normalized into the same markdown-table representation before ingest.
- **Row-first retrieval** -- each table row is stored as self-describing normalized text (`Sheet: ... | Row N | Header: value | ...`), so row-level questions work without relying on the whole table being co-retrieved.
- **Summary-grounded analytics** -- per-column table summaries persist `average`, `min/max`, `distinct count`, and `most frequent` signals, and aggregate questions are answered from those summaries instead of ad hoc LLM guessing.
- **Sparse graph by default for low-signal sheets** -- pure numeric or headerless tables stay retrieval-friendly but graph-sparse; semantic business rows remain graph-eligible and can still build useful entities and relations.

## Deploy

Prerequisite: Docker with Compose v2, or Kubernetes with Helm.

```bash
# Install without cloning
curl -fsSL https://raw.githubusercontent.com/mlimarenko/IronRAG/master/install.sh | bash

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
helm upgrade --install ironrag charts/ironrag \
  --namespace ironrag \
  --create-namespace \
  --values charts/ironrag/values/examples/bundled-s3.yaml \
  --set-string app.frontendOrigin=https://ironrag.example.com \
  --set-string app.providerSecrets.openaiApiKey="${OPENAI_API_KEY}" \
  --wait \
  --wait-for-jobs \
  --timeout 20m
```

External dependencies:

```bash
helm upgrade --install ironrag charts/ironrag \
  --namespace ironrag \
  --create-namespace \
  --values charts/ironrag/values/examples/external-services.yaml
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
- **21 MCP tools** -- search, read, upload, graph exploration, web crawl, and admin (the in-app assistant uses these tools as a normal MCP client)
- **Smart chunking** -- 2800-char semantic chunks with 10% overlap, heading-aware splitting, code-aware boundaries, boilerplate detection
- **Access control** -- API tokens with 13 permission kinds, hierarchical scopes (system/workspace/library/document), and grant-based MCP tool filtering
- **Admin CLI** -- `ironrag-cli` for user/token management, workspace/library provisioning, and data export/import without HTTP
- **Spending tracking** -- per-document and per-library cost visibility
- **Model selection** -- configurable providers and models per pipeline stage

## MCP

21 tools out of the box. Create a token in **Admin > Access**, attach grants, copy the snippet from **Admin > MCP**.

| Category | Tools |
|----------|-------|
| **Documents** | `search_documents`, `read_document`, `list_documents`, `upload_documents`, `update_document`, `delete_document` |
| **Graph** | `search_entities`, `get_graph_topology`, `list_relations` |
| **Web Crawl** | `submit_web_ingest_run`, `get_web_ingest_run`, `cancel_web_ingest_run` |
| **Discovery** | `list_workspaces`, `list_libraries` |

Search and read responses default to `includeReferences=false` to minimize token usage. Full guide: [MCP](./docs/en/MCP.md) | [IAM & Tokens](./docs/en/IAM.md) | [CLI](./docs/en/CLI.md)

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

All variables use `IRONRAG_*` prefix. Key files:

| File | Purpose |
|------|---------|
| `.env.example` | Compose variables |
| `apps/api/.env.example` | Full runtime config reference |
| `apps/api/src/app/config.rs` | Built-in defaults |

## Benchmarks

Two golden datasets: Wikipedia corpus (30 questions) and code corpus (20 questions across Go/TS/Python/Rust/Terraform/React/K8s/Docker).

```bash
export IRONRAG_SESSION_COOKIE="..."
export IRONRAG_BENCHMARK_WORKSPACE_ID="workspace-uuid"
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
- [x] 21 MCP tools for search, read, upload, and graph navigation
- [x] Typed entity coloring and edge labels in graph UI
- [x] Parallel graph extraction (up to 8 concurrent chunks)
- [x] SSE streaming for query answers
- [x] Conversation context in multi-turn queries
- [x] Incremental re-processing (diff-aware ingest)
- [x] Export/import libraries
- [x] Admin CLI (`ironrag-cli`) with granular token permissions
- [x] Ollama/local model support, verified live with Ollama `qwen3:4b`; stale-model detection verified against missing `qwen3:0.6b`.

### Next up

- [ ] Managed connectors for project workspaces: GitHub repositories, S3 buckets, and durable web sources with scoped grants and explicit sync lifecycle.

## Star History

<p align="center">
## Star History

  <a href="https://www.star-history.com/?repos=mlimarenko%2FIronRAG&type=date&legend=top-left">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/chart?repos=mlimarenko/IronRAG&type=date&theme=dark&legend=top-left" />
    <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/chart?repos=mlimarenko/IronRAG&type=date&legend=top-left" />
    <img alt="Star History Chart" src="https://api.star-history.com/chart?repos=mlimarenko/IronRAG&type=date&legend=top-left" />
  </picture>
  </a>
</p>

## Contributing

PRs welcome. Prefer the one canonical path over compatibility layers.

## License

[MIT](./LICENSE)
