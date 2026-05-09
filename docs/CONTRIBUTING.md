Run `make pre-commit-install` once after cloning to enable local gates. The hooks mirror what `make check-strict` enforces.

## Backend Security Tools

Install the Rust audit tools once before running the backend security gate:

```bash
which cargo-audit || cargo install cargo-audit --locked
which cargo-deny || cargo install cargo-deny --locked
```

Use `make backend-audit-strict` to run both RustSec advisory scanning and the backend cargo-deny policy. This gate is intentionally separate from `make backend-change-gate` because advisory and license checks are heavier than the normal commit path. Document any accepted advisory or license exception in `apps/api/AUDIT.md` before adding an ignore.
