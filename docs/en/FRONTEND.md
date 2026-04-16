# IronRAG frontend

This document describes the current web frontend structure and the QA contract for `apps/web`.

## Directory layout

```text
apps/web/src/
├── adapters/      Raw API envelopes -> domain models
├── api/           Thin HTTP clients for `/v1/*`
├── components/    Reusable view components and feature widgets
├── contexts/      Global app state such as active workspace and library
├── hooks/         Cross-page React hooks
├── lib/           Non-React utilities
├── pages/         Route shells and page-owned feature packages
├── test/          Cross-cutting UI audits
└── types/         Canonical frontend domain types
```

## Canonical frontend contracts

- `api/*` talks to the backend and returns wire payloads or already-normalized DTOs.
- `adapters/*` are the only place where Raw API envelopes become domain models.
- `pages/*` orchestrate data loading, routing state, and page-owned derived state.
- `components/*` render; they do not own transport logic.
- Page-specific helpers stay next to the page under `pages/{feature}/`.
- Shared primitives in `components/ui/*` remain presentation-only.

## Page ownership

### Dashboard

- Uses `/v1/ops/libraries/{libraryId}/dashboard` and `/v1/ops/libraries/{libraryId}`.
- Derives summary cards, health rows, recent documents, and ingest status from one dashboard payload.
- Refresh must update only the affected widgets.

### Documents

- Owns the keyset-paginated document list, uploads, batch actions, inspector, web-run list, and editor entry.
- Uses canonical list pagination via `/v1/content/documents`.
- Inspector detail, prepared segments, technical facts, revisions, and source download all load from dedicated endpoints.
- Batch rerun progress polls `/v1/ops/operations/{operationId}`.

### Assistant

- Owns session list, active session, message history, streaming state, and debug context.
- Uses `/v1/query/sessions/*` for session CRUD and turn execution.
- Streaming answers consume SSE from `POST /v1/query/sessions/{sessionId}/turns` with `Accept: text/event-stream`.
- Only the live assistant bubble should re-render during a streaming turn.

### Graph

- Loads topology from `/v1/knowledge/libraries/{libraryId}/graph`.
- Loads summary from `/v1/knowledge/libraries/{libraryId}/summary`.
- Loads entity detail on selection from `/v1/knowledge/libraries/{libraryId}/entities/{entityId}`.
- Adjacency lookup is centralized so inspector neighbor resolution stays bounded to the selected node neighborhood.
- Layout computation runs in a Web Worker at 3000+ nodes. First canvas paint is ~1.6 s on a 25k-node graph.
- Node labels are disabled above 15k nodes. Layout animation is skipped above 5k nodes.
- Hidden-edge precompute and O(degree) selection keep interaction responsive on large graphs.

### Admin

- Uses `/v1/admin/surface` as the shell bootstrap.
- Access, AI, pricing, audit, MCP prompt, snapshot, and catalog operations each own their own fetch path.
- Tabs mount lazily; inactive tabs must not keep refetching.

## Frontend quality gates

### Static and unit tests

```bash
cd apps/web
npm run lint
npm test
```

### Visual QA

```bash
cd apps/web
QA_LOGIN=admin QA_PASSWORD='<password>' \
PLAYWRIGHT_BROWSERS_PATH=$HOME/.cache/ms-playwright \
npx playwright test --config=playwright.qa.config.ts
```

The Playwright suite captures the live UI at desktop and constrained mobile viewports and stores screenshots under `apps/web/visual-qa/screenshots/`.

### Manual checkpoints

Use at least these viewport classes during manual QA:

| Viewport | Example size | Main checks |
|---|---|---|
| Mobile | `375x812` | stacked layout, horizontal overflow, drawer sizing |
| Tablet | `768x1024` | sidebar collapse, tab and panel wrapping |
| Desktop | `1440x900` | default operator workflow |
| Wide | `1920x1080` | multi-column surfaces, graph inspector width |

Verify these behaviors:

- Dashboard refresh does not rebuild the whole page.
- Documents table remains usable on narrow widths and web-run rows expand inline.
- Assistant streaming updates only the active answer bubble and keeps scroll behavior stable.
- Graph selection changes inspector state without refetching the topology stream.
- Admin tabs fetch only their own data and remain usable on narrow widths.

## Visual failure bar

The frontend is not done when TypeScript compiles. It is done when:

- layouts hold at desktop and constrained widths,
- long-running surfaces keep stable rendering during polling or streaming,
- empty, loading, and error states are legible,
- no page depends on Raw wire parsing inside the render tree.
