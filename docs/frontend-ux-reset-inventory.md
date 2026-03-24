# Frontend UX Reset Inventory (Spec 057)

## Scope and Baseline

- Runtime frontend path: `/home/leader/sources/RustRAG/rustrag/frontend`
- Canonical authenticated pages currently present:
  - `/` -> `src/pages/DashboardPage.vue`
  - `/documents` -> `src/pages/DocumentsPage.vue`
  - `/graph` -> `src/pages/GraphPage.vue`
  - `/admin` -> `src/pages/AdminPage.vue`
- Additional technical page still present: `/swagger` -> `src/pages/SwaggerPage.vue`

## Shell and Navigation Surfaces

- `src/layouts/AppShellLayout.vue`
  - Owns authenticated chrome, context bootstrapping, and create dialogs.
- `src/components/shell/AppTopBar.vue`
  - Canonical topbar composition: brand + nav on the left, context + locale + user controls on the right.
- `src/components/shell/AppNavTabs.vue`
  - Primary tabs already constrained to `Home / Documents / Graph / Admin`.
- `src/components/shell/ContextSelector.vue`
  - Workspace/library selectors.
- `src/components/shell/LocaleSwitcher.vue`, `src/components/shell/UserMenu.vue`
  - Shared action controls in topbar.

## Shared UI Primitive Layer (Current)

- Current shared primitives are in `src/components/base/`:
  - `PageSurface.vue`
  - `PageHeader.vue`
  - `SurfacePanel.vue`
  - `EmptyStateCard.vue`
  - `ErrorStateCard.vue`
  - `StatusPill.vue`
- Planned target from spec/tasks is a dedicated design-system layer in `src/components/design-system/` with canonical `PageFrame`, `PageHeader`, `SectionBlock`, `SurfacePanel`, `FeedbackState`, `StatusBadge`, and shared filter/input primitives.

## Page-Level Surface Inventory

- Dashboard:
  - `src/pages/DashboardPage.vue`
  - `src/components/dashboard/*` is already folded into page-level composition (no standalone dashboard folder currently detected).
- Documents workspace:
  - `src/components/documents/DocumentsWorkspaceHeader.vue`
  - `src/components/documents/DocumentsFiltersBar.vue`
  - `src/components/documents/DocumentsList.vue`
  - `src/components/documents/DocumentInspectorPane.vue`
  - `src/components/documents/DocumentsEmptyState.vue`
  - `src/components/documents/UploadDropzone.vue`
  - `src/components/documents/AppendDocumentDialog.vue`
  - `src/components/documents/ReplaceDocumentDialog.vue`
- Graph workspace:
  - `src/components/graph/GraphCanvas.vue`
  - `src/components/graph/GraphControls.vue`
  - `src/components/graph/GraphNodeDetailsCard.vue`
- Admin control center:
  - `src/components/admin/ApiTokensTable.vue`
  - `src/components/admin/CreateTokenDialog.vue`
  - `src/components/admin/AdminOperationsPanel.vue`
  - `src/components/admin/AdminAuditFeed.vue`
  - `src/components/admin/AdminProviderSettingsPanel.vue`
  - `src/components/admin/AdminModelPricingPanel.vue`

## Routing and Context Ownership

- `src/router/routes.ts`
  - Authenticated routes are nested under `AppShellLayout`.
  - `/swagger` still exists as a child route and should remain technical/non-primary.
- `src/router/guards.ts`
  - Auth restore + shell context initialization.
  - Guest/auth/admin gating is centralized here.
- `src/stores/shell.ts`
  - Active workspace/library ownership is centralized (canonical and aligned with spec intent).

## i18n and State Vocabulary Surfaces

- Shared labels and route copy live in:
  - `src/i18n/en.ts`
  - `src/i18n/ru.ts`
- Existing `shell.swagger` copy remains in locale files and should be treated as technical-surface copy, not primary navigation copy.

## CSS and Style Clusters To Audit During Refactor

- Global stylesheet: `src/css/app.scss`
- Current architecture still appears to include legacy and route-specific style clusters:
  - Dashboard card/hero classes
  - Documents hybrid summary + workspace class sets
  - Graph HUD/fallback variants from older iterations
  - Admin card-grid and transitional section styles
- These clusters must converge into one canonical page grammar and one tokenized style system per spec 057.
