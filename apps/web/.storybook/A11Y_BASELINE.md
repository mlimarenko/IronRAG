# Storybook A11y Baseline

Last audited: 2026-05-05

Scope:

- Static Storybook build from `apps/web/storybook-static`
- 41 CSF3 story iframes enumerated from `storybook-static/index.json`
- Axe tags: `wcag2a`, `wcag2aa`, `wcag21aa`

Current baseline:

| Impact | Count | Axe rule IDs |
| --- | ---: | --- |
| Minor | 0 | None |
| Moderate | 0 | None |

Gating result:

| Impact | Count |
| --- | ---: |
| Serious | 0 |
| Critical | 0 |

Notes:

- Initial serious/critical findings were fixed in components and story composition: select trigger labels, destructive alert contrast, shell warning contrast, and non-modal dropdown menu behavior for shell-style menus.
- No axe suppressions or rule disables are part of this baseline.
