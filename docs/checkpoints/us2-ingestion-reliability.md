# US2 Checkpoint — Ingestion Reliability

## Implemented in this slice
- ingestion job detail endpoint
- ingestion retry endpoint with explicit retryability heuristic
- project readiness endpoint with indexing-state summary
- frontend ingestion page with job detail + retry action
- frontend projects page with readiness summary
- ingestion recovery runbook scaffolded

## Validation result
- backend strict validation passed
- frontend enterprise validation passed except non-blocking Vue style warnings
- buildable/runtime behavior is intact

## Known limitation
Retry semantics are currently pragmatic and heuristic-based:
- retry is allowed for `partial` and `retryable_failed`
- retry creates a fresh ingestion job for the same context
- advanced attempt-history/deduplicating recovery remains future hardening work
