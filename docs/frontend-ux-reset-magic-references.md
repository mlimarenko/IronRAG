# Frontend UX Reset Magic References (Spec 057)

## MCP Source

- Server: `user-magic`
- Tool used: `21st_magic_component_inspiration`
- Query themes used during this cycle:
  - `minimal app shell`
  - `document workspace layout`
  - `graph canvas overlay controls`
  - `admin panel typography`

## Reference Outcomes (Used as Inspiration, Not Direct Copy)

### 1) App shell direction

- Inspiration focused on compact tab grammar and minimal navigation chrome.
- Decision for RustRAG:
  - Keep one calm top bar with `brand + primary tabs` on the left.
  - Keep `workspace + library + locale + user` controls on the right.
  - Preserve one interaction density and one active-tab treatment across all primary routes.

### 2) Documents workspace direction

- Inspiration emphasized one dominant work surface with contextual detail.
- Decision for RustRAG:
  - Keep one compact filter row and one scan-friendly document list as primary surface.
  - Keep document detail in contextual inspector, not as competing second page.
  - Keep one focused empty/onboarding state and one upload entry grammar.

### 3) Graph workspace direction

- Inspiration emphasized full-bleed canvas and overlay control docking.
- Decision for RustRAG:
  - Make graph canvas the dominant surface.
  - Place search/layout/viewport controls as on-canvas overlays.
  - Keep one centered fallback state for loading/empty/sparse/failed/rebuilding modes.

### 4) Admin typography direction

- Inspiration emphasized readable meta text with fewer sub-0.85rem labels.
- Decision for RustRAG:
  - Use >= 0.88rem as baseline for secondary metadata in control-center cards.
  - Keep empty/instruction states near body size (~0.94-1rem) with 1.5+ line height.
  - Preserve calm contrast hierarchy (primary text strong, secondary muted but legible).

## Canonical Page Grammar Decisions (Spec 057)

- Shared grammar for authenticated pages:
  - Page header
  - Primary work surface
  - Optional secondary support surface
  - On-demand tertiary detail
- Route jobs remain strict:
  - `/` -> orientation overview
  - `/documents` -> document workbench
  - `/graph` -> canvas-first exploration
  - `/admin` -> control center
- Technical routes (for example Swagger) stay out of primary product navigation.

## Guardrails From This Cycle

- Magic outputs were used only as layout inspiration and interaction cues.
- No generated variants were introduced into the codebase.
- The frontend continues on one canonical handwritten implementation path.
