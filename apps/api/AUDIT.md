# Backend Audit Notes

Last reviewed: 2026-07-14.

## Accepted Advisories

None. `make backend-audit` runs `cargo audit` without advisory suppressions.

## License Exceptions

The canonical cargo-deny policy lives at repo-root `deny.toml` so it covers the whole Rust workspace from the same path used by `make backend-deny`. The global license allow-list stays limited to the standard permissive set there; GPL and AGPL family licenses remain denied by omission. Current crate-specific exceptions are accepted because they are existing transitive dependencies with permissive, non-copyleft terms:

- `webpki-root-certs@1.0.7`, `webpki-roots@0.26.11`, and `webpki-roots@1.0.7` use `CDLA-Permissive-2.0` through Rustls root-certificate packages used by `reqwest` and `sqlx`.
- `xxhash-rust@0.8.15` uses `BSL-1.0` through `redis`.

Do not add broader license IDs to the global allow-list unless the backend intentionally adopts that license family as canonical policy.
