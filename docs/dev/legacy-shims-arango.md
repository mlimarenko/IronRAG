# ArangoDB-era legacy shims — removal inventory

Every site in this table carries the comment token `LEGACY-SHIM` so a future
release (planned ≥ 0.7.0) can find all sites in one pass:

```
grep -rn "LEGACY-SHIM" apps/api/src/ docs/dev/
```

The token format is:

```rust
// LEGACY-SHIM(arango-era, remove>=0.7.0): <one-line why it exists> — safe to delete once <concrete condition>.
```

All markers are on the line immediately above the legacy construct (or extend
an existing doc comment). No marker is duplicated across call sites that share
a definition-level marker.

---

## Marked sites

| File:line | Construct | Why it exists | Removal condition |
|-----------|-----------|---------------|-------------------|
| `apps/api/src/infra/arangodb/mod.rs:1` | Entire `infra::arangodb` module | ArangoDB knowledge-plane backend superseded by PostgreSQL in 0.5.0; retained as `IRONRAG_KNOWLEDGE_PLANE_BACKEND=arango` compat path for 0.4.x snapshot import | Retire the arango compat path and 0.4.x snapshot-import tooling |
| `apps/api/src/infra/arangodb/collections.rs:9` | `KNOWLEDGE_CHUNK_VECTOR_COLLECTION` / `KNOWLEDGE_ENTITY_VECTOR_COLLECTION` | Non-suffixed single-dim Arango vector collections; migration receivers for `ironrag-maintenance migrate vector-per-dim` | All deployments run vector-per-dim migration; Arango backend dropped |
| `apps/api/src/infra/arangodb/collections.rs:181` | `KNOWLEDGE_CHUNK_VECTOR_INDEX` / `KNOWLEDGE_ENTITY_VECTOR_INDEX` | ANN index names on the non-suffixed single-dim collections | Same as above |
| `apps/api/src/infra/arangodb/collections.rs:183` | `KNOWLEDGE_CHUNK_VECTOR_REVISION_GENERATION_INDEX`, `KNOWLEDGE_CHUNK_VECTOR_CHUNK_MODEL_INDEX`, `KNOWLEDGE_CHUNK_VECTOR_LIBRARY_INDEX` | Persistent index names on non-suffixed chunk-vector collection | Same as above |
| `apps/api/src/infra/arangodb/collections.rs:195` | `KNOWLEDGE_ENTITY_VECTOR_LIBRARY_INDEX` | Persistent index name on non-suffixed entity-vector collection | Same as above |
| `apps/api/src/infra/arangodb/collections.rs:294` | `KNOWLEDGE_PERSISTENT_INDEXES` entries for `KNOWLEDGE_CHUNK_VECTOR_COLLECTION` (×3) and `KNOWLEDGE_ENTITY_VECTOR_COLLECTION` (×1) | Bootstrap definitions that ensure persistent indexes on the non-suffixed legacy collections | Same as above |
| `apps/api/src/infra/arangodb/bootstrap.rs:200` | Legacy vector-index inspect + ensure block in `bootstrap_knowledge_plane` | Inspects and conditionally rebuilds ANN indexes on the non-suffixed single-dim Arango collections | Non-suffixed collections dropped; per-dim/per-library bootstrap loop is the only remaining path |
| `apps/api/src/infra/arangodb/document_store/types.rs:5` | `key: String`, `arango_id: Option<String>`, `arango_rev: Option<String>` in every `*Row` struct (`KnowledgeDocumentRow`, `KnowledgeRevisionRow`, `KnowledgeChunkRow`, `KnowledgeStructuredRevisionRow`, `KnowledgeStructuredBlockRow`, `KnowledgeTechnicalFactRow`) | ArangoDB document-identity fields (`_key`, `_id`, `_rev`); the PostgreSQL layer projects `NULL::text` for arango_id/arango_rev and casts the PK uuid to text for `key` | Arango backend and snapshot-import path removed; replace with plain `id: Uuid` in each struct |
| `apps/api/src/infra/postgres/pg_document_store.rs:41` | `DOCUMENT_COLUMNS`, `REVISION_COLUMNS`, `CHUNK_COLUMNS`, `STRUCTURED_REVISION_COLUMNS`, `STRUCTURED_BLOCK_COLUMNS`, `TECHNICAL_FACT_COLUMNS` — `NULL::text AS arango_id`, `NULL::text AS arango_rev`, `<pk>::text AS key` projections | SQL shims to satisfy the shared Arango-era row structs; always yield NULL for the arango fields | Same structs shed their `arango_id`/`arango_rev`/`key` fields (see types.rs marker above) |
| `apps/api/src/services/maintenance/migrate.rs:91` | `vector_per_dim` function | Drains non-suffixed single-dim Arango collections into per-dim shards; idempotent migration receiver | All deployments complete the migration; non-suffixed collections dropped |

---

## Sites intentionally not marked (inline marking unsafe or not applicable)

| Site | Reason |
|------|--------|
| `apps/api/migrations/*.sql` (all published) | All migration files are in `gh/master`; sqlx stores the raw sha384 of the file. Adding a comment to a published migration would cause `migration was modified` failures on every existing deployment. The non-suffixed legacy collections are **not** referenced in any migration SQL — no marking needed there anyway. |
| `apps/api/src/infra/arangodb/graph_store/candidates.rs` | Inside `infra::arangodb/` — covered by the module-level marker in `mod.rs`; duplicating per-file would violate the no-duplicate rule. |
| `apps/api/src/services/content/service/snapshot.rs` | Owned by another agent; off-limits per task constraints. The `knowledge_entity_candidate` / `knowledge_relation_candidate` Arango-era candidate/dedup collection references in this file are included in the inventory here for completeness. |
| `apps/api/src/services/query/search.rs:565` | Doc comment inside the Arango `embed_chunks` method (arango module, covered by module-level marker); describes the Arango `knowledge_chunk_vector` collection as background context. |
| `apps/api/src/services/knowledge/service.rs:927` | Historical note in a cache-key version bump comment; not a shim construct — no legacy code path at that callsite. |
| `apps/api/src/services/query/execution/document_target.rs`, `canonical_answer_context.rs`, `types.rs`, `search.rs`, `service/tests.rs` | Construction sites (`arango_id: None, arango_rev: None`) — downstream of the struct definitions. The compiler enforces removal of all initializers once the struct fields are removed. Marking each would be marker spam; the struct-definition marker in `types.rs` covers the chain. |

---

## Arango-era dedup / candidate collections

The following collections are Arango-era dedup/candidate tables that are never
written by the PostgreSQL knowledge plane:

- `knowledge_entity_candidate` — referenced in `infra/arangodb/graph_store/candidates.rs` (arango module, covered) and `snapshot.rs` (off-limits).
- `knowledge_relation_candidate` — same.

These have no live write path in 0.5.0+. They are included in
`DOCUMENT_COLLECTIONS` in `collections.rs` (arango module, module-level
marker covers), and in `snapshot.rs` (off-limits). Once the Arango backend is
removed, both collections and all references to them can be deleted.
