# RustRAG MCP

RustRAG exposes automation and agent memory over MCP at `/v1/mcp`.

Local compose ingress:

- app + API: `http://127.0.0.1:19000`
- MCP endpoint: `http://127.0.0.1:19000/v1/mcp`

If RustRAG is behind nginx or another proxy, use the public origin that the agent can actually reach.

## Architecture

RustRAG now splits responsibilities like this:

- Postgres: control plane, IAM, grants, async operations, billing, audit, ingest admission
- ArangoDB: knowledge plane, document revisions, chunks, entities, relations, evidence, search context bundles
- Redis: worker coordination and short-lived queue signaling

MCP sits on top of the canonical control-plane and knowledge-plane services. It is not a separate data path.

## Canonical MCP Surface

JSON-RPC route:

- `POST /v1/mcp`

Capability route:

- `GET /v1/mcp/capabilities`

Supported JSON-RPC methods:

- `initialize`
- `resources/list`
- `resources/templates/list`
- `tools/list`
- `tools/call`

Supported tool names:

- `list_workspaces`
- `list_libraries`
- `create_workspace`
- `create_library`
- `search_documents`
- `read_document`
- `upload_documents`
- `update_document`
- `get_mutation_status`

Important:

- `resources/list` and `resources/templates/list` currently return empty lists. This is intentional so MCP clients can probe resources cleanly without a separate resource surface.
- Tool visibility is grant-scoped. If the caller does not have permission for an action, that tool is not advertised in `tools/list`.

## IAM Model

MCP access is controlled by canonical IAM principals and typed grants.

There is no legacy scope JSON on tokens anymore. A token is just a principal. What it can do depends on grants attached to that principal.

Relevant grant resource kinds:

- `system`
- `workspace`
- `library`
- `document`
- `query_session`
- `async_operation`

Relevant permission kinds for MCP:

- `workspace_admin`
- `workspace_read`
- `library_read`
- `library_write`
- `document_read`
- `document_write`
- `query_run`
- `ops_read`
- `audit_read`
- `iam_admin`

Effective behavior:

- `list_workspaces` and `list_libraries` only show resources visible through grants.
- `search_documents` and `read_document` require readable library or document access.
- `upload_documents` requires write access to the target library.
- `update_document` requires write access to the logical document or its library.
- `get_mutation_status` follows the same visibility rules as the underlying async operation and document.
- `create_library` requires administrative rights on the target workspace.
- `create_workspace` is reserved for system-level administrators.

## Bootstrap And Token Setup

### 1. Claim the bootstrap administrator

Fresh local setup:

```bash
curl -sS -X POST http://127.0.0.1:19000/v1/iam/bootstrap/claim \
  -H 'content-type: application/json' \
  --data '{
    "bootstrapSecret": "bootstrap-local",
    "email": "admin@example.com",
    "displayName": "RustRAG Admin",
    "password": "rustrag-admin"
  }'
```

### 2. Create a canonical session

```bash
COOKIE_JAR=/tmp/rustrag-iam-cookie.txt

curl -sS -c "$COOKIE_JAR" -X POST http://127.0.0.1:19000/v1/iam/session/login \
  -H 'content-type: application/json' \
  --data '{
    "email": "admin@example.com",
    "password": "rustrag-admin",
    "rememberMe": true
  }'
```

### 3. Mint a token principal

Workspace-scoped token:

```bash
curl -sS -b "$COOKIE_JAR" -X POST http://127.0.0.1:19000/v1/iam/tokens \
  -H 'content-type: application/json' \
  --data '{
    "workspaceId": "00000000-0000-0000-0000-000000000000",
    "label": "codex-mcp-workspace",
    "expiresAt": null
  }'
```

System token:

```bash
curl -sS -b "$COOKIE_JAR" -X POST http://127.0.0.1:19000/v1/iam/tokens \
  -H 'content-type: application/json' \
  --data '{
    "workspaceId": null,
    "label": "codex-mcp-system",
    "expiresAt": null
  }'
```

Response shape:

- `token`: plaintext bearer token, shown once
- `apiToken.principalId`: the token principal id used for grants

### 4. Attach grants to the token principal

Example: library-scoped read/write token.

```bash
TOKEN_PRINCIPAL_ID=00000000-0000-0000-0000-000000000000
LIBRARY_ID=00000000-0000-0000-0000-000000000000

curl -sS -b "$COOKIE_JAR" -X POST http://127.0.0.1:19000/v1/iam/grants \
  -H 'content-type: application/json' \
  --data "{
    \"principalId\": \"$TOKEN_PRINCIPAL_ID\",
    \"resourceKind\": \"library\",
    \"resourceId\": \"$LIBRARY_ID\",
    \"permissionKind\": \"library_write\",
    \"expiresAt\": null
  }"
```

Example: read-only token for one document.

```bash
DOCUMENT_ID=00000000-0000-0000-0000-000000000000

curl -sS -b "$COOKIE_JAR" -X POST http://127.0.0.1:19000/v1/iam/grants \
  -H 'content-type: application/json' \
  --data "{
    \"principalId\": \"$TOKEN_PRINCIPAL_ID\",
    \"resourceKind\": \"document\",
    \"resourceId\": \"$DOCUMENT_ID\",
    \"permissionKind\": \"document_read\",
    \"expiresAt\": null
  }"
```

You can inspect the current session and effective grants through:

- `GET /v1/iam/me`
- `GET /v1/iam/tokens`
- `GET /v1/iam/grants`

## Connect From Codex

Export the plaintext token:

```bash
export RUSTRAG_MCP_TOKEN='rtrg_...'
```

Register the MCP server:

```bash
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

Check that Codex sees it:

```bash
codex mcp list
codex mcp get rustragMemory --json
```

## Runtime Semantics

RustRAG MCP is backed by canonical content, ingest, query, and knowledge services.

Practical consequences:

- `upload_documents` and `update_document` return receipts backed by shared async operations.
- Poll `get_mutation_status` until the operation reaches a terminal state.
- `search_documents` and `read_document` are grounded in Arango knowledge truth, not a separate graph projection.
- Read results can include chunk, entity, relation, and evidence references that explain why the memory was returned.
- Readability and async-operation completion are different milestones. A document can already be readable while its mutation operation is still settling.
- `update_document` can return `conflicting_mutation` when another mutation for the same logical document is still running.

## Operational Notes

- `contentBase64` must be valid base64 without wrapped newlines.
- Very large imports should be split into multiple `upload_documents` calls.
- If the raw JSON body exceeds the MCP request body limit, `/v1/mcp` returns a JSON-RPC error with `error.data.errorKind = upload_limit_exceeded`.
- If the request parses but a decoded file exceeds the upload limit, the tool returns `result.isError = true` with `structuredContent.errorKind = upload_limit_exceeded`.
- Use distinct `RUSTRAG_SERVICE_NAME` values for backend and worker processes. That name is used in leases, recovery logging, and audit correlation and must contain only ASCII letters, digits, `.`, `_`, or `-`.

## Minimal Agent Prompt

```text
Use the MCP server named rustragMemory.
List the visible tools.
List visible workspaces and libraries.
Search for the topic.
If a document is readable, read it in full mode and use the grounded references.
If you need to write memory, upload or update the document and poll get_mutation_status.
```
