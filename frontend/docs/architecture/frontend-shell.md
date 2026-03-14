# Frontend Shell Architecture

## Intent

The frontend should feel similar to LightRAG in terms of operator flow:

- choose workspace
- choose project
- inspect sources and indexing status
- query the project and inspect answer context

But the implementation should be cleaner: explicit API resources, typed client boundaries, and less hidden coupling.

## Route Groups

- dashboard
- workspaces
- projects
- providers
- ingestion
- chat

## UI Principles

- project context should always be visible
- query results should expose citations and graph context side-by-side
- provider/token management belongs in admin flows, not hidden settings drawers
- long-running ingestion should be job-driven and inspectable

## Minimal Flow UI Reference

The live minimal flow is the `/setup -> /ingest -> /ask` path.

- token and pattern reference: `frontend/docs/architecture/minimal-flow-ui.md`
- shared CSS tokens: `frontend/src/css/app.scss`
- selection persistence: `frontend/src/stores/flow.ts`

## API Contract Boundary

The frontend contract boundary is generated from the backend OpenAPI document instead of being maintained by hand.

- source spec: `../backend/contracts/rustrag.openapi.yaml`
- generated types: `src/contracts/api/generated.ts`
- re-export entrypoint: `src/contracts/api/index.ts`

This keeps page/store/client code aligned with the actual backend route and schema contract as it evolves.
