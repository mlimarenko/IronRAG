# Enterprise Scope 2 Quickstart

## Source of truth
- spec: `../spec-kit/specs/002-rustrag-enterprise/spec.md`
- plan: `../spec-kit/specs/002-rustrag-enterprise/plan.md`
- tasks: `../spec-kit/specs/002-rustrag-enterprise/tasks.md`

## Validation
- backend: `cd backend && make quality`
- frontend: `cd frontend && npm run enterprise:check`
- monorepo: `make enterprise-validate`

## Current implementation priority
1. setup and foundational operational-state scaffolding
2. workspace governance maturity
3. ingestion reliability and retry semantics
4. grounded query transparency
5. diagnostics and operator-console maturity
6. deployment support-matrix hardening
