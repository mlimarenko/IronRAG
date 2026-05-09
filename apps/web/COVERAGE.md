# Frontend Coverage

Coverage is advisory for the frontend test suite. It is available as a local
baseline check, but it is not part of `make frontend-check`.

Run the coverage gate from `apps/web`:

```bash
npm run test:coverage
```

Or from the repository root:

```bash
make frontend-coverage
```

Vitest writes the text report to the terminal, the machine-readable summary to
`coverage/coverage-summary.json`, and the browsable HTML report to
`coverage/index.html`.

## Current Baseline

The baseline was captured with the current unit tests and the canonical
coverage exclusions in `vitest.config.ts`.

| Metric | Current actual | Threshold |
| --- | ---: | ---: |
| Lines | 61.54% | 61.5% |
| Functions | 52.93% | 52.9% |
| Statements | 59.31% | 59.3% |
| Branches | 51.75% | 51.7% |

Excluded from coverage:

- `src/shared/api/generated/**`
- `src/shared/api/mocks/handlers.ts`
- `**/*.stories.tsx`
- `**/*.test.tsx`
- `tests/e2e/**`

## Raising Thresholds

After adding real test coverage, run `npm run test:coverage` and compare the
new totals with the thresholds in `vitest.config.ts`. Raise any threshold that
has a stable higher actual value, keeping a small margin below the measured
percentage so normal local runs do not flap.

Do not lower thresholds to make an unrelated change pass, and do not modify
tests only to inflate the reported percentages.
