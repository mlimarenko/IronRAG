# 058 Baseline

- Runtime repo: `rustrag`
- Feature: `058-release-e2e-validation`
- Compose profile: default local stack
- Primary validation target: library-scoped release matrix
- Baseline notes:
  - backend and worker are rebuilt from local sources
  - release validator uses REST and MCP public routes only
