# RustRAG Frontend

Frontend UI shell for the RustRAG platform.

This frontend is part of the `RustRAG/` runtime monorepo and is intended to be a client of the backend API, not a parallel control plane.

## Foundation goals

The frontend should provide shell-level product surfaces for:

- workspaces
- projects
- providers/model profiles
- ingestion
- chat/query
- usage/cost visibility

## Current expectation

The backend is ahead of the frontend. The frontend should evolve as a typed client over the backend contracts as they stabilize.

## Development

```bash
npm install
npm run api:generate
npm run dev
```

## Validation

```bash
npm run check
```

This currently verifies lint, formatting, typecheck, and a production SPA build locally.

## Backend contract alignment

The frontend uses generated TypeScript types from the backend OpenAPI spec.

- source contract: `../backend/contracts/rustrag.openapi.yaml`
- generated types: `src/contracts/api/generated.ts`
- barrel export: `src/contracts/api/index.ts`

Regenerate after backend contract changes:

```bash
npm run api:generate
```

## Compose from monorepo root

```bash
cd ..
cd frontend && npm install && npm run build && cd ..
docker compose up --build backend postgres redis
```

`docker compose build frontend` is currently not reliable in this host/runtime because containerized `npm install` fails during the `esbuild` postinstall step with `spawn sh EACCES`.

Current pragmatic workaround:

- validate frontend locally with `npm run check`
- use the local SPA build output in `frontend/dist/spa`
- run compose for backend dependencies and backend service separately until the container runtime issue is fixed
