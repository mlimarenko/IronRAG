# Backend Audit Notes

Last reviewed: 2026-05-08.

## Accepted Advisories

### RUSTSEC-2023-0071 - rsa 0.9.10

- Severity: medium, CVSS 5.9.
- Status: accepted temporarily and ignored by `make backend-audit`.
- Path reported by `cargo audit`: `sqlx -> sqlx-mysql -> rsa`.
- Rationale: IronRAG enables SQLx for Postgres only and has no MySQL runtime path or direct RSA use. `cargo audit` scans the lockfile and reports the optional SQLx MySQL dependency; root `cargo deny check advisories` evaluates the active cargo feature graph and does not include `rsa` for the backend build.
- Upstream state: no patched `rsa` release exists in the RustSec advisory. Tracking remains upstream at https://github.com/RustCrypto/RSA/issues/626.
- Revisit trigger: remove the ignore when RustSec lists a patched `rsa` version, when SQLx stops exposing the affected optional dependency, or if IronRAG adds any MySQL/RSA runtime path.

## License Exceptions

The canonical cargo-deny policy lives at repo-root `deny.toml` so it covers the whole Rust workspace from the same path used by `make backend-deny`. The global license allow-list stays limited to the standard permissive set there; GPL and AGPL family licenses remain denied by omission. Current crate-specific exceptions are accepted because they are existing transitive dependencies with permissive, non-copyleft terms:

- `webpki-root-certs@1.0.7`, `webpki-roots@0.26.11`, and `webpki-roots@1.0.7` use `CDLA-Permissive-2.0` through Rustls root-certificate packages used by `reqwest` and `sqlx`.
- `xxhash-rust@0.8.15` uses `BSL-1.0` through `redis`.

Do not add broader license IDs to the global allow-list unless the backend intentionally adopts that license family as canonical policy.
