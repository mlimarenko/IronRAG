<div align="center">

# IronRAG MCP

### Connect Codex, Cursor, VS Code, Claude Code, or any HTTP MCP client to the same knowledge base used by IronRAG

[Overview](./README.md) | [MCP (RU)](../ru/MCP.md) | [IAM](./IAM.md) | [CLI](./CLI.md) | [Benchmarks](./BENCHMARKS.md)

</div>

## Endpoint

- JSON-RPC: `POST http://127.0.0.1:19000/v1/mcp`
- Capabilities: `GET http://127.0.0.1:19000/v1/mcp/capabilities`
- Auth header: `Authorization: Bearer <token>`
- Protocol server name: `ironrag-mcp-memory`
- Default client alias used in the admin UI: `ironragMemory`

Quick probe:

```bash
export IRONRAG_MCP_TOKEN='irt_...'

curl -sS http://127.0.0.1:19000/v1/mcp/capabilities \
  -H "Authorization: Bearer $IRONRAG_MCP_TOKEN"
```

If your IronRAG instance is behind another domain or TLS terminator, replace the origin with the address your client can reach.

## 60-second setup

1. Start IronRAG with Docker Compose.
2. In `Admin -> Access`, create an API token and copy the plaintext secret.
3. Attach grants for the workspace, library, or document the agent should see.
4. In `Admin -> MCP`, copy the ready-made snippet for your client.

`tools/list` is grant-filtered. If a token cannot do something, the tool is not advertised.
The canonical JSON-RPC surface is intentionally small: `initialize`, `tools/list`, `tools/call`, and `notifications/initialized`. IronRAG does not expose an empty `resources/*` surface.
Tool arguments use canonical camelCase fields only.

## Tools

### Discovery

| Tool | Description | Required parameters |
|------|-------------|---------------------|
| `list_workspaces` | List workspaces visible to the current token. | (none) |
| `list_libraries` | List visible libraries, optionally filtered to one workspace. | `workspaceId` (optional) |

### Admin

| Tool | Description | Required parameters |
|------|-------------|---------------------|
| `create_workspace` | Create a workspace (system-admin only). | `name` |
| `create_library` | Create a library inside one workspace. | `workspaceId`, `name` |

### Documents

| Tool | Description | Required parameters |
|------|-------------|---------------------|
| `search_documents` | Search library memory and return document-level candidates. | `query` |
| `read_document` | Read one document in full or as an excerpt. | `documentId` |
| `list_documents` | List documents in a library, optionally filtered by processing status. | `libraryId` (optional) |
| `upload_documents` | Create one or more new documents in a library. | `libraryId`, `documents` |
| `update_document` | Append to or replace an existing document. | `libraryId`, `documentId`, `operationKind` |
| `delete_document` | Delete a document and its revisions, chunks, and graph contributions. | `documentId` |
| `get_mutation_status` | Check the lifecycle of a mutation receipt from upload/update/delete. | `receiptId` |

### Knowledge Graph

| Tool | Description | Required parameters |
|------|-------------|---------------------|
| `search_entities` | Search knowledge graph entities by name or label. | `libraryId`, `query` |
| `get_graph_topology` | Get a support-ranked graph topology slice (entities, relations, document links) with truncation. | `libraryId` |
| `list_relations` | List knowledge graph relations ordered by support count. | `libraryId` |
| `get_communities` | List detected graph communities with summaries and top entities. | `libraryId` |

### Web Crawl

| Tool | Description | Required parameters |
|------|-------------|---------------------|
| `submit_web_ingest_run` | Submit a web ingest run for a seed URL. | `libraryId`, `seedUrl`, `mode` |
| `get_web_ingest_run` | Load one web ingest run and its current state. | `runId` |
| `list_web_ingest_run_pages` | List candidate pages and outcomes for a web ingest run. | `runId` |
| `cancel_web_ingest_run` | Request cancellation for an active web ingest run. | `runId` |

### Runtime

| Tool | Description | Required parameters |
|------|-------------|---------------------|
| `get_runtime_execution` | Load the runtime lifecycle summary for one runtime execution. | `runtimeExecutionId` |
| `get_runtime_execution_trace` | Load the full stage, action, and policy trace for one runtime execution. | `runtimeExecutionId` |

Under the hood, MCP calls the same canonical services as the web app: Postgres for control state, ArangoDB for graph and document truth, and Redis-backed workers for ingestion.

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
- When a token is scoped to exactly one workspace or library, MCP tools and the query API auto-fill `workspaceId` and `libraryId` from the token scope.

## What the client gets

- The same searchable documents and grounded retrieval used by the built-in assistant UI.
- The same canonical document state used by uploads, updates, search, and graph-backed exploration.
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

If your client accepts raw HTTP MCP configuration, the endpoint URL and bearer token header are enough.
