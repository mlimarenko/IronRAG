# Local Development

## Prerequisites

- Rust stable toolchain
- Postgres 18
- `sqlx-cli` if you want migration workflows outside app startup
- optional Node 22+ for frontend work

## Backend

```bash
cd backend
cp .env.example .env
# required if you want the frontend/operator auth panel to mint a local session token
printf '\nRUSTRAG_BOOTSTRAP_TOKEN=bootstrap-local\n' >> .env
cargo run
```

If you prefer exporting variables instead of editing `.env`, this is enough for honest local auth/bootstrap validation:

```bash
export RUSTRAG_BOOTSTRAP_TOKEN=bootstrap-local
cargo run
```

## Planned DB Bootstrapping

```bash
createdb rustrag
# enable pgvector in the target DB if needed
```

## Initial Verification

```bash
curl -sS http://127.0.0.1:8080/v1/health
curl -sS -X POST http://127.0.0.1:8080/v1/auth/bootstrap-token \
  -H 'content-type: application/json' \
  -d '{
    "token_kind": "instance_admin",
    "label": "local-browser-check",
    "scopes": ["workspace:admin"],
    "bootstrap_secret": "bootstrap-local"
  }'
```

A successful bootstrap-token response confirms the local runtime can support honest browser validation of protected flows.
