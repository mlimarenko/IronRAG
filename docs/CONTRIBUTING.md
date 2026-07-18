Run `make pre-commit-install` once after cloning to enable local gates. The hooks mirror what `make check-strict` enforces.

The frontend has one package-manager source of truth: npm 11.17.0, declared in
`apps/web/package.json`, with `apps/web/package-lock.json`. Install it with
`npm ci --prefix apps/web`; do not add a second workspace or lockfile format.

Run `make static-max` before handing off a change. It checks whole-workspace
unused dependencies/exports, formatting, forbidden file-wide suppressions, and
the exact duplication ratchet. When a refactor removes a clone, review the
jscpd report and regenerate the tracked fingerprint baseline immediately;
baseline growth is a failed review, not a way to silence the gate.

## Backend Security Tools

Install the Rust audit tools once before running the backend security gate:

```bash
which cargo-audit || cargo install cargo-audit --locked --version 0.22.1
which cargo-deny || cargo install cargo-deny --locked --version 0.20.2
which cargo-machete || cargo install cargo-machete --locked --version 0.9.2
```

Use `make backend-audit-strict` to run RustSec advisory scanning, cargo-deny,
and unused-dependency analysis. This gate is intentionally separate from
`make backend-change-gate` because advisory and license checks are heavier than
the normal commit path. Fix findings at the dependency boundary; do not add a
blanket ignore or accepted-debt baseline.
