# RustRAG MCP

RustRAG exposes agent memory over MCP at `/v1/mcp`.

Local compose ingress:

- app + API: `http://127.0.0.1:19000`
- MCP endpoint: `http://127.0.0.1:19000/v1/mcp`

If RustRAG is behind nginx or another proxy, use the public ingress origin that your agent can actually reach.

If you run split API and worker processes, give them distinct `RUSTRAG_SERVICE_NAME` values such as
`rustrag-backend` and `rustrag-worker`. That name is used as the stable namespace for worker leases,
recovery logs and audit trails, so it must contain only ASCII letters, digits, `.`, `_` or `-`.

## Token Types

Use the smallest token that matches the agent job.

- Read-only memory:
  - `documents:read`
- Read + upload/update existing documents:
  - `documents:read`
  - `documents:write`
- Create libraries in a visible workspace:
  - `projects:write`
  - or `workspace:admin`
- Review MCP audit / admin flows:
  - `workspace:admin`
- Create new workspaces and then keep working inside them:
  - use an `instance_admin` token

Important:

- MCP tool visibility is token-scoped. If a token does not have a permission, that tool is not shown to the agent.
- A workspace-scoped token can manage only its own workspace. For cross-workspace flows, mint an `instance_admin` token.

## Create A Token

### Option A: UI

1. Open RustRAG at `http://127.0.0.1:19000`
2. Sign in with the local bootstrap account:
   - login: `admin`
   - password: `rustrag`
3. Go to Admin -> API Tokens
4. Create a token with the scopes you need
5. Copy the plaintext token immediately. It is shown only once.

### Option B: Bootstrap API

This is the fastest way to mint an `instance_admin` token in local compose.

```bash
curl -sS -X POST http://127.0.0.1:19000/v1/auth/bootstrap-token \
  -H 'content-type: application/json' \
  --data '{
    "bootstrap_secret": "bootstrap-local",
    "token_kind": "instance_admin",
    "label": "codex-mcp-instance-admin",
    "scopes": [
      "workspace:admin",
      "projects:write",
      "documents:read",
      "documents:write"
    ]
  }'
```

For a workspace-scoped token through the UI session API:

1. `POST /v1/ui/auth/login`
2. Keep the `rustrag_ui_session` cookie
3. `POST /v1/ui/admin/api-tokens`

Example:

```bash
COOKIE_JAR=/tmp/rustrag-ui-cookie.txt

curl -sS -c "$COOKIE_JAR" -X POST http://127.0.0.1:19000/v1/ui/auth/login \
  -H 'content-type: application/json' \
  --data '{"login":"admin","password":"rustrag"}'

curl -sS -b "$COOKIE_JAR" -X POST http://127.0.0.1:19000/v1/ui/admin/api-tokens \
  -H 'content-type: application/json' \
  --data '{
    "label": "codex-mcp-readwrite",
    "scopes": ["documents:read", "documents:write", "projects:write"],
    "expires_in_days": 30
  }'
```

## Connect In Codex

Set the token in the shell.
Examples below use `RUSTRAG_MCP_WRITE_TOKEN`, but any env var name is valid if you reference the same name in Codex config.

```bash
export RUSTRAG_MCP_WRITE_TOKEN='rtrg_...'
```

Register the server in Codex:

```bash
codex mcp add rustragMemory \
  --url http://127.0.0.1:19000/v1/mcp \
  --bearer-token-env-var RUSTRAG_MCP_WRITE_TOKEN
```

Resulting `~/.codex/config.toml` snippet:

```toml
[mcp_servers.rustragMemory]
url = "http://127.0.0.1:19000/v1/mcp"
bearer_token_env_var = "RUSTRAG_MCP_WRITE_TOKEN"
```

Check that Codex sees it:

```bash
codex mcp list
codex mcp get rustragMemory --json
```

## Use From An Agent

Example prompt:

```text
Use the MCP server named rustragMemory.
List the visible tools first.
Then list workspaces and libraries.
Search for <query>.
If a document is readable, read it in full mode.
```

## Live Notes From Real Codex Smoke

These behaviors were verified with real `codex exec` and raw JSON-RPC runs against the local stack on March 19, 2026:

- `create_workspace` works end-to-end with an `instance_admin` token
- `create_library` works end-to-end in the created workspace
- both `create_workspace` and `create_library` accept an optional `slug`; if omitted, RustRAG derives a stable slug from `name`
- if the derived or supplied slug already exists, RustRAG returns a conflict instead of silently renaming it
- `upload_documents` works, but `documents[].contentBase64` must be valid single-line base64 without wrapped newlines
- MCP upload sizing is now explicit and deterministic:
  - each decoded file is limited by the configured upload cap
  - one `upload_documents` call is also limited by the same decoded batch cap, so larger imports must be split into multiple calls
  - if the JSON body itself is too large to buffer, `/v1/mcp` returns a JSON-RPC error with `error.data.errorKind = upload_limit_exceeded`
  - if the request parses but the decoded file is too large, the tool returns `result.isError = true` with `structuredContent.errorKind = upload_limit_exceeded`
- `resources/list` and `resources/templates/list` return empty lists, so Codex resource probes succeed cleanly
- parallel `upload_documents` calls into the same library now settle successfully; the backend serializes library-scoped projection work instead of letting one mutation fail on graph contention
- `update_document` returns `conflicting_mutation` while a prior mutation for the same logical document is still in-flight
- appended text becomes visible through both `read_document` and `search_documents` before the append receipt reaches terminal `ready`
- operator-visible ingestion stage truth now stays aligned with runtime truth; live validation showed `ingestion_job.stage` advancing through `embedding_chunks` and `extracting_graph` together with `runtime_ingestion_run.current_stage`
- a real Codex session was able to discover tools, search `salmon-bridge`, read the full document, and extract the appended `nebula-anchor` note correctly

One verified smoke result:

- workspace: `019d068c-b47b-7c92-8fca-b0c28936a99a`
- library: `019d068c-b49b-7bd2-ba25-85ad7ec1e774`
- alpha document: `019d068c-e910-7410-9802-ee11b1856ce7`
- beta document: `019d068c-e912-7063-b433-f948a47fd0a6`
- upload receipts: `019d068c-e968-7272-a2c3-e9cdc351a287`, `019d068c-e966-7de3-baf1-9f7533982ac9`
- append receipt: `019d068d-b0d4-79b1-9d76-2c87b4bfe300`
- final state:
  - both upload receipts reached `ready`
  - `search_documents = readable`
  - `read_document = readable`
  - append content was already visible while append status was still `accepted`
  - append receipt later reached `ready`

## Quality Notes From Full-Cycle Validation

The MCP surface now behaves like durable agent memory for the tested workflows: discovery, scoped search/read, create workspace/library, multi-document upload, and append.

- `search_documents` and `read_document` stayed consistent for the verified documents; both returned `readable` content after upload completion.
- concurrent uploads into one library no longer reproduced the earlier Neo4j projection contention failure.
- `update_document` is still intentionally blocked during an in-flight mutation on the same logical document. Agents should treat `conflicting_mutation` as a retry-after-settle signal, not as a permanent failure.
- the memory surface exposes appended text before the mutation receipt reaches terminal `ready`, which is useful for agent recall but means receipt state and read availability are not strictly identical milestones.
- chunk retry cleanup no longer depends on unindexed FK cascades. Migration `0019_chunk_cleanup_fk_indexes.sql` adds the missing `chunk(document_id)` and FK-side `chunk_id` indexes; a live transactional benchmark deleting `900` chunks with matching extraction and evidence rows completed in about `13 ms`.
- backend logs during the live run showed one slow SQL warning on a runtime graph projection query under background load. Correctness was unaffected, but large-library graph projections may still need performance profiling.

Practical implication:

- As a memory tool, the current implementation is ready for normal agent workflows: discover visible scopes, search memory, read full documents, upload new memory, and append to existing memory.
- Agents should still poll `get_mutation_status` for write completion and retry `update_document` only after a `conflicting_mutation` settles.
- Agents should split very large imports into multiple `upload_documents` calls instead of trying to send one oversized JSON batch.
