# 058 Release Validation Evidence

## Scope

- Feature: `058-release-e2e-validation`
- Runtime target library: `019d14ba-df9d-73b3-91db-e51e4289935a`
- Fixtures directory: `/tmp/rustrag-release-fixtures-v3`

## Verification runs

- Backend compile gate: `cargo test --no-run` in `backend/` (pass)
- Full release validator:
  - Command: `node scripts/release-validation/run.mjs --library-id 019d14ba-df9d-73b3-91db-e51e4289935a --fixtures-dir /tmp/rustrag-release-fixtures-v3`
  - Run ID: `20260324T093938Z-43695`
  - Generated at: `2026-03-24T09:39:38.814Z`
  - Result: pass
  - Report JSON: `/tmp/rustrag-release-validation/20260324T093938Z-43695/artifacts/release-validation-report.json`
  - Report Markdown: `/tmp/rustrag-release-validation/20260324T093938Z-43695/artifacts/release-validation-report.md`
  - Verdict JSON: `/tmp/rustrag-release-validation/20260324T093938Z-43695/artifacts/release-validation-verdict.json`

## Runtime outcome snapshot

- Formats validated: `txt`, `md`, `csv`, `json`, `html`, `rtf`, `docx`, `pdf`, `png`
- Ingestion readiness: all fixtures reached graph-ready state
- Billing contract: all fixtures returned `200` from `/v1/billing/executions/ingest_attempt/{id}`
- MCP workflow: JSON-RPC flow completed (capabilities, init, tools/list, search, upload, status, read)
- Verdict: release gates pass for this run (`PASS`, format pass rate `1.000`, SLA pass)
