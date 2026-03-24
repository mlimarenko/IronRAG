# Release Notes

## 2026-03-24 - 058 Release E2E Validation

- Added full release validation workflow under `scripts/release-validation/` for ingestion, graph, billing, and MCP agent paths.
- Hardened runtime behavior discovered during validation:
  - vector-search failure now degrades gracefully to lexical results for REST and MCP search paths,
  - billing execution cost now returns deterministic zero-cost `200` for legitimate zero-provider-call `ingest_attempt` executions,
  - inline ingestion path captures billing data for graph extraction stage consistently,
  - image payload preprocessing added for robust vision extraction on problematic small/invalid-ish inputs.
- Updated billing API contract notes in `backend/contracts/rustrag.openapi.yaml` to document execution kind semantics and zero-cost behavior.
- Added regression coverage for:
  - billing zero-cost semantics,
  - knowledge search fallback behavior,
  - MCP search fallback behavior.
- Final full release validation run `20260324T093938Z-43695` (`2026-03-24T09:39:38.814Z`): **PASS**.
- Snapshot: format pass rate `1.000` (9/9), graph `110 entities / 158 relations` with `5/5` semantic terms matched, MCP pass, SLA pass, total cost `0.01985525 USD`, average duration `17000 ms`.
