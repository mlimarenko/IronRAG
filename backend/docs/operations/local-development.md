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
cargo run
```

## Planned DB Bootstrapping

```bash
createdb rustrag
# enable pgvector in the target DB if needed
```

## Initial Verification

```bash
curl -sS http://127.0.0.1:8095/v1/health
```
