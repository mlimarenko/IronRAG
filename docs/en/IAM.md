# IronRAG IAM

[Overview](./README.md) | [MCP](./MCP.md) | [CLI](./CLI.md)

IronRAG uses one authorization model across the web UI, HTTP API, CLI-created tokens, and MCP tools.

## Core concepts

| Term | Meaning |
|---|---|
| Principal | authenticated identity: user or API token |
| Grant | permission assigned at a concrete scope |
| Scope | `system`, `workspace`, `library`, or `document` |
| Permission kind | named capability such as `library_read` or `iam_admin` |
| Token | bearer secret prefixed `irt_` |

## Permission kinds

| Permission | Purpose |
|---|---|
| `iam_admin` | full system administration |
| `workspace_admin` | manage workspaces and libraries |
| `workspace_read` | view workspace metadata |
| `library_read` | read libraries, documents, graph, and related read surfaces |
| `library_write` | upload, update, delete, and web-ingest content |
| `document_read` | read a specific document |
| `document_write` | mutate a specific document |
| `query_run` | execute assistant and query turns |
| `ops_read` | read runtime and operational state |
| `audit_read` | read audit events |
| `credential_admin` | manage provider credentials |
| `binding_admin` | manage model bindings and presets |
| `connector_admin` | manage connectors |

## Permission hierarchy

- `iam_admin` implies `workspace_admin` which implies all other permissions.
- `library_write` implies `library_read` plus document read/write.

## Scope hierarchy

Broader scopes cover narrower resources:

```text
system
  -> workspace
    -> library
      -> document
```

**System** scope (`workspace_id=null`) gives full admin across all workspaces. A token issued at system scope has no workspace restriction.

Examples:

- `library_read` on a workspace applies to every library in that workspace.
- `document_write` on one document does not imply access to sibling documents.
- `iam_admin` on `system` bypasses per-resource checks.

## Session and token surfaces

Session routes:

- `POST /v1/iam/session/login`
- `GET /v1/iam/session/resolve`
- `POST /v1/iam/session/logout`

Bootstrap routes:

- `GET /v1/iam/bootstrap/status`
- `POST /v1/iam/bootstrap/setup`

API tokens use the same authorization checks as session-authenticated users.

## Token lifecycle

1. Create the token in the Admin UI or with `ironrag-cli create-token`.
2. Copy the plaintext token once.
3. The backend stores only the hashed token.
4. Clients authenticate with `Authorization: Bearer irt_...`.
5. Grants are resolved against the target scope for each HTTP or MCP call.

## MCP visibility model

`tools/list` is grant-filtered.

- Discovery and read tools require read-level access to the addressed scope.
- Document mutation and web-ingest tools require write-level access.
- Runtime tools require `ops_read` or stronger access that already covers the target library.
- Catalog creation tools require admin-level access.

If a token cannot use a tool, that tool is not advertised.

When a token is scoped to exactly one workspace or library, MCP tools and the query API auto-fill `workspace_id` and `library_id` from the token scope.

## Security rules

- Tokens are hashed before storage.
- Passwords use Argon2id.
- Expired grants are ignored.
- System-scoped admin access is intentionally broad; prefer workspace or library scopes when possible.
