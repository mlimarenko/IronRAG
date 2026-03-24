# 058 Merge Summary

## Scope completed

- Implemented release validation runner and support modules under `scripts/release-validation/`.
- Fixed runtime regressions surfaced by end-to-end validation:
  - graceful fallback for vector search failures (`knowledge` + `mcp` search paths),
  - deterministic zero-cost billing semantics for zero-provider-call ingest attempts,
  - inline ingestion billing capture alignment,
  - image payload pre-validation and normalization for vision extraction resilience.
- Added evidence artifacts and runbook/checkpoint docs for release gating.

## Key changed areas

- Backend
  - `backend/src/services/content_service.rs`
  - `backend/src/services/billing_service.rs`
  - `backend/src/interfaces/http/knowledge.rs`
  - `backend/src/services/mcp_access.rs`
  - `backend/src/shared/extraction/image.rs`
  - `backend/contracts/rustrag.openapi.yaml`
  - tests: `backend/tests/billing_rollups.rs`, `backend/tests/knowledge_search.rs`, `backend/tests/mcp_memory_search.rs`
- Validation tooling
  - `scripts/release-validation/run.mjs`
  - `scripts/release-validation/lib/*.mjs`
  - `scripts/release-validation/sql/*.sql`
  - `scripts/release-validation/generate-fixtures.py`
  - `scripts/release-validation/check-fixtures.sh`
  - `scripts/release-validation/rerun-failed.mjs`
- Docs / checkpoints
  - `docs/release-validation-runbook.md`
  - `docs/checkpoints/release-validation/*.md`
  - `rustrag.spec-kit/specs/058-release-e2e-validation/{spec.md,plan.md,tasks.md}`

## Test and gate evidence

- Backend compile gate: `cargo test --no-run` (pass)
- Release validator run: pass  
  `/tmp/rustrag-release-validation/20260323T091217Z-382070/artifacts/release-validation-report.json`

## Rollback notes

- Low-risk rollback path: revert `spec 058` changeset in backend + scripts + docs as one unit.
- If partial rollback is required:
  - keep billing zero-cost semantics and search fallback fixes together to avoid behavior drift.
  - avoid rolling back only contract docs without corresponding runtime behavior.
