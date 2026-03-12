# Backend Architecture Overview

## Target Shape

RustRAG starts as a modular monolith.

That is the right choice here. The product has enough moving pieces to need clear boundaries, but not enough validated scale yet to justify immediate service fragmentation.

## Layers

- `app` — bootstrap, config, process lifecycle, dependency wiring
- `domains` — core business concepts and invariants
- `infra` — SQLx repositories, persistence helpers, transactions
- `integrations` — provider gateways and external API wrappers
- `interfaces/http` — HTTP routes, DTOs, extraction, error mapping
- `shared` — telemetry and shared errors

## Initial Domains

- workspace
- project
- provider
- ingestion
- retrieval

## Strategic Notes

- Query/chat and ingestion are separate workflows, but they share project-scoped content state.
- Provider routing should remain independent from route handlers.
- The backend should expose explicit resources for jobs and retrieval runs, not hide them in UI-only flows.
