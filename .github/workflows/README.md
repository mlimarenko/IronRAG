# GitHub Actions Workflows

This directory holds the workflows that GitHub runs against `master`.
The repository's quality gates are local — `make check`, `make
frontend-check`, the pre-commit hooks, and the compliance tests under
`apps/api/tests/compliance_*.rs` are the canonical entrypoints.
GitHub Actions only ships release artifacts.

## release-docker-images.yml

Triggered on `release: published` (and on manual `workflow_dispatch`).
Builds the canonical `pipingspace/ironrag-backend` and
`pipingspace/ironrag-frontend` images using BuildKit + GHA cache and
pushes them to Docker Hub under the release tag (and `latest` when
the dispatch input opts in).

That is the entire CI surface for this repository.
