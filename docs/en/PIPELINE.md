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

- Text-like files: markdown, text, source code
- Structured-record files — JSON (object or array), YAML (single document, `---` stream, or sequence of mappings), JSONL/NDJSON, and TOML — through one key-agnostic record extractor. Every field at any nesting depth is flattened to searchable text, heterogeneous schemas (different keys per record) are profiled, and any value shaped like a timestamp (RFC3339 or epoch, under any key name) time-stamps its record for temporal retrieval. There is no per-format or per-field special-casing: an arbitrary export, event log, config dump, or session transcript all flow through the same generic path.
- PDF through Docling-backed document-layout extraction with durable page-range checkpoints for stored revisions
- Static raster images through Docling OCR by default, or through the active `vision` binding when the library recognition policy selects `vision`
- DOCX and PPTX through Docling-backed structured block extraction
- Spreadsheets (`csv`, `tsv`, `xls`, `xlsx`, `xlsb`, `ods`) through native row-oriented extraction
- Web pages through HTML main-content extraction

### Recognition routing

Recognition routing is explicit catalog state, not a hidden runtime fallback.
New libraries inherit `IRONRAG_RECOGNITION_DEFAULT_RASTER_IMAGE_ENGINE`, which
accepts `docling` or `vision` and defaults to `vision`. Per-library updates use
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
Before launching a Docling process, the worker checks current cgroup hard
memory headroom against the minimum one-process budget. If one process cannot
fit, the document fails with a terminal `docling_insufficient_memory` ingest
error instead of spawning Python and entering a SIGKILL/retry loop.
`IRONRAG_INGESTION_HEAVY_PIPELINE_PARALLELISM=auto` controls how many large
PDF pipelines may be active before provider-bound stages. The automatic value
uses the worker CPU and memory cgroup limits and is capped at 4 by default;
it is also bounded by the configured Docling subprocess concurrency so heavy
jobs do not pile up unbounded behind `IRONRAG_DOCLING_MAX_CONCURRENCY`.
The worker's canonical ingest claim loop has a second memory guard: it derives
the maximum active jobs for that process from the resolved cgroup soft memory
limit before claiming more leases. This guard is independent from the
deployment-wide global / workspace / library caps and protects small swapless
hosts from stacking several memory-heavy jobs in one worker process.

### Table contract

Tables have one standard path:

- spreadsheet rows,
- extracted table blocks from office documents,
- extracted table blocks from supported document parsers

all converge to the same markdown-table representation plus row-oriented normalized text. Retrieval and answering do not keep a parallel spreadsheet-only code path.

## 3. Storage model

### PostgreSQL

PostgreSQL stores the control plane and the knowledge plane:

- IAM, users, sessions, tokens, grants
- workspaces and libraries
- documents, revisions, heads, mutations, async operations, and durable ingest units
- costs, audit events, runtime execution metadata
- structured blocks, chunks, technical facts, graph data, evidence, context bundles
- pgvector embeddings and PostgreSQL full-text search material

### Document parentage

A document admitted as a dependent of another source document (a page
attachment or inline image) records a canonical `parent_document_id` and a
typed `document_role` (`primary`, `attachment`, or `attached_context`). The
role is decided once — at admission or by the per-library parentage backfill —
from structural inputs only: whether a parent was declared, plus the revision's
media class (a raster-image child becomes `attached_context`; any other child
stays a peer `attachment`; no parent makes it `primary`). Retrieval reads the
typed role; it never inspects MIME, extension, or filename. The role is mirrored
onto the knowledge-plane document row that the query path reads.

### Blob storage

Source bytes live behind `content_revision.storage_key` in the configured storage backend.

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

### Per-library retrieval configuration

Each library carries a `retrieval_config` JSON object that controls how its
retrieval lanes are parameterized. The configuration is stored in the
`catalog_library.retrieval_config` column and is read and updated through
`GET /v1/catalog/libraries/{id}/retrieval-config` and
`PUT /v1/catalog/libraries/{id}/retrieval-config` (admin write permission
required).

**Current knobs** (absent keys resolve to their defaults):

| Key path | Type | Default | Effect |
|---|---|---|---|
| `lexical.textSearchConfig` | string | `"simple"` | PostgreSQL FTS text-search configuration name used in the lexical lane (`websearch_to_tsquery` and `to_tsquery` calls). Must name an entry in `pg_ts_config`; unknown names are rejected with HTTP 400. |

The default value `"simple"` reproduces the historical hardcoded behavior
byte-for-byte: rendered SQL with the default config is string-identical to the
original constant. Changing the config to, for example, `"english"` switches
the lexical lane to the English dictionary with stemming, which improves recall
for morphologically related terms at the cost of exact-form matching.

The configuration is validated at write time: the backend queries
`pg_ts_config` and rejects config names not present in the database. This
catches typos before they silently degrade retrieval.

The query path uses one retrieval stack:

- lexical retrieval
- vector retrieval
- evidence assembly
- preflight answer preparation
- answer generation
- verification

The lexical lane planner derives its high-level and low-level seeds from the
compiled `QueryIR`, not from keyword position. Subjects, objects, target types,
document focus, comparison operands, and exact literals feed the high-level
lane; modifiers, comparison dimensions, temporal constraints, and source-slice
refinements feed the low-level lane. If the IR is absent, low-confidence,
seedless, or produces no keyword matches, both lanes use the full extracted
keyword set to preserve the previous lexical behavior.

Exact-literal technical questions use the same answer contract but may take a lexical-only fast path when the question clearly targets an endpoint, parameter name, or transport literal.

Setup and versioned procedure questions have additional structural
lanes before free-form answer generation. Broad setup requests can return a
deterministic setup-variants answer when retrieved documents expose
grounded item anchors, command anchors, paths, sections, or
parameters across multiple plausible variants. A request that already focuses
one document or subject keeps the focused path instead of being broadened by
the multi-variant shortcut. Versioned procedure questions build a
subject/acronym profile from the typed query plan and document labels, then
require ordered procedure evidence before a transition document can dominate
generic release notes or compatibility pages. Exact instruction-title procedure
anchors are protected while retrieval is truncated and while topical pruning
removes generic tails. Reranking can raise ordinary relevance chunks, but it
does not lower absolute scores from protected evidence lanes such as document
identity, focused document, query-IR focus, or procedure anchors. When a query
is typed as an update procedure, the inferred latest-version inventory fallback
is disabled so release/changelog lists cannot preempt the deterministic
procedure answer. Transport assignment rendering remains separate: it requires
typed port/protocol/connection intent plus concrete `name = value` evidence,
and typed service/port inventories without a connection signal stay on the
normal synthesis path.

Documents whose typed role is `attached_context` (raster-image attachments of a parent page) are subordinate context, not competing peers: their chunks are demoted below peer and primary content when the final context is selected, they are excluded from the clarify-vs-answer disposition, and they never become a clarify variant. A page's one-chunk image attachments therefore cannot flood an answer's context or a clarification menu and displace the parent page's own evidence. The exception is a query that explicitly focuses on the attachment itself, which keeps it in the primary band.

When retrieval shows that a subjectless question matches several distinct subjects, the answer leads with a grounded excerpt for one subject and asks which subject to focus on. The choice list is derived from the library's own data (document grouping or knowledge-graph entity evidence), never from a hardcoded subject list. This also covers the deterministic latest-version inventory path: a release-inventory question with no scoping subject clarifies when the listed release documents mention several distinct graph subjects, while a query scoped by an entity, a document focus, or a literal keeps the flat latest-versions list.

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
- `blobs/<storage_key>` — original source files (opt-in via `blobs` include)
- `summary.json` — row counts observed during export

`include=library_data` covers the PostgreSQL library data, including knowledge-plane rows. `blobs` adds the original uploaded files. The frontend uses a plain `<a href>` download without a JavaScript memory buffer.

### Import

```
POST /v1/content/libraries/{id}/snapshot?overwrite=reject|replace
Content-Type: application/zstd
Body: raw .tar.zst archive
```

The import reads the manifest from the archive to determine what was exported. `overwrite=replace` clears the existing library footprint before inserting. PostgreSQL rows are bulk-inserted via `jsonb_populate_recordset` (1000 rows per statement). The restore path accepts current v6 PostgreSQL archives and v5 archives from 0.4.x.

## 9. Hard invariants

- One standard path per source family; no alternate legacy ingestion branches.
- One table representation across file types.
- One shared query pipeline for UI and MCP clients.
- One shared graph vocabulary used by search, topology, and relation listing.
- No client-specific answer assembly logic outside the query service.
