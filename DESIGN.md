# IronRAG UI Design Contract

This file is the local source of truth for UI/UX refactors. Product logic and
public copy must stay domain-neutral: the same screens must work for code,
legal, medical, financial, literary, and other libraries without baked-in sample
questions or industry assumptions.

## Information Architecture

- The global dark shell belongs only to `AppShell`: logo, primary navigation,
  workspace/library selectors, admin entry, and user menu.
- Page bodies use one calm workbench language: flat background, compact page
  header, toolbars, data surfaces, and optional inspectors.
- Main route types are:
  - Dashboard: readiness, health, recent activity, attention.
  - Documents: upload/search/status table and document inspector.
  - Graph: light toolbar, full canvas, collapsible legend and inspector.
  - Assistant: session rail, readable scope header, conversation, optional
    evidence/debug panels.
  - Admin: one section rail plus section content; avoid nested navigation.

## Visual Rules

- Do not use marketing heroes, decorative page gradients, grid overlays, glow
  blobs, frosted glass page shells, or dark `shell-*` chrome inside page bodies.
- Use token ramps for radius, shadow, typography, status, and spacing. Avoid
  literal `rounded-[Nrem]`, ad-hoc shadows, and one-off alpha surfaces.
- Use one compact `PageHeader` pattern on all product pages.
- Use one panel surface: `Card` or a shared layout primitive built on it.
- Empty pages are allowed to stay sparse; do not fill empty space with decor.

## Interaction Rules

- Each screen has at most one primary action in the header or inspector footer.
- Toolbars are light, compact, and predictable: search/filter on the left,
  view or bulk actions on the right.
- `DataView` is the canonical list-plus-inspector behavior layer. It owns the
  main/inspector split, docked desktop layout, mobile drawer behavior, backdrop,
  and focus management.
- `InspectorPanel` and specialized inspector components are content layers
  rendered inside `DataView`'s `inspector` slot. They own title, metrics,
  details, and actions, but not page-level dock/drawer behavior.
- Inspectors share one content structure: header, optional metrics/tabs, scroll
  body, footer actions. Empty debug/inspector panes are closed by default.
- DataView tables and mobile cards use the same `xl` breakpoint as the
  inspector dock; never combine a desktop table with a modal drawer in the
  same viewport band.
- Tables and mobile cards must be derived from the same data model; avoid
  duplicated row markup with divergent breakpoints.
- Repeated row commands use `RowActionsMenu`; keep icon-button strips only for
  true toolbars where every action must be permanently visible.
- Empty/loading/error placeholders use `WorkbenchEmptyState`; delete local
  per-page empty-state variants unless a workflow has a unique interaction.
- Destructive confirmations use `ConfirmDialog`; do not hand-roll dialog chrome
  per section.

## Refactor Priorities

1. Remove the decorative `ops-*` page layer and dashboard hero.
2. Route Dashboard, Documents, Graph, Assistant, and Admin through shared
   `PageShell` and `PageHeader`.
3. Replace domain-specific assistant starter prompts with neutral prompts.
4. Consolidate table/mobile-card rendering behind one `DataView`.
5. Consolidate detail sidebars behind `DataView` behavior and shared inspector
   content primitives.
6. Verify every changed page visually on desktop and narrow viewports.
