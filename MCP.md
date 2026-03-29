<div align="center">

# RustRAG MCP

### Connect Codex, Cursor, VS Code, Claude Code, or any HTTP MCP client to the same graph-backed document memory used by RustRAG

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
