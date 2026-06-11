# PostgreSQL knowledge plane in 0.5.0

Status: superseded by the 0.5.0 release.

IronRAG 0.5.0 stores the knowledge plane in PostgreSQL 18 with pgvector and
PostgreSQL full-text search. The deployed stack does not provision the previous
document database. The baseline schema is
[`apps/api/migrations/0001_init.sql`](../../apps/api/migrations/0001_init.sql).

The knowledge plane includes documents, revisions, structured blocks, chunks,
technical facts, entities, relations, evidence, context bundles, runtime graph
data, chunk vectors, entity vectors, and lexical search material. Vector
material is organized by `(library, dim)` and tracked by a manifest table.

For existing 0.4.x deployments, the upgrade path is snapshot-based rather than
an in-place database migration. See the
[0.5.0 changelog entry](../../CHANGELOG.md#050--2026-06-02) and the
[README upgrade section](../../README.md#upgrading-from-04x).
