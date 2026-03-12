# Ingestion Recovery Guide

## Current retry model

Current scope-2 retry behavior is pragmatic, not yet a full orchestration engine.

A job is considered retryable when its status is one of:
- `partial`
- `retryable_failed`

Retry currently creates a fresh ingestion job for the same project/source/trigger context.

## Operator guidance
- use retry only for jobs explicitly marked retryable
- inspect error message and stage before retrying
- treat repeated partial/failure loops as an operational issue, not as something to spam-retry blindly
- confirm project readiness after retry before declaring the corpus query-ready

## Known limitation
This does not yet implement advanced deduplicating recovery semantics or attempt-history modeling; that remains a later scope-2 hardening step.
