<div align="center">

# RustRAG MCP

### Connect Codex, Cursor, VS Code, Claude Code, or any HTTP MCP client to the same knowledge base used by RustRAG

[README.md](./README.md) | [MCP.ru.md](./MCP.ru.md)

</div>

## Endpoint

- JSON-RPC: `POST http://127.0.0.1:19000/v1/mcp`
- Capabilities: `GET http://127.0.0.1:19000/v1/mcp/capabilities`
- Auth header: `Authorization: Bearer <token>`
- Protocol server name: `rustrag-mcp-memory`
- Default client alias used in the admin UI: `rustragMemory`

Quick probe:

```bash
export RUSTRAG_MCP_TOKEN='rtrg_...'

curl -sS http://127.0.0.1:19000/v1/mcp/capabilities \
  -H "Authorization: Bearer $RUSTRAG_MCP_TOKEN"
```

If your RustRAG instance is behind another domain or TLS terminator, replace the origin with the address your client can reach.

## 60-second setup

1. Start RustRAG with Docker Compose.
2. In `Admin -> Access`, create an API token and copy the plaintext secret.
3. Attach grants for the workspace, library, or document the agent should see.
4. In `Admin -> MCP`, copy the ready-made snippet for your client.

`tools/list` is grant-filtered. If a token cannot do something, the tool is not advertised.

## What agents can do

- `list_workspaces`, `list_libraries`
- `search_documents`, `read_document`
- `upload_documents`, `update_document`, `get_mutation_status`
- `create_workspace`, `create_library` when admin grants allow it

Under the hood, MCP calls the same canonical services as the web app: Postgres for control state, ArangoDB for graph and document truth, and Redis-backed workers for ingestion.

## Access model

- Tokens can be scoped to specific workspaces and libraries.
- Read-only tokens are useful for assistants that should only search and read.
- Write-enabled tokens can upload documents or update existing content when you want an agent to maintain the knowledge base.
- Tool visibility follows grants, so clients only see the operations they are allowed to use.

## What the client gets

- The same searchable documents and grounded retrieval used by the built-in assistant UI.
- The same canonical document state used by uploads, updates, search, and graph-backed exploration.
- A practical way to connect internal bots, support assistants, or personal agents to a controlled knowledge base without building a separate adapter layer.

## OpenAI Codex CLI

```bash
export RUSTRAG_MCP_TOKEN='rtrg_...'

codex mcp add rustragMemory \
  --url http://127.0.0.1:19000/v1/mcp \
  --bearer-token-env-var RUSTRAG_MCP_TOKEN
```

`~/.codex/config.toml`:

```toml
[mcp_servers.rustragMemory]
url = "http://127.0.0.1:19000/v1/mcp"
bearer_token_env_var = "RUSTRAG_MCP_TOKEN"
```

## VS Code or any generic HTTP MCP client

`.vscode/mcp.json`:

```json
{
  "servers": {
    "rustragMemory": {
      "type": "http",
      "url": "http://127.0.0.1:19000/v1/mcp",
      "headers": {
        "Authorization": "Bearer ${env:RUSTRAG_MCP_TOKEN}"
      }
    }
  }
}
```

If your client accepts raw HTTP MCP configuration, the endpoint URL and bearer token header are enough.
