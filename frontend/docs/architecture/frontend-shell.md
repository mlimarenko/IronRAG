# Frontend Shell Architecture

## Intent

The frontend is not a placeholder shell anymore. The current product baseline is a compact operator console built around one text-first loop:

- choose workspace/project context
- add text or a supported upload
- wait for indexing to complete
- ask grounded questions against the active collection

The implementation target stays practical: explicit API resources, typed client boundaries, and honest UI copy about what the backend can and cannot do today.

## Current Product Navigation

Primary spine:

- `/` dashboard for posture, latest signal, and next action
- `/setup` for selecting the active workspace and project
- `/ingest` for source intake and latest ingestion run visibility
- `/ask` for answer, citations, and retrieval diagnostics

Secondary surfaces:

- `/graph` for graph-oriented inspection
- `/api` for API/integration reference flows

## UI Principles

- workspace/project context should remain visible across the shell
- `setup -> ingest -> ask` is the default user journey the docs should describe first
- each route should expose one clear primary action and one honest status
- query results should keep answer and grounding diagnostics in the same page flow
- ingestion copy must match the real support matrix instead of implying generic file support
- secondary surfaces should support the main loop, not compete with it

## Minimal Flow UI Reference

The live minimal flow is the `/setup -> /ingest -> /ask` path inside the broader shell.

- token and pattern reference: `minimal-flow-ui.md`
- shared CSS tokens: `../../src/css/app.scss`
- selection persistence: `../../src/stores/flow.ts`
- ingest support baseline: `../../../docs/ingest-support-matrix.md`

## Product Guardrails

- Do not document older top-level route ideas such as standalone `/workspaces`, `/projects`, or `/providers` as the current navigation baseline.
- Treat text ingestion as the core product promise. PDF/image extraction is a later adapter step, not a hidden capability.
- Keep archive/folder ingest out of the active product narrative until there is a real API and provenance model for container sources.

## API Contract Boundary

The frontend contract boundary is generated from the backend OpenAPI document instead of being maintained by hand.

- source spec: `../../../backend/contracts/rustrag.openapi.yaml`
- generated types: `../../src/contracts/api/generated.ts`
- re-export entrypoint: `../../src/contracts/api/index.ts`

This keeps page/store/client code aligned with the actual backend route and schema contract as it evolves.
