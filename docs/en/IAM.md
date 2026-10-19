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
| Token | bearer secret prefixed `inrg_` |

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

## Integration provisioning authority

An integration provisioner is a root worker identity created by an approved
human OIDC administrator with `provisioningAuthority=integration`. Its ordinary
workspace grant is `workspace_read`, used only for catalog discovery. Catalog
creation, child-key management, and deletion use `/v1/provisioning/*`, where
authorization is bound to the resource's immutable
`provisioned_by_principal_id`; an unrelated grant never establishes ownership.

Existing integration resources are assigned once by
`ironrag-maintenance migrate integration-provisioning-ownership` with explicit
authority, workspace, and library UUIDs. The command rejects names, inactive or
foreign-parent resources, and rows that already have an owner. Runtime API
routes cannot set or change provisioning ownership.

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

- `POST /v1/iam/sessions`
- `GET /v1/iam/context`
- `POST /v1/iam/session`
- `GET /v1/iam/oidc/login` (when OIDC is configured)
- `GET /v1/iam/oidc/callback` (the registered OIDC redirect endpoint)

Bootstrap routes:

- `GET /v1/iam/bootstrap`
- `POST /v1/iam/bootstrap`

API tokens use the same authorization checks as session-authenticated users.

## OpenID Connect SSO

IronRAG supports server-side OpenID Connect Authorization Code login with PKCE.
Leave every required OIDC setting unset to disable it. If any required setting is
set, startup requires the complete set and fails rather than silently falling
back to a different login path.

| Variable | Purpose |
|---|---|
| `IRONRAG_OIDC_ISSUER_URL` | Canonical issuer URL published by the identity provider. |
| `IRONRAG_OIDC_CLIENT_ID` | Confidential client ID registered for IronRAG. |
| `IRONRAG_OIDC_CLIENT_SECRET` | Confidential client secret. Supply it only through the runtime secret environment. |
| `IRONRAG_OIDC_REDIRECT_URL` | Exact registered callback URL. For example, for a deployment at `https://ironrag.example.com`: `https://ironrag.example.com/v1/iam/oidc/callback`. |
| `IRONRAG_OIDC_REQUIRED_GROUP` | Group claim required to admit a user. |
| `IRONRAG_OIDC_BOOTSTRAP_ADMIN_SUBJECT` | Optional exact OIDC `sub` that receives the local IronRAG admin role on its first JIT provisioning. |

The client secret is a credential: keep it in the deployment secret manager
(for example, Vault) and expose it to the API as an environment variable. Never
put it in Git, a command line, or an image. The optional bootstrap subject is
an identity selector, not a secret; obtain and verify its immutable `sub` from
the identity provider before that user first signs in.

The callback accepts only the provider discovered from the configured issuer.
It verifies the issuer, audience, signature, expiry, nonce, authorization-code
PKCE binding, and one-time state before creating an IronRAG session. A state is
bound to the browser and consumed once; failed or replayed callbacks do not
create a session.

An admitted user is provisioned just in time by the pair of issuer and `sub`.
Email is profile data only: IronRAG never links a password account or another
OIDC account by email. A new OIDC user starts with the local `viewer` role. The
optional bootstrap subject grants the local admin role only when that exact
identity is first provisioned; configuring it later does not elevate an
existing user.

The required OIDC group is an admission gate, not an IronRAG permission or
role. Token creation and every token's scope remain governed by the user's
local IronRAG role and grants. An OIDC user can therefore create and use tokens
only when that local authorization permits it; group membership alone never
makes a user an IronRAG administrator.

### Local password administrator

OIDC does not remove the separately configured password administrator. Its
password remains `IRONRAG_UI_BOOTSTRAP_ADMIN_PASSWORD`, alongside the required
bootstrap login and email settings. At startup, IronRAG reconciles that password
only for the exact configured local password administrator. It never changes an
OIDC user's credential or links an OIDC identity to that account. In managed
deployments, mount this password from the same secret manager as other runtime
credentials.

## Token lifecycle

1. Create the token in the Admin UI or with `ironrag-cli create-token`.
2. Copy the plaintext token once.
3. The backend stores only the hashed token.
4. Clients authenticate with `Authorization: Bearer inrg_...`.
5. Grants are resolved against the target scope for each HTTP or MCP call.

Tokens created from an OIDC session are issued by that IronRAG user principal
and follow the same hashing, scope, revocation, and audit rules as tokens
created from a password session.

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
