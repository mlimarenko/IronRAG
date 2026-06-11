# Legacy Shim Inventory — Snapshot Import/Export

## Convention

Every backward-compatibility shim in the snapshot pipeline is annotated with
the greppable token `LEGACY-SHIM`:

```
// LEGACY-SHIM(old-archive-compat, remove>=0.7.0): <one-line why it exists> — safe to delete once <concrete condition>.
```

### Sweep workflow

```bash
grep -rn "LEGACY-SHIM" apps/api/src/services/content/service/snapshot.rs
```

Each result maps 1:1 to a row in the table below. Once the removal condition
is satisfied for a group of markers, delete every marked construct in one
sweep. Confirm the grep returns zero hits for the removed group before
releasing.

---

## Inventory

| file:line | construct | why it exists | removal condition |
|---|---|---|---|
| `apps/api/src/services/content/service/snapshot.rs:168` | `MIN_SUPPORTED_SNAPSHOT_SCHEMA_VERSION = 5` constant | Lower bound of 5 admits v5 (ArangoDB-era) archives alongside current v6 archives | Raise constant to 6 once no v5 archives remain in the field |
| `apps/api/src/services/content/service/snapshot.rs:338` | `#[serde(default)]` on `SnapshotManifest::vector_shards` | v5 manifests pre-date per-library vector dimensions and have no `vector_shards` key; `serde(default)` prevents a deserialization error | Remove once no pre-per-dim-vector (v5) archives remain in the field |
| `apps/api/src/services/content/service/snapshot.rs:1367` | Export-path comment + empty-arango-lists intent | Export produces empty `arango_doc_collections`/`arango_edge_collections` in v6; restore still accepts non-empty arango sections for the 0.4.x→0.5.0 upgrade boundary | Remove comment and the restore branches below once no v5 archives remain in the field |
| `apps/api/src/services/content/service/snapshot.rs:2121` | `arango-edges/` restore streaming branch (`else if path.strip_prefix("arango-edges/")`) | Streams and applies v5 ArangoDB-era edge sections as PostgreSQL UPDATE statements | Delete entire branch once no v5 archives remain in the field |
| `apps/api/src/services/content/service/snapshot.rs:2157` | `arango/` restore streaming branch (`else if path.strip_prefix("arango/")`) | Streams v5 ArangoDB-era document collection rows, maps them to PG tables, routes through dedup | Delete entire branch once no v5 archives remain in the field |
| `apps/api/src/services/content/service/snapshot.rs:2341` | `pg_table_for_arango_doc_row()` function | Maps v5 ArangoDB collection names (`knowledge_document`, `knowledge_chunk`, legacy `knowledge_chunk_vector`, etc.) to their PostgreSQL restore target table names | Delete function and all callers once no v5 archives remain in the field |
| `apps/api/src/services/content/service/snapshot.rs:2375` | `normalize_arango_doc_for_pg()` function | Strips Arango-internal fields (`_id`, `_rev`, `_key`, `search_tsv`) and renames `vector`→`embedding` so v5 rows are insertion-ready for PG | Delete function and all callers once no v5 archives remain in the field |
| `apps/api/src/services/content/service/snapshot.rs:2805` | `parent_document_id` NULL-remap inside `KnowledgeDocumentDedup::finalize()` | The dedup can drop a parent document while keeping its child; NULLing the dangling FK avoids an insert failure for v5/early-v6 archives that contain multiple docs per external key with parentage set | Safe to delete once no pre-parentage v5/early-v6 archives with dedup-dropped parents remain in the field; note the `KnowledgeDocumentDedup` struct itself is NOT a shim — it deduplicates live re-sync duplicates in all archive versions |
| `apps/api/src/services/content/service/snapshot.rs:2998` | `edge_targets_dropped_chunk()` function | Guards the two v5 arango edge kinds that INSERT new rows (`knowledge_bundle_chunk_edge`, `knowledge_chunk_mentions_entity_edge`) so chunks dropped by the dedup do not produce orphan rows; all other v5 edge kinds are UPDATE-only and silently no-op | Delete together with the `arango-edges/` branch once no v5 archives remain in the field |
| `apps/api/src/services/content/service/snapshot.rs:3041` | `ArangoEdgePgBatcher` struct + `impl` | Buffers v5 arango edge-section rows and flushes them in batches to `apply_arango_edges_to_pg` | Delete struct and impl once no v5 archives remain in the field |
| `apps/api/src/services/content/service/snapshot.rs:3110` | `apply_arango_edges_to_pg()` function | Applies all v5 arango edge collection kinds as targeted PG UPDATE statements (document↔revision, revision↔block, revision↔chunk, block↔chunk, entity/relation/evidence graph edges, bundle-chunk INSERTs) | Delete function once no v5 archives remain in the field |
| `apps/api/src/services/content/service/snapshot.rs:3799` | Call site: `backfill_missing_document_role(table, &mut rows)` | Invokes the pre-parentage backfill on every PG bulk-insert batch | Remove call site together with the function once no pre-0.5.0-parentage archives are restored |
| `apps/api/src/services/content/service/snapshot.rs:3841` | `backfill_missing_document_role()` function | Pre-parentage archives lack the `document_role` column; a JSONB bulk-insert supplies an explicit NULL which bypasses the `NOT NULL DEFAULT 'primary'` constraint — this function injects the default so the insert succeeds | Remove once no pre-parentage (pre-0.5.0) archives remain in the field |
