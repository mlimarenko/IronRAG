<div align="center">

# IronRAG MCP

### Connect Codex, Cursor, VS Code, Claude Code, or any HTTP MCP client to the same knowledge base used by IronRAG

[Overview](./README.md) | [MCP (RU)](../ru/MCP.md) | [IAM](./IAM.md) | [CLI](./CLI.md) | [Benchmarks](./BENCHMARKS.md)

</div>

## Endpoint

- Endpoint URL: `http://127.0.0.1:19000/v1/mcp`
- Transport: **MCP Streamable HTTP, spec `2025-06-18`**. One endpoint handles `POST`, `GET`, and `DELETE` — no separate SSE channel, no stdio proxy.
  - `POST` — every JSON-RPC message. Content negotiated from the `Accept` header:
    - `Accept: application/json` → a plain JSON body (default, curl-friendly).
    - `Accept: application/json, text/event-stream` → a single SSE frame `event: message\ndata: …\n\n`; SDK clients that advertise both formats get the transport they expect.
    - Notification-only requests (no `id`) are acknowledged with a bare `202 Accepted`.
  - `GET` — reserved for server-push streams. IronRAG emits no background notifications today, so it returns `200 OK` + `Content-Type: text/event-stream` with a single `: ready` SSE comment and no further events. Spec 2025-06-18 permits either 405 or an empty SSE stream; we pick the latter because some bundled MCP clients treat any non-200 handshake as fatal and drop the whole MCP server for that agent context.
  - `DELETE` — session termination signal. The server is stateless between requests, so it always returns `200 OK` so client cleanup flows finish cleanly.
- The `initialize` response carries an `Mcp-Session-Id` header (UUIDv7). Clients that echo it on subsequent requests are accepted without additional validation.
- Capabilities (for monitoring and UI): `GET http://127.0.0.1:19000/v1/mcp/capabilities` — this is not part of the MCP protocol, just a sidecar probe.
- Auth: `Authorization: Bearer <token>` on every request (including `GET` / `DELETE`).
- Protocol server name: `ironrag-mcp-memory`.
- Default client alias used in the admin UI: `ironragMemory`.

Quick probe (plain JSON):

```bash
export IRONRAG_MCP_TOKEN='irt_...'

curl -sS -X POST http://127.0.0.1:19000/v1/mcp \
  -H "Authorization: Bearer $IRONRAG_MCP_TOKEN" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}'
```

Quick probe (SSE frame, matching the SDK client default):

```bash
curl -sS -X POST http://127.0.0.1:19000/v1/mcp \
  -H "Authorization: Bearer $IRONRAG_MCP_TOKEN" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -d '{"jsonrpc":"2.0","id":2,"method":"initialize","params":{}}'
```

If your IronRAG instance is behind another domain or TLS terminator, replace the origin with the address your client can reach.

## 60-second setup

1. Start IronRAG with Docker Compose.
2. In `Admin -> Access`, create an API token and copy the plaintext secret.
3. Attach grants for the workspace, library, or document the agent should see.
4. In `Admin -> MCP`, copy the ready-made snippet for your client.

`tools/list` is grant-filtered. If a token cannot do something, the tool is not advertised.
The JSON-RPC surface is intentionally small: `initialize`, `tools/list`, `tools/call`, and `notifications/initialized`. IronRAG does not expose an empty `resources/*` surface.
Tool arguments use camelCase fields only.
Catalog targets use stable refs instead of opaque UUIDs: `workspace` is `<workspace>`, and `library` is `<workspace>/<library>`. Discovery responses expose these values as `ref`.

## Tools

### Grounded Q&A (prefer this for knowledge questions)

| Tool | Description | Required parameters |
|------|-------------|---------------------|
| `grounded_answer` | Ask a natural-language question and get a grounded answer with evidence references from IronRAG's grounded-answer pipeline (QueryCompiler → hybrid retrieval → graph-aware context → answer generation → verifier). Prefer this over `search_documents` + `read_document` whenever the user expects an answer, not a hit list. | `library`, `query` |

Response shape: tool text contains the answer; structured output contains `executionDetail` with chunk, prepared-segment, technical-fact, graph-entity, graph-relation, verifier, runtime, request, and response fields. Top-level `runtimeExecutionId`, `executionId`, and `conversationId` are shortcuts for trace lookup. The built-in UI assistant is itself a tool-using agent over this MCP answer surface: it may call `grounded_answer`, inspect raw search/read tools, or answer directly when no library lookup is needed. External MCP clients get the same tool vocabulary, schemas, and grounded-answer result contract.

### Discovery

| Tool | Description | Required parameters |
|------|-------------|---------------------|
| `list_workspaces` | List workspaces visible to the current token. | (none) |
| `list_libraries` | List visible libraries, optionally filtered to one workspace ref. | `workspace` (optional) |

### Admin

| Tool | Description | Required parameters |
|------|-------------|---------------------|
| `create_workspace` | Create a workspace (system-admin only). The request uses the stable workspace ref; `title` is optional display text. | `workspace` |
| `create_library` | Create a library inside one workspace. The request uses the stable library ref; `title` is optional display text. | `library` |

### Documents

| Tool | Description | Required parameters |
|------|-------------|---------------------|
| `search_documents` | Search library memory and return document-level candidates. Optionally scope the search to one or more library refs via `libraries`. | `query` |
| `read_document` | Read one document in full or as an excerpt. | `documentId` |
| `list_documents` | List documents in a library, optionally filtered by processing status. | `library` (optional) |
| `upload_documents` | Create one or more new documents in a library. | `library`, `documents` |
| `update_document` | Append to or replace an existing document. | `library`, `documentId`, `operationKind` |
| `delete_document` | Delete a document and its revisions, chunks, and graph contributions. | `documentId` |
| `get_mutation_status` | Check the lifecycle of a mutation receipt from upload/update/delete. | `receiptId` |

### Knowledge Graph

| Tool | Description | Required parameters |
|------|-------------|---------------------|
| `search_entities` | Search knowledge graph entities by name or label. | `library`, `query` |
| `get_graph_topology` | Get a support-ranked graph topology slice (entities, relations, document links) with truncation. | `library` |
| `list_relations` | List knowledge graph relations ordered by support count. | `library` |
| `get_communities` | List detected graph communities with summaries and top entities. | `library` |

### Web Crawl

| Tool | Description | Required parameters |
|------|-------------|---------------------|
| `submit_web_ingest_run` | Submit a web ingest run for a seed URL. | `library`, `seedUrl`, `mode` |
| `get_web_ingest_run` | Load one web ingest run and its current state. | `runId` |
| `list_web_ingest_run_pages` | List candidate pages and outcomes for a web ingest run. | `runId` |
| `cancel_web_ingest_run` | Request cancellation for an active web ingest run. | `runId` |

### Runtime

| Tool | Description | Required parameters |
|------|-------------|---------------------|
| `get_runtime_execution` | Load the runtime lifecycle summary for one runtime execution. | `runtimeExecutionId` |
| `get_runtime_execution_trace` | Load the full stage, action, and policy trace for one runtime execution. | `runtimeExecutionId` |

Under the hood, MCP calls the same services as the web app: Postgres for control state, ArangoDB for graph and document truth, and Redis-backed workers for ingestion.

## Graph Tool Quality Contract

- `get_graph_topology` is not a raw full-graph dump. When `limit` truncates the response, IronRAG keeps the highest-support entities first, then keeps only relations whose endpoints remain visible, then keeps only document links and documents that still support that visible slice.
- `search_entities` reads from the same admitted runtime graph snapshot as `get_graph_topology`. If an entity is visible in the current runtime graph, `search_entities` should discover that same runtime vocabulary instead of relying on a parallel stale index.
- `list_relations` is ranked by relation support, not by insertion order.
- The goal is a coherent subgraph for agents, not an alphabetical or arbitrary fragment that leaks orphaned edges and unrelated documents.
- When validating a client integration, check result usefulness as well as JSON shape: top entities should be stable across runs, the strongest relations should appear first, linked documents should still support the returned nodes or edges, and `list_relations` should resolve real endpoint labels instead of falling back to `unknown`.
- A healthy graph slice should not return duplicate normalized entity labels or duplicate `(source, relationType, target)` relation signatures inside one ranked response. Those are quality regressions, not harmless formatting noise.

## Access model

- Tokens can be scoped to specific workspaces and libraries.
- Read-only tokens are useful for assistants that should only search and read.
- Write-enabled tokens can upload documents or update existing content when you want an agent to maintain the knowledge base.
- Tool visibility follows grants, so clients only see the operations they are allowed to use.
- When a token is scoped to exactly one workspace or library, MCP tools can infer the `workspace` or `library` ref from the token scope instead of forcing the agent to pass it every time.

## What the client gets

- The same searchable documents and grounded retrieval used by the built-in assistant UI.
- The same document state used by uploads, updates, search, and graph-backed exploration.
- A practical way to connect internal bots, support assistants, or personal agents to a controlled knowledge base without building a separate adapter layer.

## OpenAI Codex CLI

```bash
export IRONRAG_MCP_TOKEN='irt_...'

codex mcp add ironragMemory \
  --url http://127.0.0.1:19000/v1/mcp \
  --bearer-token-env-var IRONRAG_MCP_TOKEN
```

`~/.codex/config.toml`:

```toml
[mcp_servers.ironragMemory]
url = "http://127.0.0.1:19000/v1/mcp"
bearer_token_env_var = "IRONRAG_MCP_TOKEN"
```

## Claude Code (remote MCP)

```bash
claude mcp add ironrag http://127.0.0.1:19000/v1/mcp \
  --transport http \
  --header "Authorization: Bearer $IRONRAG_MCP_TOKEN"
```

`claude` talks Streamable HTTP directly — no separate stdio proxy required.

## Claude Desktop

`~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) or the equivalent on your OS:

```json
{
  "mcpServers": {
    "ironragMemory": {
      "url": "http://127.0.0.1:19000/v1/mcp",
      "headers": {
        "Authorization": "Bearer ${IRONRAG_MCP_TOKEN}"
      }
    }
  }
}
```

## Cursor

`.cursor/mcp.json`:

```json
{
  "mcpServers": {
    "ironragMemory": {
      "url": "http://127.0.0.1:19000/v1/mcp",
      "headers": {
        "Authorization": "Bearer ${env:IRONRAG_MCP_TOKEN}"
      }
    }
  }
}
```

## VS Code or any generic HTTP MCP client

`.vscode/mcp.json`:

```json
{
  "servers": {
    "ironragMemory": {
      "type": "http",
      "url": "http://127.0.0.1:19000/v1/mcp",
      "headers": {
        "Authorization": "Bearer ${env:IRONRAG_MCP_TOKEN}"
      }
    }
  }
}
```

## OpenClaw

`~/.openclaw/openclaw.json`:

```json
{
  "mcp": {
    "servers": {
      "ironrag": {
        "url": "http://127.0.0.1:19000/v1/mcp",
        "headers": {
          "Authorization": "Bearer irt_..."
        }
      }
    }
  }
}
```

Or via the CLI:

```bash
openclaw mcp set ironrag '{"url":"http://127.0.0.1:19000/v1/mcp","headers":{"Authorization":"Bearer irt_..."}}'
```

## Hermes

`~/.hermes/mcp.json`:

```json
{
  "mcpServers": {
    "ironrag": {
      "url": "http://127.0.0.1:19000/v1/mcp",
      "headers": {
        "Authorization": "Bearer ${IRONRAG_MCP_TOKEN}"
      }
    }
  }
}
```

If your client accepts raw HTTP MCP configuration, the endpoint URL plus the bearer token header is enough — Streamable HTTP is the standard remote transport and no adapter layer is required.
