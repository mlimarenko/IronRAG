# Frontend UX Reset Delete List (Spec 057)

This list tracks UI patterns and style clusters that must be removed as the canonical surfaces land.

## Global Rules

- Keep only one canonical implementation path per surface.
- Do not preserve compatibility UI branches, alternate page grammars, or duplicate summary regions.
- Delete superseded classes/components in the same change set that introduces canonical replacements.

## Shell / Navigation

- Remove any primary-nav assumptions for technical routes.
  - Keep `/swagger` as technical destination only (not part of main tabs).
- Remove legacy nav active-state variants if they differ across pages.
- Remove duplicate context hero metadata on pages when workspace/library context is already shown in shell.

## Shared Primitive Layer

- Replace and then remove old base-only variants once design-system equivalents are adopted:
  - `src/components/base/PageSurface.vue` compatibility-only wrappers.
  - `src/components/base/EmptyStateCard.vue` custom empty layouts not aligned to one feedback grammar.
  - `src/components/base/ErrorStateCard.vue` route-specific error composition branches.
  - `src/components/base/StatusPill.vue` duplicate badge logic after canonical status badge migration.
- Remove any local one-off panel wrappers that duplicate `SurfacePanel` behavior.

## Dashboard (`/`)

- Delete dashboard-style metric walls and card variants that compete with orientation-first layout.
- Remove duplicate action clusters that replicate documents workbench actions on home.
- Remove any residual format-heavy blocks if they duplicate document workspace concerns.

## Documents (`/documents`)

- Delete duplicated upload regions (keep one primary upload entry point).
- Delete summary-card regions that compete with list + inspector workspace flow.
- Remove redundant filter/count badge clusters if they duplicate active filter summary.
- Remove persistent two-column desktop assumptions that break the mobile/tablet inspector overlay model.

## Graph (`/graph`)

- Delete hero/header card stacks that reduce canvas dominance.
- Remove duplicated fallback/status regions (keep one centered feedback state per mode).
- Delete legacy HUD and side-card classes after overlay controls + contextual inspector are canonical.
- Remove any second competing work area that pushes the graph canvas to secondary focus.

## Admin (`/admin`)

- Delete card-grid dashboard compositions once vertical control-center section order is canonical.
- Remove duplicate workspace/library hero metadata restated above sections.
- Remove any `override/redefine` price language in UI copy and actions.
- Delete split pricing layouts that duplicate “list + editor” concerns across multiple cards.

## Stylesheet Cleanup Targets (`src/css/app.scss`)

- Remove page-specific legacy class clusters after migration:
  - Dashboard legacy grids and hero variants.
  - Documents legacy summary/auxiliary cards.
  - Graph legacy wrapper/HUD duplication.
  - Admin card-grid legacy section classes.
- Keep one tokenized spacing, typography, panel, form-control, and state vocabulary system.
