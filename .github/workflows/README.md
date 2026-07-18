# GitHub Actions Workflows

This directory holds the workflows that GitHub runs against `master`.
The canonical repository quality entrypoint is `make check-strict`. The
`quality.yml` workflow runs it for pull requests and pushes to `master` or
`develop`; release image publication calls the same reusable workflow first.
Its parallel security job runs pinned `cargo-audit`, `cargo-deny`,
`cargo-machete`, checksum-verified Gitleaks 8.30.1, and the npm lockfile audit,
so a release cannot bypass code-quality, secret, or dependency/supply-chain
gates. Gitleaks scans every commit introduced by the event and has no finding
baseline or accepted inline allow directive.
Local `make check`, pre-commit hooks, and focused test targets remain available
for shorter feedback loops.

The strict gate resolves an immutable migration baseline from the pull-request
base SHA or the push event's previous SHA. If GitHub supplies an all-zero SHA,
it uses a verified merge-base with `origin/develop` and refuses a fallback that
would resolve to `HEAD`. `make migration-check` first runs the linter's
synthetic regression fixture, then checks every changed or unreleased migration
against that baseline.

`make static-max` is the whole-repository static entrypoint. It combines the
architecture policy, the no-file-wide-suppression scanner, `cargo-machete`,
Prettier, Knip, ts-prune, and the exact jscpd clone ratchet. The duplication
baseline contains fingerprints rather than a permissive percentage threshold:
new clones fail, and removing a clone requires immediately tightening the
baseline so the debt cannot return.

## release-docker-images.yml

Triggered on `release: published` (and on manual `workflow_dispatch`) after the
reusable strict quality workflow succeeds.
Builds the canonical `pipingspace/ironrag-backend` and
`pipingspace/ironrag-frontend` images using BuildKit + GHA cache and
pushes them to Docker Hub under the release tag (and `latest` when
the dispatch input opts in).
Each run is titled with the release Docker tag, so manual and release-triggered
runs group cleanly in the Actions list.

That is the entire CI surface for this repository.
