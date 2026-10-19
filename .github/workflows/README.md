# GitHub workflows

`quality.yml` runs on pull requests and pushes to `master` or `develop`,
and before Docker Hub publication.

Secret scanning, dependency security, baseline integrity, builds, contract checks
and functional tests are required. Style, static analysis and coverage are
advisory: their failures remain visible in Actions but do not block publication.
Formatting and lint hooks run on demand with
`pre-commit run --hook-stage manual --all-files`.

`make migration-check` verifies the single baseline
`apps/api/migrations/0001_init.sql` against its recorded checksum.

`release-docker-images.yml` publishes backend, frontend, BookStack, Confluence,
Git repositories and YouTrack images on a published GitHub release or manual
`workflow_dispatch`. Images use the release tag; stable releases also update
`latest`, while manual dispatch controls it with `push_latest`.
