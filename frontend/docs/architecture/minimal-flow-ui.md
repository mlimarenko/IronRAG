# Minimal Flow UI Tokens And Patterns

This note covers the small set of design tokens and UI patterns that should stay consistent across the live `/setup`, `/ingest`, and `/ask` flow.

## Design Tokens

- Colors come from `frontend/src/css/app.scss` CSS custom properties.
- Use `--rr-color-bg-*` for canvas and panel surfaces, not page-local hardcoded fills.
- Use `--rr-color-text-*` for hierarchy before inventing new font weights or opacities.
- Use `--rr-color-accent-600`, `--rr-color-success-600`, `--rr-color-warning-600`, and `--rr-color-danger-600` for status-driven emphasis.
- Use `--rr-space-2` through `--rr-space-6` for intra-panel spacing. `--rr-space-4` is the default gap for forms and stacked cards.
- Use `--rr-radius-sm`, `--rr-radius-md`, and `--rr-radius-lg` for controls, stat cards, and panels.
- Use `--rr-shadow-sm` for standard panels. Heavier shadows are reserved for shell chrome and stronger emphasis.

## Shared Patterns

- `PageSection` is the canonical page header for the minimal flow. Keep the page title, short description, and one clear status in that wrapper.
- `rr-stat-strip` is the top summary band. It should show the active workspace/project context and one immediate next-step signal.
- `rr-panel` is the default working surface. Add `rr-panel--accent` for the main task on the page and `rr-panel--muted` for supporting context.
- `rr-form-grid`, `rr-field`, and `rr-control` are the baseline form primitives. New flow forms should compose these classes before adding custom layout.
- `StatusBadge` and `StatusPill` are preferred over ad hoc colored text for readiness, answer quality, and job state.
- `rr-banner` is the page-level success/warning/error surface. Keep one active message per page state instead of stacking multiple unrelated alerts.

## Minimal Flow Rules

- `Setup` owns workspace and project selection. `Ingest` and `Ask` must respect the same persisted context from `frontend/src/stores/flow.ts`.
- If a stored workspace or project is no longer valid, reset to the first visible option or clear the selection instead of using stale IDs.
- `Ingest` should communicate that text submission creates an ingestion job first and indexed documents may appear after that job completes.
- `Ask` should keep the query form, answer status, and retrieval diagnostics in the same page flow.
- Keep copy practical: one primary action per panel, short hints, and no duplicate admin-level detail inside the minimal path.
