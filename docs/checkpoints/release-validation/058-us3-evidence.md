# 058 US3 Evidence (Cost, Performance, Billing Truth)

## Source run

- Run ID: `20260324T093938Z-43695`
- Report JSON: `/tmp/rustrag-release-validation/20260324T093938Z-43695/artifacts/release-validation-report.json`

## Billing assertions

- Billing endpoints for each `ingest_attempt` returned `200` for:
  - cost rollup
  - provider calls
  - charges
- Verified files: `9/9`
- Currency: `USD` for all priced attempts.
- Zero-cost behavior observed and validated:
  - `release.pdf` returned `totalCost = "0"`, `providerCallCount = 0`, `chargeCount = 0`.

## Cost samples

- `release.txt`: `0.00336975 USD`
- `release.html`: `0.00361850 USD`
- `release.docx`: `0.00322700 USD`
- `release.pdf`: `0 USD` (deterministic zero-cost path)
- Total run cost: `0.01985525 USD`

## Performance snapshot (durationMs per file)

- txt: `19290`
- md: `11746`
- csv: `11972`
- json: `9791`
- html: `19218`
- rtf: `20782`
- docx: `30665`
- pdf: `2160`
- png: `27378`
- Average duration: `17000 ms`

## Verdict contribution

- `billingVerifiedCount = 9`
- No billing-related blocking issues in final verdict.
