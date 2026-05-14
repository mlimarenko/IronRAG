# IronRAG pipeline

This document describes the current end-to-end data path from source admission to retrieval and answer delivery.

## 1. Entry surfaces

The content pipeline starts from these HTTP surfaces:

- `POST /v1/content/documents` for inline text and structured payloads
- `POST /v1/content/documents/upload` for multipart file uploads
- `POST /v1/content/documents/{documentId}/append`
- `POST /v1/content/documents/{documentId}/edit`
- `POST /v1/content/documents/{documentId}/replace`
- `POST /v1/content/web-runs` for single-page and recursive web ingestion

The query pipeline starts from:

- `POST /v1/query/sessions/{sessionId}/turns`

The same core services back the web UI, HTTP handlers, and MCP tools. There is no separate ingestion or query stack for agents.

## 2. Unified source normalization

Every admitted source is normalized into structured blocks before chunking, embedding, graph extraction, or retrieval.

### Supported source families

- Text-like files: markdown, text, JSON, YAML, source code
- PDF through Docling-backed document-layout extraction with durable page-range checkpoints for stored revisions
- Static raster images through Docling OCR by default, or through the active `vision` binding when the library recognition policy selects `vision`
- DOCX and PPTX through Docling-backed structured block extraction
- Spreadsheets (`csv`, `tsv`, `xls`, `xlsx`, `xlsb`, `ods`) through native row-oriented extraction
- Web pages through HTML main-content extraction

### Recognition routing

Recognition routing is explicit catalog state, not a hidden runtime fallback.
New libraries inherit `IRONRAG_RECOGNITION_DEFAULT_RASTER_IMAGE_ENGINE`, which
accepts `docling` or `vision` and defaults to `docling`. Per-library updates use
`PUT /v1/catalog/libraries/{libraryId}/recognition-policy`.

PDF, DOCX, and PPTX layout extraction stays on the embedded Docling CPU runtime.
Spreadsheets stay on the native tabular parser. Static raster image OCR and
embedded document-picture OCR use Docling unless the library policy explicitly
selects `vision`. If a library routes image OCR to `vision` and no vision
binding is configured, ingestion fails loudly instead of silently falling back.
Video files are not part of the current ingest surface.

Stored PDF revisions use a restart-safe Docling path: the worker reads page
count first, extracts bounded page ranges, and persists each completed range as
an ingest unit. `IRONRAG_DOCLING_PAGE_BATCH_SIZE` controls the persisted range
size, `IRONRAG_DOCLING_PAGE_STREAM_WINDOW_PAGES` controls how many contiguous
pages are streamed through one Docling process (default: 40 pages), and
`IRONRAG_DOCLING_MAX_CONCURRENCY` bounds local Docling processes. Already
completed page ranges are reused after worker restart, backend restart, lease
loss, or network interruption.
`IRONRAG_INGESTION_HEAVY_PIPELINE_PARALLELISM=auto` controls how many large
PDF pipelines may be active before provider-bound stages. The automatic value
uses the worker CPU and memory cgroup limits and is capped at 4 by default;
it is also bounded by the configured Docling subprocess concurrency so heavy
jobs do not pile up unbounded behind `IRONRAG_DOCLING_MAX_CONCURRENCY`.

### Table contract

Tables have one standard path:

- spreadsheet rows,
- extracted table blocks from office documents,
- extracted table blocks from supported document parsers

all converge to the same markdown-table representation plus row-oriented normalized text. Retrieval and answering do not keep a parallel spreadsheet-only code path.

## 3. Storage model

### Postgres

Postgres stores core control and content metadata:

- IAM, users, sessions, tokens, grants
- workspaces and libraries
- documents, revisions, heads, mutations, async operations, and durable ingest units
- costs, audit events, runtime execution metadata

### Blob storage

Source bytes live behind `content_revision.storage_key` in the configured storage backend.

### ArangoDB

Arango stores structured document and graph material used by ingestion, retrieval, and topology APIs. It is the runtime data surface for graph-oriented reads and staged extraction artifacts.

## 4. Chunking

Chunking is unified and format-agnostic:

- target size: `2800` characters
- overlap: `280` characters
- heading-aware splits
- code-aware splits
- table-aware grouping
- near-duplicate suppression

Chunks are derived from structured blocks, not directly from raw files.

## 5. Enrichment stages

After normalization and chunking, IronRAG runs these enrichment stages:

- embeddings
- technical fact extraction
- graph extraction
- document summary and quality signals

### Graph extraction contract

- entity types come from the shared 10-type vocabulary
- relation types come from the shared relation catalog
- `sub_type` is metadata, not node identity
- node identity is based on normalized `(node_type, label)`
- support counts accumulate across admitted evidence
- provider JSON is repaired only for unambiguous UTF-8 transport damage, then
  validated before persistence; unrepaired mojibake or control characters fail
  the chunk loudly

### Graph key contract

Runtime graph nodes are written by one key:
normalized `(node_type, label)`. Extracted aliases can support lookup and
relation endpoint matching, but there is no separate full-library alias
resolution pass that rewrites node identity after ingestion. The result must
stay coherent across:

- query retrieval,
- graph topology,
- MCP graph tools,
- supporting document links.

## 6. Query and answer path

The query path uses one retrieval stack:

- lexical retrieval
- vector retrieval
- evidence assembly
- preflight answer preparation
- answer generation
- verification

Exact-literal technical questions use the same answer contract but may take a lexical-only fast path when the question clearly targets an endpoint, parameter name, or transport literal.

### Turn contract

`POST /v1/query/sessions/{sessionId}/turns` creates one persisted assistant
turn and query execution. UI callers may request `text/event-stream`; the
stream carries activity, failure, and completion events for that same
execution, and the completion payload contains the grounded answer, evidence
references, verifier state, and runtime execution handle. If the transport
drops after backend work starts, the frontend recovers by reading the durable
session result created after the request boundary instead of submitting another
turn. MCP transport streaming remains isolated under `/v1/mcp`.

## 7. Worker model

Background processing is lease-based and stage-driven. The worker is responsible for:

- content extraction
- structure preparation
- chunk processing
- embeddings
- technical facts
- graph extraction
- verification
- finalization
- web discovery and page materialization

The worker pool and the HTTP API use the same services and persistence model.
Each claimed job runs with an independent heartbeat observer, so long provider
or Docling calls cannot starve lease renewal. If the lease moves away, the
pipeline stops and the job is reclaimed from durable state; finalization uses
the active attempt lease rather than a stale in-memory success flag.

## 8. Library backup and restore

A library can be exported as a self-contained `.tar.zst` archive and restored on the same or a different IronRAG deployment.

### Export

```
GET /v1/content/libraries/{id}/snapshot?include=library_data,blobs
```

The response streams a tar archive compressed with zstd. Contents:

- `manifest.json` — schema version, library id, include scope
- `postgres/<table>/part-NNNNNN.ndjson` — chunked rows per table (64 MiB soft cap)
- `arango/<collection>/part-NNNNNN.ndjson` — knowledge docs
- `arango-edges/<collection>/part-NNNNNN.ndjson` — knowledge edges
- `blobs/<storage_key>` — original source files (opt-in via `blobs` include)
- `summary.json` — row counts observed during export

`include=library_data` covers all Postgres and Arango data. `blobs` adds the original uploaded files. The frontend uses a plain `<a href>` download — no JavaScript memory buffer.

### Import

```
POST /v1/content/libraries/{id}/snapshot?overwrite=reject|replace
Content-Type: application/zstd
Body: raw .tar.zst archive
```

The import reads the manifest from the archive to determine what was exported. `overwrite=replace` clears the existing library footprint before inserting. Postgres rows are bulk-inserted via `jsonb_populate_recordset` (1000 rows per statement). Arango documents use bulk AQL inserts.

## 9. Hard invariants

- One standard path per source family; no alternate legacy ingestion branches.
- One table representation across file types.
- One shared query pipeline for UI and MCP clients.
- One shared graph vocabulary used by search, topology, and relation listing.
- No client-specific answer assembly logic outside the query service.
