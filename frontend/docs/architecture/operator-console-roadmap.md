# RustRAG Operator Console Roadmap

RustRAG frontend must evolve from a shell into a real operator console.

## Required route qualities

Every major route must show:

- current workspace/project context
- loading state
- empty state
- success state
- degraded/warning state
- error/blocker state

## Scope-2 route goals

- dashboard: summarize system/workspace/project posture
- workspaces: governance, defaults, warnings
- projects: readiness, indexing freshness, scope context
- providers: provider/model status and misconfiguration visibility
- ingestion: jobs, stages, retries, provenance
- chat: answers, references, weak-grounding signals
- diagnostics: failed jobs, degraded dependencies, remediation hints
