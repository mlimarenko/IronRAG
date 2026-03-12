# Frontend API Contracts

The backend OpenAPI document is the single source of truth for frontend contract generation.

## Source of truth

- backend contract: `../../backend/contracts/rustrag.openapi.yaml`
- generated frontend types: `../../src/contracts/api/generated.ts`

## Regenerate types

From `frontend/`:

```bash
npm run api:generate
```

From repo root:

```bash
cd frontend
npm run api:generate
```

## What the script does

`npm run api:generate` runs `openapi-typescript` against the backend contract and rewrites:

- `src/contracts/api/generated.ts`

## When to run it

- after backend API route/schema changes
- before frontend work that depends on new request/response shapes
- before release/checklist validation

## Notes

- Do not hand-edit `src/contracts/api/generated.ts`
- Commit the regenerated file together with the backend contract change when possible
