# IronRAG data pipeline

## Entry points

Content enters through:

- `POST /v1/content/libraries/{libraryId}/documents` for text JSON and
  `multipart/form-data` file uploads
- `POST /v1/content/documents/{documentId}/revisions` to append or replace text
  and replace a file through `multipart/form-data`
- `POST /v1/content/web-runs` for a single page or recursive web ingestion

Questions enter through `POST /v1/query/sessions/{sessionId}/turns`. The web UI
and MCP use the same stored content and retrieval service.

## Ingestion and retrieval

IronRAG normalizes documents before chunking, embedding, graph extraction, and
retrieval. It supports Markdown, text, source code, JSON, YAML, JSONL/NDJSON,
TOML, PDF, images, DOCX, PPTX, CSV, TSV, XLS, XLSX, XLSB, ODS, and web pages.
Video is not supported.

All current library policies are `{"rasterImageEngine":"vision"}`.
`IRONRAG_RECOGNITION_DEFAULT_RASTER_IMAGE_ENGINE` accepts only `vision`.
`PUT /v1/catalog/libraries/{libraryId}/recognition-policy` accepts that singleton
shape only; `xberg` is rejected, not treated as an alias. The library inspector
shows a fixed Vision requirement and opens the `extract_text` binding settings.
Image OCR, including scanned pages, uses the existing `extract_text` Vision
binding. Missing Vision configuration fails visual work explicitly; deterministic
native text/structure parsing does not require a Vision model.

Xberg handles PDF, DOCX, PPTX, text, Markdown/RST, HTML, CSV/TSV and ordinary
spreadsheets. Product-specific projections retain named XLSX Tables and typed
JSON/JSONL/YAML/TOML record semantics.

- `IRONRAG_XBERG_PAGE_BATCH_SIZE` controls resumable page batches (default `10`).
- `IRONRAG_XBERG_TIMEOUT_SECS` bounds each child request (default and maximum `300`).

Xberg supplies native document text/structure only. There are no local OCR model
assets or OCR enable flags. See [capacity planning](./CAPACITY-PLANNING.md) for
worker and host budgets.

PostgreSQL stores users, workspaces, libraries, documents, search data, audit
events, and pgvector embeddings. Original source bytes use the configured
storage backend. The knowledge graph holds typed entities and relations from
indexed content.

Graph degradation does not turn a readable content ingest into a failed job.
The attempt remains `succeeded` and the job `completed`, with the non-fatal
`failure_code` value `graph_degraded` on the attempt returned by
`GET /v1/ingest/attempts/{attempt_id}`. Consumers do not need stage
`details_json` to detect this outcome. A ready graph clears that code.
Transient graph owner-record writes retry once; a proven-empty upstream 404
also gets at most one retry per provider call. A nonempty or unread 404 does
not qualify for that retry.

## Retrieval configuration

A library has `retrieval_config`, available through
`GET /v1/catalog/libraries/{id}/retrieval-config` and
`PUT /v1/catalog/libraries/{id}/retrieval-config`; writing requires admin
permission.

| Key path | Type | Default | Effect |
|---|---|---|---|
| `lexical.textSearchConfig` | string | `"simple"` | PostgreSQL full-text-search configuration. The name must exist in `pg_ts_config`; otherwise the API returns HTTP 400. |

For example, `"english"` enables English stemming, which can improve matching
for related forms at the cost of exact-form matching.

Answers combine lexical, vector, and graph retrieval with citations. When the
available material cannot support an answer, IronRAG reports that limitation
instead of presenting unsupported content as grounded.

## Source visibility

Multipart CREATE can complete an unadmitted document at the same external key
without replacing its document ID or historical revision. This requires one
matching source-less revision, no descriptor, empty head ownership, no dependent
work, and matching server-staged bytes, MIME and recorded source metadata. The
normal admission creates a new source-bearing revision and queues ingestion;
only successful worker publication makes it readable. JSON `storageKey` and
claimed checksums do not authorize this completion. An unsafe incomplete target
returns HTTP 409 with problem code `document_admission_incomplete`; clients retain
their pending request and progress rather than treating it as a confirmed
duplicate or permanent content rejection. Exact idempotent replay returns the
original admission, and complete documents keep normal duplicate semantics.

The version response advertises `contentVisibilityContract: "visibility-intent/1"`.
Clients must require that capability before submitting visibility-dependent work;
its absence does not authorize an older writable policy.

`POST /v1/content/documents/{documentId}/visibility-intents` accepts an independent
denial obligation even while payload processing is busy. The request contains
`idempotencyKey`, `sourceIdentity`, an explicitly present nullable
`materializationToken`, and the complete `replacement` descriptor: `checksum`,
`byteSize`, `mimeType`, `fileName`, `title`, `languageCode`, `sourceUri`, and
`documentHint`. Nullable metadata describes this replacement; it does not inherit
the previous revision's metadata. Retransmission uses the same key and identity.

Admission fingerprints use `v1:sha256:` and the SHA-256 digest of the complete
normalized identity encoded as compact UTF-8 JSON with object keys sorted by
UTF-8 bytes at every level. Arrays keep their order; schema numbers are integers.
Stored identities must already be normalized, and their fingerprint must match
the identity. A replay returns the original verified receipt and bundle IDs.
Snapshots retain the original identity and fingerprint when ownership is remapped.

Admission withdraws the public head and captures a retained revision floor.
Submit the matching replacement with the receipt's `visibilityIntentId`; its first
bound payload key is immutable. Successful unrelated or superseded work cannot
release the obligation. Only the exact bound READY item above the floor becomes
current. Old writable `visibilityPolicy` and `visibility_policy` fields are refused.

Public source, revision, chunk, metadata, retrieval, and cached-answer reads use
`content_current_revision`. The revision-list endpoint exposes the currently
published revision, not withdrawn history. Source filenames come from the
published item's immutable descriptor, never the mutable document label.
Protected metadata updates require replacement admission rather than rewriting
the accepted descriptor. Append requires an explicit current `baseRevisionId`.

DELETE accepts `Idempotency-Key`. Reuse of that key for another document is a
conflict; an exact retry returns the original mutation and operation. Deletion
clears public pointers while retaining the accepted item's base for audit.

## Library backup and restore

Export:

```
GET /v1/content/libraries/{libraryId}/snapshot?include=library_data,blobs
```

The response is a compressed `.tar.zst` containing `manifest.json`, PostgreSQL
data, and original source files when `blobs` is selected.

Import:

```
POST /v1/content/libraries/{libraryId}/snapshot?overwrite=reject|replace
Content-Type: application/zstd
Body: raw .tar.zst archive
```

The import returns `202 Accepted` and an `operationId`. Wait for the terminal
`ready` or `failed` status through `GET /v1/ops/operations/{operationId}`. A
successful operation's `resultJson` includes `postgresRowsByTable` and
`blobsRestored`; a failure provides `failureCode`.

An archive can restore into an empty library or back into its source library. It
cannot restore into a second library while the source library exists in the same
deployment; the operation fails with `snapshot_source_already_present`.

Archive schema version 6 preserves the retained revision counter, current
visibility intent and its floor, immutable admission request identity and
fingerprint, payload-to-intent binding, and revision-owned publication admissions.
The complete inventory and content-reference closure are validated before target
writes. Malformed identities, cross-scope references, ambiguous intent lineage,
invalid counter/floor relationships and forged readable owners refuse the import.
No readable owner, identity or visibility decision is reconstructed from history.
The retired `visibility_policy` column and `visibilityPolicy` identity input are
not part of schema 6 and are rejected. Current library recognition policies accept
only `vision`; historical extraction provenance remains opaque and unchanged.

Backup is source-owned, not a public-read projection: when `blobs` is selected,
restricted historical and pending revision bytes travel too. Restriction survives
restore and null-head repair; only the exact bound READY item becomes current.
Chunk-vector sections retain the current-only lifecycle and observed lane counts.
Library and workspace archives use schema 6 exclusively; earlier versions are
refused with `snapshot_unsupported_schema_version`, without conversion or fallback.
Preserve each original archive and its immutable creator runtime. Restore an older
archive with that runtime in isolation, apply the guarded native baseline
transitions including `vision-recognition-schema`, then export schema 6 with the
current runtime. Schema-5 Xberg policies are not silently converted. The current
reader has no compatibility parser.
Library and workspace exports require existing system-administrator authority;
ordinary document/library readers and workspace administrators cannot export history.
