# Release Validation Runbook

## Scope
Feature `058-release-e2e-validation` defines the release gate for ingestion reliability, graph quality, billing truth, and MCP agent usability.

## Prerequisites
- Runtime stack is healthy in Docker Compose.
- Workspace/library target is known.
- AI provider credentials and runtime bindings are configured.
- Operator login has access to content, ingest, knowledge, billing, and MCP endpoints.

## Execute
1. Generate fixtures:
   - `python3 scripts/release-validation/generate-fixtures.py --output-dir /tmp/rustrag-release-fixtures`
2. Run full validation:
   - `node scripts/release-validation/run.mjs --library-id <library-uuid> --fixtures-dir /tmp/rustrag-release-fixtures`
3. Optional failed-only rerun:
   - `node scripts/release-validation/rerun-failed.mjs <path-to-report.json>`

## Command Matrix
- **Full run**:
  - `node scripts/release-validation/run.mjs --library-id <library-uuid> --fixtures-dir /tmp/rustrag-release-fixtures`
- **Failed-segment rerun**:
  - `node scripts/release-validation/rerun-failed.mjs <path-to-previous-release-validation-report.json>`
- **Publish-only mode** (no new execution, publish existing artifacts):
  - `jq '.verdict' <path-to-report.json>`
  - `cat <path-to-report.md>`
  - publish both files to your release checkpoint ticket/change-request

## Ingestion Reliability Acceptance

### Pass criteria
- All supported formats (txt, md, csv, json, html, rtf, docx, pdf, png) reach `graphReady = true`.
- No unexplained extraction failures or silent stalls.
- Stage order follows canonical pipeline: `extract_content → chunk_content → embed_chunk → extract_graph`.

### Failure taxonomy
- `extraction_timeout`: LLM or extraction stage exceeded configured timeout.
- `unsupported_format`: MIME type or file structure is not recognized by the extraction pipeline.
- `content_empty`: File parsed successfully but yielded no extractable text content.
- `provider_error`: Upstream AI provider returned a non-retryable error during extraction.
- `unknown`: Unclassified failure — investigate backend logs and `ingest_stage_event` table.

## Graph Quality Operations Guidance

### Investigating semantic mismatch
1. Check `GET /v1/knowledge/libraries/{library_id}/entities?limit=200` for expected semantic terms.
2. If entities are missing, verify the `extract_graph` stage completed for each document.
3. Review AI binding configuration — ensure `extract_graph` binding uses a model with sufficient reasoning capability.

### Investigating noisy edges
1. Query relations with empty or generic predicates.
2. Check `runtime_graph_filter_empty_relations` and `runtime_graph_filter_degenerate_self_loops` settings.
3. If noise persists, consider adjusting the graph extraction prompt or model.

## Failure Triage Playbook

### Graph empty
- Verify `extract_graph` binding exists and is active.
- Check backend logs for extraction errors during the `extract_graph` stage.
- Confirm the document has text content (check `text_readable` state).

### Graph noisy
- Enable `runtime_graph_filter_empty_relations` and `runtime_graph_filter_degenerate_self_loops`.
- Review the extraction model's output for excessive low-confidence entities.

### Graph semantic mismatch
- Compare extracted entity labels against expected semantic terms from fixtures.
- Check if the extraction model is producing domain-relevant vs generic labels.
- Re-run extraction with a more capable model if needed.

## Billing and SLA Acceptance

### Pass criteria
- All `ingest_attempt` executions return `200` from billing cost endpoint.
- Cost, provider calls, and charges are traceable per attempt.
- Zero-cost attempts (e.g. PDF without vision provider call) return deterministic `200` with `total_cost=0`.

### Release-blocking conditions
- Any attempt returning `404` or `500` from billing endpoints.
- Currency code mismatch across attempts.
- Missing provider call records for attempts that used AI models.

## MCP Troubleshooting

### Capability mismatch
- Verify token scope includes required permissions (`documents:read`, `documents:write`).
- Check `GET /v1/mcp/capabilities` to confirm tool visibility matches token grants.

### Permission denial
- Ensure the API token's workspace/library grants cover the target library.
- Check if the token has expired.

### Transient retry guidance
- MCP JSON-RPC calls may return transient errors during backend restarts.
- Retry with exponential backoff (250ms base, 3 attempts).
- If errors persist after 3 retries, investigate backend health and logs.

## Artifacts
- JSON report: `.../artifacts/release-validation-report.json`
- Markdown report: `.../artifacts/release-validation-report.md`
- SQL diagnostics: `.../artifacts/sql-diagnostics.json`

## Acceptance
- Format pass rate >= 95%
- Graph semantic threshold met
- MCP workflow pass
- Billing visibility available for attempts with priced provider calls
- No unexplained critical server failures

## Troubleshooting
- If ingestion stalls: inspect `/v1/ingest/jobs/{job_id}` and recent `ingest_stage_event`.
- If graph is empty: verify extraction stage completion and active extract binding.
- If billing is missing: verify execution owner kind is `ingest_attempt`.
- If MCP fails: verify `/v1/mcp/capabilities` and token scope.
