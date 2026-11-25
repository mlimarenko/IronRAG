# PostgreSQL image

The database image extends a pinned pgvector image with PostgreSQL 19.0 and
vector 0.8.7. One `apt-get install --only-upgrade` transaction moves nine
Debian packages to their fixed versions: the four Perl packages
(`5.36.0-7+deb12u4`), `apt` and `libapt-pkg6.0` (`2.6.2`), `openssl` and
`libssl3` (`3.0.22-1~deb12u2`), and `libpcre2-8-0` (`10.42-1+deb12u2`); the apt
and OpenSSL pins are the bookworm-security fixes. The build requires each pinned
package to be installed, equal to its pin and not below it, and rejects a change
to any other installed package.

The base image installs GnuPG only to verify apt repositories during the build.
Neither the entrypoint nor the chart invokes `gpg`, and the container runs with a
read-only root filesystem. The build purges `gnupg`, `gpg`, `gpg-wks-client`,
`gpg-wks-server` and `libsqlite3-0`; this removes CVE-2025-7458 without touching
PostgreSQL or vector. `gpgv` stays installed because apt uses it for repository
signatures.

`zlib1g` is reported through the zlib source package, but Debian does not build
`contrib/minizip` in bookworm and no minizip binary is installed. `libxml2` is
linked by PostgreSQL itself and has no fixed bookworm package; the image keeps
the bookworm library and the finding remains a publisher limitation.

`/usr/local/bin/gosu` is copied from the canonical
`piping-space/infrastructure/argocd/postgres` image instead of being built
here; the build checks the accepted binary's `sha256` and `gosu --version`.
PostgreSQL, libpq and vector runtime files keep their original hashes.

`image:postgres:build` publishes the source-bound artifact to the project's
`postgres` image repository. It is separate from the application release cohort;
select its immutable digest through `dependencies.postgres.image` and provide
the registry projection through the chart's existing `imagePullSecrets` value.
The chart's public pgvector default is unchanged.

For a native local build from the repository root:

```sh
docker build --file charts/ironrag/postgres/Dockerfile --target postgres \
  --build-arg CI_COMMIT_SHA="$(git rev-parse HEAD)" \
  --tag ironrag-postgres:local charts/ironrag/postgres
```

The image inherits the base entrypoint, UID/GID 999:999 and
`PGDATA=/var/lib/postgresql/19/docker`. The entrypoint refuses a volume with
data in the standard PostgreSQL 18 directory. It does not locate arbitrary
custom data directories or upgrade an existing cluster. Follow the
[major-version upgrade instructions](../../../docs/en/INSTALL.md#upgrade-bundled-postgresql-to-19).
Verify the database, vector extension, application readiness and exact-digest
vulnerability report before accepting the cutover.
