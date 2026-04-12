# IronRAG IAM — Identity, Access & Tokens

[README](../../README.md) | [CLI](./CLI.md) | [MCP](./MCP.md)

## Concepts

| Term | Description |
|------|-------------|
| **Principal** | Any identity that can authenticate: a user (login + password) or an API token. |
| **Grant** | A row linking a principal to a permission kind at a specific scope. |
| **Scope** | The boundary a grant applies to: `system`, `workspace`, `library`, or `document`. |
| **Permission kind** | A named capability (e.g. `library_read`, `query_run`). |
| **Token** | A bearer secret prefixed `irt_`. Created via CLI or Admin UI. Shown once. |

## Permission kinds

| Permission | Description | Typical use |
|------------|-------------|-------------|
| `iam_admin` | Full system administration. Implies all other permissions. | System operator |
| `workspace_admin` | Manage workspace settings, libraries, members. | Workspace owner |
| `workspace_read` | View workspace metadata. | Read-only workspace access |
| `library_read` | Read documents, list libraries, search, read graph. | Read-only agent |
| `library_write` | Upload, update, delete documents. Implies `library_read` for tool visibility. | Write agent |
| `document_read` | Read a specific document (fine-grained). | Per-document access |
| `document_write` | Write a specific document (fine-grained). | Per-document write |
| `query_run` | Execute `ask` queries against libraries. | Q&A agent |
| `ops_read` | View runtime execution status and traces. | Monitoring |
| `audit_read` | View audit log entries. | Compliance |
| `connector_admin` | Manage AI provider connectors. | Integration setup |
| `credential_admin` | Manage provider credentials (API keys). | Secret management |
| `binding_admin` | Manage model bindings per library. | Model configuration |

## Grant scopes

Grants are hierarchical. A broader scope implicitly covers narrower ones:

```
system (all workspaces)
  └── workspace (all libraries in that workspace)
        └── library (all documents in that library)
              └── document (one specific document)
```

| Scope | Meaning | Example |
|-------|---------|---------|
| `system` | Permission applies to every resource in the instance. | `iam_admin` on `system` = full admin |
| `workspace` | Permission applies to all libraries and documents in that workspace. | `library_read` on `workspace:default` |
| `library` | Permission applies to all documents in that library. | `library_write` on `library:docs` |
| `document` | Permission applies to one specific document. | `document_read` on `document:<uuid>` |

## MCP tool visibility matrix

MCP `tools/list` returns only tools the token's permissions allow. The table below shows which permission kinds make each tool visible:

| Tool | Required permission (any of) |
|------|------------------------------|
| `list_workspaces` | `workspace_read`, `workspace_admin`, `library_read`, `library_write`, `query_run`, `iam_admin` |
| `list_libraries` | `library_read`, `library_write`, `workspace_admin`, `iam_admin` |
| `create_workspace` | `workspace_admin`, `iam_admin` |
| `create_library` | `workspace_admin`, `iam_admin` |
| `search_documents` | `document_read`, `library_read`, `library_write`, `workspace_admin`, `iam_admin` |
| `read_document` | `document_read`, `library_read`, `library_write`, `workspace_admin`, `iam_admin` |
| `list_documents` | `document_read`, `library_read`, `library_write`, `workspace_admin`, `iam_admin` |
| `ask` | `query_run`, `library_read`, `workspace_admin`, `iam_admin` |
| `upload_documents` | `document_write`, `library_write`, `workspace_admin`, `iam_admin` |
| `update_document` | `document_write`, `library_write`, `workspace_admin`, `iam_admin` |
| `delete_document` | `document_write`, `library_write`, `workspace_admin`, `iam_admin` |
| `get_mutation_status` | `document_write`, `library_write`, `workspace_admin`, `iam_admin` |
| `search_entities` | `library_read`, `library_write`, `workspace_admin`, `iam_admin` |
| `get_graph_topology` | `library_read`, `library_write`, `workspace_admin`, `iam_admin` |
| `list_relations` | `library_read`, `library_write`, `workspace_admin`, `iam_admin` |
| `get_communities` | `library_read`, `library_write`, `workspace_admin`, `iam_admin` |
| `submit_web_ingest_run` | `library_write`, `workspace_admin`, `iam_admin` |
| `cancel_web_ingest_run` | `library_write`, `workspace_admin`, `iam_admin` |
| `get_web_ingest_run` | `library_read`, `library_write`, `workspace_admin`, `iam_admin` |
| `list_web_ingest_run_pages` | `library_read`, `library_write`, `workspace_admin`, `iam_admin` |
| `get_runtime_execution` | `ops_read`, `library_read`, `workspace_admin`, `iam_admin` |
| `get_runtime_execution_trace` | `ops_read`, `library_read`, `workspace_admin`, `iam_admin` |

## Preset token profiles

Common configurations for different agent types:

### Full admin

```bash
ironrag-cli create-token admin
# Grants: iam_admin (system)
# Tools: 22 (all)
```

### Read-only agent

```bash
ironrag-cli create-token admin -p library_read -p query_run -l "reader"
# Tools: 14 — list, search, read, ask, graph, runtime
```

### Write agent

```bash
ironrag-cli create-token admin -p library_read -p library_write -l "writer"
# Tools: 20 — everything except create_workspace, create_library
```

### Workspace-scoped reader

```bash
ironrag-cli create-token admin -p library_read -p query_run -w default -l "ws-reader"
# Tools: 14 — same as reader, but only for libraries in "default" workspace
```

### Monitoring / ops

```bash
ironrag-cli create-token admin -p ops_read -p audit_read -l "monitoring"
# Tools: 2 — list_workspaces, list_libraries
```

### Document uploader (no read, no query)

```bash
ironrag-cli create-token admin -p library_write -l "uploader"
# Tools: 20 — can upload/update/delete but also read (library_write implies document visibility)
```

## Token lifecycle

1. **Creation**: via CLI (`ironrag-cli create-token`) or Admin UI. The plaintext token is shown once.
2. **Storage**: only the SHA-256 hash is stored in `iam_api_token`. The prefix (`irt_`) and first 8 chars are kept for identification.
3. **Authentication**: the client sends `Authorization: Bearer irt_...`. The backend hashes the token and looks up the matching principal.
4. **Grant resolution**: all grants for the principal are loaded with workspace/library/document IDs materialized via JOINs.
5. **Authorization**: each API/MCP operation checks grants against the required permission kinds and target resource scope.
6. **Revocation**: via CLI (`ironrag-cli revoke-token <principal-id>`) or Admin UI. Sets status to `revoked`. Immediate effect.

## HTTP API authentication

All `/v1/*` endpoints accept bearer tokens:

```bash
curl -H "Authorization: Bearer irt_..." http://localhost:19000/v1/workspaces
```

Session cookies (from login) and API tokens use the same authorization system. Session tokens are created by `POST /v1/iam/sessions` (login endpoint).

## Security notes

- Tokens are hashed with SHA-256 before storage. The plaintext is never persisted.
- Passwords are hashed with Argon2id.
- System-scoped grants give access to all resources. Use workspace or library scopes for least-privilege.
- The `iam_admin` permission is special: it sets `is_system_admin=true`, bypassing all per-resource checks.
- Expired grants (`expires_at < now()`) are automatically excluded from authorization.
