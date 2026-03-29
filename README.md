<div align="center">

# RustRAG

### One-click local graph memory for documents and AI agents

Turn files into searchable text, embeddings, and graph relations, then expose the same memory in the operator UI and over MCP.

<p>
  <a href="./README.ru.md">README.ru</a> •
  <a href="./MCP.md">MCP</a> •
  <a href="./MCP.ru.md">MCP.ru</a>
</p>

<p>
  <img src="https://img.shields.io/badge/Launch-Docker%20Compose-2496ED?style=for-the-badge&logo=docker&logoColor=white" alt="Docker Compose">
  <img src="https://img.shields.io/badge/Backend-Rust-000000?style=for-the-badge&logo=rust&logoColor=white" alt="Rust">
  <img src="https://img.shields.io/badge/Graph-ArangoDB-DDE072?style=for-the-badge&logo=arangodb&logoColor=000000" alt="ArangoDB">
  <img src="https://img.shields.io/badge/Protocol-MCP-111827?style=for-the-badge" alt="MCP">
</p>

</div>

<p align="center">
  <img src="./docs/assets/readme-flow.gif" alt="RustRAG demo: home, document ingest, and graph view" width="960">
</p>

> RustRAG is a local graph-backed document database and memory stack: one `docker compose up`, one web app, one MCP endpoint, one canonical pipeline for both people and agents.

## Why RustRAG

- Fast local setup: ArangoDB, Postgres, Redis, Rust services, the UI, and MCP come up together on one stack.
- Graph-backed document memory: uploads become chunks, embeddings, entities, relations, and provenance you can actually inspect and reuse from agents.
- One surface for humans and agents: operators use the UI, agents use MCP, both hit the same canonical state.
- Ready for real workflows: API tokens, grants, model settings, and client snippets are managed from the product instead of scattered scripts.

## Quick Start

Prerequisite: Docker with Compose v2.

```bash
docker compose up --build -d
```

Open:

- App + API: [http://127.0.0.1:19000](http://127.0.0.1:19000)
- MCP JSON-RPC: `http://127.0.0.1:19000/v1/mcp`

Use another port if needed:

```bash
RUSTRAG_PORT=8080 docker compose up -d
```

On a fresh local stack, the first visit runs bootstrap: you set the admin login and password (no default portal password). The default `RUSTRAG_BOOTSTRAP_TOKEN` is `bootstrap-local` for API/bootstrap only, not the UI password. Optional: pre-provision admin with `RUSTRAG_UI_BOOTSTRAP_ADMIN_LOGIN` / `RUSTRAG_UI_BOOTSTRAP_ADMIN_PASSWORD`.

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

## MCP

RustRAG ships with an HTTP MCP server out of the box. Create a token in `Admin -> Access`, attach grants, then copy a ready-made client snippet from `Admin -> MCP`.

Tool surface includes `list_workspaces`, `list_libraries`, `search_documents`, `read_document`, `upload_documents`, `update_document`, and `get_mutation_status`, with admin tools exposed only when grants allow them.

Quick client setup lives in [MCP.md](./MCP.md).

## Contributing

PRs are welcome. Documentation improvements, UI polish, ingestion fixes, MCP integrations, tests, and cleanup all help.

If you change behavior or structure, prefer the one canonical path instead of adding compatibility layers or duplicate flows.
