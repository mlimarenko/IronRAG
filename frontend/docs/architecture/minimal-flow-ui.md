# Minimal Flow UI Tokens And Patterns

This note is the small source of truth for the live shell and the `/setup`, `/ingest`, `/ask` loop.

## Token Source Of Truth

- CSS custom properties in `frontend/src/css/app.scss` are the only design-token source of truth.
- Use `--rr-font-sans` for controls/body copy and `--rr-font-display` for titles, key stats, and shell branding.
- Use `--rr-color-bg-*` for page and panel surfaces, `--rr-color-text-*` for hierarchy, and semantic status colors only from the shared accent/info/success/warning/danger tokens.
- Use spacing from `--rr-space-2` through `--rr-space-8`; `--rr-space-4` remains the default page/card/form gap.
- Use `--rr-radius-sm|md|lg` for controls and panels, `--rr-radius-pill` for chips/badges, and `--rr-shadow-sm|md|lg` for elevation.
- Use `--rr-motion-base` for standard hover/focus transitions instead of page-local timing values.

## Canonical Patterns

- `PageSection` is the default route header: title, short description, optional action, one status.
- `rr-stat-strip` carries active context and the next operational signal at the top of a page.
- `rr-panel` is the base working surface; use `--accent` for the primary task and `--muted` for supporting context.
- `rr-form-grid`, `rr-field`, and `rr-control` are the default form primitives before inventing route-specific layout.
- `StatusBadge` and `StatusPill` are the default status language for readiness, ingestion state, and answer quality.
- `EmptyStateCard`, `LoadingSkeletonPanel`, and `ErrorStateCard` are the default async state surfaces.

## Usage Rules

- One primary action per panel. If a page needs multiple competing CTAs, split the page first.
- Do not introduce page-local colors for status meaning; status semantics must stay tokenized and reusable.
- `Setup` owns workspace/project selection; `Ingest` and `Ask` read the same persisted context from `frontend/src/stores/flow.ts`.
- `Ingest` must keep support messaging close to the form: text works now, PDF/image are planned but blocked, archive/folder are out of scope.
- `Ask` keeps query, answer, and retrieval diagnostics in one flow so grounding quality is visible without navigation churn.
- Keep copy short and operational. Avoid restating admin detail inside the main loop.
