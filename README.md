

# RustRAG

### One-click knowledge system for documents, internal bots, and AI agents

Load files, links, and images into one knowledge base, turn them into searchable text, embeddings, and graph relations, then expose the same memory in the operator UI and over MCP.

[README-RU](./README-RU.md) • [MCP](./MCP.md) • [MCP-RU](./MCP-RU.md)







> RustRAG is a practical knowledge system for LLMs: one `docker compose up`, one web app, one MCP endpoint, and one canonical pipeline for internal assistants, support bots, and private agent workflows.

## Why RustRAG

- Fast full-stack setup: ArangoDB, Postgres, Redis, Rust services, the UI, and MCP come up together on one stack.
- Built for real knowledge workflows: ingest documents and sites once, then reuse the same canonical state for search, graph exploration, and agent access.
- Practical access model: API tokens, grants, library scoping, and ready-made MCP client snippets are managed from the product.
- Useful beyond demos: pick models, see spending by document, site, or library, and test grounded answers in the built-in assistant UI.

## Quick Start

Prerequisite: Docker with Compose v2.

Choose one startup path:

### 1. Install the published release without cloning

Latest release:

```bash
curl -fsSL https://raw.githubusercontent.com/mlimarenko/RustRAG/master/install.sh | bash
```

Specific tag:

```bash
curl -fsSL https://raw.githubusercontent.com/mlimarenko/RustRAG/master/install.sh | bash -s -- 0.0.3
```

This creates `./rustrag`, downloads the release `docker-compose.yml`, `.env.example`, and `docker/nginx/default.conf`, then starts the published Docker Hub images `pipingspace/rustrag-backend` and `pipingspace/rustrag-frontend`.

### 2. Run prebuilt images from a cloned repository

```bash
cp .env.example .env
docker compose up -d
```

### 3. Build from source from a cloned repository

```bash
cp .env.example .env
docker compose -f docker-compose-local.yml up --build -d
```

Open:

- App + API: [http://127.0.0.1:19000](http://127.0.0.1:19000)
- MCP JSON-RPC: `http://127.0.0.1:19000/v1/mcp`

Use another port if needed:

```bash
RUSTRAG_PORT=8080 docker compose up -d
```

On a fresh local stack, the first visit runs bootstrap: you set the admin login and password (no default portal password). The default `RUSTRAG_BOOTSTRAP_TOKEN` is `bootstrap-local` for API/bootstrap only, not the UI password. Optional: pre-provision admin with `RUSTRAG_UI_BOOTSTRAP_ADMIN_LOGIN` / `RUSTRAG_UI_BOOTSTRAP_ADMIN_PASSWORD`.

## Configuration Model

RustRAG uses one canonical application env style: `RUSTRAG_*`.

- Root `[.env.example](./.env.example)`: the simple Docker Compose surface. Copy it to `.env` for release installs, local builds, or internal deploys.
- Backend `[backend/.env.example](./backend/.env.example)`: the fuller application reference for direct backend/worker runs and advanced overrides.
- `[docker-compose.yml](./docker-compose.yml)`: the default prebuilt deployment surface using Docker Hub images.
- `[docker-compose-local.yml](./docker-compose-local.yml)`: the source-build compose surface for manual local builds.
- `[backend/src/app/config.rs](./backend/src/app/config.rs)`: built-in defaults when a variable is omitted.

Lower-case aliases and mixed env naming are not supported.

## Where To Inspect Variables

- Root `.env`: the active compose interpolation file.
- `[./.env.example](./.env.example)`: the minimal compose-facing variable set.
- `[./backend/.env.example](./backend/.env.example)`: the broader application config reference.
- `[./docker-compose.yml](./docker-compose.yml)`: the default prebuilt deployment surface.
- `[./docker-compose-local.yml](./docker-compose-local.yml)`: the local source-build surface.
- `[./backend/src/app/config.rs](./backend/src/app/config.rs)`: the canonical defaults and setting names.
- `docker compose config`: the fully rendered compose config after `.env` interpolation.

## Release Images

- GitHub Releases publish `pipingspace/rustrag-backend:<tag>` and `pipingspace/rustrag-frontend:<tag>` to Docker Hub, plus refresh the `latest` tags.
- `[docker-compose.yml](./docker-compose.yml)` follows that release channel by default.
- Override `RUSTRAG_BACKEND_IMAGE` or `RUSTRAG_FRONTEND_IMAGE` in `.env` when you need to pin a different image tag.

## Stack

- Rust backend + worker for ingestion, graph build, query, IAM, and MCP.
- ArangoDB for graph storage, document memory, and vector-backed retrieval.
- Postgres for the control plane, IAM, audit, billing, and async operation state.
- Redis for worker coordination.
- Vue 3 + Quasar frontend behind Nginx.

## Pipeline

```text
upload -> text extraction -> chunking -> embeddings -> entity/relation merge -> graph + search -> UI and MCP
```

The same canonical document state powers search, read, update, and graph exploration instead of separate codepaths for different clients.

## Supported Inputs

- Text and text-like files: `txt`, `md`, `markdown`, `csv`, `json`, `yaml`, `yml`, `xml`, `log`, `rst`, `toml`, `ini`, `cfg`, `conf`
- Code and technical text: `ts`, `tsx`, `js`, `jsx`, `mjs`, `cjs`, `py`, `rs`, `java`, `kt`, `go`, `sh`, `sql`, `css`, `scss`
- Documents and pages: `pdf`, `docx`, `pptx`, `html`, `htm`
- Images: `png`, `jpg`, `jpeg`, `gif`, `bmp`, `webp`, `svg`, `tif`, `tiff`, `heic`, `heif`
- Links and web pages can be ingested directly into the same library as uploaded files.

## Operations And Access

- Model selection is configurable for different ingestion and query stages.
- Spending is tracked so you can inspect how much processing cost was spent on a document, a site ingestion run, or a whole library.
- Grants can be scoped so an agent sees only specific libraries.
- Read-only MCP tokens can search and read; write-enabled tokens can upload and update content when you want an agent to maintain the knowledge base.
- The built-in assistant lets you pick a library in the UI and test how grounded answers behave before wiring external agents.

## MCP

RustRAG ships with an HTTP MCP server out of the box. Create a token in `Admin -> Access`, attach grants, then copy a ready-made client snippet from `Admin -> MCP`.

Tool surface includes `list_workspaces`, `list_libraries`, `search_documents`, `read_document`, `upload_documents`, `update_document`, and `get_mutation_status`, with admin tools exposed only when grants allow them.

Quick client setup lives in [MCP.md](./MCP.md).

## Direction

- Graph editing in the UI is planned for finer manual tuning of the knowledge base.
- Audio and video ingestion are planned as future formats for the same vector and graph pipeline.
- A hosted SaaS option is planned in addition to the self-managed deployment model.

## Contributing

PRs are welcome. Documentation improvements, UI polish, ingestion fixes, MCP integrations, tests, and cleanup all help.

If you change behavior or structure, prefer the one canonical path instead of adding compatibility layers or duplicate flows.