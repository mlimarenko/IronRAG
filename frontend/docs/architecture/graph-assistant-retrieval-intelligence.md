# Graph Assistant Retrieval Intelligence

This note explains how the frontend presents retrieval intelligence and graph-quality state.

## Assistant Config

- The Graph page requests assistant config per active library.
- Config includes:
  - mode descriptors
  - scope hint key
  - grouped-reference semantics key
  - default prompt keys
  - config version
- The UI uses those keys for compact mode guidance and tooltip/help copy instead of hardcoded explanations.

## Live Ask Flow

1. Optimistic user and pending assistant messages are inserted locally.
2. `/v1/query/sessions/{sessionId}/turns` returns the live answer plus planning, rerank, context-assembly, grouped references, and warnings.
3. The assistant rail renders grouped references instead of flat chips when support groups are available.

## Persisted History Hydration

- Chat session history still comes from `/v1/chat/sessions/{id}/messages`.
- When an assistant message carries an `executionId`, the store can hydrate richer persisted detail from `/v1/query/executions/{executionId}`.
- That second pass restores:
  - grouped references
  - planning metadata
  - rerank metadata
  - context-assembly metadata
  - warning and warning kind

## Graph-Quality Surfaces

- Node detail renders canonical summary, confidence, support count, extraction recovery, and reconciliation scope.
- Document detail renders canonical graph summary preview, extraction recovery, reconciliation scope, and graph contribution counts.
- Convergence and reconciliation warnings stay visible even when the user is not looking at raw diagnostics panels.

## UX Principles

- query modes explain retrieval behavior, not “which model is running”
- default prompts push users toward natural-language questions, not keyword lists
- grouped references reduce repetition while keeping drill-down support ids available
- warnings stay compact unless they change trust semantics, such as partial convergence or fallback-to-broad rebuild
