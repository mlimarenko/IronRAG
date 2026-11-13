# PostgreSQL image

The database image extends a pinned pgvector image with PostgreSQL 18.6 and
vector 0.8.7. It upgrades only the four Debian Perl packages to the fixed
`5.36.0-7+deb12u4` versions. The build rejects changes to any other installed
package and checks that the PostgreSQL, libpq and vector runtime files retain
their original hashes.

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

The base image's entrypoint, user and data-directory contract are inherited.
Updating the image must not initialize an existing database, replace its PVC or
upgrade the vector extension. Verify PostgreSQL and vector versions, data
identity, application health and the exact-digest vulnerability report before
accepting a deployment.
