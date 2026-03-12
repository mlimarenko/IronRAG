# US3 Checkpoint — Grounded Query Transparency

## Implemented in this slice
- retrieval run detail endpoint
- query response answer-status and weak-grounding fields
- retrieval debug extraction helper for references, matched chunks, and warning state
- ChatPage query form with answer/evidence/diagnostics surface
- retrieval diagnostics detail fetch after each query

## Validation result
- backend strict validation passed
- frontend enterprise validation passed except non-blocking Vue style warnings
- frontend build/typecheck/api-check succeeded

## Current weak-grounding heuristic
A query is currently marked weakly grounded when:
- no references were selected, or
- fewer than two chunks were matched

This is a pragmatic heuristic and should later evolve into a richer retrieval-quality classifier.
