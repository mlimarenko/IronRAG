<p align="center">
  <img src="./docs/assets/readme-flow.gif" alt="RustRAG demo: dashboard, documents, grounded assistant, and graph exploration" width="960">
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

> RustRAG is a practical knowledge system for LLMs: one `docker compose up`, one web app, one MCP endpoint, and one canonical pipeline for internal assistants, support bots, and private agent workflows.

## Architecture

One published port terminates at **nginx**. **`/`** is the **React + Vite** SPA (static assets from the frontend image); **`/v1/*`** is the **Rust / Axum** backend (**REST + MCP**; **`/mcp`** redirects to **`/v1/mcp`**). The **worker** is the same backend image with a queue consumer role.

```text
                         ┌─────────────────────────┐
                         │   nginx (edge proxy)    │
                         │   single host:port      │
                         └───────────┬─────────────┘
               ┌─────────────────────┴─────────────────────┐
               │                                           │
        GET /* (SPA)                                 /v1/* (API + MCP)
               │                                           │
      ┌────────▼─────────┐                       ┌─────────▼──────────┐
      │    frontend      │                       │      backend       │
      │  React + Vite    │                       │   Rust / Axum      │
      │  static bundle   │                       │   API + MCP        │
      └──────────────────┘                       └─────────┬──────────┘
                                                           │
                         ┌─────────────────────────────────┼─────────────────────────────┐
                         │                                 │                             │
                  ┌──────▼──────┐                  ┌───────▼───────┐             ┌───────▼───────┐
                  │  ArangoDB   │                  │   Postgres    │             │    Redis      │
                  │ graph+vector│                  │ IAM + control │             │ worker queue  │
                  └─────────────┘                  └───────────────┘             └───────┬───────┘
                                                                                         │
                                                                                ┌────────▼────────┐
                                                                                │     worker      │
                                                                                │ (same image,    │
                                                                                │  ingest jobs)   │
                                                                                └─────────────────┘
```

Backend and worker both use Postgres, Redis, ArangoDB, and the shared content volume.

**Ingestion pipeline:** upload → extract text → chunk → embed → merge entities/relations → graph + search → operator UI and MCP tools.

## Quick Start

Prerequisite: Docker with Compose v2.

### Install the published release without cloning

Latest release:

```bash
curl -fsSL https://raw.githubusercontent.com/mlimarenko/RustRAG/master/install.sh | bash
```

Specific tag:

```bash
curl -fsSL https://raw.githubusercontent.com/mlimarenko/RustRAG/master/install.sh | bash -s -- 0.1.0
```

Creates `./rustrag` from the release and starts the stack. First `.env`: random `RUSTRAG_POSTGRES_PASSWORD`, `RUSTRAG_ARANGODB_PASSWORD`, `RUSTRAG_BOOTSTRAP_TOKEN`; later runs keep them.

### Run prebuilt images from a cloned repository

```bash
cp .env.example .env
docker compose up -d
```

### Build from source

```bash
cp .env.example .env
docker compose -f docker-compose-local.yml up --build -d
```

### Remote host (Ansible)

```bash
ansible-playbook -i '203.0.113.10,' ansible/deploy.yml -b -u deploy \
  -e rustrag_install_dir=/opt/rustrag \
  -e rustrag_public_host=rag.example.com
```

### After startup

- App + API: [http://127.0.0.1:19000](http://127.0.0.1:19000)
- MCP JSON-RPC: `http://127.0.0.1:19000/v1/mcp`

Use another port: `RUSTRAG_PORT=8080 docker compose up -d`

On a fresh stack the first visit runs bootstrap: set the admin login and password. Optional: pre-provision admin with `RUSTRAG_UI_BOOTSTRAP_ADMIN_LOGIN` / `RUSTRAG_UI_BOOTSTRAP_ADMIN_PASSWORD` in `.env`.

## Features

- **Document ingestion** -- text, code, PDF, DOCX, PPTX, HTML, images, and web links into one pipeline
- **Graph knowledge base** -- entities and relations extracted and merged into a browsable graph
- **Vector search** -- embeddings stored in ArangoDB for hybrid retrieval
- **Grounded assistant** -- built-in chat UI scoped to a library for testing answers before wiring agents
- **MCP server** -- HTTP MCP endpoint with tools for search, read, upload, and admin
- **Access control** -- API tokens, grants, library scoping, and ready-made MCP client snippets
- **Spending tracking** -- per-document and per-library cost visibility
- **Model selection** -- configurable providers and models for each pipeline stage
- **Multi-format support** -- `txt`, `md`, `csv`, `json`, `yaml`, `xml`, `pdf`, `docx`, `pptx`, `html`, `png`, `jpg`, `gif`, `webp`, `svg`, and 30+ more formats

## Tech Stack

| Layer | Technology |
|-------|-----------|
| API + Worker | Rust, Axum, SQLx, async tasks |
| Frontend | React, Vite, Tailwind CSS, shadcn/ui, Radix |
| Graph + Vector | ArangoDB 3.12 with experimental vector indexes |
| Control Plane | PostgreSQL 18 |
| Worker Queue | Redis 8 |
| Reverse Proxy | nginx 1.28 |
| Deployment | Docker Compose, Ansible |

## Configuration

RustRAG uses `RUSTRAG_*` environment variables.

- `.env.example` -- compose-level variables
- `apps/api/.env.example` -- full application config reference
- `apps/api/src/app/config.rs` -- built-in defaults

See `docker compose config` for the fully rendered configuration after `.env` interpolation.

## MCP Integration

RustRAG ships with an HTTP MCP server. Create a token in **Admin > Access**, attach grants, then copy a ready-made client snippet from **Admin > MCP**.

Tool surface: `list_workspaces`, `list_libraries`, `search_documents`, `read_document`, `upload_documents`, `update_document`, `get_mutation_status`, plus admin tools when grants allow.

Full setup guide: [MCP.md](./MCP.md)

## API Reference

The API is served at `/v1/`. An interactive Swagger UI is available in the web app under the API docs section.

## Release Images

GitHub Releases publish `pipingspace/rustrag-backend:<tag>` and `pipingspace/rustrag-frontend:<tag>` to Docker Hub and refresh the `latest` tag.

Override with `RUSTRAG_BACKEND_IMAGE` or `RUSTRAG_FRONTEND_IMAGE` in `.env`.

## Roadmap

### 0.2.0 — Quality & Performance
- [ ] Hybrid search (BM25 + vector fusion) for better retrieval
- [ ] Cross-encoder reranker for top-K chunk filtering
- [ ] ArangoDB entity sync (write entities from graph extraction to ArangoDB)
- [ ] SSE streaming for query answers
- [ ] Conversation context in multi-turn queries
- [ ] Incremental re-processing (diff-aware ingest for updated documents)
- [ ] Parallel graph extraction (concurrent chunk processing)
- [ ] Embedding cost tracking (currently only graph extraction tracked)
- [ ] Export/import libraries

### Future
- [ ] Video/audio extract support 
- [ ] Language/format detection with per-type extraction strategies
- [ ] Code-aware chunking (AST-based for Rust, TypeScript, SQL, Python)
- [ ] Multi-tenant workspace isolation
- [ ] RBAC with fine-grained document permissions
- [ ] Custom extraction prompts per library
- [ ] Plugin system for custom document processors
- [ ] Ollama/local model support
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

PRs are welcome. Documentation improvements, UI polish, ingestion fixes, MCP integrations, tests, and cleanup all help.

If you change behavior or structure, prefer the one canonical path instead of adding compatibility layers or duplicate flows.

## License

[MIT](./LICENSE)
