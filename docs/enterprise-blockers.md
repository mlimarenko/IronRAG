# RustRAG Enterprise Blockers

## Classification

Use one of these classes for every meaningful gap:
- **blocker** — prevents an intended workflow or release claim
- **operational risk** — workflow may work, but reliability or recoverability is weak
- **quality risk** — behavior works, but regression or correctness confidence is weak
- **deferred enhancement** — useful but not required for current milestone
- **cosmetic cleanup** — local polish without milestone impact

## Active items

### B001 — Frontend container build blocked by `esbuild` postinstall `spawn sh EACCES`
- Class: **blocker**
- Surface: `docker compose build frontend`
- Current behavior: local frontend checks/build succeed, but containerized `npm install` fails during `esbuild` postinstall in current host/runtime
- Supported workaround: use local frontend validation/build path until container-runtime issue is resolved
- Release impact: frontend container path must not be advertised as fully supported in current environment
