# IronRAG CLI

Command-line tool for IronRAG admin operations. Connects directly to PostgreSQL.

## Build

```bash
cargo build --release -p ironrag-backend --bin ironrag-cli
```

The binary is also included in the Docker image at `/usr/local/bin/ironrag-cli`.

## Configuration

The CLI reads the same environment variables as the backend server. The required variable is `DATABASE_URL` (or the equivalent setting from the application config).

## Commands

### Print CLI version

```bash
ironrag-cli version
```

Prints the CLI build version (matches the `ironrag-backend` crate version).

### List users

```bash
ironrag-cli list-users
```

Displays a table of all users with login, display name, status, and creation date.

### Create user

```bash
ironrag-cli create-user <LOGIN> <PASSWORD> [--name "Display Name"]
```

Creates a new user with admin privileges (`iam_admin` grant). The user is automatically added to the default workspace if one exists. Password must be at least 8 characters.

Options:
- `-n, --name` -- display name (defaults to login if omitted)

### Reset password

```bash
ironrag-cli reset-password <LOGIN> <PASSWORD>
```

Updates the password for an existing user and revokes all active sessions, forcing re-authentication. Password must be at least 8 characters.

### Delete user

```bash
ironrag-cli delete-user <LOGIN>
```

Permanently deletes a user and all associated records (sessions, grants, workspace memberships, principal).

### Create API token

```bash
ironrag-cli create-token <LOGIN> [--label "my-token"] [--workspace "my-workspace"] [--permission <PERM>...] [--scope <SCOPE>]
```

Creates an API token owned by the specified user. The plaintext token is displayed once and cannot be retrieved later. Tokens are prefixed with `irt_`.

Options:
- `-l, --label` -- token label (defaults to `api-token`)
- `-w, --workspace` -- limit the token to a specific workspace (by slug or UUID)
- `-p, --permission` -- permission to grant (repeatable). If omitted, defaults to `iam_admin`
- `--scope` -- explicit grant scope: `system`, `workspace:<slug>`, or `library:<slug>`

Available permissions:
- `iam_admin` -- full system administration
- `workspace_admin`, `workspace_read` -- workspace management
- `library_read`, `library_write` -- library and document access
- `document_read`, `document_write` -- document-level access
- `query_run` -- execute queries (ask)
- `ops_read`, `audit_read` -- operational and audit data
- `connector_admin`, `credential_admin`, `binding_admin` -- integration management

Scope resolution (when `--scope` is not specified):
- System permissions (`iam_admin`, `ops_read`, `audit_read`) → `system` scope
- Other permissions with `--workspace` → `workspace` scope on that workspace
- Other permissions without `--workspace` → `system` scope (access to all workspaces)

Examples:
```bash
# Full admin token
ironrag-cli create-token admin

# Read-only token for all workspaces
ironrag-cli create-token admin -p library_read -p query_run -l "reader"

# Write token scoped to a specific workspace
ironrag-cli create-token admin -p library_read -p library_write -w default -l "writer"

# Ops monitoring token
ironrag-cli create-token admin -p ops_read -p audit_read -l "monitoring"
```

### List API tokens

```bash
ironrag-cli list-tokens
```

Displays all API tokens with principal ID, label, prefix, status, issue date, and owner.

### Revoke API token

```bash
ironrag-cli revoke-token <TOKEN_PRINCIPAL_ID>
```

Revokes an API token by its principal UUID. Sets the token and principal status to `revoked`.

### List workspaces

```bash
ironrag-cli list-workspaces
```

Displays all workspaces with ID, slug, display name, lifecycle state, and creation date.

### Create workspace

```bash
ironrag-cli create-workspace <SLUG> [--name "Display Name"]
```

Creates a new workspace.

Options:
- `-n, --name` -- display name (defaults to slug if omitted)

### List libraries

```bash
ironrag-cli list-libraries <WORKSPACE>
```

Lists all libraries in a workspace. The workspace can be specified by slug or UUID.

### Create library

```bash
ironrag-cli create-library <WORKSPACE> <SLUG> [--name "Display Name"] [--description "Description"]
```

Creates a new library in the specified workspace.

Options:
- `-n, --name` -- display name (defaults to slug if omitted)
- `-d, --description` -- library description

## Docker usage

```bash
docker exec <container> ironrag-cli list-users
docker exec <container> ironrag-cli create-user admin2 secretpass --name "Second Admin"
docker exec <container> ironrag-cli reset-password admin newpassword123
docker exec <container> ironrag-cli delete-user old-admin

docker exec <container> ironrag-cli create-token admin --label "ci-token" --workspace default
docker exec <container> ironrag-cli list-tokens
docker exec <container> ironrag-cli revoke-token <TOKEN_PRINCIPAL_ID>

docker exec <container> ironrag-cli list-workspaces
docker exec <container> ironrag-cli create-workspace staging --name "Staging"

docker exec <container> ironrag-cli list-libraries default
docker exec <container> ironrag-cli create-library default docs --name "Documentation" --description "Public docs"
```
