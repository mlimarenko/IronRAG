# IronRAG pipeline

This document describes the current canonical data path from source admission to retrieval and answer delivery.

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

The same canonical services back the web UI, HTTP handlers, and MCP tools. There is no separate ingestion or query stack for agents.

## 2. Canonical source normalization

Every admitted source is normalized into structured blocks before chunking, embedding, graph extraction, or retrieval.

### Supported source families

- Text-like files: markdown, text, JSON, YAML, source code
- PDF through Docling-backed document-layout extraction
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
Spreadsheets stay on the native tabular parser. Static raster image OCR can use
either Docling or the active `vision` binding. If a library routes image OCR to
`vision` and no vision binding is configured, ingestion fails loudly instead of
silently falling back. Video files are not part of the current ingest surface.

### Table contract

Tables have one canonical path:

- spreadsheet rows,
- extracted table blocks from office documents,
- extracted table blocks from supported document parsers

all converge to the same markdown-table representation plus row-oriented normalized text. Retrieval and answering do not keep a parallel spreadsheet-only code path.

## 3. Storage model

### Postgres

Postgres stores canonical control and content metadata:

- IAM, users, sessions, tokens, grants
- workspaces and libraries
- documents, revisions, heads, mutations, and async operations
- costs, audit events, runtime execution metadata

### Blob storage

Source bytes live behind `content_revision.storage_key` in the configured storage backend.

### ArangoDB

Arango stores structured document and graph material used by ingestion, retrieval, and topology APIs. It is the runtime data surface for graph-oriented reads and staged extraction artifacts.

## 4. Chunking

Chunking is canonical and format-agnostic:

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

- entity types come from the canonical 10-type vocabulary
- relation types come from the canonical relation catalog
- `sub_type` is metadata, not node identity
- node identity is based on normalized `(node_type, label)`
- support counts accumulate across admitted evidence

### Entity resolution contract

Entity resolution merges aliases and normalized duplicates into one runtime vocabulary. The result must stay coherent across:

- query retrieval,
- graph topology,
- MCP graph tools,
- supporting document links.

## 6. Query and answer path

The query path uses one canonical retrieval stack:

- lexical retrieval
- vector retrieval
- evidence assembly
- canonical preflight answer preparation
- answer generation
- verification

Exact-literal technical questions use the same answer contract but may take a lexical-only fast path when the question clearly targets an endpoint, parameter name, or transport literal.

### Turn contract

`POST /v1/query/sessions/{sessionId}/turns` is a single JSON request/response
turn. The response contains the completed grounded answer, evidence references,
verifier state, and runtime execution handle. Incremental answer streaming is
not a separate UI assistant execution path; MCP transport streaming remains
isolated under `/v1/mcp`.

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

The worker pool and the HTTP API use the same canonical services and persistence model.

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

- One canonical path per source family; no alternate legacy ingestion branches.
- One canonical table representation across file types.
- One canonical query pipeline for UI and MCP clients.
- One canonical graph vocabulary used by search, topology, and relation listing.
- No client-specific answer assembly logic outside the query service.
