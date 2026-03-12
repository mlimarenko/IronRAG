# Frontend Release Checklist

- backend OpenAPI contract reviewed: `backend/contracts/rustrag.openapi.yaml`
- `cd frontend && npm run api:generate`
- generated TS types updated: `frontend/src/contracts/api/generated.ts`
- `npm run check` passes locally (lint + format + typecheck + SPA production build)
- if using Docker, frontend container build status is explicitly verified or the known `esbuild`/`spawn sh EACCES` blocker is documented for the target runtime
- key routes render without runtime errors
- environment points at intended backend
