# Changelog

## Unreleased

## 0.5.3 — 2026-06-26

### Changed

- **The knowledge plane is PostgreSQL-only.** The backend no longer ships the
  removed ArangoDB client, bootstrap, document/search/graph stores, dependency
  mode, or contract tests. Configuration now accepts only the canonical
  PostgreSQL knowledge-plane backend, and operational diagnostics report any
  non-canonical backend value as unsupported.

- **Snapshot archives are PostgreSQL-only.** The snapshot schema is bumped to
  v7, new exports no longer declare legacy Arango archive sections, and restore
  no longer accepts v5 `arango/` or `arango-edges/` archive payloads. v6
  PostgreSQL archives remain the minimum supported restore format.

- **Frontend container builds now target Node 26.** The web Dockerfile and
  package metadata are aligned on the Node 26 runtime used for release builds,
  while the lockfile records the same engine contract.

- **Release metadata is aligned for 0.5.3.** The Rust workspace package
  version, generated OpenAPI `info.version`, Helm chart, and README image-tag
  example now point at 0.5.3.

### Fixed

- **Library restore is stricter about archive boundaries and object payloads.**
  Restore now validates snapshot object paths before opening them, keeps
  per-document restore state isolated, and avoids carrying stale revision or
  storage metadata across documents.

- **Public configuration examples no longer mention the removed ArangoDB
  dependency.** `.env.example` and MCP parity notes now describe the current
  PostgreSQL/Redis-only local contract.

## 0.5.2 — 2026-06-25

### Changed

- **Backend and embedded extraction images now consume Debian Trixie
  backports during image builds.** The Rust builder and Docling runtime stages
  enable a pinned `trixie-backports` source and run a package upgrade before
  installing build/runtime dependencies, keeping the shipped image base current
  without changing the public runtime contract.

- **The embedded Docling extraction runtime is refreshed to Docling 2.107.0.**
  The Dockerfile and local Compose build default now agree on the same Docling
  version, so local image builds exercise the extractor stack that the release
  image ships.

- **The local Compose build now uses the same PyTorch CPU runtime defaults as
  the release Dockerfile.** The local backend image build no longer overrides
  the Dockerfile's torch and torchvision versions with older defaults.

- **Release metadata is aligned across runtime, API contract, Helm, and docs.**
  The Rust workspace package version, generated OpenAPI `info.version`, Helm
  chart, and README image-tag example now point at 0.5.2.

- **Release Docker workflows are titled by tag.** Manual and release-triggered
  image publication runs now show the Docker tag as the run title in GitHub
  Actions instead of a generic workflow title.

- **Frontend build tooling lockfile entries are refreshed.** The web
  package-lock now tracks current transitive tooling packages from the existing
  dependency graph, reducing stale frontend build/runtime dependencies without
  changing the product UI contract.

### Fixed

- **Local Compose builds no longer drift from the release Dockerfile's
  extraction stack.** The default Docling, torch, and torchvision build
  arguments in `docker-compose.yml` now match the Docker image defaults,
  preventing local validation from accidentally testing older embedded
  extractor dependencies.

- **Fresh release image builds can resolve PyTorch CPU transitive
  dependencies.** The Dockerfile keeps PyTorch wheels on the CPU index while
  allowing dependency packages such as resolver-only pure Python wheels to come
  from PyPI, fixing the `mpmath` resolver failure seen in the 0.5.2 image
  publication workflow.

- **Release image builds tolerate transient external tool download failures.**
  The backend Dockerfile now bounds and retries the linker archive download,
  keeps checksum verification for any downloaded archive, and falls back to the
  system linker when the external asset is temporarily unreachable.

- **Installer upgrades no longer keep stale official IronRAG image tags.**
  Re-running the installer for a new release now updates official
  `pipingspace/ironrag-*` image pins in `.env` to the selected release tag while
  preserving secrets, resource caps, and custom/local image overrides.

## 0.5.1 — 2026-06-18

### Changed

- **Grounded answer generation is now driven by typed intent and evidence
  shape instead of broad relevance alone.** Compared with 0.5.0, query
  execution relies more on document identity, exact literals, table rows,
  ordered procedures, source-local context, and verifier-visible citations.
  Deterministic answer builders now cover setup variants, versioned
  procedures, latest-version inventories, exact literal recovery, and
  table-field inventories without corpus-specific routing or knowledge-domain
  keyword lists.

- **Local and single-host deployments now default to bounded ingest fan-out.**
  The Docker-local envelope is aligned with the worker's memory-aware lease
  claiming, so small swapless hosts prefer predictable progress over claiming
  many memory-heavy extraction or graph jobs at once.

### Fixed

- **Ingest workers no longer stack memory-heavy jobs beyond their cgroup
  budget.** The worker process now derives a local job-claim cap from its
  resolved memory soft limit, reserving headroom and budgeting each active
  canonical ingest job before claiming more leases. Docker Compose and
  `.env.example` now default the deployment-wide ingest caps to
  `global=4`, `workspace=2`, `library=1`, so several replicas cannot
  simultaneously pile graph-merge / extraction work onto a small swapless host.
  This addresses host-level OOM pressure by reducing in-process fan-out rather
  than relying on larger containers.

- **Docling extraction fails fast when a worker cannot fit one extractor
  process.** Before spawning the embedded Python extractor, the worker checks
  detected cgroup memory headroom against the one-process Docling budget.
  Undersized workers now mark the affected document with a terminal
  `docling_insufficient_memory` ingest error instead of entering a
  SIGKILL/retry loop; larger PDF / office / OCR imports should raise
  `IRONRAG_WORKER_MEMORY_LIMIT` explicitly.

- **Focused setup answers and latest-version inventories use the typed query
  plan more precisely.** Configure/how-to questions that name one concrete
  subject now enter the focused-document retrieval lane even when the compiler
  emitted generic target types, while ambiguous, comparative, temporal, or
  literal-constrained queries stay out of that shortcut. Explicit "latest N"
  source-slice answers can also build deterministic ordered responses from
  focused release-history chunks when version markers live in the chunk body
  rather than the document title, without admitting ordinary relevance noise.

- **Broad setup and versioned procedure questions stay on grounded procedure
  evidence instead of clarifying or drifting to adjacent transition text.**
  Multi-document setup requests can now return a deterministic
  setup-variants answer built from item, command, path, section, and
  parameter anchors, while document-focused setup requests keep their focused
  answer path. Versioned procedure questions use subject and acronym overlap
  plus ordered procedure evidence to rank transition documents ahead of
  generic release or compatibility pages. Exact instruction-title anchors are retained
  through context truncation and topical pruning, rerank no longer lowers
  absolute scores from protected evidence lanes, and inferred latest-version
  source-slice fallback now yields to typed lifecycle-procedure intent instead of
  preempting the deterministic procedure extractor. Exact transport assignment
  rendering is gated by typed `QueryIR` targets and concrete `name = value`
  evidence, so service/port inventories do not steal setup answers.

- **Table-column inventory answers keep table metadata separate from row
  fields.** Row-oriented table chunks now treat `Sheet` and `Table` as
  structural metadata only before the `Row N` marker; after the row marker they
  remain ordinary cell fields. This prevents index or relation tables whose
  data columns are named `Table`/`Column` from being mistaken for the schema
  table being described, while preserving the same generic table representation
  across PDF, office, and spreadsheet extraction paths.

### Added

- **Retrieval rank metrics for live grounded-query benchmarks.** The benchmark
  runner now emits document and chunk ranking metrics for cases with known
  relevance: `hit@1`, `hit@3`, `hit@5`, `hit@10`, and `MRR`. Document relevance
  is derived from each synthetic suite's expected document checks, with optional
  `rank_relevance.json` entries for explicit document IDs and stable chunk
  markers. Each run writes per-suite and matrix summaries and appends a compact
  `rank_metrics_trend.jsonl` record under the configured output directory;
  `compare_benchmarks.py` now reports rank-metric deltas between two result
  directories.

- **Shared per-provider outbound concurrency budget with query-lane priority.**
  All outbound provider calls (embedding, chat, tool-use, vision) now pass
  through an aggregate concurrency limiter keyed by provider endpoint identity,
  shared across the ingest and query lanes. A configurable per-provider cap
  (`IRONRAG_PROVIDER_CONCURRENCY_MAX_OUTBOUND`) bounds in-flight calls so a
  self-hosted or rate-limited provider is not overwhelmed when heavy ingest
  fan-out and concurrent query turns hit the same endpoint at once. A
  configurable query reserve (`IRONRAG_PROVIDER_CONCURRENCY_QUERY_RESERVED`)
  guarantees latency-sensitive query turns a minimum of concurrent permits even
  under saturating ingest load, so ingest can never starve query. The default is
  unlimited (cap `0`), preserving previous behaviour until an operator opts in.

- **Declarative per-library retrieval configuration.** Each library now stores a
  typed `RetrievalConfig` (jsonb column `retrieval_config` on `catalog_library`,
  added by migration `0002_retrieval_config.sql`). The initial knob is
  `lexical.textSearchConfig` — the PostgreSQL FTS text-search config name used
  in the lexical search lane (default: `"simple"`, matching historical
  behaviour). New GET/PUT endpoints `/v1/catalog/libraries/{id}/retrieval-config`
  let operators read and update the config; the text-search config name is
  validated against `pg_ts_config` at write time, so unknown config names are
  rejected with a 400 error.

- **Typed answer candidates and clarification metadata in `grounded_answer`.**
  When the pipeline resolves an ambiguous query to multiple distinct subjects,
  `structuredContent` now includes `answerCandidates` (array of
  `{ label, kind, confidence?, provenance: { entityId?, documentId?, chunkId? } }`)
  and `clarification` (`{ required, question? }`). On unambiguous answers both
  fields are present but empty/false for contract stability. The same fields are
  carried through the REST turn-completion payload (`AssistantExecutionDetail`)
  for MCP–UI parity. No new retrieval is performed; the data is sourced from
  already-computed clarify structures in the answer pipeline.

- **Installer non-interactive mode and TUI wizard.** `install.sh` now answers
  every prompt from flags and environment variables, so the same install is
  reproducible from CI / Ansible. New `--port`, `--profile`, and `--admin-login`
  flags join the existing `IRONRAG_PORT` / `IRONRAG_PROFILE` / `IRONRAG_ADMIN_LOGIN`
  and provider-key environment variables; precedence is flags > env > prompt for
  non-secret values. Secrets (admin password, provider API keys) are accepted via
  environment variables or a pre-seeded `.env` only — never as flags — because
  argv is visible to other processes and shell history. Non-interactive mode is
  auto-selected when stdin is not a TTY (the canonical `curl | bash` path) and
  fails fast with the exact flag/env var to set rather than hanging when a
  required value has no safe default. `--help` documents every flag and variable.
  Interactively, the wizard now shows numbered step headers and a review screen of
  the chosen values before writing `.env`. Behaviour with default answers is
  unchanged: the generated `.env` is byte-identical to the previous installer.

### Changed

- **Typed QueryIR now drives lexical high/low retrieval lanes.** The retrieval
  planner removed the positional keyword split and derives lane seeds from the
  compiled `QueryIR`: subject/object entities, target types, document focus,
  comparison operands, and exact literals feed the high-level lane; modifiers,
  comparison dimensions, temporal constraints, and source-slice refinements feed
  the low-level lane. When the IR is absent, low-confidence, seedless, or does
  not match any extracted keyword, both lanes fall back to the full keyword set,
  preserving the previous lexical SQL behavior instead of reintroducing
  position-based routing.

### Removed

- **Unreachable query helper paths.** The dead multi-query and sub-question
  helper functions were removed after reachability checks found no production
  callers. The HyDE flag path remains in place because it is still consumed by
  the embedding planning flow.

### Fixed

- **Postgres pool fan-out is bounded by one deployment-wide app connection
  budget.** `IRONRAG_DATABASE_MAX_CONNECTIONS` now represents the total app
  connection budget for the expected API and worker process count, rather than a
  large per-process SQLx pool size. The runtime divides that budget across API
  and worker roles, caps per-process pools, uses shorter idle/lifetime bounds,
  and limits each worker's DB-backed ingest claim loop to the slots it can
  service. Docker Compose exposes `IRONRAG_API_REPLICAS` and
  `IRONRAG_WORKER_REPLICAS`; Helm wires the same values and reserves one API
  rollout surge slot. This prevents worker/API scale-out from opening oversized
  idle Postgres backend sets and pushing bundled PostgreSQL into memory
  pressure.

- **`search_documents` now fuses per-lane scores instead of taking a raw
  cross-lane `max`.** The MCP document search merges three retrieval lanes
  (document metadata, lexical chunk, vector chunk) whose scores live on
  incommensurate scales (metadata/lexical text-rank are unbounded `>= 0`,
  vector cosine is `0..=1`). The merge combined them by `max` over the raw
  values, so a hit on a numerically larger lexical/metadata scale always
  buried a vector-strong document and cross-lane ordering was wrong. Each
  lane is now normalized with Reciprocal Rank Fusion (rank-based, scale-free)
  before combining: a document earns one RRF contribution per lane it appears
  in (using its best rank in that lane), and the contributions are summed, so
  a document present in multiple lanes ranks above an otherwise
  equally-ranked single-lane document. Ordering is deterministic with a
  stable document-id tie-break. Readability bucketing (readable hits before
  failed/unavailable) is unchanged; fusion only reorders within a bucket.

- **Redis outage degrades turn caching instead of failing the turn.** The
  query turn took a Redis-based fill/coalesce lock before computing a fresh
  grounded answer. When Redis was unreachable, the lock-acquisition error
  propagated and every fresh turn returned a 5xx for the whole outage, even
  though the answer pipeline never needs Redis to compute. A Redis
  *connectivity* failure (server down, connection refused/dropped, or timeout —
  classified structurally on the typed Redis error kind, never by message text)
  now degrades to cache-miss latency: the turn logs one warning and proceeds
  without the fill lock. Genuine lock contention (the lock is held by another
  filler) keeps its existing wait/coalesce semantics, and a non-connectivity
  Redis fault still fails closed so a real malfunction stays visible.

- **Question-embedding cache keys on full identity, not a 64-bit hash.** The
  in-process LRU for question embeddings keyed entries by a `u64` from
  `DefaultHasher` and returned silently on collision, so a hash collision could
  hand back the wrong embedding for a different question — wrong retrieval with
  no error — and `DefaultHasher` is not stable across releases. Entries are now
  keyed by a 32-byte SHA-256 of the canonical identity tuple (trimmed question
  text plus every embedding-scoping binding dimension) with equality on the full
  digest width, so distinct questions or bindings can never alias. LRU bounds
  and eviction are unchanged.
- **Retryable ingest stage failures now requeue instead of permanently failing
  the document.** A transient provider error (rate limit / timeout / connection
  reset) during ingest previously killed the document on the first failure: the
  attempt was requeued, but the dispatcher's terminal-failure handler then
  clobbered that requeue back to `failed`, so the retry path was unreachable.
  Retryable failures now requeue with exponential backoff via the existing
  `available_at` mechanism, bounded by a per-job attempt budget; only once the
  budget is exhausted is the document marked terminally failed, with the attempt
  count surfaced in the failure message.

- **Persisted chunk-embedding batches survive a transient embed failure.** When
  the chunk-embedding stage failed partway through a large document, the cleanup
  path discarded every embedding batch already persisted for the revision, so a
  blip near the end threw away all the work done so far. Successfully persisted
  vectors are now kept across a transient failure; the retry resumes by
  re-embedding only the chunks still missing a vector. Readiness remains gated on
  full coverage — a revision is promoted to ready only when the persisted vector
  count equals its chunk count — and terminal/correctness failures still clear
  partial state.
- **UI/MCP agent loop bounds each tool call and stops no-progress loops early.**
  Every tool execution future is now wrapped in a per-call timeout bounded by the
  smaller of the remaining turn deadline and a canonical per-tool-call max, so a
  single hung tool can no longer run past every budget; a timed-out call yields a
  structured `is_error` outcome (which clears the dedup fingerprint, so a retry is
  not suppressed). Separately, when several consecutive iterations produce no
  successful *new* tool result — every call errored, was suppressed as a
  duplicate, or replayed an earlier result — the loop now routes into the existing
  forced-final path instead of spinning the rest of the iteration/deadline budget.
  The early stop is observable in the turn debug payload via a new `no_progress`
  `stoppedReason`, distinct from `deadline`/`iteration_cap`.

- **Source-format append tolerates a missing source blob.** Appending text to a
  document whose stored source bytes are absent — for example after a snapshot
  restore that carried content rows without their blobs (`library_data` without
  `blobs`) — no longer fails the whole request with a 500. The append now starts
  a fresh source revision from the appended text and logs a warning. A source
  blob that *is* present but cannot be read still surfaces as an error, so a
  transient storage outage never silently drops existing content.

- **UI-agent tool-call dedup no longer suppresses a distinct follow-up
  question.** The agent loop dedups same-iteration and cross-iteration tool
  calls by an *effective* fingerprint computed after argument normalization
  (history compaction, default `conversationTurns`/`topK` injection). Two
  genuinely different raw queries could normalize to the same effective
  fingerprint, so a correct follow-up call was rejected as a duplicate of an
  earlier unrelated call and the assistant answered the wrong subject. The
  dedup map now also keys on a hash of the raw, pre-normalization arguments:
  only a true same-raw in-flight call is suppressed, a duplicate of a call
  that already completed replays the cached successful result instead of
  returning a refusal, and a distinct raw call is never dead-ended. Errored
  entries continue to clear so the call can be retried.

## 0.5.0 — 2026-06-11

ArangoDB is removed as the knowledge-plane database. The knowledge plane —
documents, revisions, structured blocks, chunks, technical facts, entities,
relations, evidence, context bundles, the chunk and entity vectors, and the
runtime graph — now lives entirely in PostgreSQL 18 with pgvector and
PostgreSQL full-text search. A 0.5.0 deployment runs no ArangoDB.

> ⚠️ **Clean install only — there is no in-place 0.4.x → 0.5.0 upgrade.**
> 0.5.0 replaces ArangoDB with PostgreSQL + pgvector and starts from a single
> baseline migration. Provision a **fresh** 0.5.0 stack; never point a 0.5.0
> backend at a 0.4.x database. Carry your data across **manually** by exporting
> each library's snapshot from the old stack and restoring it into the new one
> (see the upgrade procedure below).

### Breaking changes

- **Single baseline migration.** `apps/api/migrations/0001_init.sql`
  replaces the entire previous migration chain. A 0.5.0 stack starts from this
  one baseline and does not apply the historical `0001`–`0017` migrations.
- **Knowledge-plane backend.** `IRONRAG_KNOWLEDGE_PLANE_BACKEND=postgres` is the
  default and the only backend provisioned for new deployments. Chunk and entity
  vectors are stored in per-`(library, dim)` pgvector relations tracked by a
  manifest table.
- **PostgreSQL-only deployment.** The Compose files and the Helm chart no longer
  provision ArangoDB; the PostgreSQL service uses the `pgvector/pgvector` image.
  The readiness/diagnostics payload reports a `knowledgePlane` dependency where
  it previously reported `arangodb`.
- **Default agent AI binding.** A new `agent` binding purpose is seeded by
  default, reusing the configured `query_answer` provider and model, so a clean
  stack resolves an active agent binding without extra operator configuration.
- **Snapshot archive format v6.** Library snapshots are exported from PostgreSQL
  (schema version 6). Restore accepts both v6 (PostgreSQL) archives and v5
  (ArangoDB) archives, so a snapshot taken on a 0.4.x stack can be restored into
  a 0.5.0 stack.

### Deprecations

- **ArangoDB is deprecated.** The ArangoDB knowledge-plane backend
  (`IRONRAG_KNOWLEDGE_PLANE_BACKEND=arango`) is no longer provisioned by the
  Compose files or the Helm chart and is retained in 0.5.0 only as a
  compatibility path for reading older v5 snapshot archives during the upgrade.
  It is expected to be removed entirely in a future release: ArangoDB's
  free/community tier caps a deployment at 100 GiB of data, which the PostgreSQL
  knowledge plane removes. New deployments must use PostgreSQL. If you still run
  the `arango` backend, export your libraries and restore them onto a PostgreSQL
  0.5.0 stack now (see the upgrade procedure below); do not expect the `arango`
  backend to remain available.

### Upgrade procedure (manual, data-preserving)

The 0.4.x → 0.5.0 upgrade is manual; there is no automatic ArangoDB-to-PostgreSQL
migration. To carry your data across:

1. **Upgrade your existing 0.4.x stack to 0.4.23 first.** 0.4.23 contains a
   snapshot-export fix required to export libraries whose vectors were promoted
   to per-`(library, dim)` shards; without it the export of such a library
   fails and writes an `EXPORT_FAILED.json` sentinel instead of data.
2. **Export each library you want to keep** with
   `GET /v1/content/libraries/{libraryId}/snapshot` and save the `.tar.zst`
   archive.
3. **Deploy a fresh 0.5.0 stack** (new PostgreSQL baseline, no ArangoDB).
4. **Restore each library** with
   `POST /v1/content/libraries/{libraryId}/snapshot`. Restore is idempotent and
   safe to re-run; it rebuilds the per-`(library, dim)` vector relations and the
   vector manifest, and accepts both v5 and v6 archives.

To start fresh without preserving data, deploy 0.5.0 directly — no export or
restore is required.

### Added

- **Structural document parentage and attached-context retrieval.** A document
  admitted as a dependent of another source document (e.g. a page attachment or
  inline image) now records its canonical `parent_document_id` and a typed
  `document_role` (`primary`, `attachment`, or `attached_context`), derived once
  at admission/backfill from structural inputs (declared parentage + the
  revision's media class). Connectors declare the link with an optional
  `parentExternalKey` on document admission; a per-library `ironrag-cli`
  backfill recovers parentage for already-ingested corpora and is idempotent.
  In retrieval, raster-image attachments (`attached_context`) are demoted below
  peer and primary content during final context selection, excluded from the
  clarify-vs-answer disposition, and never offered as a clarify variant — so a
  page's one-chunk image attachments can no longer flood an answer's context or
  a clarification menu and displace the parent page's own evidence. A document
  the query explicitly focuses on is never demoted, so an exact attachment ask
  still resolves it. The signal is the typed role only; no MIME, extension, or
  filename inspection at the retrieval layer. Non-image attachments (e.g. a PDF
  manual) keep peer-document recall. See
  [docs/en/PIPELINE.md](docs/en/PIPELINE.md).
- **Corpus-gloss acronym aliases.** When a document spells a short form next
  to its full form as a parenthetical gloss (`Full Form (FF)`), graph
  extraction now captures the short form as an alias of the full-form entity
  node. The signal is purely structural (Unicode tokenization plus the
  existing identifier-shape gate) and evidence-local: a different sense
  glossed in a different chunk attaches to its own node, so polysemous
  acronyms keep their senses separate. A new `ironrag-cli` per-library
  backfill recomputes these structural aliases over already-ingested data
  idempotently — re-running it is a no-op, and it removes initials-shaped
  aliases no gloss justifies.
- **Graph anchoring for short identifier mentions.** A compiled target entity
  whose label is a short identifier-shaped token (for example a two-letter
  acronym) previously built no graph anchor profile at all, so retrieval fell
  back to the vague vector neighbourhood. Such mentions now anchor through an
  exact label-or-alias match, guarded by a sense-dominance filter: when the
  short token matches several homograph nodes, only nodes whose evidence
  support is within a fixed ratio of the strongest match seed neighbourhood
  expansion, keeping incidental low-evidence senses out of the frontier.
- **Release-inventory clarify-with-fallback.** A "what's new in the latest
  releases?"-class query that names no explicit subject now checks, on the
  deterministic latest-version inventory path, whether the listed release
  notes mention several distinct subjects in the knowledge graph (entity
  evidence spanning at least two of the listed release documents). If so, the
  answer leads with a grounded excerpt for one subject and asks which subject
  to focus on, listing the graph-derived choices; a query that already scopes
  the inventory — by a named entity, a document focus, or a literal — keeps
  the flat latest-versions list. The choices come entirely from the library's
  own graph; no subject names or language-specific lists exist in code.
- **Horizontal ingest-worker scale-out.** The number of ingest workers is a
  single variable: `IRONRAG_WORKER_REPLICAS` for Docker Compose (default `1`,
  keeping the baseline stack light for weak hosts) and `worker.replicaCount` for
  the Helm chart. Scale-out is safe at any time — workers claim queued jobs with
  `SELECT … FOR UPDATE SKIP LOCKED` under per-library, per-workspace and global
  concurrency caps, so replicas never double-process a job. See
  [docs/en/CAPACITY-PLANNING.md](docs/en/CAPACITY-PLANNING.md).
- **Universal structured-record ingest.** JSON (object or array), YAML (single
  document, `---` stream, or sequence of mappings), JSONL/NDJSON, and TOML now
  ingest through one key-agnostic record extractor. Every field at any nesting
  depth is flattened to searchable text, heterogeneous schemas are profiled, and
  any value shaped like a timestamp (RFC3339 or epoch, under any key name)
  time-stamps its record for temporal retrieval — with no per-format or per-field
  special-casing, so an arbitrary export, event log, config dump, or session
  transcript all flow through the same generic path. Spreadsheets (`csv`/`tsv`/
  `xls*`) stay on the native tabular extractor. The knowledge graph built from
  these records captures the real value-entities (named things, identifiers,
  paths, places) and their relations — not the structural scaffolding (field-path
  keys, format counters, raw timestamps), which stay as record metadata. See
  [docs/en/PIPELINE.md](docs/en/PIPELINE.md).
- **Workspace-level snapshots.** `GET`/`POST /v1/catalog/workspaces/{id}/snapshot`
  export and restore an entire workspace — every library at once — as a single
  archive, instead of one library at a time. The archive is a plain tar holding
  a `workspace-manifest.json` plus one per-library `.tar.zst`; restore
  provisions a fresh library per entry and reuses the per-library restore. This
  is the operator path for moving a whole stack between deployments.
- **Selectable AI-configuration snapshots.** Library export accepts an
  `include=ai_config` kind alongside `library_data`, `blobs` and `workspace`.
  It bundles the portable AI configuration needed to resolve the library on
  another stack — provider and model catalogs, prices, model presets, provider
  credentials and binding assignments — covering both instance-scoped
  (deployment-global) and workspace/library-scoped rows. Provider `api_key`
  values are never exported, and import is non-destructive: a row that
  collides with the target's existing AI configuration is skipped, so an
  import only populates an empty target.
- **System-role RBAC.** Each user now carries a system role
  (`viewer` | `operator` | `admin`). A viewer reads documents and the graph and
  asks the assistant; an operator additionally creates and edits libraries and
  their content; an admin additionally manages workspaces, system settings, AI
  configuration and users. The role is surfaced on the session bootstrap and
  gates the UI shell's capability matrix. The `0001_init.sql` baseline carries
  the system-role column and promotes the bootstrap administrator to `admin`.
- **User-management endpoints and admin surface.** New admin-only endpoints
  `GET`/`POST /v1/iam/users` and `PATCH /v1/iam/users/{principalId}/role` list
  users, create a user (login, email, display name, password, role) and change
  a user's role. Admin access is enforced from the caller's own system role, and
  the role-change path refuses to demote the last remaining administrator. The
  admin Users surface lists users, creates them and changes their roles against
  these endpoints.
- **Role-aware authorization and per-user access management.** The system role
  is now enforced end-to-end: it decides the capability (read, write or admin)
  while resource grants decide which workspaces and libraries a user can reach.
  An admin passes every authorization check; an operator can create and edit
  libraries and content in the workspaces they have been granted access to; a
  viewer stays read-only even where a resource grant would otherwise match.
  Library creation no longer requires a workspace-admin grant — operator access
  to the workspace is sufficient. New admin-only endpoints
  `GET`/`PUT /v1/iam/users/{principalId}/access` read and set a user's workspace
  and library access, and the admin Users surface gains a per-user access editor
  built on them.

### Observability

- **Anonymous performance telemetry on by default.** The backend ships
  OpenTelemetry traces and metrics to the project's default collector to help
  improve performance. These signals carry request timings, stage durations,
  counters and identifiers (library/document/chunk UUIDs) — not your documents,
  queries, answers or credentials. Disable with `IRONRAG_OTEL_ENABLED=false`, or
  redirect to your own collector with `OTEL_EXPORTER_OTLP_ENDPOINT`; the shipped
  default lives in `apps/api/observability.toml`.
- **Logs off by default.** Logs are the only content-bearing signal, so the
  Compose and Helm defaults set `OTEL_LOGS_EXPORTER=none`; set it to `otlp` to
  ship logs to a collector you control.
- **Per-deployment attribution.** Every signal carries a stable
  `ironrag.deployment.id`, derived from the PostgreSQL cluster identity and
  overridable with `IRONRAG_DEPLOYMENT_ID`, so deployments are distinguishable
  and never mixed into one bucket.
- **No verbatim content in diagnostics.** The exact-technical preflight stage no
  longer records the question text or document/chunk previews; it emits only
  shapes and counts.

### Performance

- **Large-graph view stays at frame rate during every interaction.** On a
  ~90k-node / ~430k-edge library the graph previously stalled for seconds on
  pan, zoom and node drag. The bottleneck was rasterising the full edge set
  every frame, so the view now draws a uniform stride-sampled subset of edges
  above a render cap (the full topology is a few hundred-thousand-edge
  hairball anyway), with a toolbar toggle to render all edges on demand. Node
  highlighting still uses the complete adjacency, so hover/select neighbours
  are unaffected by sampling. Measured on the reference library, pan / zoom /
  drag / hover / select / layout-switch all hold the display refresh rate with
  no long tasks, down from multi-second main-thread blocks.
- **Large-graph interaction is reindex-free.** Every graph interaction —
  hover, node drag, legend/search filtering, layout/mode switching and node
  selection — repaints through Sigma's reducer pipeline with
  `refresh({ partialGraph, skipIndexation: true })` instead of rebuilding the
  Graphology instance or forcing a whole-graph reprocess. Hover, drag and
  filter touch only the affected neighbourhood; the layout/mode switch runs
  its force-directed math off the main thread in the existing graphology Web
  Worker; and node selection emphasises the focus neighbourhood (O(degree))
  rather than dimming all nodes and edges, so a click no longer restyles the
  whole graph. A partial-refresh guard also prevents a "can't be repaint"
  error from losing the WebGL context when a fresh instance is mid-build.
- **Smaller initial web bundle.** The document editor (Tiptap) is lazily
  loaded and Radix/form vendors are split into their own chunks, cutting the
  initial `index` chunk roughly in half (~70 kB gzip) and reducing
  re-renders via memoised context and row components.

### Fixed

- Release-inventory clarifications keep the full flat inventory in the tool
  answer: the clarification is appended after the complete top-N list instead
  of replacing it, so an agent relaying the tool result (the in-product UI
  agent or an external MCP client) delivers the data in one round trip instead
  of re-querying with a guessed subject.
- A compiled follow-up retrieval query that drops the recovered history
  subject is no longer trusted: a structural guard detects a resolved query
  sharing zero tokens with the follow-up's scope segment and searches the full
  scoped question instead, so bare refinements ("give more detail") stay on
  the prior subject.
- Procedural how-to answers open with the grounded command lines as ordered
  steps when the retrieved context contains them; a note that the
  documentation lacks one end-to-end procedure document may only follow the
  steps as a coverage remark.
- The corpus-gloss acronym capture is bidirectional: a node labeled with the
  short form gains the glossed full-form phrase as an alias (add-only, from
  its own evidence chunks), so full-form queries anchor short-labeled graph
  nodes too.
- Restoring a library snapshot exported before document parentage existed no
  longer fails on the `document_role` not-null constraint: the import injects
  the default role for legacy archives.
- Snapshot export and restore handle per-`(library, dim)` promoted vector
  shards instead of failing the export.
- Grounded-answer context bundles persist correctly on PostgreSQL: the bundle
  entity, relation, and evidence foreign keys reference the canonical runtime
  graph, so a grounded answer records its chunk and segment references instead
  of returning an unreferenced answer.
- Restoring a library snapshot no longer fails when the archive's library slug
  is already taken by another library in the target workspace. The restore
  rewrites the incoming row to the target library's own slug, so an import is
  collision-free regardless of the source slug.
- Service-unavailable (HTTP 503) responses now report an accurate `error.kind`.
  Content-storage read failures surface as `content_storage_unavailable` and
  cache failures as `cache_unavailable`, instead of the misleading
  `arangodb_bootstrap_failed` left over from the removed ArangoDB backend — a
  missing content blob no longer looks like a database bootstrap error.

### Deploy

- The four Compose files (base, local-build, large-host, s4) are unified into a
  single `docker-compose.yml`. Variants are now driven by env and one profile:
  the `s4` profile adds the bundled s4core object store (with
  `IRONRAG_CONTENT_STORAGE_PROVIDER=s3`), the `IRONRAG_*_MEMORY_LIMIT` vars
  raise the per-role caps for larger hosts, and `IRONRAG_BACKEND_IMAGE` /
  `IRONRAG_FRONTEND_IMAGE` select a local source build. The default
  `docker compose up -d` is unchanged: filesystem storage on a 16 GiB-safe
  memory budget.

## 0.4.22 — 2026-06-02

### Fixed

- The knowledge-graph topology endpoint no longer intermittently returns HTTP
  500 (`could not resize shared memory segment … No space left on device`).
  The heaviest topology scans are pinned to a non-parallel query plan so they
  never allocate dynamic-shared-memory segments in the PostgreSQL container's
  `/dev/shm`, which the stock 64 MiB mount could exhaust under concurrent graph
  loads. The bundled PostgreSQL `/dev/shm` is also raised to 256 MiB as defense
  in depth (Compose + Helm).
- A grounded-answer follow-up question now retrieves as widely as a fresh one.
  A lower retrieval-breadth floor on contextual turns let the in-product agent
  narrow `topK` on follow-ups, which starved recall on how-to / configuration
  questions: the concrete answer chunks ranked below the cutoff and the model
  hedged instead of answering. The contextual floor is pinned to the canonical
  default.

### Changed

- Resident memory is driven down with jemalloc configured for background page
  decay, so freed pages are returned to the OS instead of accumulating in
  per-thread arenas (which had pinned an idle worker at multiple GiB and
  tripped the cgroup OOM-killer in a restart loop). The community-detection
  guard now also rejects oversized graphs before loading their rows.
- Compose files are slimmed to operator-facing settings. Narrow runtime tuning
  that does not change per deployment (allocator config, async-runtime pool
  size, ingest parallelism, document batching, vector-index parameters,
  bootstrap toggles, telemetry exporter selection) is baked into the image and
  the compiled-in defaults and stays overridable via the matching environment
  variable.

### Build

- Rust toolchain bumped to 1.96.0.

## 0.4.21 — 2026-05-31

### Changed

- Container memory limits are re-sized for a 16 GiB swapless host and the
  `docker-compose.yml` resource anchors are reorganized from generic
  t-shirt tiers into explicit role-based tiers
  (`frontend`/`cache`/`db`/`worker`/`backend`/`vector`). The previous caps
  summed to more memory than a single 16 GiB host provides; on a swapless
  host a combined resident set above physical RAM trips the kernel's
  *global* OOM-killer, which then SIGKILLs the largest-RSS bystander
  (typically ArangoDB mid-IVF-build, causing a crash loop) instead of a
  rebuildable service. The new tiers keep the steady-state sum of `memory`
  limits at ~14 GiB, leaving headroom for the OS and page cache. The API
  backend is 3.5 GiB, the ingest worker 3 GiB (still resolving docling
  auto-concurrency to 1 via `auto_docling_max_concurrency_for_limits`), and
  ArangoDB keeps a dedicated 5 GiB tier sized for the IVF build of the
  largest active per-(library,dim) vector shard. The Helm chart
  (`charts/ironrag/values.yaml`) mirrors these tiers 1:1.
- Every Compose service now declares an explicit `oom_score_adj` so that,
  when the host is genuinely out of memory, the kernel sacrifices the
  rebuildable services first (worker, then frontend, then backend) and the
  stateful stores (PostgreSQL, Redis, ArangoDB) last. The Helm tiers use
  Burstable QoS (requests < limits) to express the same eviction ordering.
- The ArangoDB IVF k-means training memory budget is now overridable at
  runtime via `IRONRAG_VECTOR_INDEX_TRAINING_BUDGET_BYTES` (default 3 GB),
  so a larger host can widen the budget in lockstep with a raised ArangoDB
  cap without recompiling. The builder still fail-soft-caps `nLists` to the
  budget divided by the empirical per-list cost, so a smaller budget yields
  a cheaper build rather than an OOM. A new `docker-compose.large.yml`
  overlay bundles the wider caps and budget for 24-32 GiB hosts.

### Fixed

- The Helm `/tmp` volume on the API, worker, and startup pods is now
  disk-backed (node ephemeral storage) instead of RAM-backed
  (`emptyDir.medium: Memory`). The app writes uploaded and extracted file
  bytes to `/tmp`; a RAM-backed `/tmp` charged those bytes to the pod's
  memory cgroup and could OOM the pod on a large file. The bytes now land in
  ephemeral storage, capped by the per-tier `ephemeral-storage` request and
  off the memory budget.

### Added

- `scripts/ops/check-mem-budget.sh`, wired into `make check`, fails the
  build if the steady-state sum of Compose `memory` limits exceeds the host
  ceiling, preventing a future edit from silently re-oversubscribing the
  host. The `docker-compose.large.yml` overlay is intentionally exempt.

## 0.4.20 — 2026-05-30

### Changed

- Container memory caps for the API backend and ingest worker are
  rebalanced for host-level OOM containment. Both now run with a 4 GiB
  limit, which resolves the docling ingest auto-concurrency to 1 (see
  `auto_docling_max_concurrency_for_limits`) so a docling/python memory
  spike is bounded to — and cgroup-OOM-killed inside — the worker's own
  limit (the job then retries) instead of exhausting host pages and
  letting the kernel OOM-killer pick the larger-RSS API backend as the
  victim. The worker additionally carries an `oom_score_adj` bias so the
  kernel prefers it over the backend when the host is genuinely out of
  memory. ArangoDB keeps its dedicated 6 GiB tier, sized for the IVF
  build of the largest active per-(library,dim) vector shard. The
  `docker-compose.yml` resource anchors and the Helm chart
  (`charts/ironrag/values.yaml`) are updated in lockstep.

### Fixed

- Graph-extraction failure no longer discards already-embedded chunk
  vectors or blocks a document from becoming searchable. Graph
  entities/relations are an enrichment layer over chunk-vector
  retrieval; previously a terminal graph-extraction failure on a single
  chunk marked the whole revision's vector state failed, deleted the
  chunk vectors that were already embedded, and failed the document so
  it was unsearchable even via pure vector/lexical retrieval — and the
  idle self-healing loop could never recover it because the failed
  revision was never promoted to active. Now, when the chunk vectors
  already exist, a graph-extraction failure degrades gracefully: the
  vectors are kept, the document is promoted to readable, the graph
  layer is marked `graph_degraded`, and the idle graph re-extract loop
  backfills the graph on a later tick. Only a genuine embedding failure
  (no vectors to preserve) still fails the revision. Both the async
  ingest worker and the inline mutation pipeline share this behavior.
- Graph-extraction JSON parsing tolerates provider output that wraps the
  JSON in a markdown code fence or surrounds it with prose. The
  normalizer previously required the entire provider response to be a
  single clean JSON document (`serde_json::from_str` on the whole
  string), so an intermittently chatty model response failed extraction
  outright. Parsing now keeps the fast path for clean JSON and adds
  structural fallbacks — strip a wrapping code fence, then scan for the
  first balanced top-level JSON object/array (honoring string literals)
  — without inspecting the natural language of any surrounding text, so
  it stays language-agnostic.
- Library snapshot export no longer silently returns HTTP 200 with a
  truncated archive when an upstream stage (e.g. arango vector
  collection export) fails. The tar `Builder` and zstd stream are now
  finalized on every code path, and the failure path writes a sentinel
  `EXPORT_FAILED.json` tar entry so clients can detect failure even
  when the response status has already been committed. The underlying
  panic (`Builder dropped without finalizing` from `async-tar`) is
  eliminated, and the per-dim chunk/entity vector shards are exported
  with a smaller AQL cursor `batchSize` to keep Arango cursor and
  in-process memory bounded on libraries up to ~117k chunk vectors.
- Library snapshot export, v2: large libraries no longer produce
  silently-truncated archives when the underlying ArangoDB cursor
  errors deep into a multi-batch vector collection. Vector
  collections (per-dim chunk/entity shards plus the legacy single-dim
  `knowledge_chunk_vector` / `knowledge_entity_vector` collections)
  now stream each AQL cursor batch directly into its own tar part
  instead of buffering up to 64 MiB before flushing, and a per-
  collection 30-minute wall-clock budget aborts hung cursor stages
  with a clear error message instead of letting the exporter hang.
- Library / workspace deletion: the sync delete path now cascades the
  Postgres delete into ArangoDB. Previously `delete_library` and
  `delete_workspace` removed only the Postgres row; knowledge vertex
  collections, every per-dim vector shard, and the edge collections
  in Arango have no FK back to PG and were left behind as orphans.
  Both code paths now call the canonical snapshot-restore replace
  sweep (`clear_library_arango_footprint`) after the PG cascade so
  every knowledge_* row tied to the library is removed in the same
  request. `delete_workspace` enumerates the workspace's libraries
  before the PG cascade and sweeps each one's Arango footprint after
  the PG transaction commits.
- Library snapshot import: bulk inserts into Arango vector collections
  use a smaller batch size (100 rows vs 1000) to stay within the
  ArangoDB document-bulk HTTP request size limits. Combined with the
  earlier streaming-cursor export fix, libraries with >100k chunk
  vectors can now snapshot+restore end-to-end. Full anyhow error
  chain is also logged at the import entrypoint so future failures
  surface their underlying cause instead of a top-level wrapper.
- The deterministic module-configuration answer renderer now uses only
  generic localized labels for grounded answer fields (source document,
  command anchors, configuration paths, parameter rows) and keeps
  domain-specific phrasing out of the answer path. `Auto` language
  selection follows the question script before falling back to English,
  so direct answers do not leak English labels into Cyrillic questions.
- The startup AI-catalog preset seed no longer overwrites an operator-set
  `max_output_tokens_override` on every boot. An operator-raised model
  output budget now persists across restarts instead of silently reverting
  to the catalog default, which previously could truncate output and fail
  ingest stages after a restart.

### Added

- `ironrag-audit-orphan-data` one-shot binary scans every ArangoDB
  knowledge_* collection (vertex collections, every per-dim vector
  shard, and edge collections) for rows whose `library_id` does not
  match a live PostgreSQL `catalog_library` row, reports per-library
  per-collection orphan counts as JSON, and — with `--purge --yes` —
  deletes the orphan rows through the same canonical sweep snapshot
  restore uses. Useful for cleaning up deployments that predate the
  delete cascade fix.

#### Debug inspector & per-turn observability

- The assistant debug inspector is redesigned into one cohesive panel: a
  per-turn timeline (pipeline stages with time-share bars), the model
  calls with generation time and tool-wait split plus token counts, and a
  drill-down into each `grounded_answer` child execution. Hovering any row
  reveals the exact figures in a styled tooltip rather than a delayed
  native title; conversational filler was removed from labels.
- Per-execution span capture: DB queries, retrieval lanes
  (vector / lexical / companion loaders), question embedding, and HyDE are
  recorded as timed spans on the turn's debug snapshot and shown in the
  inspector grouped by kind and sorted by duration, so operators can see
  the heaviest sections of a turn and catch regressions over time. Spans
  propagate across the same-task concurrency the retrieval path uses.
- Arango query spans now resolve the real collection name (e.g. the active
  per-dim vector shard) instead of a generic placeholder.
- The pipeline-stage view in the debug inspector is now a compact
  interactive stacked bar with a wrapping legend and instant hover
  tooltips (stage name, duration, share of total). The "total time" stat
  reflects the real end-to-end turn wall-clock (matching the chat's
  reply-latency badge), and the LLM-iteration stat reflects the actual
  model call count — removing the earlier inconsistency where the
  header, the stat card, and the rendered list showed different values.
- Chat messages show a timestamp and, on assistant replies, the response
  latency, so answer speed is visible at a glance.
- Diagnostics (OTLP traces, metrics, logs) ship by default to the
  configured collector and carry canonical resource labels; disable with
  `IRONRAG_OTEL_ENABLED=false` or repoint with `OTEL_EXPORTER_OTLP_ENDPOINT`.

#### Per-library vector dimensions

- ArangoDB chunk and entity vector storage is now sharded by embedding
  dimension. Each library writes into the collection whose dim equals
  its active `embed_chunk` binding's vector length
  (`knowledge_chunk_vector_d<dim>` and `knowledge_entity_vector_d<dim>`),
  with its own ANN index per shard. Libraries on different embedding
  models coexist without dropping each other's vectors.
- `ironrag-vector-rebuild <library-uuid>` rebuilds **only** that
  library — the previous cross-library precondition check that forced
  every library to agree on dim is removed. Other libraries on
  different dims keep running.
- New binary `ironrag-vector-migrate-to-per-dim` moves rows from the
  legacy single-dim `knowledge_chunk_vector` / `knowledge_entity_vector`
  collections into the per-dim shards. Idempotent: a second run with
  the legacy collection empty is a no-op.
- Snapshot export/restore enumerates per-dim shards at runtime instead
  of from a static collection list; the snapshot envelope carries a
  vector-shard manifest so restore on a fresh deployment lazy-ensures
  the same shape.
- `gc-stale-chunks` now walks every per-dim chunk and entity vector
  shard in addition to the legacy collection.

#### Per-library vector shards

- Building on the per-dim sharding above, chunk vectors are now stored
  in and searched from a per-`(library, dim)` shard
  (`knowledge_chunk_vector_d{dim}_l{library}`) so an approximate-
  nearest-neighbour scan covers only one library's vectors instead of
  the whole instance's. New libraries are born sharded. A library that
  still has rows in the shared per-dim shard keeps its new chunk-vector
  writes on that shared shard as well — so the whole library stays on a
  single shard and an upgrade never strands its older vectors — while
  reads fall back to the shared shard until the migration moves the
  library. Entity vectors remain on the shared per-dim shard
  (`knowledge_entity_vector_d{dim}`).
- New idempotent maintenance command
  `ironrag-maintenance migrate chunk-vector-per-library` drains the
  shared per-dim shard into per-library shards. Re-runs after
  interruption are safe.

#### Rate-limited null-head auto-recovery

- New `repair null-heads-auto` subcommand performs the same head
  promotion as the existing `repair null-heads`, but each per-document
  outcome is recorded on `content_document_head` so the recovery
  budget is bounded. Three consecutive same-error failures stamp
  `dead_letter_at`; documents touched in the last hour are skipped.
  A different error code resets the consecutive-failure counter so a
  flaky upstream that produces multiple error shapes still consumes
  one attempt per shape.
- New `repair clear-recovery-dead-letter --document <uuid>` operator
  knob to resume auto-recovery on a single document after the
  underlying failure has been diagnosed and fixed.

#### Maintenance scheduler + PG sweepers

- New durable scheduler loop runs in the worker role and walks the
  canonical maintenance classes on a rolling cadence (default 30 s
  tick, 1 hour between successful runs of the same scope per class).
  Each tick reaps stale leases, bootstraps missing
  `maintenance_job_run` rows, and atomically picks the
  oldest-pending due row per enabled class. Currently dispatches
  `gc.stale-chunks`; the new evidence and retention sweepers below
  ship as operator-CLI first and graduate to the scheduler after
  burn-in. Settings: `IRONRAG_MAINTENANCE_ENABLED` (default `true` for
  worker role), `IRONRAG_MAINTENANCE_TICK_INTERVAL_SECONDS` (`30`),
  `IRONRAG_MAINTENANCE_CLASS_INTERVAL_SECONDS` (`3600`),
  `IRONRAG_MAINTENANCE_STALE_LEASE_SECONDS` (`300`).
- New `gc stale-evidence` sweeper deletes `runtime_graph_evidence`
  rows whose revision is no longer the readable or active head of
  the document, plus rows whose `chunk_id` points at a chunk that
  has already been swept. Holds the per-library graph advisory lock
  and refuses to run while ingest is in flight on the library.
- New `retention stage-events` sweeper issues batched DELETEs (10 000
  rows / 100 ms pause) on `ingest_stage_event` older than
  `--older-than-days` (default 90 days when called from the
  scheduler). Migration 0017 adds the matching
  `idx_ingest_stage_event_recorded_at` so the predicate compiles to
  an index range scan instead of a sequential scan.

#### Maintenance binary consolidation

- The per-task maintenance binaries that previously shipped alongside
  `ironrag-backend` are removed and their behaviour moved into
  `ironrag-maintenance` subcommands. Canonical surface is one CLI with
  one `--help` tree, one init path, and one `--dry-run` / `--json` /
  `--library` flag style. The legacy binary names are dropped without
  shim aliases.

  | Removed binary | Replacement subcommand |
  |---|---|
  | `ironrag-gc-stale-chunks` | `ironrag-maintenance gc stale-chunks` |
  | `ironrag-audit-orphan-data` | `ironrag-maintenance audit orphan-libraries` (read-only) + `ironrag-maintenance gc orphan-libraries --yes` (destructive) |
  | `ironrag-promote-null-heads` | `ironrag-maintenance repair null-heads` |
  | `ironrag-vector-migrate-to-per-dim` | `ironrag-maintenance migrate vector-per-dim` |
  | `ironrag-vector-rebuild` | `ironrag-maintenance rebuild vector-plane --source-library <uuid>` |
  | `ironrag-backfill-chunk-temporal-bounds` | `ironrag-maintenance migrate chunk-temporal-bounds` |
  | `rebuild_runtime_graph` | `ironrag-maintenance rebuild runtime-graph` |

  Each old binary's logic now lives under the corresponding
  `services::maintenance::{audit,gc,repair,migrate,rebuild}` module so
  the same canonical code path is used by the operator CLI and by the
  scheduler tick. Docker image installs only `ironrag-backend`,
  `ironrag-cli`, `ironrag-emit-openapi`, and `ironrag-maintenance` —
  the previous bin clutter (10 binaries) is reduced to 4.

#### Maintenance pipeline foundation

- New canonical operator binary `ironrag-maintenance` with grouped
  subcommands: `audit storage-summary`, `audit index-bloat`,
  `audit null-head-docs`, `gc stale-chunks`, `lease show`,
  `lease summary`, `lease clear-failure`, `lease reap-stale`. All
  subcommands share a single init path, a uniform `--dry-run` /
  `--json` / `--library` flag style, and a stable `--help` tree;
  future sweepers (retention, repair, index reindex, …) plug into
  this layout without renames.
- New migration `0017_maintenance_baseline.sql` adds the durable
  `maintenance_job_run` lease table (per-(class, scope) lifecycle
  row carrying state, cursor, attempts, heartbeat, dead-letter and
  next-due timestamps) plus indexes for the scheduler tick and the
  stale-lease reaper. Idempotent: `CREATE … IF NOT EXISTS`, enum
  via DO block, safe to re-apply.
- The same migration adds `recovery_attempts_count`,
  `last_recovery_error_code`, `last_recovery_attempt_at` and
  `dead_letter_at` columns to `content_document_head`, plus a
  partial recovery-candidate index. These back a bounded null-head
  retry contract that the ingest worker will use to rate-limit
  recovery attempts before marking a document dead-letter.
- New module `services::maintenance::lease` exposes the canonical
  lease primitives (`acquire_next_due`, `heartbeat`, `update_cursor`,
  `complete`, `fail`, `clear_dead_letter`, `reap_stale_leases`,
  `count_by_state`). The scheduler tick and the operator CLI both
  depend on the same surface — there is one source of truth for
  "what is currently running, what failed, what to retry next".
- New module `services::maintenance::gc` houses the canonical
  implementation of `gc.stale-chunks`. The sweeper now acquires the
  per-library graph advisory lock and refuses to run while any
  `ingest_job` for the library is `queued` / `leased` / `paused` —
  closing a race where a concurrent ingest could write chunks the
  sweeper had already classified as stale. Optional
  `--include-null-head` flag opts in to deleting chunks and vectors
  for failed-ingest documents whose head is null on both pointers
  (off by default; the conservative path keeps recoverable docs
  intact).

## 0.4.19 — 2026-05-22

### Connectors

- Added the production [Confluence connector](https://github.com/mlimarenko/IronRAG.Confluence) for syncing pages and attachments into IronRAG with periodic polling and webhook intake, alongside the existing BookStack connector line.

### Admin

- The ingest queue now uses the shared document-style pagination footer with
  the same 50/100/250/1000 row-size choices and adds an explicit selection
  mode for bulk pause, resume, cancel, and clear-selection actions.
- The ingest queue now has workspace and library filters; changing scope resets
  pagination and row selection so bulk actions only apply to the visible scope.
- Added a Libraries administration tab with cross-workspace library summaries,
  document-style filters, pagination, inspector actions, bulk backup/delete
  actions, and direct navigation to the selected library's documents.
- Libraries bulk selection now follows the documents table pattern: checkboxes
  appear only after entering selection mode, and pagination uses numbered page
  controls with ellipses and the same page-size choices as documents.
- The Libraries administration table now explains each column through localized
  hover/focus tooltips, including the difference between readiness and lifecycle
  state.
- Libraries administration sorting now covers every data column, including
  provider calls, readiness, and lifecycle state.
- Workspace switching now selects the first library in the newly selected
  workspace when the previously active library does not belong to it.

### Web ingest

- Recursive web ingest now captures meaningful image resources from HTML
  documentation pages, including responsive and lazy-loaded images, while
  filtering page chrome, hidden markup, script/style text, and tiny/decorative
  assets before OCR ingestion. HTML text extraction also skips structurally
  hidden class/style content and unresolved template-only fragments without
  dropping ordinary prose about template syntax, and recursive discovery now
  ignores edit-action links that point at page-authoring chrome. Title-only
  HTML pages no longer produce retrievable text unless the page body yields
  visible content.
- Recursive web ingest now treats directly discovered image resources as
  embedded page assets instead of materializing them as separate document rows;
  standalone image uploads remain supported through the file upload path.
- Raster OCR now prefers the configured Vision binding for standalone images
  and embedded document figures, while Docling remains the structured parser for
  PDF/DOCX/PPTX and the fallback image OCR path when no Vision binding exists.

### Rollout checks

- MCP-agent rollout checks now fail closed when catalog discovery, shell
  readiness, or direct binding API responses have malformed JSON shapes or
  invalid UUID identifiers, instead of silently skipping affected libraries.
- Assistant benchmark gates now validate question files, library identifiers,
  latency counters, success counters, and failure counters before spending API
  work or reporting a rollout as passing.

### Assistant

- Removed backend source-footer mutation from assistant answers. Retrieved
  evidence links now stay on the structured evidence surface and the UI renders
  them separately from model-authored answer text.
- MCP document search now bounds suggested read offsets against the readable
  revision length, so following `suggestedStartOffset` with `read_document`
  cannot produce an empty first window when chunk spans point at the document
  tail.
- Structured source-context expansion now treats query-focused companion chunks
  as local neighbor anchors, no longer requires a source-profile chunk before
  loading neighbors, and widens the preceding setup window for typed procedural
  questions, so configuration answers keep adjacent examples and setup blocks
  in context without corpus-specific routing.
- Procedural single-document answers now retain the expanded structured source
  context through final assembly even when the caller uses a small default
  retrieval budget, so configuration instructions keep file paths, sections,
  examples, and parameter descriptions together without increasing broad-query
  fan-out.
- Wildcard graph-inventory answers now reserve a larger share of the existing
  context window for matching graph nodes before sampled document excerpts, so
  broad list questions can surface more known entities without corpus-specific
  routing or extra retrieval fan-out.
- Short focused setup questions now treat a rare clipped target token as related
  to longer canonical graph labels and technical chunks, keeping configuration
  retrieval anchored to the requested entity without language-specific aliases,
  corpus-specific rules, or additional retrieval fan-out.
- Graph evidence lookup now gives priority to explicit QueryIR entity/document
  targets before filling the bounded target set from the broader retrieval
  bundle, so focused procedural questions can pull the matched source document's
  tables and configuration snippets instead of being crowded out by generic
  graph context.
- Single-document procedural retrieval now treats `document_focus` as the
  primary graph/document anchor and keeps requested value facets from becoming
  competing target profiles, so setup questions stay on the named source while
  still retrieving the requested files, sections, parameters, and examples.
- Document-scoped comparison and procedural questions now prepend the focused
  source to typed facet probes before graph and lexical retrieval, keeping
  variant/setting comparisons anchored to the requested source instead of
  drifting into generic facet nodes while falling back to explicit entities
  when the focused graph profile is absent.
- Focused-document heading extraction now abstains for typed procedural
  configuration questions, preventing deterministic preflight from replacing a
  grounded setup answer with only a section heading.
- Latest-version inventory retrieval now honors the typed QueryIR source-slice
  count before falling back to numeric literals, so latest-N answers keep the
  requested number of version documents without keyword routing or
  corpus-specific rules.
- Graph-backed inventory questions now keep explicit entity matches ahead of
  broad expansion nodes, support literal prefix wildcard lookups, and preserve a
  larger entity budget for list-style answers without increasing the initial
  retrieval fan-out.
- Follow-up grounded answers now preserve dense code-literal inventories from
  the prior assistant turn before truncating history, reuse the parent retrieval
  budget for those dense list follow-ups, surface graph-node summaries as
  answer evidence, route wildcard identifier inventories through the canonical
  answer tool instead of document listings, and keep internal scope wrappers out
  of model-authored answers with a stricter no-continuation-offer boundary.
- Broad comparison answers now preserve module, package, configuration-file,
  and filesystem-path intent through QueryIR compilation and retrieved-document
  briefs, keeping exact technical identifiers near introductory prose in the
  final grounded context without corpus-specific routing or extra retrieval
  fan-out.

### Documents

- The document editor now loads a canonical editor-markdown representation
  instead of rebuilding editable content from prepared retrieval segments.
  Markdown documents open in the rich editor with image/link/table support, and
  PDF documents use the stored source as a read-only viewer frame.
- The document editor toolbar now exposes icon actions for common Markdown
  formatting, links, images, tables, and history while preventing unchanged
  documents from being saved and reprocessed.
- Docling-backed PDF extraction now preserves escaped technical identifiers in
  prepared text and keeps short OCR fragments out of graph labels without
  dropping code-like identifiers or measurement/formula labels.
- Batched PDF extraction now preserves Vision OCR usage from every page range,
  so embedded-image parsing and document billing stay aligned.

## 0.4.18 — 2026-05-19

### Outbound HTTP security

- Added one standard outbound public-HTTP resolver for webhook delivery.
  Webhook targets are now resolved and policy-checked before delivery,
  redirects are not followed, private and loopback address ranges are
  rejected, and response excerpts are bounded before persistence.
- Webhook failure handling now records rejected targets without leaving
  attempts stuck in an in-flight state, and retry dedupe keys track the
  effective attempt number.

### Web ingest boundaries

- Added a third recursive web-ingest boundary policy: same host plus
  subdomains of the selected host. The new policy is exposed through
  REST, MCP, OpenAPI, generated web types, and the documents-page web
  ingest dialog.
- IP-address seed URLs now reject the subdomain boundary mode and the UI
  disables that option automatically for IP hosts. URL identity handling
  now classifies exact seed-host matches separately from seed-subdomain
  matches, so crawl and materialization rules share the same boundary
  semantics.

### Assistant citations

- Assistant source references are now rendered as styled links instead
  of plain text echoes. Stored-document links and external source URLs
  are deduplicated, attached under the answer, and covered by component
  and browser smoke tests.

## 0.4.17 — 2026-05-19

### Migrations

- Fixed the graph read-path index migration so populated installations
  with long extracted graph labels can apply it successfully. The exact
  label lookup index now uses a bounded hash expression plus a full-text
  equality check instead of storing unbounded label text in the btree key.
- Removed free-form label and relation-type text from graph support
  btree keys while keeping the intended support-count ordering. This
  prevents PostgreSQL index row-size failures without changing answer
  semantics.
- Added a regression test that applies the graph index migration after
  inserting a synthetic long graph label and verifies exact entity search
  still returns the expected node.

## 0.4.16 — 2026-05-19

### Kubernetes / Helm

- Fixed Helm chart defaults so API, worker, and web deployments reference
  the Docker image tag derived from chart `appVersion` instead of stale
  per-component tags in `values.yaml`. The rendered default image refs now
  use the current published `v`-prefixed Docker tags.
- The shared web nginx template now proxies to the configured upstream
  directly, without a Docker-only resolver directive. Docker Compose
  and Kubernetes use the same image template and only differ by the
  `IRONRAG_API_UPSTREAM` value supplied by the runtime.
- Added Helm lint/template rendering to the main local check gate so
  bundled, filesystem, and external-service chart profiles are validated
  with every full `make check`.
- The minikube HA validation script now restarts app deployments after
  rebuilding same-tag dev images, so repeated smoke runs exercise the
  images produced by the current run instead of stale pods.
- The web image now ships one nginx security-header snippet and applies it
  without changing API proxy behavior.
- Swagger UI now has a route-specific CSP that allows same-origin embedding
  inside the application while keeping the rest of the web app frame-locked.

## 0.4.15 — 2026-05-19

### Assistant MCP agent loop

- The web UI assistant now runs as a regular MCP-client-style agent over
  the answer tool surface. It sees the same tool schemas that
  external MCP clients see, decides which tools to call, formulates tool
  arguments, can run independent calls in parallel, reads tool results,
  and only then writes the final answer. `grounded_answer` remains the
  high-level grounded-answer tool instead of being auto-invoked with the
  raw UI message.
- UI turns now persist parent agent executions separately from child
  tool executions: the parent stays on the UI surface, while child and
  direct grounded-answer executions use the MCP surface and expose their
  own trace ids.
- Tool-backed answers that used MCP tools but received no verifier-grade
  evidence now store `insufficient_evidence` with a
  `no_verifiable_tool_evidence` warning. Direct no-tool assistant answers
  now also fail closed as `insufficient_evidence` with a
  `no_agent_tool_evidence` warning instead of looking verifier-clean.
- Assistant SSE activity now reports real model/tool events through a
  bounded, panic-safe stream. Diagnostic activity no longer backpressures
  agent/tool execution, while terminal completion and failure events are
  still delivered through the required stream path.
- UI assistant turns no longer auto-forward the raw UI message into
  `grounded_answer` as a hidden collection step. The model now sees the
  complete read-only MCP answer surface on its first iteration, chooses
  the needed tools and arguments itself, and can run independent tool
  calls in parallel before final synthesis.
- Composite UI turns now use a structural distinct-tool floor only after
  the agent has already mixed evidence categories such as document,
  graph, runtime, and answer tools. This keeps multi-part questions in a
  real MCP exploration loop while avoiding unnecessary latency for
  simple questions and minimal-permission tokens.
- Verifier input for UI-agent turns now accepts citable content returned
  by document search/read, graph, and runtime-inspection MCP tools, while
  catalog and inventory listings remain non-grounding metadata. This
  prevents correctly explored composite turns from being marked
  unverifiable only because they did not finish through `grounded_answer`.
- High-fanout UI-agent read tools now receive bounded default/capped
  `limit` arguments from the parent turn budget, so a model cannot
  silently expand graph and search fanout while probing several tools in
  parallel.
- UI-agent tool calls are now scoped to the active session library before
  dispatch. Missing library arguments are filled from the session, while
  model-authored cross-library arguments fail loudly instead of being
  silently rewritten into an unrelated answer.
- UI assistant turns now switch from tool collection to final synthesis
  once a soft evidence budget is reached and enough MCP results have
  been collected. Complex questions can still use multi-step,
  multi-tool MCP exploration, while long-running turns no longer spend
  the full hard deadline on redundant follow-up tool calls.
- Runtime binding resolution and graph-evidence retrieval now avoid
  broad catalog scans and unbounded graph probes on every turn. Focused
  query-plan probes run first, while the raw user question remains in the
  bounded graph evidence budget as a fallback signal.
- Graph-evidence text lookup now bounds per-query candidate fanout
  before dedupe/final ranking, reducing Postgres work when several
  independent agents probe the same large library concurrently.
- UI-agent `grounded_answer` child calls now inherit the parent turn's
  resolved `topK` ceiling instead of silently expanding to the broader
  MCP default, keeping focused subquestions from inflating retrieval
  work under concurrent agent traffic.
- Verified `grounded_answer` tool results now short-circuit the UI agent
  only when that call is the sole tool call in the iteration. If the
  model asks for several tools in parallel, the grounded answer becomes
  context for the next synthesis pass instead of discarding the other
  MCP results.
- Verifier fallback matching is stricter for exact technical claims:
  split fragments no longer validate HTTP route pairs, code assignments,
  slash alternatives, decorated versions, or structural literals unless
  the evidence is co-located in one source fragment. Conflict fact groups
  are still evaluated even when some exact literals are grounded, and
  document titles and excerpts are accepted as citation evidence without
  treating a bare source URL as proof for unrelated answer text.
- Added idempotent graph read-path indexes for concurrent agent and MCP
  exploration in a single SQLx migration. The migration creates the
  `pg_trgm` extension when needed and builds indexes inside SQLx's normal
  migration transaction, so populated installations should apply it during
  a maintenance window with a role that can create extensions.

### Operational readiness

- Readiness checks now use cached, coalesced control-plane snapshots and
  the dedicated heartbeat Postgres pool. Concurrent assistant traffic no
  longer makes `/v1/ready` synchronously repeat expensive bootstrap
  diagnostics on every poll. Cached readiness can be briefly stale while
  a background refresh is in flight, with a bounded synchronous refresh
  fallback if the cached snapshot ages out.

### Assistant debug inspector

- Redesigned the assistant debug inspector around timeline, MCP calls,
  request context, and raw JSON views. Operators can inspect each tool
  call's arguments, result text, full MCP response, model messages,
  final answer, token usage, and child runtime traces across desktop and
  mobile layouts.
- Updated MCP/UI parity docs so the release contract is based on shared
  schemas, dispatcher behavior, payloads, traceability, and guardrails
  across UI assistants and external MCP clients.
- Expanded OpenAPI summaries and descriptions for every HTTP operation
  so Swagger explains each method's purpose, usage pattern, request
  shape, streaming/debug behavior where relevant, and agent/MCP
  tool-loop contract.

## 0.4.14 — 2026-05-18

### Docker stack stability

- Bundled Redis now runs as a bounded volatile cache in docker compose
  and the Helm chart: AOF/RDB persistence is disabled, the default
  `maxmemory` keeps allocator headroom below the container limit, and
  eviction uses `volatile-lru`. This prevents persisted cache growth
  from making the stack unhealthy after restart while keeping Postgres
  and ArangoDB as the durable data stores.

### Web ingest URL filters

- Web ingest URL selection now uses a two-stage policy:
  independent crawl and document-selection filters each expose allow and
  block pattern lists. The previous single-stage URL filter shape was
  removed from REST, MCP, run snapshots, persisted library policy,
  generated clients, and the documents-page web ingest dialog.
- Added migration 0013 for the new run snapshot columns and library policy
  shape. Legacy blocklist patterns are preserved as both crawl and
  document-selection blocks, while legacy allowlist patterns become
  document-selection allows.
- Recursive crawl settings are now capped at 100 levels and 10,000 pages.
  Default crawl blocks were expanded for common documentation-site utility
  URLs so recursive discovery spends its page budget on content pages
  instead of login, attachment, history, metadata, and version surfaces.
- Filter-only recursive runs now finish as partial completions with visible
  excluded-page counts instead of being treated as system failures, so
  operators can distinguish "nothing matched the URL policy" from crawl or
  fetch errors.
- The documents-page web ingest dialog now includes an inline URL checker
  that evaluates a typed URL against both crawl and document-selection rules
  before the operator starts a run.

### Library backup restore

- Fixed library backup restore from the admin UI: `.tar.zst` archives are
  now sent as the raw request body instead of being JSON-serialized by
  the generated client, restore into an existing selected library remaps
  archive ownership and blob storage keys to that library, snapshot
  graph restore treats context bundles as part of the graph
  snapshot while reporting and skipping dangling graph edges, and the
  frontend nginx route now accepts large streaming snapshot imports.

### document_hint citation surface — fixes + edit UX

- `POST /v1/content/documents/{id}/replace` now accepts and persists the
  optional `document_hint` multipart form field; the 0.4.13 release wired
  it for `POST upload` only, so connectors that always go through the
  replace path (any sweep of pre-existing documents) silently dropped
  the hint.
- The 0.4.13 release also missed plumbing `document_hint` through the
  ArangoDB knowledge-revision store, so the API detail endpoint and the
  MCP `grounded_answer` citation render returned `null` even when the
  value was correctly persisted in Postgres. 0.4.14 wires the field end
  to end across both stores; existing revisions surface `null` until the
  next replace or operator edit attaches a hint.
- New `PATCH /v1/content/documents/{id}` endpoint updates
  `document_hint` on the active revision in place (no new revision, no
  ingest pipeline retrigger). Lets operators edit the user-facing
  citation label after the fact.
- The documents inspector now exposes inline editing of `document_hint`
  with an info-icon tooltip explaining the field's effect on MCP
  citations.
- The documents upload UI now collects the hint in a small confirmation
  dialog after the file picker, with clearer copy explaining what the
  hint does, why it matters, and that it is optional. The dialog states
  that the same hint applies to every file in the batch, and falls back
  to no hint when left empty.

Includes migration 0013 for web ingest URL policy storage. The
`document_hint` fixes reuse the existing 0012 schema.

## 0.4.13 — 2026-05-17

### Connectors: document_hint citation surface

- New per-revision `document_hint` field surfaces a free-form
user-facing label (browser URL or arbitrary text) to MCP
agents in `grounded_answer` citations and in the built-in answer
pipeline. Defaults to None; when unset the citation resolver falls
back to `source_uri` for web-ingested revisions, then to the
document title. The internal `source_uri` is no longer surfaced to
the agent (closes a class of citation-as-link XSS by removing
unsanitized `upload://` and arbitrary scheme leakage to LLM
context). Admin list/detail responses keep `source_uri` for
trace/audit.
- New per-library `include_document_hint_in_mcp_answers` flag (default
ON). Admin → MCP tab exposes a per-library toggle for operators to
opt out of source citation in MCP/answer payloads without removing
the data itself.
- Connectors can now attach a `document_hint` per item; the
multipart upload form accepts an optional `document_hint` field
(≤1024 chars, free-form text).
- Documents inspector now shows the document hint (clickable when it
is a URL).
- Manual upload form gained an optional "Hint" input.
- The new
[IronRAG.ConnectorTemplate 0.0.2](https://github.com/mlimarenko/IronRAG.ConnectorTemplate)
framework exposes `SourceItem.document_hint` and the
[BookStack connector 0.0.2](https://github.com/mlimarenko/IronRAG.BookStack)
fills it with the per-kind BookStack URL (page / attachment /
inline image).

## 0.4.12 — 2026-05-17

### Connectors: external_key surfaced

- `GET /v1/content/documents` now returns `externalKey` on every list
item. Source connectors can now look up "do I already own a document
for this external_key?" in a single paginated pass instead of
fetching each document's detail endpoint individually.
- The documents inspector panel now displays the external key for
connector-owned documents alongside the document id, so operators
can see which BookStack page (or other source item) backs each
IronRAG document.
- New companion projects:
  - [IronRAG Connector Template](https://github.com/mlimarenko/IronRAG.ConnectorTemplate)
  — Python framework for writing custom source connectors.
  - [BookStack connector](https://github.com/mlimarenko/IronRAG.BookStack)
  — production reference adapter (pages + attachments + inline images,
  periodic poll + webhook intake), published as
  `pipingspace/ironrag.bookstack` on Docker Hub.

## 0.4.11 — 2026-05-15

### Startup: fixed clean-stack migration

- Fixed the `0.4.10` clean-stack startup failure where migration `0010`
added the `paused` ingest queue enum value and used it in the same partial
index definition before PostgreSQL had committed the enum change.

### AI providers: OpenRouter embeddings

- OpenRouter now has first-class 3072-dimension embedding presets for
`embed_chunk` and `query_retrieve`, including the Qwen embedding model that
requires an explicit 3072-dimension request parameter, so fresh stacks can
bind compatible OpenRouter embeddings without waiting for credential-driven
model discovery.
- The OpenRouter provider capability table and multi-provider smoke profile now
mark embeddings as supported instead of falling back to another provider.
- Arango vector rebuilds now use the active `embed_chunk` / `query_retrieve`
bindings of a source library as the dimension source of truth, recreate the
instance-wide chunk and entity vector indexes from freshly materialized
vectors, and rebuild all existing vector material before search resumes on
the selected embedding dimension.
- Vector rebuild, vector writes, and vector search now share a PostgreSQL
advisory lock across backend, worker, and CLI processes, so dimension
switches cannot race the ingest pipeline or live retrieval paths.
- The public multi-provider smoke script now requires
`IRONRAG_ADMIN_PASSWORD` instead of carrying a built-in test password.

## 0.4.10 — 2026-05-15

### Ingest: document progress profiles

- Document ingest progress now uses source-aware stage weights instead of
splitting the pipeline equally by stage count, so small text documents no
longer jump straight from early extraction to graph-stage percentages while
extraction-heavy files still show meaningful page/unit progress.

### Administration: ingest queue observability

- Added a global administration queue view for active ingest work across
workspaces and libraries, with running/queued counts, searchable job list,
document navigation, queued-job reordering, and operator cancellation.
- Running jobs now open an inspector with progress, scope, heartbeat, failure
text, and the live ingest stage timeline so operators can see what a worker
is doing without leaving the admin console.
- The ingest queue now follows the documents table interaction pattern with a
paginated list, persistent page-size control, and a fixed selected-job
inspector instead of a detached two-column card layout.
- Queue rows now sort by the operator-visible rank, and administrators can
reorder queued or paused jobs, pause queued/running jobs, and resume paused
jobs once the current worker attempt has stopped.

### Ingest: adaptive Docling throughput

- Docling extraction concurrency now defaults to `auto`, resolving the
effective cgroup CPU and memory limits at worker startup and choosing a
bounded local Docling process count instead of relying on a fixed
one-process default.
- Docker Compose now gives the worker a dedicated resource profile with a
6 CPU / 8 GiB limit and a 2 CPU / 2 GiB reservation, leaving API and
database service profiles unchanged.
- Restarted or requeued large PDF jobs now sync active-stage progress from
durable page-range ingest units before waiting on the heavy-document slot,
so already extracted pages remain visible in progress immediately after
worker replacement or lease recovery.

## 0.4.9 — 2026-05-14

### Assistant: chat transcript and runtime activity UX

- Grounded-answer generation now sends prior user/assistant turns as real chat
messages and exposes runtime retrieval evidence as a synthetic tool-call plus
matching tool result, so provider debug payloads read like a normal agent
conversation instead of a flattened prompt block.
- Query rewrite and coreference sharpening remain scoped to short follow-up
questions, while standalone questions still carry visible prior chat history
for answer continuity without rewriting the effective query.
- The in-chat runtime activity bubble now focuses on the current agent step,
the active tool name, and a single live elapsed timer with one processing
animation. Once the assistant answer starts streaming, the chat focuses on
the answer instead of leaving completed runtime cards below it.
- The assistant layout now uses a collapsible session rail, a composer-level
debug button, and a resizable right debug inspector for full context,
messages, tool calls, runtime stages, and evidence. The old top MCP toggle
and per-message debug popover were removed.

## 0.4.8 — 2026-05-14

### Admin access: token cleanup panel

- Revoked API tokens can now be deleted from the access inspector through a
dedicated `DELETE /v1/iam/tokens/{tokenPrincipalId}` endpoint; active tokens
must still be revoked first, and deletion is recorded in the audit log.
- The token inspector now stays pinned as the right-side panel while operators
scroll long token lists, matching the document table inspector behavior.

### UI state: persistent table controls

- Document and admin tables now persist table-specific pagination size and sort
choices in browser `localStorage`, while explicit URL parameters on the
documents page still take precedence for shareable links.

### Catalog deletion: async workspace and library cleanup

- Workspace and library DELETE endpoints now return `202 Accepted` with a
stable async operation id, while destructive storage/database cleanup runs
in the background. The shell closes delete dialogs immediately, removes the
catalog item optimistically, and polls the operation until it is terminal.
- Catalog delete admission now reuses an already active delete operation for
the same workspace or library and enforces partial unique indexes so repeated
clicks/retries do not spawn competing cleanup workers.
- Workspace billing cost summaries now have a covering workspace-scope index,
reducing the chance of large-workspace cost reads timing out before deletion.

## 0.4.7 — 2026-05-14

### Installer: fail fast on startup migration drift

- `install.sh` now starts stateful services and the startup authority
before creating API/worker/frontend dependents, then watches the
startup container directly. This prevents Compose from sitting forever
on `service_completed_successfully` dependents when startup restarts.
- The startup wait loop now reads the startup container from `docker compose ps -a`, so a fast successful one-shot migration container is detected as
`Exited (0)` instead of being missed and waited on until timeout.
- Backend images now include the migration SQL files under
`/app/migrations`, so checksum-drift recovery can apply the exact
idempotent migration file from the running image before updating
`_sqlx_migrations.checksum`.

### Ingest: restart-safe large documents and graph encoding

- Stored PDF revisions now extract through durable Docling page-range
units. Completed ranges are reused after worker restart, backend
restart, lease recovery, or transient network loss instead of
discarding already processed pages.
- Long-running ingest attempts now heartbeat from an independent
runtime and finalize only while the attempt still owns the active
lease, preventing stale workers from marking recovered jobs complete.
- Graph extraction now repairs unambiguous UTF-8 transport damage at
the provider boundary and rejects unrepaired mojibake/control
characters before persistence, so corrupted labels do not reach the
graph UI.
- Large graph finalization no longer depends on a blocking full-library
summary refresh; summary refresh is treated as derived cache work.
- Docker Compose now exposes the large-document tuning knobs used by
the runtime: `IRONRAG_DOCLING_PAGE_BATCH_SIZE`,
`IRONRAG_DOCLING_PAGE_STREAM_WINDOW_PAGES`,
`IRONRAG_DOCLING_MAX_CONCURRENCY`,
`IRONRAG_INGESTION_HEAVY_PIPELINE_PARALLELISM`,
`IRONRAG_INGESTION_EMBEDDING_PARALLELISM`, and
`IRONRAG_INGESTION_GRAPH_EXTRACT_PARALLELISM_PER_DOC`.
- `IRONRAG_INGESTION_HEAVY_PIPELINE_PARALLELISM` now defaults to
`auto`, resolves from the worker CPU and memory cgroup limits, and caps
automatic scheduling at four heavy PDF pipelines. The automatic value is
also bounded by configured Docling subprocess concurrency so heavy jobs
do not pile up unbounded behind `IRONRAG_DOCLING_MAX_CONCURRENCY`;
invalid values and missing resource detection fall back to the safe
single-pipeline mode.

### Documents: processing progress and failure visibility

- Document list rows now expose primary ingest progress and failure
details from the active ingest attempt, so the table can show processing
percentage without an extra polling surface.
- The documents UI fills the existing blue processing status badge as a
compact progress bar and keeps failed-document errors visible in the
document inspector, including the mobile drawer presentation.
- The document inspector now shows processing-stage model names and
costs in readable stacked rows, with billing-derived model attribution
for stages whose lifecycle event did not carry the model directly.
- Document lifecycle now keeps terminal stage timing, zero-cost provider
calls, and stage detail metadata on the read model, so the inspector
shows per-stage duration, model, cost, call count, and extraction/chunk
counts instead of only the aggregate total.
- Document inspector pipeline stages now render as a compact vertical
stage list with inline focused details, active-stage emphasis, and no
horizontal carousel scrolling.
- Billing now resolves provider calls against model catalog entries by
capability, so embedding stages use embedding prices and tiny non-zero
stage costs are not rounded to `$0.0000` in the inspector.
- Ready documents no longer keep the finalizing stage highlighted, stage
rows surface document-level content, embedding, and graph costs across
retries, and inspector actions use compact icon buttons with hover labels.
- Inspector action tooltips now open inside the inspector bounds and wrap
long disabled-action reasons instead of being clipped at the panel edge.
- Ready-document inspectors now show the ready badge without a redundant
100% progress bar, while in-progress documents still show percent and bar.
- Ready PDF, image, and other non-editable document formats now open
from the inspector in a read-only viewer instead of showing a disabled
edit action.
- The document inspector no longer shows the redundant preparation
summary block; action buttons now sit below the processing pipeline with
slightly larger icon targets.
- Document viewer/editor content now treats MIME-typed PDFs as prose,
collapses excessive blank lines, and hides embedded-image OCR scaffolding
from non-image documents so garbled OCR captions do not pollute the
main reading surface.
- Document inspector totals now use the document lifecycle cost, matching
stage rows that include retry/failure spend instead of mixing them with
only the last successful attempt total.

### Assistant: grounded-answer execution

- The UI assistant now executes turns through the same
`QueryService::execute_turn` path as MCP `grounded_answer`, so both
surfaces share query compilation, hybrid retrieval, graph context,
answer generation, verification, citations, caching, and audit records.
- The separate in-process UI MCP-agent path was removed. The session-turn
endpoint now returns the assistant execution detail directly,
and the SSE form emits safe runtime activity events (`started`,
`tool_call_started`, periodic `tool_call_progress`,
`tool_call_finished`, `persisting`) before the terminal `completed` /
`failed` event.
- Query-result cache hits are accepted only when the cached execution is
verified and still has grounding references; stale or
reference-free cache rows are evicted instead of replayed.
- Grounded-answer retrieval now degrades across independent vector and
lexical chunk-search lanes instead of failing the whole assistant/MCP
turn when only one search lane or one lexical subquery has a transient
backend error.
- Query-IR focus chunk searches now degrade like the other additive
retrieval sources: failed focus subqueries are logged, successful focus
hits are retained, and an all-focus failure no longer aborts a turn that
already has primary retrieved chunks.
- Graph evidence text search now preserves the indexed search path for
full-text and literal lookups before applying the library filter,
preventing large evidence tables from falling back to long per-library
scans during grounded answers.
- The admin MCP setup examples and public docs now include Hermes and
explicitly list common MCP-compatible agents such as OpenClaw, Codex,
Cursor, VS Code-based agents, and Claude clients.
- Assistant turns now recover from mid-stream browser/proxy transport
drops by loading the completed durable session result after the request
boundary, while explicit backend `failed` events still surface as real
errors.
- Assistant LLM debug context is now persisted per query execution, so
the debug view can show the exact provider prompt, tool activity,
messages, response, and usage even after reloads or cached answer
replays.
- `/v1/ready` now validates Postgres migration state against the
embedded migrations in the running binary and returns degraded/503 on
checksum drift, missing migrations, dirty migration rows, or unexpected
applied versions instead of reporting a false Ready state.

### Provider credentials: allow custom baseUrl overrides

> Reported by [@dkomchenko](https://github.com/dkomchenko).

- Fixed a regression where hosted providers rejected explicit `baseUrl`
overrides with `...does not allow baseUrl overrides`.
- Creating/updating provider credentials now accepts explicit `baseUrl`
overrides for all providers; when no override is supplied, runtime still
falls back to each provider's catalog default URL.

### Docker build: incremental compilation and profile tuning

- `target/` cache mount in the builder stage persists compiled
Rust artifacts across `docker build` runs.  Combined with
`CARGO_PROFILE_RELEASE_INCREMENTAL=true`, Cargo rebuilds only the
crates whose source files actually changed — 5–10× faster than a
cold build on the second iteration.
- **Release-profile overrides** (`incremental=true`, `codegen-units=256`,
`opt-level=2`) are set via environment variables so the public-release
Dockerfile inherits the defaults without a custom Cargo profile.
- **Three-stage Dockerfile** isolates the heavyweight docling/Python
warmup from the Rust builder, so changing application code no longer
re-downloads HuggingFace models.

### Docling timeout: raised default and env-override

> Reported by [@VasKorotkov](https://github.com/VasKorotkov).

- Raised `DEFAULT_TIMEOUT_SECS` from 300 s → 900 s so Docling has
enough headroom for large PDFs with OCR.
- The timeout is already overridable via `IRONRAG_DOCLING_TIMEOUT_SECS`.

### Vector index: dimension reconciliation and health probe

- `ensure_vector_index_reconcile` compares the active embedding
model dimension with the existing ArangoDB index and drops+recreates
when they diverge.  Driven by the resolved `dimensions` field in
`ai_model_catalog.metadata_json`.
- **Health probe** (`probe_vector_index`) tests `APPROX_NEAR_COSINE` at
startup and logs an error when the index is corrupted — operators see
a clear signal instead of silent zero-hit retrievals.
- `known_embedding_dimensions` stamps OpenAI embedding model sizes
(3072 / 1536) into the catalog during provider discovery.
- **API binding change detection:** creating or updating an
`embed_chunk` binding now returns `embeddingDimensionChanged: true`
(with previous/new dimensions) when the new model's vector size
differs, so the UI can warn the operator.
- **UI toast** on the admin AI panel shows «Размерность векторов
изменена…» when the binding response carries the flag.

### Re-upload after soft delete

> Reported by [@VasKorotkov](https://github.com/VasKorotkov).

- Replaced the unconditional `UNIQUE(library_id, external_key)`
constraint with a **partial index** (`WHERE document_state = 'active'`)
so a soft-deleted document no longer blocks re-uploading the same
file (migration `0008`).

### Swagger UI: iframe isolation

> Reported by [@VasKorotkov](https://github.com/VasKorotkov).

- Swagger UI is now served as a **standalone HTML page**
(`public/swagger.html`) loading `swagger-ui-dist` locally and
embedded via `<iframe>`.  This completely isolates the vendor
stylesheet from Tailwind preflight, which was collapsing operation
tags into empty lists.

### Admin AI panel: scroll and optional-binding warnings

> Reported by [@VasKorotkov](https://github.com/VasKorotkov).

- The bindings/presets/credentials content area now has `overflow-auto`
so small viewports can scroll the AI configuration tab.
- A **warning banner** appears when optional bindings (`extract_text`,
`vision`) are not configured: keep raster-image OCR on the Docling
engine unless the library is explicitly routed to `vision`; a missing
vision binding fails loudly.

## 0.4.6 — 2026-05-11

### Fix: LoginPage TypeError on fresh stack with the new Agent purpose

- `**bootstrapPurposeMetadata` was missing an `agent` entry**, so when
the bootstrap bundle preview iterated its presets the `Record<...>`
lookup returned `undefined` and `LoginPage.tsx:553` threw
`TypeError: Cannot read properties of undefined (reading 'labelKey')`,
collapsing the auth view into the error boundary on a fresh stack.
v0.4.5 added Agent to `AiBindingPurpose` (and the bootstrap synthesis
pipeline) but left this purpose-metadata table behind. v0.4.6 adds
the missing entry, declares Agent on `BootstrapBindingPurpose` in the
generated TS types so the `satisfies` check covers it, and ships
`login.purposeAgent` / `login.purposeAgentDesc` in `ru.json` and
`en.json`.

## 0.4.5 — 2026-05-11

### UI assistant: complete the MCP-agent wiring + chat-model role recompute

- `**AiBindingPurpose::Agent` is now a first-class required runtime
binding.** v0.4.4 shipped migration 0006 that added the `agent` enum
value but left half of the binding wiring incomplete: the admin UI
did not list the purpose, the compile-time required-binding-purpose list
did not include it, and `BootstrapBindingPurpose` had a deliberate
`unreachable!()` for it. As a result every UI-assistant turn 409'd
with `library X has no active 'agent' binding configured`. v0.4.5
promotes Agent to the primary required tier on both ends:
`PURPOSE_ORDER` / `REQUIRED_RUNTIME_PURPOSE_ORDER` / i18n labels in
`apps/web`, plus `BootstrapBindingPurpose::Agent` + bootstrap preset
synthesis cloned from the active `query_answer` profile (Agent
shadows QueryAnswer-class chat models with tool-loop semantics).
- **Migration 0007 recomputes chat-model `defaultRoles`** so every
`chat`/`text` model in `ai_model_catalog` advertises the standard
text-chat purpose set (`extract_text` + `extract_graph` +
`query_compile` + `query_answer` + `agent`); multimodal chat models
also include `vision`. Migration 0005 had frozen newer chat models
with a narrower legacy purpose set, which is why the admin UI
refused to surface them under `extract_text` even when the provider
clearly supported it.
- **Idempotent Agent backfill.** Same migration walks every active
`query_answer` binding (instance / workspace / library) and inserts
a paired `agent` binding pointing at the same credential and a new
`<Provider> Agent · <model>` preset, on conflict do nothing. Existing
stacks become Agent-ready on the next startup without any operator
action.
- **Reasoner-class `reasoning_content` echo.** Some chat APIs
(notably the hosted reasoner backend that fronts the `*-flash` /
`*-pro` family) reject the second tool-loop turn with HTTP 400
`The 'reasoning_content' in the thinking mode must be passed back to the API` if the assistant message in the prior history strips
the field. `ChatMessage` and `ToolUseResponse` now carry an optional
`reasoning_content`, the OpenAI-compatible gateway parses + echoes
it, and the in-process MCP-agent loop builds the assistant turn via
`assistant_with_reasoning_and_tool_calls(...)` so reasoner multi-turn
runs cleanly. Non-reasoner providers ignore the optional field on
the wire.
- **Provider-aware `tool_choice` policy.** Reasoner-class models
reject `tool_choice="required"` with HTTP 400 (`* does not support this tool_choice`). The unified gateway now sends
`tool_choice="auto"` on those providers and `tool_choice="required"`
only on chat-class models on the first agent iteration when the
loop has not yet produced a tool call.
- **Library ref auto-injection for the in-process agent.** The shared
`grounded_answer` MCP descriptor lists `library` as required because
external MCP clients pick which library their token addresses. The
UI agent always operates inside one fixed library; we now strip
`library` from the schema we hand to the LLM and auto-inject the
unified `<workspace_slug>/<library_slug>` ref before invoking the
tool. The model is freed from inventing identifiers it cannot know,
and the call resolves deterministically.
- **Provider-error body is no longer swallowed.** `sanitize_provider_error_detail`
used to return the placeholder `upstream provider request failed; response body was not included` even when the body had no
credential markers — making 4xx provider responses undebuggable. It
now preserves the body when no marker matches and only redacts when
it actually finds an `sk-` / `Bearer` / `Authorization` / `api_key`
fragment, so operators can read structured provider errors directly
from logs.

## 0.4.4 — 2026-05-10

### Install: detect & explain sqlx migration checksum drift

- `install.sh` now polls the backend health endpoint after `docker compose up -d` and watches `ironrag-startup-1` for the familiar
sqlx error `migration N was previously applied but has been modified`. If the recorded DB checksum diverges from the file
bundled in the new release image, the script prints the offending
migration version, the exact `sha384sum` and `UPDATE _sqlx_migrations` commands needed to recover, stops the stack, and
exits non-zero — the previous behaviour left the operator staring
at an indefinite `Container ironrag-startup-1 Waiting`.

## 0.4.3 — 2026-05-10

### Embedded picture OCR via the active vision binding

- **PDF screenshots, diagrams, and image-only blocks now reach the
graph.** Until v0.4.2 the Docling adapter dropped embedded raster
pictures behind `<!-- image -->` placeholders and the local
rapidocr / tesseract fallback could only handle simple cases. v0.4.3
routes every embedded picture through the active `vision` binding —
the same multimodal LLM the operator already configured for
image-file uploads — so screenshots of UI flows, JSON payload
examples, and configuration tables are OCR'd at multimodal-LLM
recall, not at small-CPU-OCR-model recall.
- Docling's Python extractor now exposes each picture as a
base64-encoded PNG alongside its placeholder ordinal. The Rust
ingest pipeline iterates the list, calls
`extraction::image::extract_image_with_provider` per picture (same
helper as the standalone-image route), and appends the per-picture
text to the document's `content_text` so chunking, embedding, and
graph extraction see it. Cost per call is the same as one image
upload (~$0.001 with `gpt-5.4-mini` vision); for a typical PDF
with 5–10 embedded pictures the augmentation adds <$0.01 to the
document.
- The local rapidocr / tesseract fallback is preserved for offline
deployments and runs first; the vision-binding pass replaces those
snippets when the binding is configured. Cyrillic / CJK / Latin
Rec model selection from v0.4.2 stays in place.
- Failures on individual pictures (provider error, content moderation
rejection) are logged into the extraction warnings array and do not
fail the document — the rest of the pictures and the text layer
proceed.

## 0.4.2 — 2026-05-09

> Provider catalog now ships seven profiles (OpenAI, DeepSeek,
> Qwen / DashScope-intl, GPTunnel, OpenRouter, RouterAI, Ollama)
> with USD pricing and env-keyed credential auto-bootstrap. The
> bootstrap bundle covers all standard binding purposes including `vision`,
> so multimodal chat models are bindable to `vision` directly from
> the admin UI. Ingest stages propagate cooperative cancellation.
> OpenTelemetry replaces the prior Sentry path on backend and
> frontend. Storybook, Playwright snapshot regression, axe a11y,
> and React Query devtools land as baselines. Compliance scans
> (no-prod-dataset-names, no-handler-sql, no-panic-in-handlers,
> no-dbg-macro, no-hardcoded-languages) enforce the shared baseline
> policy gates in CI.

### Multi-provider catalog

- Provider catalog ships seven profiles (OpenAI, DeepSeek,
Qwen / DashScope-intl, GPTunnel, OpenRouter, RouterAI, Ollama)
declared in `ai_provider_catalog` with capability flags, runtime
paths, model-discovery configuration, and bootstrap-preset list.
- Multimodal chat models (`gpt-4o`, `claude-4-*`, `gemini-3.x`,
`qwen3-vl`, `pixtral`, `llama4-vision`, `mistral-medium`/`-large`,
and similar) carry the `vision` purpose in `defaultRoles` and
`multimodal` modality, so they can be bound to the `vision`
pipeline purpose from the admin UI.
- Setting `IRONRAG_<PROVIDER>_API_KEY` in `.env` is sufficient to
register a credential — startup creates one instance-scope
`Bootstrap <DisplayName>` credential per provider, idempotently
(`ai_catalog::ensure_env_provider_credentials`).
- Catalog stores prices per `(model_catalog_id, billing_unit)` in
USD. Per-call billing rows are written for every LLM request and
rolled up per document and per query in the UI.
- Catalog-level capability validation: writing a binding requires
the chosen model to declare the binding's purpose in
`defaultRoles`; `embed_chunk` and `query_retrieve` are upserted
as a paired counterpart on every write.
- New end-to-end harness `apps/api/scripts/multi-provider-e2e.py`
exercises one provider at a time on a self-contained workspace +
library: upload → ingest → `extract_graph` → grounded answer,
asserting the answer references the expected entities.

### Provider router and bootstrap readiness

#### Added

- OpenAI-compatible provider-router profiles can now describe runtime
paths, credential policy, base URL policy, model discovery, bootstrap
presets, and provider capabilities from one unified catalog model.
- First-run setup exposes AI bootstrap bundles through the generated
contract surface, including the full runtime purpose vocabulary:
`extract_text`, `extract_graph`, `embed_chunk`, `query_compile`,
`query_retrieve`, `query_answer`, and `vision`.

#### Changed

- Library shell readiness now separates ingest prerequisites from runtime
query readiness. Missing runtime bindings are surfaced as distinct
purposes, and `query_retrieve` is handled as an embedding-style runtime
purpose instead of being collapsed into answer generation.
- Provider runtime profiles now expose `structuredOutput` through the
shared contract, OpenAPI schema, and generated TypeScript client, so
JSON-schema, JSON-object, and unsupported structured-output modes use
one shared provider profile field.
- The admin AI and bootstrap frontend paths now consume generated API
types at the boundary, with provider catalog validation for capability
and credential metadata instead of duplicate raw response DTOs.
- Web ingest run parameters, document detail adapters, graph topology
adapters, and admin catalog surfaces now use the generated contract
types instead of hand-shaped raw response DTOs.
- Document list status/readiness, document readiness summaries, graph
topology loading, source-access mapping, and snapshot import reports
now use generated contract types across backend and web
boundaries.
- Async operation status now uses a typed shared enum in the backend
contract and generated frontend API instead of raw string DTOs.

#### Fixed

- Missing `query_compile`, `query_retrieve`, or `query_answer` bindings
now make `queryReady` false without making ingest readiness report a
query-only failure.
- Runtime query embeddings now use the active `query_retrieve` binding
and fail loud when it does not target the same vector source as
`embed_chunk`, preventing silent empty-vector retrieval after a binding
change. The query-embedding cache is scoped by provider catalog, model,
credential, and base URL.
- Bootstrap presets and admin binding writes now enforce the same
vector-model invariant for `embed_chunk` and `query_retrieve` before
the runtime can enter a broken retrieval state.
- Grounded-answer retrieval fixed release-blocking regressions around
over-eager clarification, same-stem multi-format document targeting,
graph namespace endpoint disambiguation, and multi-document comparison
truncation.
- QueryIR routing now consumes one unified target vocabulary, including
source-slice row requests, instead of accepting alias ladders or raw
question wording as a parallel routing path.
- Graph extraction now validates the entity/relation schema
exactly and stores only schema-defined relation types, removing parser
aliases and provider-specific structured-output fallbacks.
- First-run setup now blocks completion until a ready provider bundle is
loaded and selected; credential-discovered provider models no longer
make valid configured bindings look missing before credential-scoped
discovery has returned.
- Bootstrap and admin UI errors no longer render raw backend/provider
messages in the main operator path.
- Provider model discovery now uses declared discovery capability paths
instead of model-name heuristics, so opaque router model identifiers
are preserved and classified by the provider profile contract.
- Vector binding lifecycle updates now keep `embed_chunk` and
`query_retrieve` counterparts in sync for active, inactive, update,
and delete paths instead of leaving stale runtime assignments behind.
- UI and MCP grounded-answer probes now send the same retrieval depth by
default, keeping parity measurements tied to the same runtime shape.
- Agent-surface and release-readiness probes now call MCP graph and
document tools with primary catalog library references instead of
stale UUID alias fields.
- Agent-surface probes now execute the primary assistant JSON turn
endpoint instead of waiting for obsolete UI SSE completion frames.
- MCP `grounded_answer` structured output now embeds the same
assistant execution detail instead of maintaining a parallel evidence
projection, restoring evidence-reference parity with the UI assistant turn
payload.
- Agent-surface release gates now avoid over-specific graph-search
top-label and broad negative-token assertions when the grounded answer,
verifier, runtime id, and reference parity checks already cover the
user-visible contract.
- Agent-surface reports no longer include the stale SSE-era tool-start
budget after the assistant probe moved to the primary JSON turn
endpoint.
- Grounded-answer generation now explicitly preserves source-evidence
polarity for existence and capability questions; the live technical
benchmark now checks answer-opening polarity and no longer rejects
correct negated mentions of an unavailable endpoint as forbidden content.
- Standalone assistant questions no longer inherit entity anchors from the
previous assistant answer, so topic changes inside one conversation keep
retrieval focused on the current question while follow-up questions still
use conversation context.
- Grounded-answer prompts now preserve source bindings for multi-role
questions instead of substituting adjacent workflow components when a
direct source document answers the requested role.
- Docling extraction now treats image-placeholder-only markdown as empty
and uses the available text layer as the primary extracted content,
preserving PDF/DOCX text when markdown conversion emits only placeholders.
- Document search now runs expanded lexical chunk probes and evidence
hydration with bounded parallelism and batch-loads document/revision
metadata, reducing release-gate latency without changing the
ranking model.
- Document search now honors `evidenceSampleLimit=0`, runs independent
lexical/entity/relation/provider stages concurrently, caps vector probes
to the requested response surface, and uses term-bounded revision
backfill instead of full-revision scans.
- Live grounded benchmark GET requests now refresh the HTTP session and
retry transient connection drops, so idempotent search probes do not
abort a release run after a closed keepalive socket.
- Snapshot import now authorizes the target library before accepting a
restore and only imports manifest-declared export sections, closing
dynamic table/collection import gaps.
- Snapshot export no longer includes AI provider credentials or runtime
binding state; provider access remains deployment configuration rather
than portable library data.
- Snapshot restore now validates row ownership, declared blob scope, and
graph edge endpoints before import; replace restores also quarantine
old library blobs so stale objects cannot be served after a restore.

### Frontend modernization

#### Added

- Shared server-state now uses TanStack Query 5, code-first OpenAPI from `utoipa`, and a generated TypeScript SDK from `@hey-api/openapi-ts`.
- Storybook 8, MSW-backed API mocks for tests, Sentry-compatible observability, lazy route loading, a gzip bundle budget, and the `make frontend-check` CI gate now cover the web app.

#### Changed

- Frontend ownership is vertical under `apps/web/src/{app,features,shared}`: feature code lives in `src/features/<feature>/`, and reusable API, component, and hook surfaces live in `src/shared/{api,components,hooks}/`.
- ESLint hard-blocks ad-hoc `useEffect` + `fetch` and `useEffect` + `*Api` server-state loops.
- The main bundle budget is calibrated to 220 KB gzip after transitive dependency growth.

#### Fixed

- Ten components were migrated off ad-hoc `useEffect` + `fetch` loops onto the shared server-state path.
- Sentry-compatible initialization no longer lands in the eager bundle; it loads behind `requestIdleCallback` via dynamic import.

### Assistant transient-network handling

- The assistant turn dispatcher no longer surfaces a long
blame-the-browser placeholder ("Browser blocked the request to the
server. Typical causes: extension, tracking protection, corporate
proxy…") on every transient fetch failure. Pre-fetch network
rejects (`NetworkError`, `Failed to fetch`, `Load failed` — the
request never reached the server) are now retried once
transparently in the same handler. Anything that fails after that
surfaces with the raw error message via the existing retry banner,
so operators see the actual cause instead of a misleading
extension-blame guess. Removed the unused
`assistant.errorDiagnosis.*` i18n strings.

## 0.4.1 — 2026-05-03

### Ollama provider catalog: full library coverage

- **68 Ollama models registered out of the box** across the major
families: Qwen3 (0.6B–32B + Coder), Qwen2.5 (0.5B–72B + Coder), QwQ
reasoning, Llama 3.1 / 3.2 / 3.3, Mistral / Mistral-Nemo, Mixtral
8x7B / 8x22B, Gemma 2 / Gemma 3, Phi 3 / 3.5 / 4 / 4-mini, DeepSeek-R1
(1.5B–70B reasoning), DeepSeek-Coder-V2, IBM Granite 3.3, plus
vision (Qwen2.5VL / Qwen3-VL, Llama 3.2-Vision, LLaVA, MiniCPM-V,
Moondream) and embedding (Qwen3-embedding, nomic-embed-text,
mxbai-embed-large, bge-m3, snowflake-arctic-embed2, all-minilm,
granite-embedding) families. Models not yet `ollama pull`-ed appear
in the UI dropdown anyway; selecting one and saving the binding
triggers `ollama pull` on first use.
- **Default roles per model size and capability.** ≥4B chat models gain
the full set `extract_graph,query_answer,query_compile,utility,rerank`
so a single local model can serve every text purpose; <4B chat models
get `extract_graph,query_answer,utility` because they cannot reliably
emit the structured-JSON schemas used by `query_compile` and
`rerank`; reasoning models (DeepSeek-R1, QwQ) get
`query_answer,query_compile,utility`; vision models get
`extract_graph,query_answer,vision`; embedding models keep
`embed_chunk` only.
- **Bootstrap presets.** Each registered model gets an instance-scoped
preset named `Ollama <model>`, so the UI binding picker shows a
ready-to-select option per model without operator setup. 75 Ollama
presets total after seeding (was 7 in 0.4.0).

### Catalog parser hardening

- The `defaultRoles` parser used by `resolve_active_runtime_binding`
no longer treats unknown role labels (e.g. forward-compatible
`rerank` / `utility` from new Ollama seeds) as fatal catalog
corruption. A single unmapped role used to silently return
`ApiError::Internal`, which propagated as a bare "internal server
error" through every binding lookup that listed the model catalog
— breaking embed_chunk and extract_graph for fresh ingest jobs and
surfacing in the admin AI tab as "failed to load AI configuration".
Unknown labels are now skipped.

### Documents header cost summary

- Library and workspace totals always render together when the cost
banner is shown, so a library with no billed executions yet (e.g.
attachments still queued) no longer looks like the library cost
field "disappeared" — both rows render with the standard
`$0.000` placeholder while workspace totals remain visible.

## 0.4.0 — 2026-05-03

### Highlights

- Temporal hard-filter for `record_jsonl` chats and any document with
`occurred_at` headers — date-anchored questions now return only chunks
whose timestamp overlaps the requested window.
- Ordered source-slice (`source_slice` head/tail/all) honours the same
temporal bounds, so "last 20 messages in March 2026" returns the
chronological tail within March, not the tail of the file.
- Vector-row temporal mirror: ANN post-sieve filters directly on the
candidate without a per-row chunk lookup; `over_fetch` returns to 8×
default and gains a hard 8 192 cap.
- Defence-in-depth tenant isolation on every cross-revision lookup:
`library_id` filter is now in the AQL itself, not just in the caller.
- Disambiguation gate is bypassed when the question carries resolved
temporal bounds — date-scoped turns now return a grounded answer or a
clean refusal instead of the off-topic "could be one of: X, Y, Z" reply.

### Retrieval and answer quality

- `QueryIR.temporal_constraints` (compiled from natural language by the
LLM compiler, RFC3339 bounds) is consumed end-to-end: lexical lane,
vector lane, source-unit slice loader, and library-source-profile
fallback. Helper `QueryIR::resolved_temporal_bounds()` aggregates
`min(start)` / `max(end)`; structural RFC3339 parsing only — no NL
word lists.
- AQL adds `FILTER ... occurred_at ... occurred_until ...` to all four
`search_chunks` lanes (text view, title-identity, title-soft, backstop)
and to `search_chunk_vectors_by_similarity`. RFC3339 strings sort
lexicographically equal to chronological order.
- Source-unit slice (`apply_ordered_source_slice_context`) now resolves a
record-stream candidate at library scope when no top-K chunk is a
record stream, then filters head/tail blocks via AQL substring match
on `occurred_at=ISO` headers.
- Search ranking uses generic, language-agnostic best-chunk and
top-chunk evidence-coverage signals; document-search no longer carries
corpus-specific query expansions or topic boosts.
- Deterministic technical answers abstain unless the candidate covers
the typed technical facets requested by `QueryIR` and grounded in
evidence.
- Document targeting normalizes filename separators and natural phrasing
to the same primary document identity through deterministic
longest-match scoring.

### Grounded-answer prompt and policy

- Multi-entity disambiguation rule: the prompt now requires the LLM to
enumerate every distinct entity in context that matches the queried
name, including incidental references inside long chunks; collapsing
them or silently picking the most prominent one is forbidden.
- Live ingest metadata is no longer injected into the answer prompt.
Recent-documents data with mutating `pipeline_state` and
`preview_excerpt` drifted between back-to-back identical calls and
produced divergent UI/MCP answers; the prompt is now a deterministic
function of `(query, retrieved evidence, library summary)`. Locked by
`assemble_answer_context_excludes_recent_documents_for_mcp_ui_parity`.
- `grounded_answer` MCP and UI now use the same `top_k=24` default —
prior 8 vs 24 split was a constitutional §16 parity violation.

### Multiformat ingestion

- Spreadsheet extraction is native-only for `csv`, `tsv`, `xls`, `xlsx`,
`xlsb`, `ods`. Docling adapter is reserved for document-layout formats
and configured raster-image OCR.
- Grounded multiformat benchmark covers PDF, DOCX, PPTX, XLSX with
sheet-level table questions.
- Reprocess parses stored source bytes first and falls back to derived
text only when no source blob is available; record-stream reprocess
derives JSONL from prepared source-unit blocks instead of degrading to
plain text.
- Append processing is diff-aware: persists a source blob for future
retries and reuses unchanged chunk embeddings and graph extraction
records by normalized chunk content.

### Graph extraction quality

- Script-preserving extract (v8): labels and endpoints are copied
verbatim from prepared chunk text; source writing is never converted
into another writing system; look-alike glyph substitution is
forbidden. Bumping the version invalidates older cache entries.
- OCR text quality gate: structural quality score, low-confidence
blocks skipped from summaries, chunks downranked, graph extraction
drops or avoids low-confidence artifacts. Reuse and reconcile paths
apply the same eligibility policy.
- Graph extraction reuse is keyed by rendered prompt + active
provider/model contract, not mutable database row ids. Storage-row
noise no longer invalidates legitimate reuse.
- Coreference rule rewritten language-neutrally: short anaphoric
references resolve structurally to the previously named entity
without enumerating language-specific word lists.
- Record-stream graph extraction is bounded structurally to source
profile + first/last + fact-supported + evenly spaced units.

### Schema and operations

- `content_chunk` gains `occurred_at TIMESTAMPTZ`, `occurred_until TIMESTAMPTZ`, partial index `idx_content_chunk_occurred_at`, and a
range check constraint. PG and Arango chunk row mirror these fields;
ingest populates them from `record_jsonl::extract_chunk_temporal_bounds`.
- `KnowledgeChunkVectorRow` mirrors `occurred_at` / `occurred_until` so
the ANN sieve operates on the candidate row directly.
- `ironrag-backfill-chunk-temporal-bounds` binary: idempotent, cursor-
paginated, Arango-first then PG-flip so failed mirrors stay
retry-eligible; non-zero exit on partial completion.
- `ironrag-gc-stale-chunks` rewrites the library-wide AQL into per-
document batches with `chunk.document_id == ?` and
`vector.revision_id IN @stale`, fitting comfortably under the Arango
per-query memory cap on libraries that previously OOMed.
- Library-scoped record-stream fallback in
`first_record_stream_candidate_profile` finds record-stream profiles
for the active library revision even when no top-K chunk is record-stream.
- `list_source_profile_chunks_by_revisions` AQL now filters by
`library_id` for defence-in-depth tenant isolation.

### Reprocess and append

- Concurrent answer cache fills are single-owner; identical grounded-
answer turns wait for the active fill instead of starting duplicate
fill executions. Coordination failures fail loudly.
- Embedding coverage is verified after reuse: partial-vector revisions
become explicit ingest failures instead of later retrieval misses.
- Reprocess works for text-recoverable revisions without source blobs;
documents with neither stored source nor recoverable text fail loudly.
- Ingest concurrency defaults are bounded for CPU-only hosts.

### Webhooks and operations

- Inbound webhook receiver removed; outbound delivery only. Outbound
pipeline hardened with retries, idempotency, and image_checksum on
delivered events.
- Web-ingest runs carry a single `urlFilter` snapshot with explicit
`blocklist`/`allowlist` mode; documents UI rejects empty allowlists.
- Workspace cost rollup, searchable workspace/library selectors, and
evidence-gated comparison answers landed.

### Assistant transport

- UI assistant uses one direct JSON `POST /v1/query/sessions/{id}/turns`
request — no SSE branch in the primary JSON path. MCP streaming remains
isolated under `/v1/mcp`. Session history hydrates from
`{ session, messages }` so existing conversations restore correctly.

### Tests

- 959 lib tests (was 953 in 0.3.2). New unit coverage for
`resolved_temporal_bounds` (full-range, half-open start, half-open
end, mixed parseable/unparseable, empty), and rewritten orphan-only
contract for `map_chunk_hit` / `map_companion_chunk` (drops chunks
only when the document has no head pointer; runtime is now lenient
on revision-id mismatch since strict equality silently hid ~80% of
chunks for documents with overlapping incremental revisions).

## 0.3.2 — 2026-04-24

### Web ingest

- **Library-owned web ingest ignore policy.** A new `web_ingest_policy`
JSONB column on `catalog_library` stores per-library ignore patterns
(`url_prefix`, `path_prefix`, `glob`). The hardcoded
`classify_confluence_system_page` helper is replaced by a dynamic
matcher in `shared/web/ingest.rs`. New endpoint:
`PUT /v1/catalog/libraries/{libraryId}/web-ingest-policy`.
- **Per-run extra ignore patterns.** Each ingest run can carry
additional patterns on top of the library default, stored in
`content_web_ingest_run.ignore_patterns` and merged at match time.
- `**WebIngestRunSummary.ignorePatterns`** is a required field in the
OpenAPI contract; UI surfaces the active ignore policy in the
web-runs panel.

### Reliability & operability

- **Null-head recovery** (`ironrag-promote-null-heads`): idempotent CLI
that promotes `document_head` for any document whose head is NULL but
whose latest revision has persisted chunks.
- **Fail-loud Postgres ↔ Arango head sync.** `promote_knowledge_document`
now returns `Result<(), ApiError>`; silent warn-on-fail is gone.
Regression test asserts the error path when Arango is unreachable.
- **Fail-soft post-answer reference hydration.** Transient Arango errors
during reference-panel lookups no longer flip a 200-answered turn
into a 500 after the answer body has streamed.
- **Reverse-proxy POST fix.** Dropped `proxy_request_buffering off`
from `nginx.conf.template`; POSTs on `/v1/query/sessions` and
`/v1/mcp` no longer hang for 15 s behind the proxy.
- **ArangoDB memory caps** pinned in compose and Helm
(`--query.memory-limit`, RocksDB caps separate) — eliminates the
5-parallel-turn OOM on small hosts.
- **Pool / threading tuning.** `TOKIO_WORKER_THREADS` default 2 → 8;
heartbeat pool 6 → 24 with 15 s acquire_timeout, removing the reaper
false-positive that was releasing healthy leases under merge load.

### Retrieval quality

- **QueryIR routing.** `QueryCompilerService` compiles the
natural-language question into a typed `QueryIR` (act / scope /
target_types / literal_constraints / confidence); downstream stages
read routing signals from the IR instead of re-classifying the raw
string. Replaces scattered keyword classifiers.
- **Entity-bio fan-out for biographical queries.** When the IR carries a
named target entity (proper noun), retrieval fans out over graph
evidence plus a lexical pass by entity label, and post-filters to
keep only chunks that literally contain the label. Single-word
surname queries now surface every corpus mention rather than the
top BM25 hit. Capitalized mentions take precedence over common
concept nouns when both are present.
- **Title-aware BM25 boost + ngram fuzzy title match.** Typos in
product or proper-noun titles no longer shred recall; the ngram
analyzer on `title` / `file_name` plus a separate title-token
BM25 boost keeps the intended document in top-K.
- **Real chunk embedding in ingest** with library-wide vector filter.
Vector search no longer silently matches stale embeddings from
other libraries.
- **Lexical fallback scan only on view-lag**, bounded bind vars.
Full-scan triggers only when the ArangoSearch view is behind the
freshness budget.
- **Parallel tool dispatch + atomic topology prewarm.** The grounded-
answer tool loop dispatches independent calls in parallel; graph
topology cache is prewarmed atomically at boot.
- **Revision-coherence gate hardened.** `map_chunk_hit` is strict on
strict revision equality, backed by the null-head recovery above.

### Ingest performance

- **Bulk-upsert in merge.** One-shot bulk preload of runtime graph nodes by stable merge keys
preload plus `bulk_upsert_runtime_graph_nodes` replace ~35 sequential
round-trips per chunk; +112 % throughput and −71 % slow statements
on an internal stress stack.
- **Stuck-document terminal marker** with extended backoff: the
stale-lease reaper stops reactivating revisions that have
deterministically failed extraction.
- `**sub_type_hints` cache** (60 s TTL, per `(library_id, projection_version)`)
drops a per-batch Postgres aggregate from the slow-statement log.
- **Bulk chunk-vector UPSERT** per embedding batch replaces the
previous per-row round-trip.

### Assistant UX

- **SSE transport fallback.** When the browser or proxy blocks SSE,
the UI falls back to non-streaming POST and shows an inline retry
alert instead of a NetworkError dead-end.
- **Evidence panel relevance formatter** distinguishes normalized
probabilities (0..1 → percent) from raw BM25 scores (> 1 → decimal);
fixes the `6384 %` overflow for high-BM25 hits.
- **SSE keep-alive 15 s → 3 s.** The longer interval was racing with
Firefox Enhanced Tracking Protection's idle close on large-corpus
retrievals, producing `stream ended without a completed frame`.
- **Assistant i18n.** Diagnosis and error strings moved into the
translation layer; no hardcoded locale text remains in the UI.
- **Operations admin panel simplified.** Noisy status badges, pill
banners, and verbose audit rows removed in favour of a compact
one-line-per-event view.

### Miscellaneous

- Optional `query_compile` and `vision` bindings.
- Graph extraction parser accepts alternative field names from
smaller models.
- Billing, `/v1/content/web-runs`, and graph-cache perf fixes that
were causing concurrent-request timeouts.
- Tuning knobs centralised in `services/query/execution/tuning.rs`.
- Clippy-clean.

### Measured

- Grounded-answer parity benchmark passes on both UI and MCP lanes
for clarify + provider follow-ups + release-history scenarios on
the internal stress corpus.
- Concurrency stress (5 parallel turns): 5 / 5 × 200, p95 21 s;
previously 0 / 5 × 500 with ArangoDB OOM.

## 0.3.1 — 2026-04-17

### QueryCompiler: NL → typed QueryIR replaces hardcoded keyword classifiers

The fix replaces the scattered keyword-based routing with a single unified layer: a compiled `QueryIR` (act, scope, target_types, literal_constraints, conversation_refs, confidence) produced by a new `QueryCompilerService` from the natural-language question. Every downstream stage now reads routing signals from the IR instead of re-classifying the raw question string.

- `**QueryIR` schema.** Finite enums for `act` (retrieve_value, describe, configure_how, compare, enumerate, meta, follow_up), `scope`, `language`, `literal_kind`, `entity_role`, `conversation_ref_kind`. Open-ended ontology tags for `target_types` and `comparison.dimension` so new concepts enter through data, not code. Strict OpenAI-compatible JSON Schema (`domains/query_ir.rs::query_ir_json_schema`) generated by hand so the compiler output validates under structured-output strict mode.
- `**QueryCompilerService`.** Stateless service in `services/query/compiler.rs` that calls whatever model the operator binds to the new `AiBindingPurpose::QueryCompile` — no model is hardcoded in code. Falls back to a safe `Describe` IR with `confidence: 0.0` when the binding is missing, the provider is down, or the model returns invalid JSON, so the pipeline keeps working in degraded mode.
- **Two-tier IR cache** (`migrations/0003_query_ir_cache.sql`). SHA-256 of normalised question + history fingerprint + `QUERY_IR_SCHEMA_VERSION`. Redis hot tier with 24 h TTL, Postgres persistent tier keyed by `(library_id, question_hash)` for cross-session reuse and debugging. Fallback outcomes are never cached. Cache hits carry `provider_kind` sentinels (`cache:redis` / `cache:postgres`) so diagnostics distinguish them from LLM runs.
- **Verification guard now driven by IR.** `verify_generated_answer` reads `QueryIR::verification_level()` → `Strict / Moderate / Lenient`. Only `RetrieveValue` requests with explicit `literal_constraints` stay in strict suppression mode; `ConfigureHow`, `Describe`, `Enumerate` drop to lenient (warnings in metadata, answer reaches the user). The hardcoded gremlin/sparql/cypher/2019 branch in `verification.rs::question_specific_verification_warnings` and its twin answer-builder branch in `answer.rs` are gone — that benchmark case is now covered by the general `unsupported_literal` path at strict level.
- **Consumer migration (9 files).** `search.rs::strong_markers` (15 words), `answer.rs::build_graph_query_language_answer`, `table_row_answer.rs::is_value_inventory_request` (10), `document_target.rs::is_multi_document_comparison` (57), and `technical_literal_focus.rs::ignored_keywords` (37) now route on `QueryIR` fields instead of keyword lists. `planner.rs::STOP_WORDS` (48) and `SYNONYM_GROUPS` (24) were removed — the ontology-aware `target_types` in the IR replace the synonym union and a language-agnostic `TOKEN_MIN_LEN = 3` replaces the stop-word filter. `service/mod.rs::PREPARED_SEGMENT_FOCUS_STOPWORDS` (22 EN+RU entries) and the three `session.rs` follow-up marker lists (38 + 27 + 13) were replaced with length cutoffs; the real follow-up signal lives in `QueryIR.conversation_refs`. `service/session.rs::COMMON_WORDS` (46 English filters) deleted from entity extraction.
- **Observability.** `LlmContextSnapshot.query_ir` surfaces the compiled IR in the debug panel alongside LLM iterations. Structured tracing event `ironrag::query_compile` carries act / scope / target_types / confidence per compile. `#[cfg(debug_assertions)]`-only `validate_ir` enforces schema invariants (Compare → comparison present, FollowUp → refs or low confidence, confidence ∈ [0, 1]).
- **Eval gate.** `tests/query_ir_golden.jsonl` holds 330 hand-labelled questions (202 en / 128 ru, balanced across all seven acts). `tests/query_ir_golden_parses.rs` is a CI gate: every golden row must deserialise into `QueryIR`. `tests/query_compiler_openai_smoke.rs` is an `#[ignore]` smoke test against real OpenAI that catches schema drift before deploy.

### Known follow-ups

Three hardcoded lists remain pending a column-semantic ontology in Arango: `table_row_answer.rs::HEADER_MARKERS` (13 column-name → alias pairs), `document_target.rs::DOCUMENT_LABEL_KEYWORD_MARKERS` + `DOCUMENT_LABEL_ACRONYMS` (focused-document scoring), and `question_intent.rs::INTENT_TABLE` (classify_question_intents). `technical_literal_focus.rs::LEGACY_IGNORED_KEYWORDS` is fallback-only when the IR has no literal_constraints. These move out in the next consumer-migration PR once the ontology tables land; they do not block the verification fix.

### Snapshot scope expansion

- `GET /v1/content/libraries/{id}/snapshot?include=workspace` now packs `catalog_workspace`, `ai_provider_credential`, `ai_model_preset`, and `ai_binding_assignment` rows scoped to the workspace/library alongside the existing library-data and blobs scopes. Restore uses `ON CONFLICT DO NOTHING` on those tables so cloning a library onto a stack that already has the same AI bindings preserves the target's configuration. IAM rows (api_token / secrets / principals) stay out — they are tied to deployment-specific secrets and must be re-issued on the target stack.

### Admin & documents tables

- Fixed paginated sorting on the documents list: sortable columns now order the full result set on the backend instead of reordering only the current UI page. Added primary server sort keys for document cost, pipeline time, and finished time, and fixed cursor pagination for non-default document sorts.
- Fixed the admin pricing table to use one shared server-driven list path for search, provider filtering, sorting, and pagination. `GET /v1/ai/prices` now returns `{ items, total, limit, offset }`, and the UI sorts prices across the full paginated dataset instead of locally inside the loaded slice.
- The documents header now shows the library-wide total cost from the primary billing summary instead of summing only the currently loaded page, so the number stays correct across pagination, search, and local table state.
- The admin token screen now resolves workspace and library scope names server-side and shows effective permissions directly in the list and detail panel. Long raw `workspace:<uuid>` / `library:<uuid>` strings were removed from the primary UX path.

### MCP–UI Parity: `grounded_answer` tool

The IronRAG UI assistant is the reference implementation of grounded Q&A. Every MCP agent plugged into the same library now receives the same answer quality, evidence references, and guard-rails — MCP is no longer a degraded lane, it is the same pipeline exposed as a first-class tool.

- New MCP tool `**grounded_answer`** (`apps/api/src/interfaces/http/mcp/tools/grounded.rs`). Input: `library`, `query`, optional `conversationTurns`, optional `topK`, `includeDebug`. Handler is a thin translator — it creates an ephemeral conversation and delegates to the query service `execute_turn` entry point on application state, i.e. the same entry point the UI handler `POST /v1/query/sessions/{id}/turns` uses. No parallel retrieval, ranking, or answer-generation logic is introduced.
- Structured output surfaces `executionDetail`, the same assistant DTO the UI consumes, including chunk, prepared-segment, technical-fact, graph-entity, graph-relation, verifier, runtime, request, and response fields. Agents receive exactly the data the UI debug panel shows for the equivalent turn, plus top-level `runtimeExecutionId`, `executionId`, and `conversationId` shortcuts for trace lookup.
- Tool is gated by the `query_run` grant — identical to the UI path. An MCP token that can ask questions in UI can ask them over MCP; a scope denied in UI is denied in MCP. Parity is observable at the grant level, not just at the code level.
- MCP tool registry in `apps/api/src/interfaces/http/mcp.rs` extended; `visible_tool_names` advertises `grounded_answer` when the token has `query_run` somewhere.
- Docs updated (`docs/ru/MCP.md`, `docs/en/MCP.md`) — `grounded_answer` is now the top-of-list tool, with explicit guidance "prefer this over `search_documents` + `read_document` for knowledge questions". Admin UI (`apps/web/src/components/admin/McpTab.tsx`) shows an MCP–UI parity disclosure card and updates the recommended system-prompt description so every external client (Claude Code, Claude Desktop, Cursor, Codex, OpenClaw, VS Code) is told to call `grounded_answer` first.

### MCP Streamable HTTP transport (spec 2025-06-18)

- The `/v1/mcp` endpoint now implements the current MCP Streamable HTTP transport and nothing else — no legacy HTTP+SSE split, no parallel POST-only alias, no ad-hoc JSON-RPC surface. One URL handles the full client lifecycle:
  - `POST /v1/mcp` carries JSON-RPC requests, notifications, and batches. Content is negotiated from the `Accept` header: `application/json` → single JSON body (default, curl-friendly); `text/event-stream` (optionally alongside JSON) → one-shot SSE frame `event: message\ndata: …\n\n` so SDK clients that advertise both formats get the transport they expect. Notification-only requests (no `id`) are acknowledged with a bare `202 Accepted`.
  - `GET /v1/mcp` returns a well-formed but silent SSE stream (`200 OK`, `Content-Type: text/event-stream`, one `: ready` comment, connection idle). Spec 2025-06-18 permits either a 405 or a zero-event SSE stream; we choose the latter because some bundled MCP clients (notably OpenClaw's `bundle-mcp`, which spawns a fresh subprocess per chat session) treat any non-200 handshake as fatal and drop the whole MCP server for that agent context. A valid empty stream satisfies them without introducing real event traffic.
  - `DELETE /v1/mcp` returns `200 OK`. The server is stateless between requests — session termination is a no-op — but cleanup flows in SDK clients succeed instead of erroring.
- The `initialize` response now includes a freshly minted `Mcp-Session-Id` header (UUIDv7). Clients that pin the session via this header on subsequent calls are accepted without additional validation; the server does not correlate calls across the id because no state depends on it, but the header satisfies every compliant client (Claude Code remote, Cursor, OpenAI Responses API tools, the official `@modelcontextprotocol/sdk`, OpenClaw bundle-mcp).
- Protocol version is `2025-06-18`. An optional `Mcp-Protocol-Version` request header is tolerated but not required — the server advertises its version via the `initialize` response payload.
- `GET /v1/mcp` and `DELETE /v1/mcp` no longer run the Bearer-auth extractor; both answer with their unauthenticated handshake responses immediately. Bundled MCP clients (notably OpenClaw's `bundle-mcp`, spawned fresh per chat session) speculatively open the SSE stream before carrying the session Bearer, and the earlier 401 response made them drop the whole MCP server registration — group-chat agents were left without tools even though direct-message agents worked. Spec 2025-06-18 allows answering without auth on these methods.
- Smoke-tested end-to-end from the compose stack and from production with OpenClaw on a group chat: `application/json` returns `content-type: application/json` + valid JSON-RPC; `application/json, text/event-stream` returns `content-type: text/event-stream` with one `event: message` frame; `GET` returns 200 + silent SSE handshake (`: ready`); `DELETE` returns 200. Group-chat MCP tool calls now work identically to direct-message ones.

## 0.3.0 — 2026-04-16

Large-library performance release. On a 4900-document reference library the documents page drops from 26 MB / 1.2 s to 3 KB gzip / 50 ms per page, graph from 60 MB / 1.7 s to 2 MB zstd / 60 ms on Redis hit, `get_library_summary` from a broken 11 s to a correct 1.3 s, and batch rerun moves from a 5 s sync handler that timed out beyond a hundred docs to an async job with polling.

### AI catalog & admin UX

- AI model compatibility is now classified uniformly across OpenAI, Qwen, DeepSeek, and Ollama from one binding-purpose model. Hosted DeepSeek remains text-only; multimodal and embedding roles are exposed only where the provider or discovered model family actually supports them.
- `GET /v1/ai/models` no longer performs live provider `/models` discovery on every admin-page load. Generic reads now resolve against the persisted catalog plus visible credentials, while provider discovery sync runs on credential save and on narrow credential-specific refreshes only. In local profiling the AI admin read path dropped from multi-second stalls to low-millisecond responses.
- The Admin `AI` tab was redesigned for high-cardinality catalogs: scope cards, provider inventory, binding cards, searchable credentials and presets, and lazy Ollama model checks in the editor. Fixed the library-override crash caused by the missing `selectedCredentialModelSet` binding state.

### Document operations & ingest hardening

- `GET /v1/content/documents` now honors the documents UI's largest page size: the hard clamp moved from 200 to 1000 rows so bulk-selection and batch actions no longer silently operate on only the first 200 matches of a filtered result set.
- The documents surface now supports true "select all matching" expansion across paginated filtered results, keeps failed uploads visible after a batch completes, and avoids clearing operator-visible per-file errors immediately after acceptance.
- Upload MIME admission is now sniffing-first for unknown text filenames: generic binary MIME plus unknown suffixes like `.env.backup.20260116-162838` are accepted when the payload is clearly text, while explicitly unsupported declared formats such as `video/mp4` still fail fast.

### Documents list

- `GET /v1/content/documents` is now keyset-paginated: `cursor`, `limit` (50/200), `search` (ILIKE), `sortBy`, `sortOrder`, opt-in `includeTotal`. Response is a slim `ContentDocumentListItem` with server-derived `status` and `readiness`; heavy fields moved to the per-document detail endpoint used by the inspector.
- Backend list path is a single Postgres CTE with LATERAL joins replacing the previous 6-call batch prefetch. The `(library_id, created_at DESC, id DESC)` keyset index and a GIN trigram index for `search` over `lower(external_key)` are part of the baseline `0001_init.sql` migration.
- `get_library_summary` now uses a single aggregate SQL (`aggregate_library_document_readiness`) instead of iterating the document list. Fixed a Cartesian `LEFT JOIN ingest_job` that was inflating counts ~4000× during integration.
- Frontend `DocumentsPage` rewritten around server pagination + `IntersectionObserver` infinite scroll. Debounced server search and sort. Client-side status/readiness filters and local `slice()` paging removed — no mixed client/server filtering.

### Graph topology

- New `GET /v1/knowledge/libraries/{id}/graph` replaces `/graph-topology`. Wire format is compact NDJSON: sections `meta → id_map → docs → nodes → edges → doc_links → end`, UUIDs emitted once through a `u32` id map, nodes use short field keys, edges are 4-tuples `[from, to, rel, support]`. Fields the frontend never reads (`metadata_json`, workspace/library/timestamp columns, `normalized_assertion`, …) are stripped from the wire. No truncation — the full graph is always delivered.
- Redis cache by `graph:{library_id}:v{projection_version}` with a 24 h TTL, invalidated from every `upsert_runtime_graph_snapshot` call site in `projection.rs` and `rebuild.rs`.
- Frontend `getGraphTopologyStream` parses the NDJSON through a `ReadableStream` and surfaces `onProgress` to `GraphPage` for a live loading indicator.

### Observability

- `/metrics` Prometheus endpoint via `axum-prometheus` (`axum_http_requests_total`, `axum_http_requests_duration_seconds` labelled by `method`, `endpoint`, `status`). Published on `127.0.0.1:9464` in the local dev compose; in prod compose it stays off the host and is scraped via `docker exec`.
- `#[tracing::instrument]` on the hot paths: `list_documents`, `prefetch_document_summary_data`, `get_graph_topology`, the runtime-graph repository functions, `batch_reprocess_documents`, `reprocess_single_document`. Spans carry `library_id`, relevant counts, and `elapsed_ms`.

### Worker — heartbeat starvation fix

- Root cause of the repeated `lease_expired / stale_heartbeat` failures: synchronous full-library and targeted graph projection paths in the `projection` module, and the per-chunk `serde_json::from_value::<GraphExtractionCandidateSet>` inside `rebuild.rs`'s `buffer_unordered(8)` were fully synchronous CPU-bound paths on tokio worker threads with no yield points. They pegged every tokio worker at 100 %, starving the heartbeat task so leases expired while the worker was still alive.
- Fix: those CPU hot spots now run inside `tokio::task::spawn_blocking`. The heartbeat task moved onto a dedicated `heartbeat_postgres: PgPool` (`min_connections=1`, `max_connections=2`) so the main pool can be fully checked out without blocking heartbeats. New stage-level timeout `IRONRAG_RUNTIME_GRAPH_EXTRACT_STAGE_TIMEOUT_SECONDS` (default 600) wraps `materialize_revision_graph_candidates` + `reconcile_revision_graph` via `tokio::time::timeout`, surfacing `stage_timeout` failures through the existing "degraded to readable" branch instead of silently holding a lease.

### Batch rerun — async job with polling

- `POST /v1/content/documents/batch-reprocess` now returns `202 Accepted` with `{ batchOperationId, total, libraryId, workspaceId }`. A spawned task admits the child mutations via `buffer_unordered(N)` (default 4, `IRONRAG_BATCH_REPROCESS_PARALLELISM`) bounded by `IRONRAG_BATCH_REPROCESS_TIMEOUT_SECS` (default 3600). Library consistency is enforced up front — every `documentId` must belong to the same library.
- `ops_async_operation` carries a nullable `parent_async_operation_id` FK directly in the baseline `0001_init.sql` migration. This is the single-table batch-ops mechanism — any future batch endpoint reuses the same parent/child shape.
- `GET /v1/ops/operations/{id}` returns the parent row plus `progress { total, completed, failed, inFlight }` via a single `LEFT JOIN` + FILTER aggregate. For any parent with children, `status` and `completedAt` are derived on read from progress counts (`processing` while any child pending, `failed` on any child failure, else `ready`) — no race between the spawned admit phase and child lifecycle.
- Old `BATCH_MAX_DOCUMENTS = 1000` cap replaced by a DoS sanity cap of 100 000 ids.
- Frontend shows a progress banner that polls `opsApi.getAsyncOperation(id)` with 1.5 s → 5 s adaptive backoff, refreshes the document list on terminal states, and auto-dismisses 4 s after completion.

### Library backup / restore (tar.zst)

- **Format redesign: NDJSON → tar.zst.** The old NDJSON stream truncated in browsers because the frontend buffered the entire response via `fetch().blob()`. The new format is a streaming tar archive compressed with zstd level 3 (~13× on NDJSON text). Archive layout: `manifest.json` → chunked NDJSON per table/collection (64 MiB soft cap per part) → raw blob entries → `summary.json`. `tokio-tar` + `async-compression` write into a `tokio::io::duplex` pair piped into `Body::from_stream` with natural back-pressure.
- **Streaming Arango cursor.** New `ArangoClient::query_json_batches` yields cursor batches via an async callback instead of merging the entire result into memory. The old path OOM-killed the backend on `knowledge_structured_block` (545 k rows / ~1 GB JSON). Export now streams through a bounded `mpsc` channel (depth 2) between the cursor producer task and the tar writer.
- **Edge `library_id` propagation.** Every Arango edge insert now carries `library_id` (10 wrapper methods, 12 call sites, 8 files). Persistent indexes on `library_id` for all 15 edge collections. Snapshot export edge query uses `FILTER edge.library_id == @lid` instead of the old per-edge `DOCUMENT()` lookup — edge export stage dropped from **11.3 s → <2 ms** on the large-library reference fixture.
- **Batched import.** `PgBatcher` flushes every 1 000 rows via `jsonb_populate_recordset`; `ArangoBatcher` flushes via `FOR doc IN @docs INSERT`. Large-library full restore (107 k pg + 466 k arango rows) dropped from **>5 min (nginx 504)** to **~78 s**.
- **Bulk Arango timeout.** `query_json_bulk` and `query_json_batches` carry a 10-minute per-request timeout override. The default 15 s Arango client timeout was truncating edge cleanup and large-batch inserts.
- **Replace mode.** `POST /snapshot?overwrite=replace` clears the library footprint (reverse-FK Postgres delete, Arango edge/doc purge, blob stash) before restoring. Import trusts the archive manifest for include kinds — no duplicate selector on the request.
- **Simplified include scope.** `IncludeKind` collapsed from `content / runtime_graph / knowledge / blobs` to `**library_data` + `blobs`**. A library is one atomic domain unit; the old split leaked storage tiers into the UI. Back-compat shim maps old tokens to `library_data`. UI dialog: one always-on "Library data" card + one "Include source files" checkbox.
- Export throughput on the large-library reference fixture (24 k nodes, 82 k edges, 445 k structured blocks): **6.1 s / 42 MiB** (was 17.4 s before edge indexes, truncated before tar.zst).

### Graph rendering performance

- **Web Worker layout offload.** `applyGraphLayout` runs in a dedicated module worker (`graphLayout.worker.ts`, ~~70 KB chunk) for graphs ≥ 3 000 nodes. The main thread builds Graphology + inits Sigma while the worker computes coordinates off-thread; positions are returned via a `Float32Array` transferable buffer (zero-copy). Large-library first canvas paint: **~~1.6 s** (was browser "page is slowing down" warning).
- **Hidden-edge precompute.** The `hiddenIds → hiddenEdgeIds` derivation moved from the per-hover reducer effect (O(M) on every hover commit) to a dedicated effect keyed only on `hiddenIds` change. Hover branches read a pre-built `Set<edgeId>` ref — O(1) per edge per frame.
- **O(degree) selection.** Click-mode connected-edge lookup uses `graph.edges(selectedId)` (O(degree)) instead of `graph.forEachEdge` (O(M)). On the large-library reference fixture: ~8 edges vs 82 k scan per click.
- **Instant layout at density.** Layout transitions skip the 280 ms per-frame interpolation at ≥ 5 000 nodes — the animation burned 1.5 M `setNodeAttribute` calls/sec with no visual value at that density.
- **Labels disabled at extreme density.** `renderLabels: false` at > 15 000 nodes eliminates Sigma's label collision pass (the dominant per-frame cost). `selectProminentGraphLabelIds` O(N log N) sort also skipped in that regime.
- **Byte-buffered NDJSON parser.** `getGraphTopologyStream` uses a `Uint8Array` ring buffer with direct 0x0A scan instead of the old `pending += chunk; pending.slice()` pattern, preventing O(N²) string churn at 100 k+ node topologies.

### MCP performance

- **N+1 eliminated from capability snapshot.** `visible_workspaces` used to loop N workspaces issuing one `load_visible_library_contexts` per workspace. New `visible_catalog` loads workspaces + all libraries in 2 concurrent queries via `tokio::try_join!` and groups in memory. `mcp.capabilities`: 132 ms → **50 ms**; `mcp.initialize`: 116 ms → **53 ms**.

### Release-readiness tooling

- `**scripts/ops/release-check.py`** — consolidated pre-release smoke + perf suite. 24 checks covering auth, catalog, content, knowledge, snapshot export, and MCP tools. Per-check latency budgets with pass/warn/fail verdicts, top-10 latency table, machine-readable JSON output. Replaces ad-hoc curl scripts.

### Extraction pipeline refactor

- **tree-sitter AST extraction for 15 languages.** Code identifier extraction switched from substring heuristics (`"fn "`, `"class "`, `"const "`) to real AST parsing via tree-sitter. Supported: Python, JavaScript, TypeScript, Bash, Rust, Go, Java, C, C#, Ruby, PHP, Swift, Scala, YAML, Proto. Each grammar tested with per-language unit tests. Heuristic fallbacks removed entirely — if tree-sitter doesn't support the language, no code identifier fact is produced.
- **Structural config parsing (JSON, YAML, TOML).** Config key extraction via `serde_json`, `serde_yaml`, `toml_edit` replaces the old `split_once(':')` / `split_once('=')` heuristic that produced false positives on prose. Only declared config-language blocks are parsed.
- **URL and version parsing via real crates.** `url::Url::parse` (RFC 3986) replaces `starts_with("http://")`. `semver::Version::parse` replaces manual digit splitting.
- **Block-family dispatch.** The extraction loop dispatches extractors by block kind instead of running all 14 on every line. CodeBlock → identifiers + env vars + config keys; EndpointBlock → full HTTP surface; Table → endpoints + config + params; Prose → URLs + versions + error codes.
- **Parser-derived confidence.** Confidence now derived from the parser that produced the fact (AST node → 0.98, structural parse → 0.97, keyword heuristic → 0.94) instead of hardcoded values per block kind.
- **Typed QuestionIntent.** `QuestionIntent` enum with 10 intents (Endpoint, Parameter, HttpMethod, Version, ErrorCode, EnvVar, ConfigKey, Protocol, BasePrefix, Port) and a structured bilingual keyword table. Replaces scattered `contains()` chains across 4 files.
- **Fact-grounded deterministic answers.** Endpoint and parameter answers now require matching technical facts in the evidence store, not substring parsing of chunk text.
- **Branded identifier heuristics removed.** Heading phrase matching and catalog link guessing deleted from the pipeline — entity extraction is the LLM's job, not the fact store's.

### Token scopes & MCP

- **System-scope tokens.** New scope level for tokens with no workspace restriction — full admin across all workspaces. UI scope selector: System / Workspace / Library.
- **Auto-inference of workspace_id / library_id.** MCP tools and the query API auto-fill IDs from the token scope when exactly one workspace or library is accessible. `list_documents.libraryId` is now optional; query session `workspaceId` is optional.
- **Leaner MCP initialize response.** Removed `tokenId`, `tools` list (duplicate of tools/list), `generatedAt` from the initialize JSON-RPC response. 833 → 270 chars.
- **Permission hierarchy in token mint UI.** Grouped cards (Admin, Workspace, Library & Content, Operations) with lucide icons and human-readable labels. Implied permissions show as checked+disabled. `connector_admin` added. Labels explicitly mention import/export capabilities.

### Query execution performance

- **O(1) edge lookup.** `QueryGraphIndex.edges` switched from `Vec` with linear `.find()` to `HashMap<Uuid, GraphViewEdgeWrite>`. On the large-library reference fixture (82 k edges), `map_edge_hit` dropped from ~41 M comparisons to ~500 hash lookups per query.
- **Batched extract candidate inserts.** `replace_extract_node_candidates` and `replace_extract_edge_candidates` switched from per-row INSERT to `QueryBuilder::push_values()` bulk INSERT … RETURNING. On a 5 k-doc library with ~50 candidates per chunk, this eliminates millions of sequential round-trips during ingest.
- **Batched audit subject inserts.** `append_audit_event` subject loop switched to single-statement `push_values` INSERT.
- **Extract candidate indexes.** New migration `0003_extract_candidate_indexes.sql` adds `CREATE INDEX CONCURRENTLY` on `extract_node_candidate(chunk_result_id)` and `extract_edge_candidate(chunk_result_id)` — the tables lacked FK indexes and every per-chunk DELETE/SELECT was a full table scan.

### AI catalog bootstrap

- **Pre-seeded model presets for all providers.** `seed_all_provider_presets` runs at bootstrap and creates presets for OpenAI, DeepSeek, Qwen, and Ollama (4 purposes × 4 providers = 16 presets) regardless of whether API keys are configured. Operators can immediately assign bindings after adding a credential — no manual preset creation required.
- **Preset lookup fix.** `select_runtime_preset` no longer falls back to a single-model match when the expected preset name doesn't match. The old behavior returned the wrong preset when two purposes (e.g. ExtractGraph + QueryAnswer) shared the same model (qwen3:0.6b), preventing the second preset from being created.

### Code quality

- **Named constants for vector kinds.** `VECTOR_KIND_CHUNK` / `VECTOR_KIND_ENTITY` (8 sites). `FACT_FETCH_MULTIPLIER` / `FACT_FETCH_MIN`. Domain status constants `ASYNC_OP_STATUS_*`, `MUTATION_KIND_*`, `GRAPH_STATUS_*` (15+ sites).
- `**execution_id_of()` helper.** Deduplicates 5-site `.expect(...)` pattern.
- **ServiceRole enum dispatch.** Config string matching replaced with existing `ServiceRole::Api/Worker/Startup` enum.
- **N+1 query eliminated in document lifecycle.** Batch `list_ingest_attempts_by_jobs` / `list_ingest_stage_events_by_jobs` via `WHERE job_id = ANY($1)` — 2 queries regardless of job count.
- **Sequential awaits → `tokio::try_join!`** in AI catalog service.
- **Dead code removal.** Query execution scaffolding, branded identifier heuristics, unused types.
- **In-place dedup.** BTreeSet roundtrip pattern (4 sites) → `sort_unstable() + dedup()`.
- **MCP library_ids capped** at 50 per request.

### Platform bits

- `tower-http::compression` layer (gzip + br + zstd) added globally.
- `nginx.conf.template`: `proxy_buffering off` and `proxy_request_buffering off`.
- New deps: `tokio-tar`, `async-compression`, `tokio-util`, `tree-sitter` 0.25 + 15 grammar crates, `serde_yaml`, `toml_edit`.
- All backend + frontend deps updated to latest compatible versions.

## 0.2.3 — 2026-04-14

### Diff-aware reuse correctness & graph fail-fast

- **Diff-reuse no longer poisons the new revision's lifecycle**. `materialize_revision_graph_candidates` synthesizes a `runtime_graph_extraction` record for every chunk whose text checksum matches the parent revision, but used to clone `raw_output_json` verbatim — including the parent revision's `lifecycle.revision_id`. The downstream `reconcile_revision_graph` filter then dropped every reused record because `extraction_lifecycle.revision_id != Some(current_revision_id)`, leaving the merge with **0 records → 0 nodes → 0 edges** even though all 100 chunks looked "ready" in the table. Fix: rewrite `raw_output_json.lifecycle.revision_id` to the current revision before persisting the synthetic record. Triggered for any retry/edit on a CSV-style document where every row is structurally identical.
- **Fail-fast graph pipeline**. `run_inline_post_chunk_pipeline` used to swallow a "0 contributions" reconcile by recording `extract_graph` as `completed` with `graphReady=false` and degrading to `graph_state='processing'`, leaving the document permanently stuck in a misleading half-state with no error visible. The pipeline now returns `ApiError` whenever `materialize` errors, `reconcile` errors, or `graph_ready` is false; the ingest job is marked `failed` and the revision's `graph_state='failed'`. Single unified path — no `degradedToReadable` shim.
- **Honest stage timings**. The "embed_chunk" stage used to fire a fake "deferred" `started`/`completed` pair before `materialize` even ran, then re-emit `completed` after reconcile finished, which made `merge_stages` report the full wall clock (~19.6 s) for both `Extract Graph` AND `Embed Chunk`. The pipeline now uses two independent timers: `extract_elapsed_ms` covers only the LLM materialization phase, `embed_elapsed_ms` covers only the reconcile phase, and the `embed_chunk` stage event is emitted **only** when `embedding_usage` is actually present (no no-op fallback row).
- **Single source of truth for `Extract Graph` model name**. `materialize_revision_graph_candidates` now reads `provider_kind` and `model_name` directly from the active extract-graph binding (`graph_runtime_context.provider_profile.indexing`), so the pipeline event always carries the binding model — even when 100 % of chunks are reused and the per-chunk LLM call never ran. Removed the `provider_kind` / `model_name` fields from `ChunkExtractAggregate` since they were dead code under the new scheme.

### Document status priority chain

- **In-flight job beats stale readiness**. Both `interfaces/http/ops.rs::map_document_status` and `apps/web/src/pages/documents/mappers.ts::mapApiDocument` checked `readiness_is_ready` before checking `queue_state`, so a document with previous successful readiness and a freshly enqueued retry job stayed visually "Ready" until the retry actually finished — operators couldn't tell if their click had registered. The priority chain is now: terminal failure → `canceled`/`failed` → `leased` (Processing/Blocked/Retrying/Stalled) → `queued` → `graph_ready`/`readable` → `graph_sparse` → zombie `completed`. Backend and frontend mappers stay symmetric.

### Documents page live updates

- **Quiet 5-second background refresh**. The documents page used to poll only when at least one document was in `processing` state, on a 15-second interval, and replaced the entire `documents` array on each tick — which left selected-row inspector data stale until the user clicked the row again. Polling now runs unconditionally on a 5-second cadence whenever the page is mounted, refreshes both the table rows and the selected document's full inspector payload (lifecycle, segments, facts) in place, and never resets row selection or any inspector sub-state. `silent=true` keeps the spinner and error banner out of the polling path.

### Ingest throughput & worker stability

- **Parallel per-chunk graph merge**. `reconcile_revision_graph` used to walk `latest_records_by_chunk` sequentially, paying ~130 sequential DB round-trips per chunk × N chunks per doc — pinning all 4 tokio runtime worker threads at 100 % CPU and starving the heartbeat / dispatcher / cancel-poll tasks. Wrapped the chunk loop in `stream::iter().buffer_unordered(8)` with `Arc`-shared pool / quality_guard / document / merge_scope captures, and consume `latest_records_by_chunk.into_values()` so `record.normalized_output_json` is `mem::take`'d straight into the deserializer (no per-chunk deep clone of `serde_json::Value`). Empirically reproduced on prod data: worker CPU dropped from **502 % → 7 %** (62× less starvation), tokio threads moved from `state=R` (running) to `state=S` (sleeping on futex), pipeline throughput unblocked.
- **Bulk `runtime_graph_evidence` insert**. New `bulk_create_runtime_graph_evidence_for_chunk` repository function batches every per-chunk evidence row (typically 50+ for 10 entities + 10 relations) into a single `INSERT ... SELECT FROM unnest(...)` round-trip. `merge_chunk_graph_candidates` collects `GraphEvidenceTarget`s into a `Vec` during the per-entity / per-relation walk and flushes once at the end. Replaces ~50 sequential single-row INSERTs per chunk with one bulk insert.
- **TLS `close_notify` retry coverage**. The transport retry classifier was missing the rustls "peer dropped TLS session mid-response" pattern. Production worker would see `error decoding response body: ... peer closed connection without sending TLS close_notify` from the LLM provider, fail to recognize it as retryable, and surface `extract_graph` as failed after ~51 s at the outer recovery layer. Added `peer closed connection` and `close_notify` to `is_retryable_transport_error_text` so these cases now go through the standard `[1, 3, 10, 30, 90]` s schedule.

### Dashboard / documents page consolidation

- **Single source of truth for "Failed" counts**. The dashboard `failed_documents` and the documents-page filter pill used different mappers and disagreed by an order of magnitude (dashboard reported 142 failed, the documents page reported 4829 for the same library). The backend `map_document_status` in `interfaces/http/ops.rs` now mirrors the frontend `mapApiDocument` rules: `queue_state='canceled'` and zombie completions (`queue_state='completed'` with no readable / sparse readiness) both map to `DocumentStatus::Failed`, matching the frontend bucket. Both surfaces now report the same count.
- **Empty graph view fix**. `get_graph_workbench` and the graph topology endpoint relied on `runtime_graph_state` to derive an entity's `entity_state`, but the function read `metadata.extraction_recovery_status` first — which contains values like `clean`, `recovered`, `partial` describing how the node was extracted, **not** whether it is admitted. With "active" hard-coded as the filter target, ~99 % of nodes were dropped: a large library's graph displayed as "0 nodes / Graph is empty". Fixed by removing `extraction_recovery_status` from the state derivation chain — entity_state now resolves from `metadata.entity_state` / `metadata.relation_state` and falls back to `"active"`.

### Graph viewer interactivity (frontend)

- **Edges no longer disappear during pan/zoom** — `hideEdgesOnMove: false`. The previous `hideEdgesOnMove: denseGraph` made the graph feel broken on every cursor move.
- **Hover ≠ click**. Hover used to dim every other node and re-color edges; now hover only highlights the node + neighbors, and edges are never touched. Click owns the focus mode (selected node + neighbors keep color, every other node fades to white, only edges incident to the selection light up).
- **Dwell-time hover (140 ms)**. Hover state commits only after the cursor pauses on a node — fast sweeps across a dense graph never trigger the expensive `sigma.refresh()` (~120 ms on 25 k nodes). The dwell gate keeps `HOVER_FPS` at the 60 fps baseline during sweeps.
- **DOM tooltip card anchored to node viewport**. Hover shows a floating card with the full node label, neighbor count, and the first 12 neighbor names. The card is positioned via `sigma.graphToViewport(node.x, node.y)` and re-anchored on every camera `updated` event, so it stays glued to the node during pan/zoom instead of trailing the cursor.
- **Density-aware label rendering**. `labelRenderedSizeThreshold` scales 8 → 10 → 14 → 20 with `nodes.length` and `labelGridCellSize` jumps 100 → 240 on dense graphs. `hideLabelsOnMove` is now true for every graph above 5 k nodes (was 140). Hover no longer `forceLabel`-s neighbors on dense graphs.
- **Pre-computed neighbor index + label lookup**. `useMemo<Map<string, Set<string>>>` rebuilt only when `nodes` / `edges` change; hover and click both read it as O(1) instead of walking the graphology adjacency list each time.
- **Layout spacing tuned for `autoRescale`**. `layoutBands`, `layoutSectors`, `layoutRings` now spread cells aggressively (`orderRoot * 1.4` for bands, `× 4` inner radius for sectors, `× 1.6` ring gaps) so even after Sigma compresses 25 k nodes into the viewport the cells stay visually distinct.
- **Density-aware node radius**. Per-node `size` shrinks with the visible node count (3..13 → 2..7 → 1.4..4 → 1..2.6 across density tiers) so dense graphs do not paint as a solid color block.

### Local profiling environment

- Reproduced the 0.2.2 symptoms on a local stack seeded from a production pg_dump of a large reference library (~5 k docs, ~25 k graph nodes, ~80 k edges, ~230 k evidence rows). Root-cause analysis behind the 0.2.3 changes and the reproduction harness used to validate the merge-loop and viewer fixes (Playwright + Chromium hover-FPS measurement) captured in `tmp/prod-0.2.2-perf-analysis.md`.

## 0.2.2 — 2026-04-13

### Ingest resilience

- Startup lease sweep (`reclaim_orphaned_leases_on_startup`) requeues orphan `leased` rows before the dispatcher claims anything, so a backend/worker restart no longer freezes in-flight docs. Steady-state reaper tightened to 15s interval / 60s stale threshold.
- Dispatcher claim query now counts **all** `queue_state='leased'` rows against the global / workspace / library limits — the previous `heartbeat_at` freshness filter introduced a TOCTOU that let the per-library cap be bypassed, stacking parallel docs past the cgroup.
- Cancel now reaches active leases. `cancel_jobs_for_document` flips both `ingest_job → canceled` and `ingest_attempt → canceled` in one CTE; the heartbeat loop polls `queue_state` and signals a `JobCancellationToken` that pipeline stages check between steps, so a cancel is observed in ≤15 s.
- Retry unsticks stalled documents (`force_reset_inflight_for_retry`), falls back to the latest `content_revision` when the head promote never fired, and tombstones orphans with zero revisions. Web retries re-fetch the source URL via `WebIngestService::refetch_document_source`; uploaded docs reuse their stored source. Diff-aware chunk reuse is preserved across retries.

### Worker memory footprint

- Vision LLM image bytes moved instead of cloned (`describe_extracted_images` takes `Vec<ExtractedImage>` by value, drops each image after its call). `extraction_plan` is moved into the primary extracted-content aggregate instead of deep-cloned. Per-chunk graph-extract uses `Arc<…>` for shared state and `try_fold` instead of `buffer_unordered + try_collect` so nothing accumulates across a document's chunk stream.
- PDF extractor parses the `lopdf::Document` once (was twice), `drop`s it before vision phase, and caps images at 30 / 150 MB per doc. HTML extractor truncates the decoded source to 4 MB at a tag boundary before `scraper::Html::parse_document` so pathological Confluence exports can't blow the `html5ever` arena. Both paths warn but never reject a document.
- Noisy third-party `tracing` crates (`scraper`, `html5ever`, `selectors`, `hyper`, `h2`, `reqwest`, `rustls`, `sqlx`, `mio`, `tower`, `tonic`, `tungstenite`) are clamped to `warn` in `telemetry::init` regardless of `IRONRAG_LOG_FILTER`, so `debug` no longer amplifies HTML parse events into gigabytes of log allocation.
- `build_chunk_reuse_plan` stores `Arc<RuntimeGraphExtractionRecordRow>` in all three HashMaps so the diff-reuse path refcount-bumps instead of deep-cloning `raw_output_json` + `normalized_output_json` three times per doc.

### Throughput & CPU

- Per-library dispatcher ceiling raised to 16 with a **memory-aware throttle**: before each claim, `fill_available_job_slots` reads worker RSS and holds new claims when the process is over `ingestion_memory_soft_limit_mib`. The soft limit auto-resolves to 90 % of the detected cgroup (or `/proc/meminfo`) memory — any container size gets a sensible cap with zero manual tuning; an explicit positive value overrides.
- Per-document graph-extract fan-out decoupled from the cross-doc limit via the new `ingestion_graph_extract_parallelism_per_doc` (default 8). Heavy docs get proper chunk parallelism without raising `per_library`.
- LLM transport retries now follow a fixed `[1, 3, 10, 30, 90]` second schedule (5 retries, 134 s total) covering timeouts and the retryable 4xx/5xx set (408, 409, 425, 429, 500, 502, 503, 504, 520–524, 529). `llm_transport_retry_attempts` bumped 3 → 5 and `runtime_graph_extract_recovery_max_attempts` 2 → 4 so both layers have room to run.
- Synchronous extractors (`extract_pdf`, `extract_docx`, `extract_tabular`, `extract_pptx`, `extract_html_main_content`) now run on `tokio::task::spawn_blocking`, freeing the async runtime for concurrent I/O. Postgres connection pool raised 20 → 64 so 16 parallel jobs don't contend on connections. Heartbeat loop now respects `ingestion_worker_heartbeat_interval_seconds` instead of using a hardcoded constant.

### Vocabulary-aware graph extraction

- New `list_observed_sub_type_hints` aggregates existing `metadata_json->>'sub_type'` values per `node_type` for the library + projection version. `build_graph_extraction_prompt_plan` renders them as a `sub_type_hints` section so the model converges on in-use vocabulary instead of inventing fresh near-duplicates.

### Document status taxonomy (UI)

- `DocumentStatus` with strict priority: `canceled` → `failed` → `ready` → `blocked` / `retrying` / `stalled` → `processing` → `queued`. Timer only ticks for in-flight states. `documentStatusSortRank` + `Finished` column give operators proper severity sort and completion time.
- Filter pills rebuilt around four buckets: **All | In Progress | Needs Attention | Ready | Failed**. Retry toasts surface real outcomes with accurate skipped / failed counts; documents that no longer exist come back as `skipped` instead of `failed`. `queue_state='completed'` + `readiness='processing'` zombies are reclassified as `failed` with an explicit reason.
- Inspector cleanup: dropped the legacy "Re-ingest from URL" button, summary truncated to 280 chars with a "Show full" toggle, `source_uri` shown for `web_page` docs instead of the `viewpage.action` basename.

### Documentation

- `docs/{en,ru}/PIPELINE.md` refreshed for the new parallelism model, memory-aware throttle, auto-resolved soft limit, LLM retry schedule, and updated lease thresholds.

## 0.2.1 — 2026-04-12

- Fixed web snapshot persistence for long percent-encoded page and attachment URLs.
- Fixed graph edge rendering during zoom and selection so focused neighbors stay visible above unrelated nodes.
- Fixed web-ingested documents to render as `Web page` instead of leaking raw source/file extensions into the type column.
- Added standard token minting for an explicit target workspace plus multi-library grants in one token, fixed library-scoped discovery so tokens only enumerate selected libraries, and closed the workspace-scoped `iam_admin` auth bypass.
- Hid unfinished connector-admin permissions from user-facing token issuance surfaces while keeping connector support on the backend roadmap.

## 0.2.0 — 2026-04-12

### Highlights

- Rebranded the shipped product from `RustRAG` to `IronRAG` across env vars, packages, images, charts, OpenAPI, and release-facing docs.
- Added a full-screen document editor for text, `docx`, and `xlsx` uploads with table-aware markdown editing and automatic reprocessing after save.
- Unified `xlsx`, `docx`, `pdf`, and `csv` table handling on one shared extraction path with grounded row and column-summary semantics.
- Added a full document lifecycle inspector with per-stage duration, model/provider identity, token usage, and cost from the primary billing path.
- Rebuilt the in-app assistant on one unified MCP-tool agent loop and added a DeepSeek bootstrap preset for first-run provider setup.
- Added diff-aware ingest reuse so unchanged chunks can skip repeated graph extraction on document replacement and edit flows.

### Breaking Changes

- **Schema reset**: the database baseline was consolidated to one `0001_init.sql` migration; legacy execution and accounting paths were removed.
- **Assistant/MCP cutover**: the standalone `ask` shortcut and parallel special-case assistant flow were removed; assistant Q&A now runs only through the unified MCP tool loop.
- **IRONRAG rename**: release-facing configuration now uses `IRONRAG`** naming instead of `RUSTRAG`**.

### Platform

- Billing and pipeline cost rollups now come from one primary source of truth, including vision and embedding calls.
- System-level IAM grants now authorize correctly across workspaces, libraries, and documents.
- Document delete and revision-head writes were hardened so primary Postgres state can commit cleanly even when read-model cleanup degrades to warnings.
- Grounding guardrails now refuse conflicting or insufficiently supported answers instead of shipping hallucinated output.
- Added `ironrag-cli` for users, tokens, workspaces, libraries, and scoped permission management.
- Deployment surfaces were aligned around the split `web` / `api` / `worker` / `startup` topology for Docker and Helm.

### Refactor

- Split major backend hotspots into focused graph-store, MCP, AI catalog, ingest, graph-service, and query submodules while deleting legacy release artifacts and dead code.
- Trimmed the MCP protocol to the declared tool surface only: no fake resources capability, no legacy aliases, and permission-filtered `tools/list`.
- Removed silent error swallowing and cleaned up the release line to a zero-warning backend build.

### Validation

- Added release-gate coverage for end-to-end pipeline quality and graph mutation correctness.
- Revalidated the Docker and Helm deployment paths for the `0.2.0` release line.

## 0.1.3 — 2026-04-10

### Performance — Ingestion Parallelism

- **Parallel embedding batches** (`ingest/runtime.rs`): node and edge embedding now sends batches in parallel via `futures::stream::buffer_unordered`. New env var `IRONRAG_INGESTION_EMBEDDING_PARALLELISM=4` (default) controls how many embed batches run concurrently per job. For a 200-chunk document this is ~4x faster end-to-end.
- **Per-library job isolation** (`ingest_repository::claim_next_queued_ingest_job`): SQL claim now optionally caps the number of `leased` jobs per library. New env var `IRONRAG_INGESTION_MAX_JOBS_PER_LIBRARY=0` (0 = unlimited) prevents one busy library from starving others when many docs are queued at once.
- **Parallel web crawl fetches** (`ingest/web::discover_recursive_scope`): the BFS frontier is now drained in waves of N candidates and HTTP fetches run in parallel via `buffer_unordered`, while DB writes stay sequential per result for stable seen-set determinism. New env var `IRONRAG_WEB_INGEST_CRAWL_CONCURRENCY=4`.
- All three knobs are independent and tunable per deployment via env vars. Defaults are conservative (4) and tested against the local stack.

### Refactor — Bootstrap Flow Cleanup

- **Removed legacy `/iam/bootstrap/claim` endpoint** entirely: route, handler, `BootstrapClaimRequest/Response`, `BootstrapClaimCommand/Outcome`, service method, OpenAPI paths and schemas, integration test, and the `bootstrap_token` / `bootstrap_claim_enabled` config fields with their `IRONRAG_BOOTSTRAP_TOKEN` env var. Single bootstrap surface is now `/iam/bootstrap/setup` only.
- **Display name optional** in bootstrap setup: backend already accepted `Option<String>` and falls back to login; frontend no longer validates it as required and passes `undefined` when empty.
- **Required field markers** in `LoginPage`: red `*` next to required labels (Admin login, Password) plus `(optional)` hint next to Display name; matching `login.optional` i18n key in en/ru locales.
- **Cursor pointer everywhere**: added `cursor: pointer` to all `button:not(:disabled)`, `[role="button"]`, `a[href]`, `label[for]`, `summary`, `select` via `@layer base` global rule in `index.css`, plus baked into the `Button` cva so the shadcn variant gets it explicitly. Disabled controls get `cursor: not-allowed`.

### Refactor — Code Quality + Hierarchy

- `**shared/` restructure**: 7 files moved into `shared/extraction/` (chunking, file_extract, structured_document, text_render, technical_facts) and `shared/web/` (ingest, url_identity). Root keeps only core shared primitives.
- `**services/` restructure**: 43 flat files reorganized into 8 domain folders — `graph/`, `query/`, `content/`, `ingest/`, `mcp/`, `ops/`, `iam/`, `knowledge/`. ~80 import sites updated.
- `**query/execution.rs` split started**: 7909-line megafile reduced to 6346 in `mod.rs` plus 5 extracted submodules — `embed`, `hyde_crag`, `technical_literals`, `verification`, `port_answer` (-20%, 1631 lines moved out).
- *Dead `legacy`_ bootstrap flags removed**: `legacy_ui_bootstrap_enabled`, `legacy_bootstrap_token_endpoint_enabled`, `allow_legacy_startup_side_effects` deleted from config and 5 integration tests. `legacy_ui_bootstrap_admin` renamed to `ui_bootstrap_admin`.
- **Silent error swallowing fixed**: 14 `let _ = state...` audit/append sites converted to explicit `if let Err(e) = ... { warn!(stage=..., error=%e, ...); }` with structured logging. 31 cosmetic `let _ = HashSet::insert()` cleaned up.
- **Frontend `any` elimination**: 52 `any` removed from `pages/{Documents,Graph,Assistant,Admin}.tsx`, all `apiFetch<any>` calls in `api/*.ts` typed against `Raw`* interfaces, `ApiError.body` typed as `ApiErrorBody`. Added 11 new typed interfaces.
- **Observability**: HyDE/CRAG/multimodal extraction stages emit structured `stage=` tracing per the constitution severity convention.
- **Backend test debt cleanup**: split `content_lifecycle.rs` into a reusable `tests/support/content_lifecycle_support.rs` fixture plus a dedicated lineage test file, bringing the main lifecycle test back under the 1000-line cap; removed stale unused worker imports in web-ingest integration tests.

### Deploy

- Added the primary Helm chart for `web`, `api`, `worker`, and one `startup` job.
- Added `docker-compose-s4.yml` for the bundled stack with [s4core](https://github.com/s4core/s4core) and S3 storage.
- Kept `docker-compose.yml` as the classic bundled stack with filesystem storage.

### Changed

- Split runtime into `api`, `worker`, and `startup`.
- Moved migrations and bootstrap out of serving pods into the startup authority.
- Added the standard storage contract: `filesystem` or S3-compatible object storage.
- Added real source links for documents and grounded answers.
- Fixed replace/delete cleanup so query-chunk references from superseded or deleted revisions are removed instead of leaving stale graph/query state behind.
- Fixed deleted document detail responses to stop exposing stale readiness, prepared counts, and source download links after terminal deletion.
- Made `/v1/ready` report actual deployment, dependency, storage, and topology state.
- Fixed first-run OpenAI bootstrap validation for the current chat-completions API.
- Fixed Swagger/OpenAPI rendering by removing a duplicate YAML key and adding a duplicate-key test.
- Split the documents page into smaller modules and added a staged 1000-line file limit in pre-commit.

### Validation

- `docker compose config` for filesystem and `s4core` profiles — pass
- `helm lint` and `helm template` — pass
- live upload, query, source-download, and Swagger/OpenAPI checks on the Minikube Helm release — pass

## 0.1.2 — 2026-04-08

### Highlights

- **Sigma.js WebGL graph renderer**: replaced Canvas2D with Sigma.js for rendering 11K+ nodes and 54K+ edges via WebGL. 7 layout algorithms (cloud, circle, rings, lanes, clusters, islands, spiral), node dragging, connected-edge overlay, pointer cursor on hover.
- **Entity sub-type extraction**: LLM pipeline now extracts freeform `sub_type` for entities (e.g., person→engineer, artifact→framework). Flows through ArangoDB storage, API, and frontend legend.
- **Vertical graph legend**: left-side collapsible legend with clickable types and sub-types, counts, show-all/invert/hide controls.
- **Documents page tabs**: split into Documents and Web Ingest tabs with independent views, filter bar with status icons and counts, total cost inline.
- **Full dependency upgrade**: React 18→19, TypeScript 5→6, Vite 5→8, Tailwind CSS 3→4, Zod 3→4, ESLint 9→10, plus 50+ other packages updated.
- **Dashboard cleanup**: removed duplicated status layers, consolidated the main library overview, and made dashboard tiles actionable with direct deep-links into filtered documents and graph views.
- **Truthful operational metrics**: fixed dashboard totals and graph counters so document counts, failed counts, nodes, and edges reflect the full active library instead of truncated recent slices.
- **Admin operations clarity**: replaced raw `degraded` signaling with explicit operator guidance, recommended next actions, and direct navigation to failed documents or graph troubleshooting paths.
- **Audit usability upgrade**: added server-backed audit pagination, result/surface filters, and free-text search in the admin panel.

### Graph

- **Sigma.js WebGL renderer**: replaces Canvas2D. Handles 11K nodes / 54K edges at interactive frame rates via GPU-accelerated rendering.
- **7 layout algorithms**: cloud (force-directed jitter), circle (scaled), rings (concentric by type), lanes (horizontal rows by type), clusters (Vogel-disc per type), islands (BFS connected components), spiral (golden-angle, degree-sorted).
- **Connected-edge overlay**: selected node's edges render on a separate Canvas2D overlay on top of all other edges, with curved arrows and blue highlight.
- **Edge z-index**: `zIndex: 2` for connected edges in Sigma's edge reducer ensures visual priority.
- **Node dragging**: `downNode` + `mousemovebody` + `mouseup` events with camera lock during drag.
- **Pointer cursor**: cursor changes to pointer on node hover via `enterNode`/`leaveNode` events.
- **Vertical legend**: collapsible left-side panel with type counts, clickable sub-types, show-all/invert/hide-legend buttons.
- **Layout toolbar**: monochrome icon buttons (⬡○◎≡⬢◇✺) with active state highlight using `bg-primary`.

### Pipeline

- **Entity sub-type extraction**: added `sub_type: Option<String>` to `GraphEntityCandidate`. LLM prompt updated with sub_type in schema and few-shot examples (framework, database, microservice, http_status_code, etc.).
- **Sub-type storage**: `candidate_sub_type` / `entity_sub_type` fields added to ArangoDB entity documents. Schema-less — no migration needed, `#[serde(default)]` handles old documents.
- **Sub-type API**: `entitySubType` returned in entity list/detail HTTP responses via `metadata_json`.

### Frontend

- **Documents page tabs**: split into Documents tab (table + filters + pagination + upload) and Web Ingest tab (run list + add link). Independent views, shared inspector panel.
- **Filter bar redesign**: status filter buttons now include icons (⏱ processing, ✓ ready, ⚠ sparse, ✕ failed) and count badges. Total cost moved inline into the filter bar.
- **Web ingest status fix**: `COMPLETED_PARTIAL` now treated as terminal state, no longer triggers "in progress" banner.
- **Graph sub-type filtering**: `hiddenSubTypes` state allows hiding individual sub-types from the graph. Sub-type badges are clickable in the legend.
- **Node inspector**: shows sub-type below the primary type label with translated label.
- **i18n**: added 8 new keys (showLegend, hideLegend, showAll, invert, resetFilter, subType, tabs.documents, tabs.webIngest) in both en.json and ru.json.

### Dependencies

- **React** 18.3 → 19.2, **TypeScript** 5.8 → 6.0, **Vite** 5.4 → 8.0
- **Tailwind CSS** 3.4 → 4.2 (migrated from JS config to CSS-based `@theme`, PostCSS removed, `@tailwindcss/vite` plugin)
- **Zod** 3.25 → 4.3, **ESLint** 9.32 → 10.2, **react-router-dom** 6.30 → 7.14
- **recharts** 2.15 → 3.8, **sonner** 1.7 → 2.0, **lucide-react** 0.462 → 1.7
- **Rust**: tokio 1.51.0→1.51.1, zip 8.5.0→8.5.1 (minor patches)
- 50+ other npm packages updated to latest versions

### Backend

- Batch document endpoints, audit pagination, URL-backed document pagination.
- Dashboard totals and graph counters fixed to reflect full active library.

## 0.1.1 — 2026-04-07

### Highlights

- **Universal entity taxonomy**: 10 domain-agnostic entity types (`person`, `organization`, `location`, `event`, `artifact`, `natural`, `process`, `concept`, `attribute`, `entity`) designed to work across any domain — programming, medicine, law, finance, biology, engineering, and beyond. Domain-specific granularity via `sub_type` metadata.
- **Pipeline intelligence upgrade**: graph extraction v6 with few-shot examples, relation catalog expanded from 49 to 88 standard relation types, semantic chunking (2800 chars, 10% overlap, heading-aware), boilerplate detection, quality scoring, entity resolution, document summaries, and post-extraction type refinement.
- **Hybrid search**: BM25 + vector cosine similarity merged via Reciprocal Rank Fusion (RRF) with field-weighted scoring (heading boost 1.5x, quality score multiplier).
- **21 MCP tools**: added `ask` (grounded Q&A), `search_entities`, `get_graph_topology`, `list_relations`, `list_documents`, `delete_document`. Token-efficient responses with `includeReferences=false` by default.
- **Canvas2D graph renderer**: replaced SVG with Canvas2D for rendering 10K+ nodes and 50K+ edges. Zero React re-renders during pan/zoom. Viewport culling, level-of-detail labels, adaptive edge budget.
- **Bulk document actions**: batch delete, cancel processing, and reprocess via UI selection mode and REST endpoints.

### Pipeline

- Graph extraction prompt v6 with comprehensive entity type guidance, coreference resolution rules, and 2 few-shot examples.
- Relation catalog expanded from 49 to 88 standard relation types: `calls`, `implements`, `extends`, `authenticates`, `contains`, `returns`, `validates`, `transforms`, `deployed_on`, `inherits_from`, `imports`, and 27 more.
- Post-extraction type refinement: regex-based pass auto-reclassifies env vars, URL paths, HTTP methods, file paths, and status codes from generic `entity` to specific types.
- Post-extraction mentions reduction: summary-based heuristic upgrades `mentions` to `uses`, `depends_on`, `contains`, `defines`, `provides`, `authenticates`, and other specific types when the summary text implies a concrete relationship.
- Semantic chunking: increased default from 1,600 to 2,800 chars, added 10% overlap between adjacent chunks, heading-aware splitting (headings always start new chunks).
- Boilerplate detection: nav links, breadcrumbs, cookie banners, copyright notices filtered from chunking.
- Chunk quality scoring: 0.0-1.0 score based on text length, word diversity, heading/code/table presence.
- SimHash near-duplicate detection for chunk deduplication.
- Document-level summary generation from structured blocks during ingestion.
- Entity resolution service: deterministic merge by exact alias, normalized prefix, and acronym detection.
- Graph extraction parallelism bumped from max 4 to max 8 concurrent chunks.
- Query expansion: 24 synonym groups for automatic search term broadening.
- Extended technical fact extraction: 8 new fact kinds (environment variables, version numbers, database names, configuration keys, error codes, rate limits, dependency declarations, code identifiers).
- Entity summary upsert changed from last-write-wins to longest-wins.
- Verification feedback loop: warnings from answer verification now flow into the response instead of being silently discarded.
- Error handling: replaced silent `let _ =` patterns in ingestion worker with proper error logging for `promote_document_head`, entity resolution, and document summary generation.

### MCP

- Added `ask` tool: grounded Q&A in a single call (replaces 3-call workflow of create_session + create_turn + get_execution).
- Added `list_documents` tool: browse library contents with optional status filter.
- Added `delete_document` tool: complete CRUD lifecycle for agents.
- Added `search_entities` tool: search knowledge graph entities by label.
- Added `get_graph_topology` tool: graph structure with truncation limits (default 200 entities / 500 relations).
- Added `list_relations` tool: explore graph relationships ordered by support count.
- `search_documents` and `read_document` responses now default to `includeReferences=false`, reducing token usage by ~80%.
- Fixed `list_relations` description (was misleading about query parameter).
- Updated MCP.md and MCP-RU.md with all 21 tools organized by category.

### Frontend

- **Graph page**: Canvas2D renderer replaces SVG. Handles 10K+ nodes at interactive frame rates. Edge labels for selected node connections. 10 distinct entity type colors with updated legend and type filter dropdown. Level-of-detail label rendering. Adaptive edge budget scaling with zoom.
- **Graph page**: all pan/zoom/hover interactions use refs instead of React state — zero re-renders during interaction.
- **Documents page**: selection mode with checkboxes, select-all, and sticky bulk action toolbar (delete, cancel processing, retry). i18n translations for en/ru.
- **Assistant page**: Markdown renderer for answer messages (code blocks, tables, lists). Removed cosmetic attachment UI.
- **Dashboard page**: renders API metrics; web ingest activity strip wired.
- GraphNodeType contract: 10 universal entity types across contracts crate, API mapping, and frontend.

### Backend

- Batch document endpoints: `POST /content/documents/batch-delete`, `batch-cancel`, `batch-reprocess` (max 100 per call).
- Ingestion worker: skip-deleted document guard, skip-cancelled job guard.
- ArangoDB relation type bug fix: `predicate` field now correctly maps to `relationType` in REST API responses.
- BM25 field-weighted scoring: heading_trail matches boosted 1.5x, section_path 1.3x, quality_score multiplier.
- quality_score persisted to ArangoDB `KnowledgeChunkRow`.
- Async reranking infrastructure (rerank_structured_query made async).
- RRF hybrid search fusion in `merge_chunks`.

### Benchmarks

- Golden benchmark corpus: 72 files across 5 semantic directories (wikipedia, docs, code, documents, fixtures).
- 10 benchmark suites with 102 test cases covering: Wikipedia recall, cross-document QA, noisy layouts, graph traversal, multiformat upload, programming docs, infrastructure docs, protocols, code comprehension, and PDF/DOCX/PPTX extraction.
- Code-only dataset: 8 large real-world code files (Go, TypeScript, Python, Rust, Kubernetes operator, React, Terraform, Docker Compose) with 20 comprehension questions.
- Multiformat dataset: 5 generated documents (2 DOCX, 1 PPTX, 2 PDF) with 12 extraction questions.
- Compare benchmarks tool for side-by-side result analysis.
- `make benchmark-golden` target runs all 5 golden suites.

### Documentation

- README.md and README-RU.md: updated pipeline diagram, features list, roadmap (0.1.1 done items), MCP tool table.
- MCP.md and MCP-RU.md: all 21 tools documented with descriptions and required parameters.
- Benchmark README: restructured corpus description with directory layout table.
- `.env.example`: added Redis URL, rerank flags, web crawl defaults, fixed model names.

### Schema

- Migration `0002_document_summaries.sql`: added `content_document_head.document_summary` and `catalog_library.ai_summary` columns.

### Gates (all green)

- `cargo fmt --all` — pass
- `cargo clippy -p ironrag-backend --all-targets -- -D warnings` — pass
- `cargo test -p ironrag-backend` — 381 tests, 0 failures
- `npx tsc --noEmit` (strict mode) — pass
- `npx vite build` — pass

## 0.1.0 — 2026-04-06

### Highlights

- **Full frontend rewrite**: replaced Leptos/Thaw Rust UI with React + shadcn/ui + Tailwind stack. Production-grade UI with i18n (en/ru), interactive knowledge graph, document management, AI assistant with evidence panel, and admin panel.
- **Backend vocabulary consolidation**: eliminated all legacy `project_id` vocabulary, collapsed dual-head revision semantics, removed 63 clippy dead-code warnings, achieved green `make backend-lint` for the first time.
- **Billing pipeline**: added cost tracking for graph extraction and query execution. Per-document and per-library cost summaries displayed in UI.
- **Entity references in queries**: answers now include matched knowledge graph entities with labels and types.
- **Source attribution**: segment references include document title and source URI, enabling users to trace answers back to specific documents and web pages.
- **Docker-first deployment**: separate frontend and backend images, nginx reverse proxy, one-command `docker compose up -d`.

### Architecture

- Replaced `apps/web` Leptos crate with React SPA (Vite + TypeScript strict mode).
- Removed `vendor/thaw-0.5.0-beta` patched UI library and all Leptos/leptos_axum dependencies.
- Frontend served as separate nginx container; API proxied through nginx reverse proxy.
- Added `apps/web/Dockerfile` for multi-stage Node build → nginx static serve.
- System admin now bypasses workspace/library discovery authorization.
- Shell bootstrap loads libraries from all visible workspaces.

### Backend

- Renamed all `project_id` to `library_id` across 17 repository functions, 5 service files, all callers.
- Replaced `DocumentRow`/`ChunkRow` shadow vocabulary: `current_revision_id` → `active_revision_id`, `active_status` → `document_state`, `active_mutation_kind/status` → `mutation_kind/status`.
- Added `ContentDocumentHead::effective_revision_id()` and `latest_revision_id()` methods replacing ad-hoc fallback patterns.
- Removed dead code: entire `document_accounting.rs`, 30+ unused functions from `ingestion_worker.rs`, `runtime_ingestion.rs`, `graph_extract.rs`.
- Added billing capture for graph extraction embeddings and query execution embeddings.
- Rewrote billing queries to aggregate costs from all execution kinds (graph_extraction + ingest_attempt).
- Added PostgreSQL-based entity search fallback for query pipeline when ArangoDB entities are empty.
- Added `documentTitle` and `sourceUri` to `PreparedSegmentReference` in query responses.

### Frontend

- 8 pages: Dashboard, Documents, Graph, AI Assistant, Admin, Swagger (live OpenAPI), Login, 404.
- API layer with typed clients: `auth`, `documents`, `dashboard`, `query`, `knowledge`, `admin`, `billing`.
- i18n with `react-i18next`: full English and Russian translations across all pages.
- Interactive knowledge graph: force-directed layout, 8 layout modes, draggable nodes, curved edges, adjacency-based selection highlighting, adaptive labels, document-entity connections.
- Document inspector: file info, web source with clickable URL, preparation summary, actions (upload, append, replace, download, delete, re-ingest).
- Web ingest: run history with expandable page lists, re-ingest with parameter editing.
- AI Assistant: session management, evidence panel with document titles and source URIs, entity/fact/relation references, verification state.
- Admin: AI provider/credential/preset management, library binding configuration, token management, MCP setup with dynamic origin URLs, audit log, pricing management.
- Session persistence via localStorage (workspace/library selection survives page refresh).
- Toast notifications for all operations with actual API error messages.
- TypeScript strict mode enabled with zero errors.

### Deployment

- `docker-compose-local.yml`: 7 services (postgres, redis, arangodb, backend, worker, frontend, nginx).
- `docker-compose.yml`: production config with pre-built images.
- `install.sh`: one-command installation from GitHub releases.
- Nginx reverse proxy: `/v1/` → backend, `/` → frontend, `/mcp` redirect, SPA fallback.

### Gates (all green)

- `cargo fmt --all` — pass
- `cargo check -p ironrag-backend --tests` — pass
- `cargo clippy -p ironrag-backend --all-targets -- -D warnings` — pass
- `cargo test --workspace` — pass
- `make check` / `make check-strict` / `make enterprise-validate` — pass
- Frontend `npx tsc --noEmit` (strict mode) — pass
- Frontend `npx vite build` — pass

## 0.0.4 - 2026-04-04

### Highlights

- Added GitHub Release automation around the published Docker Hub release channel used by the Rust-only stack.
- Split Compose surfaces into one default prebuilt deployment path, one manual local-build path, and one internal GitLab deployment path.
- Added one-click `install.sh` installation without cloning the repository, with release-tag or `latest` resolution from GitHub.
- Cut query and extraction orchestration over to one typed agent runtime with runtime-backed lifecycle, stage trace, and policy summaries across REST, MCP, and the assistant UI.

### Platform

- Switched the default `[docker-compose.yml](./docker-compose.yml)` to published Docker Hub images so release installs no longer depend on local image builds.
- Historical note: the legacy deployment surface `docker-compose-gitlab.yml` was removed (deleted); active compose files are now `[docker-compose.yml](./docker-compose.yml)` and `[docker-compose-local.yml](./docker-compose-local.yml)`.
- Updated root env documentation and release docs around the primary `docker-compose.yml` and `docker-compose-local.yml` split.
- Tracked the repo-root `[Cargo.lock](./Cargo.lock)` in release artifacts so clean GitHub checkouts can build the API Docker image without missing-file failures.
- Added the `apps/api/src/agent_runtime/` subsystem with typed task contracts, staged execution, explicit policy decisions, and owner-linked runtime persistence.
- Replaced full-library graph rebuilds during ingestion with targeted graph reconciliation.
- Reworked knowledge generation and operations read models around primary revision readiness.

### Product

- Assistant execution surfaces now render runtime lifecycle, stage summaries, policy interventions, and explicit policy-rejected or policy-terminated outcomes instead of generic failures.
- Documents, Dashboard, Graph, and auth surfaces were rebalanced around the primary workbench layout.

### Reliability And Performance

- Fixed long-running document ingestion stalls by eliminating graph-persistence races in primary merge/write paths.
- Restored truthful operator state reporting so idle libraries now surface `healthy` plus `graph_ready`.

## 0.0.3 - 2026-04-03

### Highlights

- Added the full structured preparation pipeline: semantic sections, structure-aware chunks, typed technical facts, grounded graph evidence, and answer verification.
- Added standard URL ingestion for `single_page` and `recursive_crawl`.
- Completed the first-run bootstrap flow with primary provider/model bindings.

## 0.0.2 - 2026-03-31

### Highlights

- Added the dedicated Assistant surface with preserved chat history, attachments, grounded context, and responsive layouts.
- Added the Admin `MCP` section with setup snippets for Codex, Cursor, Claude Code, VS Code, and generic HTTP clients.
- Added the grounded-query benchmark harness.
- Added the standard web-ingest run model.

## 0.0.1

- Initial release.
