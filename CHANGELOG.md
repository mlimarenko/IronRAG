# Changelog

## 0.6.2

### Added

- Browser telemetry consent is stored per account and can be changed in the user
  menu. Account preferences override browser choices; loading or unavailable
  preferences keep optional telemetry off.
- MCP diagnostics tools for connector ingest history (`list_ingest_jobs`,
  `get_ingest_job`, `retry_ingest_job`, `cancel_ingest_job`) and for IAM
  (`list_iam_tokens`, `revoke_iam_token`, `get_principal_access`). Ingest reads
  follow the connector read policy, mutations the write-only connector policy;
  the IAM tools require a system administrator.
- The bundled connector catalog (`bookstack`, `confluence`, `gitrepos`,
  `youtrack`) has one code-side owner instead of duplicated image and run-mode
  lists.
- MCP connector catalog and IAM administration tools (`list_connector_sources`,
  `mint_iam_token`, `update_iam_token`), plus MCP resources `ironrag://manifest`
  and `ironrag://typescript` served by `resources/list`, `resources/read` and
  `resources/templates/list`.
- Ingest job and attempt cursors plus the accepted queue-state vocabulary are
  shared by the REST lists and the MCP ingest tools.
- One trace per request. The REST API returns the OpenTelemetry trace id in an
  `x-trace-id` response header, stdout log lines carry `trace_id` and `span_id`,
  and REST audit rows store the same trace id. Retrieval phases, PostgreSQL
  searches, LLM chat and embeddings calls, Xberg parsing, ingest enqueue and
  each ingest stage now emit spans, so a slow or failed answer can be followed
  from the request through the worker in the trace backend, the logs and the
  audit log.

### Fixed

- Consent writes reject a changed account session before modifying preferences.
  Revocation disables local telemetry immediately, including while a save is
  pending; sign-out remains immediate and silent.
- The consent banner occupies its own layout row instead of covering navigation
  or page actions on desktop and narrow screens.
- Malformed AI and queue responses are rejected before entering the query cache.
  Contract errors stay visible instead of being reset by automatic retries or
  queue polling; manual retry remains available.
- Updated HTML parsing and development-tool dependencies, and pinned official
  SQLx support for patched SQLite bindings. The backend runtime uses the official
  SQLite package containing the three reported HIGH-severity fixes.
- Log redaction compiles without unsupported regex look-around: Bearer and
  Basic credentials are still removed while ordinary prose after the scheme
  word stays readable.
- Split code blocks keep per-segment source spans, so citations and table
  anchors remain unambiguous when the same text repeats.
- GitHub Release descriptions show the selected changelog section and preserve
  editorial corrections when publication is retried.

### Upgrade

- Existing deployments must complete the documented additive schema rollout
  before starting the new API and worker. Account preferences add a guarded
  schema transition; keep the existing database and credential master key.
  See [the schema rollout instructions](./docs/en/CLI.md#schema-rollout).

### Known limitations

- Sign-out does not wait for a pending consent save. If the server could not
  persist a refusal, the account's previous preference can remain on other
  devices; optional telemetry in the signing-out tab stops immediately.
- Debian image scans still report the three patched SQLite HIGH findings because
  the trixie advisory mapping does not recognize the pinned sid package's fixes.
  Other Debian MEDIUM/LOW findings have no supported trixie fix at release time;
  their reachability has not been established. Findings remain visible without
  new suppressions.
- Historical ingestion failures are not automatically repaired or replayed.

## 0.6.1

### Fixed

- Graph publication and currentness handling, with bounded maintenance rebuilds.
- Interface updates and stabilization fixes.

### Known limitations

- Large graphs can still take minutes to load.
- N100/p99, full semantic verification and end-to-end UI, MCP and live-connector
  acceptance are not certified by this release.
- Historical ingestion failures are not automatically repaired or replayed.
  The original byte-identical orphan recovery scenario and upstream dependency
  exceptions remain unresolved.

## 0.6.0 — 2026-10-20

A breaking release that removes backward-compatibility layers, one-shot
migration code, and dead surfaces accumulated across the 0.5.x line. The 0.5.x
migration chain is replaced by a single schema baseline, so there is no
in-place database upgrade from any earlier release: an existing deployment's
database is recreated rather than migrated. Credentials carried forward from a
0.5.x deployment must be rewrapped there first — see **Upgrade** below.

### Breaking

- **Credential encryption is now mandatory.** `IRONRAG_CREDENTIAL_MASTER_KEY`
  is required at startup and every provider credential and webhook header is
  stored as an encrypted `ironrag:enc:` envelope. The plaintext read path, the
  legacy v1/v2 envelope formats, and the `IRONRAG_CREDENTIAL_ENCRYPTION_WRITE_ENABLED`
  staged-rollout flag are removed. Deployments still holding plaintext or
  v1/v2 credentials must run `migrate credential-secrets --apply` on 0.5.x
  before upgrading; afterwards those rows are unreadable.
- **The schema is one baseline migration.** The eighteen 0.5.x migration files
  are replaced by a single `0001_init.sql`, and a second migration file
  is rejected by the migration linter. Because the migration runner verifies
  the checksum of what it already applied, a database created by any earlier
  release cannot boot 0.6.0: there is no in-place schema upgrade, and an
  existing deployment must be recreated. `make migration-check` compares the
  baseline against a recorded checksum so this decision cannot be made by
  accident; see [CLI.md](./docs/en/CLI.md#schema-rollout).
- **Snapshot archives are format v4, and older archives are not importable.**
  The archive schema version is v4 alongside the single schema
  baseline, and the restore path accepts that one version: an archive exported
  by any 0.5.x release is rejected before a single row is written. There is no
  converter for earlier public archives — a 0.5.x library moves forward by re-ingesting its sources into a
  0.6.0 stack, the same way the database is recreated rather than migrated.
- **Pricing is part of the model catalog row.** A model can no longer exist
  without a price. The separate price entity — effective dating, variants,
  workspace overrides, its admin endpoints and its repository — is gone;
  `ai_model_catalog` carries the currency, four per-million rates and an
  optional long-context tier under check constraints, and a charge is priced
  from the model row inside the usage-capture transaction. Library snapshots no
  longer carry a price table, and the admin model form requires pricing.

### Added

- **Colour accents and consistent settings navigation.** Light and dark themes
  keep neutral surfaces with blue actions and semantic dashboard indicators.
  Documents and AI settings share keyboard-accessible tabs. The model picker
  starts with compatible models, keeps the selection visible during search, and
  omits comparison columns when no model supplies that metadata. Binding
  parameters expand separately from the account and model selection.

- **Compact web interface and white-label identity.** The shell and screens use
  neutral surfaces, subtle borders and a compact type scale. Deployment branding
  supplies the display name, icon and favicon without changing the frontend
  image; document, graph, assistant and administration workflows remain available.

- **The connectors live here now.** The adapter framework and the BookStack,
  Confluence, Git-repository and YouTrack connectors moved out of five separate
  repositories into `connectors/`, as one Python distribution with one lock
  file, one Ruff/mypy/pytest configuration and one test run. They ship on this
  version: the release that publishes the backend and the interface publishes
  their images too, from the same tag.

  What this removes is a defect generator, not duplication. Each connector
  pinned the framework by tag in its own CI, so a framework fix reached a
  connector only when someone bumped the pin — three past connector releases
  existed for no other reason, and one shipped an image without the fix its
  notes claimed. Three of the four also ran no tests in CI at all. Their ~230
  tests now run in every merge request, against the framework they will
  actually ship with.

  For an operator the image tag is now the IronRAG release tag, `v`-prefix
  included (`IRONRAG_VERSION`, required rather than defaulted, because a stale
  default quietly served a two-minor-old image), and the environment file the
  compose templates read is `.env` rather than `.env.local` — compose
  interpolates the tag from `.env`, so the old name left the required variable
  unset. See **Upgrade** for the state-volume ownership step. Each
  connector keeps its own deployment template under `connectors/deploy/<source>/`
  and its own environment prefix; the containers additionally run as a
  non-root user with `no-new-privileges` and all capabilities dropped, which
  three of the four did not do before.

- **The web interface is a new application.** Every screen — shell, dashboard,
  documents, graph, assistant and the admin surfaces — is rewritten on the
  redesigned UI, including a webhooks administration screen the previous UI
  never had. The generated SDK, the streaming assistant turn with durable
  recovery, NDJSON graph streaming, cookie sessions, the capability matrix and
  the deploy contract are carried over unchanged; the browser-side mock data
  layer is gone and no feature imports it.
- **Graph rendering is an own WebGL2 pipeline.** The third-party graph canvas
  and its layout library are replaced by a renderer and a worker-side layout
  written for this product. A large library paints a deterministic seed
  immediately and refines in place instead of leaving the canvas blank while
  layout runs, and switching layouts no longer blocks the interface.
- **Cost reads report their coverage.** With calls that carry no price, a cost
  total is no longer indistinguishable from a genuinely free scope: every cost
  read carries the number of unpriced provider calls and a derived coverage
  status (complete, partial or unknown), and the interface renders that instead
  of a confident zero. The missing-price warning is emitted once per provider
  call rather than once per usage row.
- **Per-endpoint provider rate limiting and retry.** Every LLM, streaming,
  tool, vision, and embedding request now passes a token bucket and a
  concurrency semaphore keyed on the resolved provider endpoint, and a
  `429`/`503` is retried with bounded exponential backoff plus jitter that
  honours `Retry-After` in both delta-seconds and HTTP-date form. Configured
  through `IRONRAG_PROVIDER_RATE_LIMIT_*` and `IRONRAG_PROVIDER_RETRY_*`; the
  budget is per process, so size it against the number of running replicas.
- **Query-turn admission and index-accelerated matching-terms retrieval.**
  Outermost grounded/assistant turns take a bounded permit
  (`IRONRAG_QUERY_TURN_MAX_CONCURRENT`, queue, 15s wait) and excess bursts
  fail closed with retryable `503 query_admission_rejected`. Matching-terms
  SQL keeps the historical `strpos` rank order and prefilters with
  `lower(column) LIKE $pat` against `pg_trgm` expression indexes. Bind
  already-escaped `'%term%'` patterns as `$5` and drive a `UNION` of
  per-column candidate joins from those few patterns so each `gin_trgm`
  index is usable. An `OR` of three `LIKE`s plus a revision join makes
  Postgres ignore the indexes and time out. Do not restore a `simple`
  tsquery prefilter. `IRONRAG_DATABASE_MAX_CONNECTIONS` is the real
  process budget (the old 16-connection cap is gone). Live databases receive
  the new indexes through `scripts/ops/apply-knowledge-chunk-lower-trgm-indexes.sql`
  plus a guarded `_sqlx_migrations.checksum` update; see
  [schema rollout](./docs/en/CLI.md#schema-rollout).
- **DeepSeek V4 is now a first-class AI provider.** The provider catalog
  includes `deepseek-v4-pro` and `deepseek-v4-flash` with their context,
  output, binding-purpose, and vendor-pricing metadata. OpenAI-compatible
  profiles now select their token-limit request field: DeepSeek uses
  `max_tokens`, while existing profiles retain `max_completion_tokens`.
- **Semantic rerank has its own `query_rerank` binding purpose.** Provider-backed
  reranking no longer resolves the `query_compile` profile: it resolves the
  seventh binding purpose, `query_rerank`, so it can be bound, budgeted and
  switched to a fast non-reasoning model without touching the typed-query
  compiler, which keeps HyDE expansion and query consolidation. An unbound
  `query_rerank` degrades the stage to the deterministic lexical fallback and
  never falls back onto `query_compile`. Fresh installs get the enum value from
  the baseline; a live database receives it through
  `ironrag-maintenance migrate query-rerank-binding-purpose --apply`, which must
  run from the IRG-173 revision — after `integration-provisioning-schema`, the
  manual IRG-112 FK-index `psql` hop **and** the
  `ops-failure-disposition-schema` hop; see
  [schema rollout](./docs/en/CLI.md#schema-rollout).

### Fixed

- Table sort/filter menus stay inside the available viewport, including narrow
  screens; the first sort action no longer appears above the screen.

- **A detached inline append and its progress task no longer leak work.**
  `POST /v1/content/documents/{id}/revisions` is documented as asynchronous,
  but the append materialized inside the request future: a client or proxy
  disconnect at its deadline killed the pipeline mid-extraction, the heartbeat
  guard dropped silently, the attempt stayed `leased`, and the stale-lease
  reaper requeued the job and redid the whole ingest. The materialization now
  runs in a detached task that survives the dropped caller (with request-span
  tracing, a panic-safe finalizer, and a health guarantee that the attempt
  never outlives it while `leased`), and the `extract_graph` progress reporter
  is aborted on every exit path instead of only on success.
- **Connector revision creation waits for the server's inline materialization.**
  `create_revision` answered only once the append finished, but rode the
  general `request_timeout_seconds` (60s) and aborted long appends before the
  reply; it now uses the dedicated `revision_create_timeout_seconds` (300s
  default), leaving other calls on the general timeout.
- **A document the library shows is no longer reported as absent.** The graph
  takes a node's label from the knowledge-document store, while the query
  preflight compared a file-like word in the question against the searchable
  index alone and by exact string equality, so a connector key such as
  `repo:docs/OPS-490.md` produced "Document `X` is not present in the active
  library" for a document that exists. Presence now resolves the library's
  documents in any state, states plainly when a document is present but not
  searchable yet, and a deterministic system statement is terminal: no
  follow-up probe, no "last completed answer" notice, no unrelated sources.
- **Graph node markers scale with the camera zoom.** Every marker drew at its
  fitted screen radius, so zooming into a 140k-node graph multiplied the
  distance between dots while the dots stayed 1–2.6 px specks. Markers now
  grow with the square root of the zoom-in factor up to an 8 px radius cap;
  hit-testing and label offsets read the same law, and the fitted view is
  unchanged.
- **Snapshots keep the current-only vector lifecycle.** A library archive now
  carries only the chunk vectors retrieval can still select, and a restore
  attests the invariant inside its own transaction: an archive that still
  holds superseded vectors (any schema version 1 archive) is refused instead
  of re-seeding the search index with vectors the superseded sweep already
  removed. The restored manifest's per-lane counts are recomputed from the
  rows that actually landed rather than trusted from the archive.
- **The Confluence attachment sweep runs against instances that reject
  `order by id asc`.** Leading with `id` made one source instance
  (`docs.artix.su`) materialize an entity it cannot convert when the walk
  expands `container`, so every attachment sweep answered `400
  "globaldescription ... is not ContentConvertible"` and the whole sync
  stalled with an incomplete census. The walk now orders by `created asc,
  id asc`: deterministic, and accepted by that instance.
- **OIDC sign-in is rate limited per origin.** `GET /v1/iam/oidc/login` charged
  a single issuer-wide Redis budget, so one anonymous client sending more than
  two requests per second locked every other user of the deployment out of
  sign-in. Admission now charges two buckets atomically: the existing
  issuer-wide one and a per-source bucket (1 start/s, burst 2) keyed by the
  same trusted client origin the password-login limiter already derives.
  A source that exhausts its own budget no longer spends the shared one.
- **The backend image rebuilds its apt layer on demand.** The docling-base
  stage's `apt-get upgrade` was frozen by the BuildKit registry cache: the
  layer was built once and reused, so Debian security updates published later
  never reached the image and the Trivy gate failed on new OS-package CVEs
  (glib, pcre2, sqlite3, libssh2, perl). `ARG APT_CACHEBUST` now marks the apt
  layer the same way `CACHEBUST` marks the builder stage; bumping it rebuilds
  the layer with current package versions.
- **MCP tool text carries the full payload.** `content[].text` of every MCP
  tool result now contains the complete result payload as JSON in addition to
  the human summary; `structuredContent` is preserved for clients that consume
  it directly. Text-only MCP clients (opencode, Claude Code, Codex) surface
  only `content` to the model and previously saw discovery tools such as
  `list_workspaces` and `list_libraries` as empty lists, because the payload
  lived solely in `structuredContent`.
- **A settled snapshot import publishes what it restored.** The import worker
  computed the rows PostgreSQL actually wrote per table, logged them once and
  dropped them, so the only surface a caller can poll settled to `ready`
  carrying nothing but a status — a restore that moved every row and one that
  silently moved none were indistinguishable. `GET
  /v1/ops/operations/{operationId}` now carries `resultJson` with
  `postgresRowsByTable` and `blobsRestored` for both the library- and
  workspace-scoped imports. The baseline gains a nullable
  `ops_async_operation.result_json jsonb`; the write is guarded on an operation
  that has not settled yet, so the result is written once and a later transition
  cannot replace it. Two schema-identity holes are closed with it: readiness now
  verifies that the objects the recorded baseline checksum claims are actually
  present, and `migrate service-identity-schema` refuses to stamp a checksum
  covering baseline changes its own DDL did not apply, stamping by compare-and-set
  from the digest it read rather than unconditionally.
- **Superseded revision vectors are removed from the hot PostgreSQL ANN plane.**
  The scheduled maintenance class is now the single-purpose
  `gc.superseded-vectors`: it discovers every physical chunk-vector shard,
  deletes revisions that are neither readable nor an active revision owned by
  an exact leased content-mutation job/latest attempt, in 10,000-row
  transactions, and atomically decrements a proven manifest lane;
  a missing or under-counted lane rolls the physical delete back. Source
  revisions, historical knowledge chunks, query references, and citations are
  retained. The sweep refuses active ingest and divergent source/knowledge
  heads, preserves null-head documents unless explicitly requested, and no
  longer hides graph/evidence cleanup behind a misleading vector-GC name.
  The scheduler preserves that cleanup under the independent
  `gc.runtime-graph` lease class, so either backlog can retry without blocking
  the other. The worker Deployment now uses `Recreate`; after all old workers
  stop, startup deletes obsolete `gc.stale-chunks` lease rows before seeding
  the two canonical classes. Non-Kubernetes upgrades must likewise stop every
  old worker before starting this release.
- **Deleting a library now takes its knowledge plane with it.** The delete
  relied on foreign-key cascades that eight tables never had, so a deleted
  library left its knowledge documents, revisions, chunks, structured blocks,
  vector rows, vector manifest and graph communities behind — permanently, and
  keyed by identities a later import of the same archive needs. The schema now
  declares the missing cascades, per-dim vector shards get theirs when the
  shard is ensured (both the search store and the restore path, so shards that
  already exist are covered too), and an integration gate reads the live schema
  to require every table carrying `library_id` to either cascade, deliberately
  null its reference, or be exempt with a written reason.

- **A snapshot import can no longer report success while dropping rows.** The
  knowledge-plane sections were inserted with `ON CONFLICT DO NOTHING`, which
  was meant as a replay guard but also swallowed collisions with rows another
  library had left behind: the restore finished `ready` on a library with no
  chunks, which then answered every query with `knowledge_not_ready` forever.
  Every section now compares the rows Postgres wrote against the rows it was
  handed and refuses the import when they differ, naming the library that owns
  the colliding identities; the workspace-identity and AI-config sections keep
  their deliberate keep-what-the-target-has behaviour. The import report counts
  rows actually written rather than rows offered.

- **Import refusals say which refusal they are.** A populated target under
  `overwrite=reject`, an archive whose identities already live in this
  deployment, an unsupported archive version and a malformed archive all
  recorded the same `snapshot_import_failed` on the operation, making a policy
  refusal indistinguishable from a crash. Each now records its own failure code
  (`snapshot_target_populated`, `snapshot_source_already_present`,
  `snapshot_unsupported_schema_version`, `snapshot_archive_invalid`), and the
  OpenAPI description no longer promises a `409` that an asynchronous import
  cannot return.

- **A failed workspace import no longer leaves half a workspace behind.** The
  bundle created each library before restoring into it, so a failure part-way
  through left the already-created libraries in place, including slug-suffixed
  duplicates nobody asked for. A failed bundle now deletes the libraries it
  created.

- **`overwrite=replace` clears the tables the archive does not carry.** Graph
  communities, graph extractions, filtered artifacts and the entity dedup cache
  are owned by the library but derived rather than exported, and the replace
  purge listed only exported tables — so a replaced library kept decisions
  taken about content it no longer held.

- **`scripts/ops/snapshot-library.sh` no longer reports success on a failed
  import.** It sent NDJSON content types to a zstd endpoint, had no way to pass
  `overwrite`, and treated the `202` admission as the result — so an import
  that failed afterwards printed "import done". It now polls the operation to a
  terminal status, exits non-zero on failure, and checks an export for the
  `EXPORT_FAILED.json` sentinel that a streamed `200` can still hide.

- **Test coverage now actually reaches SonarQube.** The frontend, Rust and
  Python coverage jobs had been green for months while the server reported
  0.0% coverage, so the project's 80% coverage gate could never pass. The
  reports were built, uploaded, and downloaded into the scan workspace, and
  then never named to the scanner: the shared CI component generates its own
  empty `project.properties` and passes `-Dproject.settings=` pointing at it,
  so the repository's `sonar-project.properties` was read by nothing. The
  JavaScript and Rust report paths now travel as component inputs, the dead
  properties file is gone, and a CI contract gate compares every coverage
  job's declared artifacts against the paths handed to the scanner so a report
  can no longer be produced and silently ignored. Measured on a throwaway
  project against the same commit's CI artifacts: coverage 0.0% to 53.9%.

- **SonarQube now analyses test code as test code.** Every test file counted
  as production, which put the duplication condition out of reach of any
  amount of deduplication: most of the duplicated lines in new code lived in
  test files, so deleting all duplication in production code still left the
  measure above the threshold. The scan now declares its main and test scope
  through component inputs — `sonar.sources` and `sonar.tests` are
  scanner-only properties that the server's settings API cannot set — and a
  gate reads the Rust sources to prove that every file classified as a test is
  compiled only under `#[cfg(test)]`, and that no whole-file test module is
  left counted as production. Measured on a throwaway project against the same
  commit's CI artifacts: duplicated lines 21732 to 6624, density 4.3% to 1.8%,
  with indexing, `ncloc` and the violation count unchanged. Coverage is
  unaffected — no test file appears in any coverage report — so this corrects
  the duplication measure, not the coverage one.

### Removed

- The `concurrency_gateway` provider wrapper. Its limiting responsibility
  moved into the provider gateway itself, so there is one request path.

- Credential-encryption backward-compatibility surface: the plaintext read
  path, the v1/v2 envelope formats and their AAD helpers, the
  `SecretStorageFormat`/`EnvelopeVersion` classifiers, the disabled/optional
  cipher state (`from_optional_base64`, `require_configured`), and the
  `credential_encryption_writes_disabled` API error. The cipher now requires a
  master key at construction and reads only v3 envelopes.
- Dead server-driven-UI contract families in `ironrag-contracts`
  (`AdminConsoleData`, the API-reference surface, the assistant
  conversation/evidence aggregates, `SurfaceDiagnostics`, the documents and
  graph workbench surfaces, `ShellScopeSelection`, and the duplicate
  login/bootstrap request and response types), together with the crate's
  now-unused `rust_decimal` dependency.
- One-shot maintenance migrations and stubs: the `vector-profile-keys` re-key
  and `chunk-temporal-bounds` backfill subcommands, the ArangoDB-era
  `orphan-libraries` audit and gc stubs, the unreachable non-PostgreSQL
  knowledge-plane branch, and the eleven never-dispatched `MaintenanceClass`
  variants. The null-head promotion twins now share one helper.
- The dead `scope:`/`question:` follow-up retrieval guard and the stale
  `literals:`/`literal anchors:` instructions in the assistant and compiler
  prompts (their producer was removed in 0.5.10).
- Frontend dead code: the divergent `'passed'` verification remap, the graph
  `'bands'` layout fallback, the hand-rolled `updateDocumentHint` fetch wrapper
  (now the generated patch operation), the multi-format graph-topology
  tolerance, unused `AssistantMessage`/`RuntimeSummary`/`PricingRule` fields,
  the non-streaming `createTurn`, and three dead barrel re-exports. The stale
  MSW `PUT` mocks are corrected to `PATCH` and the `ts-prune` gate now analyzes
  real files.
- Stale migration-era API documentation, two unreachable `OPERATION_PURPOSES`
  entries, and a duplicate chunk-vector relation predicate.
- One-shot operational scripts that had already run: the parallel-graph-edge
  and stuck-mutation heal scripts, the MCP-agent rollout precondition checker,
  the legacy ArangoDB benchmark comparator, and the removed-`executionId`
  regression probe. The duplicate `http-body-util` dev-dependency is dropped.
- Tooling/MCP compatibility surfaces: `inline://payload/` source identities and
  the legacy benchmark comparator. Agent-turn benchmarks now require the JSON
  `cases` envelope and authoritative ops-suite fields; benchmark integrity artifacts
  and the comparator accept schema version 1 only.
- Frontend dependencies the rewrite made unnecessary: the third-party graph
  rendering and layout stack, and the rich-text editor stack (the redesigned
  editor is a plain text area). The end-to-end specs, Playwright configuration
  and Makefile targets that only served removed screens are deleted with them.

### Fixed

Ingest:

- **A clean install without provider credentials no longer requeues every
  upload forever.** An absent embedding binding was reported as an untyped
  error, flattened to a string, and fell through to the retryable default, so
  the job requeued indefinitely and the interface showed no reason. Absent
  configuration cannot be fixed by retrying and is now terminal: the failure
  carries a typed class and code, the document settles as failed after one
  attempt, and the API and the interface name the binding that is missing.
  Extraction faults keep their own retry decision and unclassified faults stay
  retryable, so transient provider and infrastructure failures still self-heal.
- **A bulk import into a fresh library no longer fails a share of its
  documents.** The vector write fence pinned the library-wide source-truth
  version, so during a bulk import every subsequent upload invalidated the
  in-flight embedding writes of the other documents until their retries ran
  out — a library that already held content never reached that state, which is
  why reuse-based testing stayed green while every fresh import broke. The
  fence now rejects exactly what it exists to reject: the embedding profile
  changing mid-write, and a writing attempt losing its per-revision authority.
  Re-key and migration flows keep the full version pin.
- **Documents waiting for a provider slot are no longer failed as if they had
  broken.** A bulk import oversubscribes the ingest provider lane, and waiters
  died on a fixed acquire timeout and were terminalized. Ingest-lane
  acquisitions now wait for capacity while the query lane keeps its fast
  timeout, effective extraction parallelism is derived from the configured lane
  capacity at startup, and a residual acquire failure is classified retryable.
  The graph-extraction watchdog counted the same permit waits as idleness and
  failed the attempt into a retry that met the identical contention; it now
  treats permit acquisition and in-flight provider calls as progress and fires
  only on a genuine stall.
- **One terminally failed job can no longer stop a library from ingesting.**
  Reconciling a failed mutation terminalized the operation but left its queue
  job claimable. The next worker could not lease an attempt — the lease refuses
  to drag a terminal operation back to processing — and the reaper returned the
  job to the head of the library queue, so the same rows were claimed over and
  over. Because every leased row counts against the per-library parallel-job
  limit, a small number of them held a whole library indefinitely. The reconcile
  now retires the job it has just declared failed, and the lease retires a job
  whose linked operation is already terminal.
- **Graph re-extraction can retry, and it stays inside live libraries.** The
  re-extract sweeper's dedupe key collided with terminal ingest-job rows, so
  repair was never re-enqueued and documents stayed readable but unlinked.
  Enqueue now retires a terminal collision inside the same transaction and
  retries the insert, while queued and leased duplicates are still suppressed.
  The sweeper walks only active libraries, its lifecycle re-checked under a row
  lock at enqueue, and library deletion first archives the library and cancels
  its pending jobs — so repair can no longer resurrect work for a library that
  is on its way out, or spend provider budget on it.
- **Comments in a configuration file reach the searchable text.** A YAML or
  TOML document uploaded with operator annotations stored nothing of them:
  comments are not part of the value model the record parser feeds, so no
  chunk, block or fact carried them and a question about an annotated value
  could not be answered. Comment runs are now lexed before the value parse and
  anchored to the chunk holding the adjacent fields, and a leading comment run
  is treated as the file header and also joins the document-level prose plane.
  Comment-free documents are stored byte-identically.
- **A soft-wrapped paragraph is stored as one block.** Structured preparation
  emitted one block per physical line, so a value and the clause that scopes it
  landed in separate blocks and retrieval could return either without the
  other. A prose run now stays together.

Retrieval and answer quality:

- **A cross-document comparison is always synthesized.** Comparison questions
  spanning several documents were finalized by a deterministic builder that
  concatenated the best excerpt from each document, so the answer model was
  skipped entirely and the user received an evidence dump. Those questions now
  route to the answer stage with the same chunks as context, and the collation
  builder is deleted.
- **A deterministic answer has to prove it fits the question.** The structured
  list, focused table-row and source-inventory builders could finalize an
  answer from the wrong document under a mixed corpus, admit questions with no
  focus terms, fuse unrelated blocks into one list, or answer a plural question
  from a single arbitrary row. Every leg is now reject-only: a candidate that
  cannot show the question's focus terms in itself or its evidence declines and
  the turn escalates to synthesis, which already holds the right chunks. Value
  lookups stay ungated.
- **Mechanical slice rendering is reserved for record inventories.** An
  enumeration question could return a bare count header followed by raw
  labelled unit bodies, with each document rendered twice under two label
  variants. Units must now match the requested inventory shape, bodies are
  deduplicated by normalized identity, internal serializations render only
  their human part, and the count header is a sentence. The slice never fires
  from a fallback context and is anchored to the top-ranked retrieval document;
  everything else falls through to ordinary synthesis.
- **The answer stage no longer discards evidence retrieval already found.**
  Context assembly aborted at the first block that did not fit the
  remaining budget, and because long identity and source-profile blocks are
  ordered first, one oversized early block suppressed every later chunk —
  including short lines that answered the question directly. Assembly now skips
  a block that does not fit and keeps scanning, with no budget raised.
  Separately, the structured-list builder rendered a bounded number of items
  but evaluated its completeness contract against the already-truncated text,
  so the remainder vanished silently; it now declines and escalates.
- **Answer context follows retrieval rank.** The document a summary or
  inventory was built from was elected by candidate count rather than by final
  retrieval rank, context chunks were not aligned to the ranked bundle, and
  source-coverage augmentation was unbudgeted, so an off-ranking document could
  dominate the prompt. Election now follows the final rank, augmentation is
  bounded and planned per ranked document, and the subjects a comparison
  question names lead the context.
- **A one-item answer to a plural question is no longer reported as verified.**
  Grounding is not completeness. When the evidence holds several distinct
  values of a technical-fact kind the answer touches and the answer mentions
  fewer than two, the verification is demoted to partially supported with an
  explicit coverage warning.
- **A document that merely shares words with the question no longer claims to
  be the document the question is about.** The lexical lane emits a
  document-identity signal, which is a contract read downstream rather than a
  weight, and derived probe text could earn it by matching a file name. Lexical
  search now carries the origin of the text it is given, and only a user's own
  query can earn identity terms; every derived caller was checked against what
  it does with the score.
- **A declaration the question never names is reachable.** For an enumeration
  or description question scoped to a single document, every selection path
  keyed on overlap between query tokens and chunk text, so a symbol the
  question does not mention could not be retrieved by construction. A bounded
  declaration inventory of the dominant document — top-level signatures without
  bodies, in document order, deduplicated and capped well below the context
  budget — now competes inside the context budget, harvested from that
  document's own chunk rows rather than the retrieved subset. Outside the
  trigger, selection is byte-identical.
- **A question naming an exact identifier is no longer answered from a document
  that never declares it.** Ranking weighed topical density, so a prose
  overview that discusses a subject in general could beat the one file that
  defines the identifier the question names. A rare exact identifier match now
  earns a bounded relevance uplift, and it may take the lead over a
  well-separated rival only when the candidate actually declares the
  identifier — recognised structurally in both indented and flattened stored
  text — and the current leader declares none. An identifier that appears only
  as a configuration value does not qualify.
- **A compound literal survives synthesis.** An answer that was factually
  correct could still be wrong in detail because synthesis decomposed a
  compound literal from the evidence into its bare value. The literal-fidelity
  revision pass, which already repaired the opposite direction, now restores
  the compound form as a byte-for-byte copy from the evidence when the answer
  carries only the bare value, including comment-annotated multi-word labels.
  For a single-document question the restoration scans only the subject
  document's evidence, so a foreign document's literals cannot piggyback on a
  bare-value collision.
- **A failed query expansion degrades instead of failing the turn.** The
  hypothetical-passage expansion had an output cap that left reasoning models
  no visible output at all — the whole budget went to hidden reasoning tokens —
  so the passage came back empty and the error failed the entire turn with an
  embedding error. The failure now degrades to question-only vector retrieval
  with a warning, and the budget and timeout account for reasoning tokens.
  Terminal query failures now also log their cause and keep the full context
  chain; every site used to drop the message, which is why this class of
  failure was undiagnosable from the logs.
- **Raw tool output is never served as a grounded answer.** When the structured
  answer body was missing or malformed, the repair path fell back to the
  rendered tool output and served long evidence dumps as user-visible answers.
  The structured body is now the only valid answer text, and without it the
  loop degrades through its honest cannot-answer path. Error payloads are also
  no longer parsed as completion envelopes, which is what produced a pair of
  warnings on every failed call.

The assistant:

- **The assistant answers instead of returning an internal error while a
  library is catching up or has no graph.** A transient content-projection
  state was recognised on the grounded-answer surface but not on the agent-loop
  surface, which wrapped it opaquely and reported a generic loop failure; and a
  library whose graph was never published — empty, still ingesting, or with
  extraction disabled — failed the whole turn. The transient state is now
  classified as retryable end to end, including where the loop reconstructs a
  terminal error from a stop reason, and a missing graph snapshot serves an
  empty projection while retrieval continues on the vector and lexical lanes.
- **A long assistant session no longer dies on its first iteration.** The
  conversation history offered to the model was longer than the tool schema
  accepted, so model-generated calls were rejected as invalid. The two limits
  now share one source of truth, a malformed call is fed back as a correctable
  tool outcome within the existing iteration budget, and failed or degraded
  prior turns are filtered out before the prompt is built. Separately, a
  retired wire value surviving in stored turn metadata failed a new turn in an
  existing session instantly; history loading now tolerates unknown values for
  every wire-persisted enum it reads.
- **An exhausted repair returns a marked partial answer.** A completed but
  non-terminal grounded answer is retained as an exhausted-repair fallback and
  returned as an explicitly partial response, instead of the turn failing.

The web interface:

- **The interface acts on typed API problems instead of one generic failure.**
  The shared API boundary rebuilt every error into a bare message, dropping the
  problem type, the request id and every extension member, so every backend
  condition looked identical and offered a retry that often could not help. The
  client now preserves the whole problem document. A duplicate upload reopens
  the duplicate dialog from the server's conflict response and its document id
  rather than a client-side filename guess; the assistant distinguishes a
  library with no usable content, a calm retry on a transient failure, and a
  typed title with no futile retry otherwise; and operational diagnostics no
  longer appear in the chat transcript.
- **The dashboard states conditions instead of implying progress.** Stale
  vector and relation counters are raised by pending work, so a queue that had
  not moved for days read as a rebuild in progress; a warning now fires when
  claimable work has no running attempt. A failed rebuild is shown only while
  its owning job is still failed, an ingest failure superseded by a later
  successful job of the same kind is dropped from the attention list, and a
  readable document whose optional background refresh failed stays in the ready
  bucket instead of disappearing from the counts. Attention items are localized
  by code rather than rendering server-side English, readiness is described in
  terms of what is available rather than internal projection vocabulary, and
  numbers are formatted per locale.
- **Choosing a different workspace keeps it.** The scope switcher persisted the
  whole scope record twice in one event handler, and the second write was built
  from a render closure still holding the previous workspace, so the selection
  reverted within a second and the session refresh then restored the old scope
  in full. There is now one writer for the transition, and the session resolve
  is pinned to the chosen workspace.
- **Screens no longer render raw translation keys.** A set of keys was stored
  as literal dotted strings inside nested objects, so the translation library
  could not resolve them and the screens using them showed the key itself. They
  are nested correctly and the i18n audit now covers the shape.

Operations and reliability:

- **Routine database states are reported as what they are.** A duplicate
  document upload surfaced as an internal error, which connectors retried,
  producing request storms; it is now a conflict with a stable neutral detail
  across create, replace, append and revision. Serialization failures,
  deadlocks and lock timeouts are classified as transient and answered as
  retryable rather than internal, so automation stops retry-storming a lock
  wait. Several queries decoded into a shared row struct while omitting columns
  added later — one crashed the billing path, another silently defaulted queue
  lease fields — and every projection is now generated from one shared
  definition per row type.
- **An exhausted provider billing quota is terminal end to end, and a rate
  limit is not.** Quota exhaustion is classified terminally rather than retried
  into the same wall. Provider `429`s are retried against the pipeline deadline
  instead of a fixed attempt count, honouring `Retry-After`, and the degraded
  answer wrapper fires only when the deadline genuinely cannot fit another
  attempt — so a rate-limit burst that clears in time produces a clean answer.
  Rate-limit, retry and degradation counters join execution telemetry.
- **A failed stage is diagnosable from the database.** The persisted failure
  summary stored the bare failure code twice, so a stage failure carried no
  information beyond its code. Classification now captures a value-free detail
  token at the boundary where the typed error still exists — provider kind,
  HTTP status, transport timeout, decode fault and the like — and never
  response text, which may carry upstream bodies. The provider failure type is
  shared across the service error enums instead of being flattened to a string
  at each boundary.
- **A build no longer stamps a version it does not have.** When no version was
  supplied, three separate layers each substituted a plausible stand-in — the
  Compose default, the image build argument, and a fall back to the package
  version, the last of which reads as a genuine release number, so a deployed
  image could report a release it was not built from. The version is now
  derived on the host, where the repository is available, and a build that
  cannot determine one identifies itself as an unversioned local build.
- **A document whose stored original is gone still serves its source.** The
  filesystem provider wrapped a missing file into an opaque internal error, so
  the handler's existing reconstruction fallback was never reached and the
  source download failed outright for documents that otherwise answered
  normally. A missing file is now classified as not-found and falls back to the
  stored normalized text; other storage faults still surface as faults.
- **Library and workspace deletion runs in one order.** Library-scoped mutation
  aggregates are deleted before their catalog parents in the same transaction,
  sharing one ordering between library and workspace deletion, with the
  existing document-integrity blockers preserved.

Internal structure, tests and gates:

- **A merge request runs the repository's own gates.** The gates a developer
  runs locally — the architecture, duplication, contract, migration, coverage
  and distribution checks — are now the gates a merge request runs. The backend
  integration suites, several of which had never executed, are enumerated with
  the number of tests each is expected to select, and a suite that selects none
  fails the gate instead of exiting zero. The answer-runtime cache fingerprint
  is checked structurally, so splitting a hashed source into a module directory
  can no longer silently drop its logic out of the hash.
- **The backend is split into small modules with no behavior change.** Every
  production module now meets the file-size policy; the backend source file
  count more than doubled while the code itself moved verbatim. The word
  *canonical* is retired from the codebase in favour of terms that say what is
  meant, except where it names a wire form — a database identifier, an HTML
  link relation — which stays as it is.

### Upgrade

- **Connector state volumes need one `chown`.** The BookStack, Confluence and
  Git-repository images ran as root and now run as uid 10001, so a state volume
  written by an earlier image is unreadable to the new one. Before the first
  start: `docker run --rm -v <volume>:/state alpine chown -R 10001:10001 /state`.
  A deploy key mounted for the Git connector must be readable by that uid too,
  and it now belongs at `/var/lib/ironrag-connector/.ssh/` rather than
  `/root/.ssh/`.


0.6.0 boots from a single schema baseline, so its database is created fresh;
the steps below apply to credentials being carried forward from a 0.5.x
deployment rather than to migrating a database in place.

Releases before 0.6.0 could store plaintext or v1/v2 credential envelopes.
0.6.0 reads **only** v3 envelopes, so rewrap every credential **before** you
upgrade, while still running the previous version:

```bash
docker compose run --rm backend ironrag-maintenance migrate credential-secrets
docker compose run --rm backend ironrag-maintenance migrate credential-secrets --apply
docker compose run --rm backend ironrag-maintenance migrate credential-secrets
```

The final dry run must report no remaining rewrap candidates. Only then deploy
0.6.0. In 0.6.0 the same command rewraps v3 rows during master-key rotation
(see [CREDENTIAL-ENCRYPTION.md](./docs/en/CREDENTIAL-ENCRYPTION.md)). A row that
is still plaintext or v1/v2 when 0.6.0 reads it fails closed and is reported as
invalid rather than being decrypted.

- Deployments that stored provider credentials without encryption must set
  `IRONRAG_CREDENTIAL_MASTER_KEY` and run `migrate credential-secrets --apply`
  on 0.5.x before deploying 0.6.0; otherwise the backend refuses to start
  (missing key) and previously stored credentials cannot be decrypted.

## 0.5.13 — 2026-07-19

### Fixed

- Polling an asynchronous operation is now part of the write contract: any
  principal allowed to admit a mutation can follow the `202` + `Location`
  response to `GET /v1/ops/operations/{operationId}` (and the diagnostics-mount
  `get_operation` MCP tool) without also holding the ops-observability
  permission. Least-privilege connector tokens could previously create
  revisions but never confirm them.
- A document-scoped grant now surfaces its parent library in catalog
  discovery, so a principal holding a document can navigate to it.
- `GET /v1/chunks` and the other content-surface document reads resolve
  documents and chunks from authoritative content storage instead of the
  eventually-consistent knowledge projection, which returned 404 or empty
  lists for freshly created documents.
- Denied provider-credential creation attempts are audited again with a
  rejected governance event, matching the catalog and IAM surfaces.

### Changed

- The continuous-integration database test matrix now runs the governance,
  audit, and atomic-admission suites so authorization and audit contracts
  cannot drift silently.

## 0.5.12 — 2026-07-19

### Fixed

- Revisions on documents restored from a library snapshot failed with an
  internal error: snapshots deliberately exclude operational ingest-attempt
  history, but the restore preserved each document head's
  `latest_successful_attempt_id` while foreign-key enforcement was disabled
  for the restore transaction, leaving a pointer at a row that no longer
  exists. The validated head write now distinguishes a pruned pointer (row
  gone entirely — tolerated and rewritten as null) from a live but
  wrongly-linked one (still rejected), so admissions on restored documents
  succeed and heal the pointer in place.
- Snapshot imports now drop `latest_successful_attempt_id` when writing
  restored document heads, so restores no longer materialize dangling
  foreign-key references that would also break a later dump/restore cycle.

## 0.5.11 — 2026-07-19

### Fixed

- The assistant view crashed on render ("assistant failed to render") because
  the session rail treated the session list response as a bare array while the
  endpoint returns a `{items, nextCursor, total}` page envelope. The UI now
  consumes the envelope everywhere, including optimistic cache updates. The
  browser mocks used by the end-to-end suite had drifted to the pre-envelope
  shape, which is how the regression escaped CI; they are now typed against the
  generated API contract so a future shape drift fails the type check instead
  of shipping.

### Changed

- One session representation everywhere: `POST /v1/query/libraries/{libraryId}/sessions`
  and `PATCH /v1/query/sessions/{sessionId}` now return the same camelCase
  session list item shape the list and detail endpoints use (including the
  live turn count), instead of a raw snake_case database entity. The
  `QueryConversation` wire schema is gone from the public contract.

## 0.5.10 — 2026-07-18

### Added

- **A zero-cost upgrade path for pre-hardening vector inventories.**
  `ironrag-maintenance migrate vector-profile-keys` re-keys legacy vector
  lanes (bare model-catalog keys) onto the authoritative embedding-execution
  profile fingerprint instead of forcing a paid provider re-embed. A library
  is re-keyed only when its active embed-chunk binding still targets the same
  model catalog entry and the lane dimensions match; mixed or retargeted
  states are skipped with a warning that prescribes the full
  `rebuild vector-plane`. Supports `--dry-run`, `--library`, and `--json`,
  and is idempotent. Operators upgrading existing deployments should run it
  once before serving queries; without it every pre-upgrade library fails
  closed at the exact-profile inventory check.
- **Projection-parity repair commands unblock libraries stuck in
  `query_content_projection_converging`.**
  `ironrag-maintenance repair orphan-knowledge-documents` purges
  knowledge-plane documents whose authoritative row no longer exists (sweeping
  their chunk vectors and graph evidence in the same transaction and
  re-projecting the runtime graph), and
  `repair knowledge-projection-metadata` reconciles drifted projection
  metadata field-for-field from the authoritative content plane. Either kind of
  historical inconsistency previously wedged its whole library behind the
  strict parity gate with no operator remedy. Both commands advance the
  library source-truth generation so running processes re-evaluate parity
  immediately, support `--dry-run`/`--library`/`--json`, and are idempotent.
- **Foreign-key indexes for the knowledge-plane delete cascade.** Deleting a
  document cascades through revisions, chunks, and their dependents, and
  several referencing columns had no usable index, so each parent delete
  sequential-scanned the referencing tables; on a populated library a single
  delete could run for minutes and an orphan purge effectively never
  finished. Migration 0018 adds one index per uncovered foreign-key column
  (partial indexes for the nullable set-null columns), turning those
  cascades into index probes.
- **The public REST API now gives every domain entity complete CRUD
  coverage.** Previously partial or missing operations were filled in rather
  than left absent for lack of a current caller: user removal, API-token
  detail/rename, AI provider and model hard-delete alongside the existing
  disable toggle, an addressable binding-validation resource, ingest job and
  attempt history lists, per-execution billing cost detail, knowledge
  relation/context-bundle/generation lists, and an addressable query-turn
  read.
- **Errors are RFC 9457 `application/problem+json`.** Every error response
  now carries a stable machine `code`, a mechanically derived human-readable
  `title`, and, where relevant, typed extension members — for example a
  duplicate-document conflict now carries the conflicting document's id
  directly in the error body — replacing the previous bespoke error shape.
- **Document creation and revisions are single, content-negotiated
  endpoints.** One route accepts either a JSON metadata body or a multipart
  file upload for document creation; a second route replaces three separate
  append/edit/replace endpoints with one, discriminated by request shape.
  Both return `202 Accepted` with a `Location` header pointing at the
  authoritative async-operation resource.

- **Assistant session management is durable and server-authoritative.** The web
  session rail now renames and deletes UI conversations through authoritative
  authenticated API mutations instead of browser-local title/visibility
  overlays. Title validation is shared by create and rename paths; ownership,
  write capability, UI-surface isolation, active-execution state, and external
  replay provenance are enforced atomically in PostgreSQL. Query-owned child
  state cascades on deletion, while independent runtime and audit retention
  records remain intact.

- **Content and web-ingest admission now have transactional units of work.**
  Mutation identity and fingerprinting, authoritative document/revision/head
  projections, async operations, mutation items, queue jobs, web runs, and
  discovery seeds commit together. Scoped idempotency replays the complete
  bundle, rejects payload conflicts, repairs only structurally unambiguous
  legacy gaps, and is covered by rollback failpoints and concurrent database
  races.

- **Release publication is gated by a reusable quality workflow.** Pull
  requests, protected branches, and Docker releases share formatting, Clippy,
  tests, migration immutability, OpenAPI/SDK drift, frontend, Compose, Helm,
  installer, and benchmark-contract checks. A parallel supply-chain gate runs
  pinned Rust audit tools and the npm lockfile audit; database-backed admission
  races run against real PostgreSQL and Redis services.
  Gitleaks scans both every introduced commit and the complete tracked or
  non-ignored worktree with fully redacted output; synthetic token-shaped
  fixtures are generated at test time instead of being hidden behind a finding
  baseline or inline allow directive.

- **UI and external MCP agents share a versioned answer-tool contract.** The
  ordered tool names, descriptions, and declared schemas now carry a stable
  SHA-256 contract hash. Grounded answers expose bounded `compact` and diagnostic
  `full` profiles, typed completion/readiness state, preserve spans, trace IDs,
  and a one-call `repairPolicy` for external agent runtimes. The MCP descriptor
  explicitly advertises both `repairPolicy.required` and
  `repairPolicy.maxAdditionalGroundedAnswerCalls`, so clients can enforce the
  bounded repair loop before their first answer call.

- **Optional provider-backed semantic reranking has bounded shadow and active
  modes.** Operators can keep it off, compare provider scoring against the
  existing deterministic order without changing answers, or apply it with
  bounded candidates, prompt size, concurrency, and a deterministic lexical
  fallback. The decision deadline covers binding resolution, durable billing
  reservation, the provider request, and usage accounting.

- **AI-account and webhook secrets can be encrypted at rest.** New
  row-and-purpose-bound XChaCha20-Poly1305 envelopes support explicit key IDs,
  a bounded previous-key ring, authenticated rotation, redacted diagnostics,
  and best-effort memory zeroization. The maintenance CLI inventories,
  migrates, and rewraps legacy values through a dry-run/apply/dry-run workflow.

- **Outbound webhooks have a durable delivery control plane.** A transactional
  outbox, lease-token ownership, heartbeats, bounded retry scheduling, dead
  letters, operator resolution, scoped pagination, and workspace quotas replace
  best-effort publication. Delivery history keeps typed status and failure
  summaries without persisting remote response bodies or transport URLs.

- **Release benchmarks carry a fail-closed integrity and latency contract.**
  Result schema v3 fingerprints suites, cases, corpus bytes, runtime knobs,
  artifacts, cache policy, round order, and host identity. The comparator pairs
  identical cases, rejects correctness/ranking regressions, and enforces both
  relative p50/p95/p99 budgets and absolute answer-latency ceilings. MCP probes
  issue a unique JSON-RPC request ID per sample, so one timed-out server request
  cannot invalidate the remaining cases as duplicate requests.

- **The Helm chart exposes production availability and network controls.** New
  HPA, PodDisruptionBudget, NetworkPolicy, runtime-secret, security-context,
  and resource-schema surfaces keep the simple bundled profile while allowing
  hardened and externally managed deployments.

### Changed

- **BREAKING: the public REST API was redesigned across every domain for REST
  consistency.** No compatibility shims are kept. Scoped resources move
  required identifiers from query strings into the path, list responses use
  one cursor-pagination envelope everywhere, and endpoints that acted like
  RPC verbs became `PATCH` state transitions or resource-shaped sub-endpoints.
  HTTP QUERY (RFC 10008) was evaluated for the handful of endpoints with
  complex structured filters and deliberately not adopted in favor of staying
  on plain REST. See the redesign plan checked into this repo's `memory/`
  history for the full endpoint-by-endpoint disposition and rationale.
- **The MCP tool contract was renamed and consolidated for naming consistency
  with the REST redesign.** Four tool families were renamed to match the new
  REST resource nouns; the JSON-RPC transport, mount paths, and every other
  tool name are unchanged. A single tool registry and a mandatory
  audit-emission wrapper around every tool call replace several
  independently-maintained, duplicated implementations of the same
  visibility/audit logic.
- **The connector SDK client was rewritten against the redesigned REST
  shapes.** Typed response models and a real poll-to-terminal helper for
  async operations replace untyped dict responses and ad hoc timeout
  handling; the legacy per-connector cursor-migration subsystem was removed
  outright rather than kept behind a compatibility flag.

- **MCP Streamable HTTP is session-bound and version-strict.** `initialize`
  negotiates every well-formed client request to the single authoritative
  `2025-11-25` version and issues an authenticated, TTL-bounded distributed
  session; every later
  `POST`, `GET`, and `DELETE` must present both that owned session and the exact
  negotiated protocol header. Missing, malformed, expired, terminated, or
  foreign sessions fail closed, session teardown cancels local and remote
  in-flight work, and the former token-scoped/stateless coordination path is
  deleted. Every MCP transport connection with an `Origin` header is also
  checked against the operator-configured configured origin set before auth,
  session lookup, or body parsing; malformed, duplicate, and untrusted origins
  fail with HTTP 403 while native clients may omit the header. Configured
  browser origins may preflight both mandatory MCP request headers, and the
  initialize response exposes `Mcp-Session-Id` so browser transports can bind
  their subsequent requests. The authoritative probes and private eval harness
  perform the same initialize/use/terminate handshake as external clients.
  Compact grounded
  answers no longer carry a compatibility copy of `executionDetail`; verifier,
  warnings, readiness, repair policy, bounded references, and trace IDs each
  have one authoritative compact field, while the full diagnostic profile retains
  the complete execution object.

- **AI configuration is consolidated around five required profiles and one
  optional profile.** Indexing and query search use one authoritative Embeddings
  model, Query Understanding owns both typed compilation and semantic rerank,
  and multimodal Document Understanding owns complex extraction and visual
  analysis. The redundant `query_retrieve`, `rerank`, and `vision` binding
  purposes are removed from runtime binding resolution, API, UI, and the final
  PostgreSQL enum; the forward migration folds existing configuration into
  active profiles, with no compatibility aliases or advanced overrides left
  behind.
  Before catalog rewrites can fire library-generation triggers, the upgrade
  path also clears historical creator references whose principals no longer
  exist, so valid pre-release databases cannot fail the authoritative-purpose
  migration on an already-orphaned optional foreign key. Versioned migration
  regressions cover both the pre-policy and pre-purpose upgrade paths. Vector
  storage and lookup use a secret-free execution-profile fingerprint, fail
  closed when the active profile has no exact vector inventory, and require an
  explicit rebuild after any incompatible profile change even when dimensions
  match. Empty libraries skip embedding and ANN work, while rebuilds stream
  keyset pages, write vectors in batches, and reconcile manifests once per
  lane instead of recounting after every row. Operators must complete that
  atomic maintenance rebuild before serving the affected library; upgrades do
  not silently reuse vectors from a different execution profile.

- **The six AI purposes are one strict public contract, not a compatibility
  layer.** Backend bootstrap, runtime resolution, OpenAPI, the generated web
  client, and the administration UI consume the same shared purpose type.
  Unsupported spellings and removed purpose names fail validation instead of
  being normalized through aliases. `Agent` has its own explicit bootstrap
  configuration and never inherits or synthesizes a `QueryAnswer` binding.
  Snapshot restore accepts only the authoritative AI-account, model-catalog, and
  binding tables; the pre-simplification credential, preset, and assignment
  import path is deleted. The administration client likewise keeps the
  authoritative `active`, `invalid`, and `disabled` binding states and rejects
  unknown state spellings instead of translating compatibility aliases.
  Agent discovery, bootstrap, writes, and runtime resolution require an
  Agent-role chat model plus provider-declared `chat` and `tools` support;
  unknown tool capability fails closed. The one-time migration materializes
  Agent eligibility from historical answer configuration only under that typed
  contract while preserving existing dedicated Agent configuration.
  Catalog models whose migrated roles collapse to an empty set are retained
  only as disabled historical billing references and cannot be reactivated by
  provider discovery without a authoritative role.

- **Query provider usage is durably attributed per actual call.** Runtime query
  compilation, question embedding, HyDE, primary agent turns, direct answer
  generation, and both accepted and rejected revisions reserve a stable local
  event ID before provider I/O. Provider failures terminalize that reservation,
  while every returned response commits its authoritative usage before parsing,
  verification, answer selection, or terminal query persistence. Repeating the
  same local reservation or completion is idempotent. A response guard owns the exact
  usage before its first database write and retries transient or ambiguous
  completion failures by the same event ID; only pre-response failures may be
  terminalized without usage. Cost rollups remain a repairable derived
  projection, and the runtime ledger carries the committed event ID through
  completed, recovered, failed, and canceled outcomes for diagnostics. The
  former terminal batch projection has been removed. A process crash in the
  narrow response-before-usage-commit window still leaves an observable
  `started` reservation whose exact usage cannot be reconstructed locally;
  this does not claim provider-side exactly-once execution.

- **Vector rebuilds publish atomically without taking search offline.** A
  library rebuild writes chunk and entity vectors under an opaque staging
  profile while queries continue using the previous authoritative inventory. A
  short library-scoped promotion transaction verifies the source generation,
  exact lane counts, profile, and dimensions before replacing active rows
  and manifests together; a failed rebuild removes only staging data.
  Library-scoped coordination replaces the former global exclusion, and
  generation-fenced inventory checks are singleflight-cached for one minute
  with a short error cooldown.

- **Embedding dimensions and filtered HNSW search use one fail-closed runtime
  contract.** A validated effective dimension is resolved once per binding;
  explicit binding parameters override model-catalog metadata, while invalid or
  unsupported values are rejected instead of falling through to a different
  vector space. Dimension resolution uses only typed configuration or authoritative
  vector inventory and never issues a hidden paid probe. Runtime query
  embeddings, including cache hits, are validated directly against the
  request-scoped exact-profile inventory, while mixed persisted dimensions
  fail closed instead of selecting an arbitrary vector lane. Catalog-only
  dimensions fence the execution-profile key without being injected into
  provider requests, and dimension lookup no longer rereads the model catalog.
  When a new profile has no declared or previously observed dimension, its
  first ordinary chunk batch establishes the dimension without a separate
  provider probe; that exact response is validated and persisted once before
  the remaining batches continue. Embedding responses now fail closed on
  missing, non-numeric, non-finite, empty, oversized, count-mismatched, or
  mixed-width vectors. Exact-profile manifest dimension claims are unique per
  library and vector kind, while an empty source library atomically purges stale
  vectors and manifests without invoking an embedding provider. Filtered
  authoritative writes, staged lane writes/reconciliation, destructive revision or
  entity cleanup, empty purges, and staged cleanup/promotion share a per-library
  cross-process serializer. Source and effective-profile generation is checked
  in the same short transaction as manifest, vector, and count updates, with no
  data-plane lock held during provider calls. A failed embedding attempt removes
  only vector UUIDs created by that attempt; pre-existing/replacement-attempt
  rows survive, and cancellation or lease loss preserves committed batches for
  count-gated retry/GC. Cleanup also proves that the attempt is still the latest
  leased authority in the delete transaction; after a retry takes ownership,
  even an adopted UUID is preserved. Inline job leasing and attempt creation now
  commit as one library/job-locked transaction. Physical vector lanes enforce
  one row per object, exact profile, kind, and freshness generation; a newer
  authoritative write atomically replaces the stored UUID and vector. Migration
  `0014_vector_logical_identity.sql` installs this uniqueness and aborts with an
  explicit repair requirement if existing duplicates make provenance ambiguous,
  rather than choosing rows to delete. Architecture lint rejects raw
  revision/library/exact-ID vector delete APIs in production call sites. Filtered
  pgvector searches now use bounded iterative HNSW scans with
  operator-configurable tuple and memory
  ceilings, structured underfill diagnostics, and one optional exact retry
  limited to small logical lanes. The storage contract now describes the actual
  shared per-dimension relations rather than claiming physical per-library
  shards.

- **Semantic routing is typed and domain-neutral.** Query compilation produces
  one schema-validated `QueryIR`; retrieval, intent, clarification, structural
  coverage, literal probes, and reranking consume its typed entities, literals,
  targets, and document focus instead of reclassifying raw questions with
  language dictionaries, casing rules, manual stems, or prefix guesses. The
  provider-free path recognizes only formal syntax and protocol structure.

- **Grounded-answer readiness has one finalizer-owned typed contract.** The
  finalizer persists `factual_ready`, `safe_fallback`, `clarification`, or
  `non_terminal` with typed clarification metadata; live reads, cache replay,
  HTTP, MCP, and the UI agent consume that disposition without reconstructing
  readiness from verifier prose or warning lists. Strict safe fallbacks and
  clarifications terminate without another answer-tool call, while conflicting,
  unsupported, missing-evidence, and empty candidates remain non-terminal.
  Conversation history remains typed end to end, including multiline turns,
  and model capabilities come from explicit catalog metadata rather than model
  names.

- **Clarification and verification are terminal, evidence-driven states.** A
  compiler-requested clarification is returned directly without a futile
  focused retry. Ordinary generated prose can no longer become `Verified`
  merely because some evidence exists: exact authoritative answers may verify,
  supported formal claims plus unchecked prose are partial, and prose without
  a checkable claim remains not-run. Assignments must be grounded in one source
  line or structured fact, including bracketed qualifiers.

- **Cache-write token accounting no longer inflates ordinary input cost.**
  Provider-reported cache creation is stored as a distinct typed billing unit.
  When the operator has not configured an explicit cache-write catalog price,
  usage remains auditable with `pricing_missing` semantics and no charge is
  fabricated from the ordinary input rate.

- **Environment-managed provider credentials use one generic, reversible
  contract.** One strict standard-base64 JSON map replaces provider-specific
  environment-variable tables across the backend, installer, Compose, Helm,
  Ansible, and smoke tooling. Exact provider kinds are preserved without alias
  guessing, credential strings remain byte-exact, bounded duplicate/malformed
  maps fail closed, ordering is deterministic, and secret values remain out of
  diagnostics.

- **Provider traffic is fail-closed and backpressured per endpoint.** Every LLM,
  embedding, vision, batch, stream, and model-discovery request uses one
  no-proxy/no-redirect transport with DNS pinning, exact-origin credential
  binding, bounded response/SSE bodies, and explicit private-network opt-in.
  Gateway-local concurrency registries reserve query capacity, bound identity
  cardinality and idle lifetime, reject deadlocking configurations at startup,
  and return a typed timeout instead of waiting forever or silently bypassing a
  closed limiter.

- **Critical background tasks are supervised as part of the service role.** An
  unexpected worker, maintenance, or webhook-outbox exit triggers coordinated
  shutdown and a non-zero process result; normal shutdown drains tasks for a
  bounded grace period and aborts a hung task instead of leaving a partially
  alive process.

- **Graph merge and projection coordination works across replicas.**
  Library-scoped PostgreSQL advisory locks replace the unbounded process-local
  merge-lock map. Support recalculation, node/edge pruning, and orphan-summary
  cleanup use atomic transactions, while topology/prewarm, hint, dimension,
  debounce, and terminal-marker registries now have TTL/cardinality eviction.

- **Query architecture debt is ratcheted in CI.** Production query stages may
  not use glob imports/re-exports or reverse dependencies into retrieval;
  newly extracted modules have an 800-line ceiling, and every oversized legacy
  module has a fixed ratchet ceiling until it is split behind an explicit
  stage API.

- **Handwritten query-semantic dictionaries are rejected by CI.** The
  architecture gate now catches constant, static, and function-local string
  tables in production query code while excluding comments and test fixtures.
  Semantic intent remains the responsibility of validated `QueryIR`; local
  deterministic code is limited to formal syntax, protocol structure, typed
  state, evidence ordering, and literal fidelity.

- **Backend linting is warning-free without project-wide Clippy exceptions.**
  Large enum payloads and upload rejection details use explicit indirection,
  mechanical control-flow and borrowing debt is removed across every target,
  and a deny-by-default `large_futures` ratchet puts heap boundaries around
  oversized MCP, UI-agent, ingest, retrieval, snapshot, and test futures. The
  release MCP request frame is reduced from about 1.21 MiB to 23 KiB, removing
  the Tokio worker stack-overflow crash without increasing thread stacks. Rust
  documentation is built with broken links and invalid markup denied.
  Deterministic repair, partial-answer, and assistant-tail copy now lives in the
  query localization catalog instead of language-specific execution branches,
  and the language compliance scan distinguishes production code from test-only
  fixtures. Exact dependency-license exceptions are synchronized with the
  lockfile so supply-chain checks fail on stale or newly introduced license
  decisions.

- **Frontend artifacts and budgets are reproducible.** Swagger UI assets are
  synchronized from the pinned package with license files and a drift check;
  generated SDK normalization, unused dependency/export scans, RU/EN key
  parity, coverage thresholds, and per-chunk bundle limits are enforced by the
  same release quality workflow. The production image installs dependencies
  before version-specific build arguments, so release-only version changes
  reuse the lockfile layer without reusing stale application assets; local
  reports, test artifacts, environment files, and logs stay outside the Docker
  context.

- **Question intent and focused retrieval preserve the requested answer shape.**
  Provider-compiled typed intent retains procedures,
  troubleshooting/remediation intent, exact quoted diagnostics, and ordered
  latest-N inventories; provider-free fallback stays formal and does not guess
  natural-language intent. Optional code/transport lanes are limited to the
  first retrieved candidate documents and fail softly only when primary
  evidence is already available. Directional version-transition guides are
  excluded from release inventories without excluding compound release
  headings that publish parallel versions.

- **Deterministic answer shortcuts are intent-safe.** Generic how-to questions
  continue to grounded procedure synthesis instead of being replaced by a
  configuration inventory, while troubleshooting questions stay on evidence
  that contains issue-local remediation instead of entering generic update
  runbooks. Short structured identities require action-bound evidence before a
  sibling procedure can be selected, including when the identity originates
  from a typed literal rather than an entity. Configuration variants use mutual
  structural discriminators, so a shared path or package cannot merge distinct
  provider sections into one direct answer; focused single variants remain on
  the fast path. The compiler no longer invents a document target for an
  otherwise self-contained procedure request.

- **Completion and output boundaries follow structural answer evidence.**
  Release inventories distinguish a release identifier even when every item
  shares the same product-version prefix, while troubleshooting completion is
  driven by typed intent and action-bearing evidence rather than a
  language-specific phrase list. A bare trailing media source token is removed
  only after an already complete sentence, while action-bearing filenames
  remain untouched.

- **The UI answer contour starts with the authoritative MCP answer path.** The first
  `grounded_answer` call is runtime-dispatched before any model round. An
  incomplete procedure, remediation, clarification, or ordered inventory gets
  at most one non-identical focused retry, capped at 45 seconds; a ready repair
  is returned directly without another synthesis call. Initial failures stop
  without a hidden model retry, while an unresolved repair can return a clearly
  marked completed partial result instead of an opaque tool error.

- **Grounded answers are verified from explicit evidence before they can be
  returned or cached.** Verification now separates policy, claim extraction,
  evidence matching, and support assembly; exact literals, ordered procedures,
  tables, quoted clauses, temporal facts, and citations fail closed when their
  supporting source cannot be proven.

- **Retrieval is scope-safe, bounded, and deterministic.** PostgreSQL search
  lanes apply tenant, readable-revision, state, vector-dimension, and legacy
  summary filters before their limits. Typed reciprocal-rank fusion and bounded
  reranking replace permissive post-filtering and unbounded candidate work.
  Unresolved entity labels no longer launch a corpus-wide content-anchor lane.
  Broad multi-revision searches reserve bounded per-revision GIN pools before
  exact substring rescoring, apply a database-side statement deadline so a
  disconnected caller cannot leave an orphaned scan, hydrate at most 256
  diverse candidates, and keep exhaustive semantics for small or mixed
  unindexable requests. Release identity selection excludes attached context
  and image artifacts, including media URLs with query/fragment suffixes,
  before they can consume ordered inventory slots.

- **PostgreSQL is the authoritative grounded-answer cache.** Cache identity now
  includes runtime, policy, content, graph, and binding generations. Redis is
  limited to fill coordination and notifications; a Redis outage cannot invent
  or extend a winner, and PostgreSQL's clock owns expiry and replacement.

- **Provider billing supports reservation-first, repairable stages.** Stages
  that opt into the reservation API receive a durable caller-known reservation
  before network I/O, atomic usage and charge completion, idempotent
  terminalization, monotonic dirty generations, bounded rollup repair,
  stale-reservation reaping, and explicit mixed-currency failure instead of
  silently combining incompatible totals. Query compilation, embedding,
  semantic reranking, query-agent turns, direct answers, and answer revisions
  all use that reservation contract; the former terminal best-effort usage
  projection has been removed. Historical query charges now reconcile from
  their durable workspace/library scope after
  conversation retention removes the query execution, preventing an orphaned
  generation from retrying forever and blocking library/document reads with
  `billing_rollup_pending`.

- **Readable content and graph publication use durable generations and fencing.**
  Document/revision/chunk parity is checked against the readable projection,
  cached by generation with per-library singleflight, and invalidated only by a
  real truth change. Graph builds use a durable build epoch, monotonic projection
  versions, compare-and-set publication, and an explicit mixed-writer protocol
  fence. Reconciliation stages no longer advance source truth independently:
  the atomic terminal lifecycle publisher performs the single authoritative
  graph-generation bump, and the deferred summary refresh holds a generation
  fence so stale post-commit work is skipped instead of publishing mixed truth.

- **Knowledge revision projections use one stable state vocabulary.** A
  forward-only migration rewrites historical text, vector, and graph spellings,
  rejects unknown rows before changing data, and installs validated database
  constraints. Runtime readers and writers no longer accept readiness aliases
  or silently map unknown extraction states to `accepted`.

- **AI configuration changes invalidate affected answer generations in the
  same transaction.** Statement-level triggers compute the affected library
  set, lock parents in deterministic order, and advance source truth before a
  stale cache writer can publish or replay an answer under an obsolete binding.

- **Telemetry is opt-in.** Backend OTLP traces and metrics, content-bearing
  logs, and browser tracing stay disabled unless an operator explicitly
  configures them and supplies a collector endpoint. Query developer tools are
  a separate local preference and remain disabled until the user enables them.

- **The installer commits `.env` updates atomically.** It writes a restrictive
  same-directory staging file, validates secret preservation, and renames it
  only after the full batch succeeds. EXIT, INT, and TERM cleanup removes an
  uncommitted secret-bearing stage; no full `.env.bak` is retained.

- **Generated frontend API code remains strict and reproducible.** OpenAPI
  normalization handles exact optional properties without weakening TypeScript,
  generated output is checked for drift, and shared table pagination keeps one
  typed set of supported page sizes.

- **Operator dashboards expose one authoritative metrics projection.** The API,
  OpenAPI contract, generated client, fixtures, and UI now consume only
  `documentMetrics`; the duplicate preformatted `overview` and `metrics`
  projections and their transport type are removed.

- **Literal-fidelity answer revisions share one fixed-context provider path.**
  Direct grounded answers and both revision modes use the same binding,
  reservation, usage-commit, debug, and failure semantics instead of carrying
  parallel copies that could drift.

- **Pre-release HTTP inputs have one single wire name.** Knowledge search
  accepts `query` only; the hidden `q` deserialization alias is removed and an
  architecture gate rejects new production `serde` compatibility aliases.

### Removed

- **Duplicate REST surfaces were removed in favor of their authoritative
  replacement.** The generic content-mutation list/create/get endpoints are
  superseded by the typed document/revision endpoints and the audit log; the
  imperative IAM-grants collection is superseded by the existing declarative
  per-user access document, which now carries optimistic concurrency instead;
  several knowledge-domain endpoints that exactly duplicated the authoritative
  content-document list and detail were removed with no replacement needed.

- **The dormant RAPTOR summary builder and CLI command are removed.** Existing
  RAPTOR rows still cannot prove revision-safe leaf lineage or atomic
  publication, so every retrieval lane continues to exclude them until an
  explicit maintenance operation deletes the retired data.

- **The placeholder inbound-webhook endpoint is removed.** It previously
  advertised a route that could only return `501 Not Implemented`. External
  source middleware should use the documented upload, replace, and delete APIs.

### Fixed

- **Duplicate-document creation returns a clean `409 Conflict` instead of a
  masked `500`.** A duplicate `externalKey` on document creation previously
  surfaced as either an opaque internal error, or — past the point where the
  underlying unique-constraint violation aborted the surrounding database
  transaction — a second, unrelated-looking internal error when the server
  tried and failed to look up the conflicting document inside the now-aborted
  transaction. The conflict is now caught and reported cleanly, with the
  existing document's id included in the response.
- **Multipart document upload no longer requires a redundant scope field.**
  The multipart form previously still required an independent `library_id`
  field left over from before the target library became a path segment,
  rejecting every multipart upload with a spurious `400`.

- **Local image builds and redeploys use one deterministic service graph.**
  Compose builds the shared backend image only through the backend service;
  startup and worker execute that exact image instead of racing duplicate
  builds into one tag. The local workflow always builds both backend and
  frontend, runs the one-shot startup migration to a verified zero exit, and
  only then recreates API, worker, and frontend without restarting PostgreSQL
  or Redis.

- **Transient MCP conversation titles respect the aggregate domain limit.** The
  fixed prefix and Unicode-safe truncated question now fit the authoritative
  72-character boundary together, so a valid long question cannot fail before
  grounded retrieval starts.

- **The container runtime entrypoint preserves maintenance command arguments.**
  Its unprivileged `su` handoff forwards option-prefixed `argv` verbatim, so
  `docker compose run` maintenance commands no longer have their flags parsed
  by `su` itself.

- **Release fixtures and frontend images pass the high-severity supply-chain
  gate.** Frontend base images are digest-pinned and upgrade the patched Alpine
  `c-ares` and `libexpat` packages; the neutral benchmark Compose fixture reads
  its synthetic payment token from the environment instead of embedding a
  secret-shaped literal.

- **Detached MCP and assistant-stream work owns every value captured by its
  spawned future.** Request IDs, JSON arguments, application/auth state, query
  services, and audit commands no longer borrow handler stack frames across a
  task boundary; the strict `test-support` build now checks the same `Send +
  'static` contract used by production spawning.

- **Ingest admission and terminal publication preserve one lock order and one
  transaction.** Job attempts, queue or inline leases, and linked async
  operations are created together under library-then-job locks. A terminal
  operation can no longer be reopened by a replay, a waiter revalidates the
  library after acquiring the parent lock, and injected failures roll back the
  complete admission bundle instead of leaving orphan work.

- **Configuration preflight preserves already retrieved setup evidence.** When
  exact-scope authoritative selection is empty for a setup question, the answer
  stage now reuses only the bounded document-identity setup lanes and leaves
  ordinary relevance tails out. Technical-fact selection also diversifies
  repeated occurrences by document, revision, kind, normalized value, and
  qualifiers before filling the answer budget, so duplicate rows cannot crowd
  distinct facts out while their provenance remains intact in storage.

- **Ordinary prose can no longer pass as a verified named claim.** The verifier
  verifies exact authoritative evidence and typed formal literals, marks a mixture
  of supported claims and unchecked prose as partial, and leaves prose without
  checkable claims not-run. Markdown numbering or capitalization does not
  change that contract.

- **Malformed persisted graph extraction can no longer publish an empty
  success.** Blocking-task panics and candidate-contract decode failures now
  propagate with document/chunk context, preserving the previous ready graph
  instead of turning corrupt normalized JSON into `GraphExtractionCandidateSet::default()`.

- **UI and external MCP default to the same compact grounded-answer profile.**
  An omitted response profile no longer expands one surface while compacting
  the other; explicit debug requests remain the only path to full diagnostics.

- **Grounded-answer readiness and synthesis fail closed without discarding good
  evidence.** Empty bodies, clarification prompts, generic troubleshooting
  advice, truncated latest-N lists, non-verified results, and blocking warnings
  cannot be marked final. Readiness follows typed intent and evidence structure
  without language-specific action morphology, while newly invented operations
  still fall back to verified evidence.

- **Latest-N inventories use grounded release identities and version order.**
  Versions found in source text are ordered newest-first before retrieval score
  and chunk position. Retrieval deduplicates only the same authoritative document
  revision, while answer assembly deduplicates only the same document,
  revision, and formal version. Distinct sources are therefore never collapsed
  because their labels or version literals happen to match, and completion
  counts the same top-level source units that the renderer emits.

- **Latest-N inventories preserve source provenance while selecting formal
  release evidence.** Release markers are matched to their own adjacent
  versions, compound release identifiers select the newest grounded value, and
  dates, addresses, and later dependency versions are ignored. Eligible source
  and context evidence is merged without title-token families, dominant-family
  filters, or cross-document title deduplication.

- **Delete replay and legacy mutation repair are scope-safe under concurrency.**
  A fresh idempotency key on an already deleted document creates and durably
  binds its own authoritative mutation instead of borrowing the previous delete.
  Legacy unbound mutation items are claimed with a compare-and-set update over
  the exact mutation and item identity; concurrent losers re-read and fail
  closed, and structurally ambiguous multi-item repairs are rejected. Missing
  documents, deleted targets, scope conflicts, and reference-integrity failures
  now remain distinct typed repository outcomes instead of being inferred from
  a generic database `RowNotFound`. Successful-attempt lookup proves that the
  attempt belongs to the exact mutation item, document, and revision selected
  by the current head, so an attempt from a sibling item in the same mutation
  cannot satisfy publication.

- **Procedural answers fail closed on ambiguous product identity and incomplete
  instructions.** Acronyms remain useful retrieval hints but cannot impersonate
  an exact document target; deterministic lists require action-bound identity
  corroboration and completion-policy coverage. Configuration section names and
  natural-language sentences are no longer misclassified as executable commands.

- **Exact troubleshooting remediations are not downgraded by procedure-only
  coverage checks.** Verified issue-local fixes can finalize without a false
  partial-coverage warning, and answer cleanup no longer appends a bare target
  name as a cosmetic final sentence.

- **Ordered revision inventories and acronym-prefixed literals keep their
  completion guarantees.** Provider-compiled typed inventory intent prevents a
  verified but truncated list from finalizing. Exact-literal recovery uses only
  compiler-supplied literals, entities, and document focus; capitalization no
  longer promotes raw question tokens into the literal retrieval lane.

- **The UI MCP dispatcher cannot execute tools outside its advertised answer
  surface.** Hallucinated or prompt-injected write/diagnostic calls are rejected
  before argument parsing or dispatch, matching external MCP preflight. Runtime
  repair calls bypass ordinary query compaction without leaking their private
  marker to the handler, so the second call cannot collapse into a replay of the
  first.

- **UI and external MCP agents now receive the same authoritative ready-answer
  contract.** The UI returns a single fresh, complete, verified
  `grounded_answer` byte-for-byte after local evidence verification and
  persistence, without an extra parent-model rewrite or literal-revision pass;
  external MCP clients receive the same answer/readiness contract. Incomplete,
  repair, replay, and multi-tool UI turns continue through the bounded
  composition path.

- **External MCP agents can start safely without a preconfigured library ref.**
  When `library` is omitted, IronRAG infers it only if the caller has
  `query_run` on exactly one library; zero and ambiguous matches fail closed
  with the same non-disclosing error. Explicit library calls retain their
  direct authorization path and add no catalog lookup.

- **Long MCP tool calls stream transport progress before the answer is ready.**
  SSE calls flush an immediate readiness comment and bounded keepalives before
  one final JSON-RPC message and disable proxy buffering. Bounded Redis leases
  coordinate duplicate ownership and explicit cancellation across API replicas;
  transient disconnects do not cancel work, while deadlines, process caps, and
  generation-safe cleanup bound every detached execution. JSON responses retain
  their existing shape.

- **External MCP inputs and evaluation probes are bounded and secret-safe.**
  Unknown grounded-answer fields, empty or oversized text, invalid `topK`, and
  excessive conversation history are rejected deterministically; debug requests
  cannot silently use a compact response. Profiling runners pass credentials and
  the private suite runner's case inputs through protected stdin/files instead
  of process arguments, and verify identical tool contracts before comparing
  answer surfaces.

- **Cached-answer replay cannot authorize stale or ungrounded output.** Replay
  locks and rechecks the exact fresh cache winner, source generation, completed
  runtime, verified context bundle, authoritative assistant answer, grounding
  references, request scope, and target conversation in one transaction. Turn
  creation and replay provenance commit or roll back together.

- **Replay provenance survives source-conversation retention races.** Automatic
  conversation eviction skips sources referenced by another conversation, an
  isolated source deletion is rejected, and deferred constraints still permit
  a complete workspace or library cascade in one transaction.

- **Provider base-URL policy can no longer expose a hidden API key.**
  `allowOverride=false` is enforced by create, update, bootstrap, validation,
  and runtime resolution, including legacy or direct-SQL rows. Even when an
  override is allowed, moving an existing hidden key to a different scheme,
  host, or port requires the caller to submit the replacement key explicitly.

- **Optional semantic reranking cannot extend answer latency past its configured
  decision budget.** Slow binding lookup and billing reservation now time out
  from the answer path, while owned reconciliation tasks safely finalize late
  or ambiguous reservations and preserve paid-call usage accounting.

- **Webhook delivery is fenced against duplicates, stale workers, and unsafe
  targets.** Subscription/event identity is serialized, current workers must
  present the lease token, DNS and redirect targets are revalidated against the
  outbound policy, custom headers remain bounded, and mixed-version writers
  cannot persist remote-controlled diagnostics.

- **Content projection drift fails closed without causing scan storms.** The
  parity fingerprint covers authoritative document, revision, chunk text, checksum,
  offsets, metadata, readiness, and scope. Negative parity results are cached
  until the durable generation changes, and concurrent checks share one scan.

- **Stale graph/config writers cannot overwrite newer truth or deadlock catalog
  deletion.** Terminal graph publication compares the durable epoch, old
  protocol writers receive a serialization failure after the v2 boundary, and
  configuration moves/deletes share a deterministic serializer and parent-lock
  order.

- **Result-cache maintenance is bounded and observable.** Garbage collection
  deletes oldest expired winners with `SKIP LOCKED` in fixed-size batches,
  performs a bounded indexed backlog probe instead of a full count, and reports
  progress, oldest expiry age, budget exhaustion, and failures.

- **Projection-transition retries no longer leave orphan user turns or start a
  paid query on stale content.** Generation changes during cache fill/wait
  return a retryable convergence error and remove only the still-unexecuted
  request turn atomically.

- **Credential and webhook error paths no longer leak protected material.**
  Secret-bearing domain objects have redacted debug/serialization surfaces,
  encrypted envelopes are bound to their stable record and field purpose, and
  persisted webhook failures use bounded static summaries rather than remote
  response content or raw request errors.

- **Maintenance repairs preserve authoritative/readable parity and invalidate stale
  answers exactly once.** Parent-role and temporal backfills lock the library
  first, update both content planes transactionally, reject missing or
  wrong-scope readable mirrors, and advance source truth only after a real
  answer-visible change. The duplicate-content healer is dry-run by default,
  repairs both planes in one transaction, and remains idempotent on retries.

- **Assistant execution deadlines have a stable, non-leaking HTTP contract.**
  A turn that exhausts its own end-to-end deadline now returns `504` with the
  `query_deadline_exceeded` kind instead of exposing the question, execution
  identifier, or an internal failure chain as a generic `500`.

- **Environment-managed AI accounts no longer make startup depend on credential
  migration mode.** Bootstrap defers secret-bearing account writes while
  encrypted credential writes are intentionally disabled, allowing a staged
  key rollout without preventing migrations or application readiness.

## 0.5.9 — 2026-07-10

### Changed

- **GPT-5.6 models are available in supported provider catalogs.** Luna, Terra,
  Sol, and the provider-exposed Pro aliases now include multimodal chat roles,
  context limits, and token prices. Provider bootstrap bundles keep graph
  extraction on GPT-5.4 Nano, use Luna for the remaining chat and vision roles,
  and use Sol for the agent role. Tool-bearing GPT-5.6 defaults now carry the
  same reasoning contract across direct and OpenAI-compatible proxy routes.

### Fixed

- **GPT-5.6 requests work through OpenAI Chat Completions.** Requests omit
  unsupported sampling overrides, while tool-bearing calls disable reasoning
  and receive a 65,536-token output default when the binding has no explicit
  limit. Existing explicit output limits and earlier model families keep their
  current behavior.

- **Long grounded assistant calls no longer fail at the generic tool timeout.**
  The authoritative grounded-answer tool has a bounded 90-second call budget inside
  the existing 180-second turn deadline, while all other tools retain the
  shorter collection budget.

- **Rotated environment credentials reach restored and scoped bindings.**
  Startup synchronizes every authoritative bootstrap account that still carries an
  older env-managed key, including workspace and library copies, while
  preserving an operator-disabled state and provider base URL.

- **Interrupted assistant executions cannot remain active indefinitely.** A
  cancellation guard atomically closes query, runtime, and async-operation rows
  when a turn future is dropped. The maintenance scheduler also reaps stale
  non-terminal executions after the turn deadline, covering process restarts
  and failed best-effort cleanup.

- **Reloading during an assistant answer no longer creates debug-notification
  storms.** Optional missing LLM-context snapshots are de-duplicated per
  execution, recognized across generated-client error shapes, and kept as one
  inline inspector state without repeated requests or global toasts.

## 0.5.8 — 2026-07-09

### Changed

- **Release image workflows use Node 24-compatible GitHub Actions.** The Docker
  image publication workflow now uses current major versions of the GitHub and
  Docker actions that run on the Node 24 runtime, removing the runner warning
  from release builds without changing image tags or publication semantics.

- **Assistant retrieval gives explicit content anchors higher priority.** The
  answer pipeline now promotes bounded chunks that match quoted phrases,
  document-focus hints, target entities, literal constraints, or the active
  retrieval query before filling the remaining context budget, improving
  precision for focused questions without introducing domain-specific routing.

### Fixed

- **The assistant works with GPT-5.5 bindings.** OpenAI-compatible requests now
  omit sampling parameters that GPT-5.5 only accepts at provider defaults, while
  preserving existing sampling behavior for models that support it. GPT-5.5
  tool calls also use provider-compatible automatic tool choice, so switching
  the MCP-backed assistant binding from GPT-5.4-mini no longer fails before the
  answer step.

- **The assistant chat layout stays bounded on narrow desktop and macOS
  windows.** The session rail, answer thread, debug pane, and composer now share
  the same constrained flex/scroll contract, keeping the input pinned inside the
  chat surface and preventing page-level horizontal overflow when answers or
  source lists are wide. The debug inspector now docks only on very wide
  desktops and behaves as a right-side drawer on laptop widths, so it no longer
  squeezes the answer column into an unusable narrow strip.

- **Assistant answers stay clean while evidence remains inspectable.** The
  answer formatter now keeps the visible chat response separate from retrieved
  context, debug traces, source dumps, and low-level artifact text. Evidence and
  diagnostics remain available through the dedicated side panels and developer
  inspector instead of being mixed into the assistant bubble.

- **Assistant sessions survive page reloads during a running answer.** Hydrated
  conversations now include an in-flight assistant placeholder for active
  executions, the web UI polls that active session until the durable answer
  lands, and duplicate sends are blocked while the restored turn is still
  pending.

- **Assistant sends show immediate progress even when the provider is slow.**
  The chat now preserves the local pending turn across lagging session
  hydration, so users see an assistant working card immediately after submit
  instead of a blank thread while the backend/provider starts the execution.
  The same local overlay prevents stale history refetches from briefly erasing
  a just-finished answer.

- **Missing optional debug snapshots no longer create notification storms.**
  The assistant debug inspector now treats a missing LLM-context snapshot as an
  inline diagnostic state, caches the miss per execution, and reserves global
  error toasts for real debug fetch failures.

- **Assistant debug state follows the current turn.** Starting a new question
  now clears stale debug-context diagnostics from the previous execution while
  the next assistant run is pending, so the inspector no longer shows an old
  missing-context warning for a newly submitted request.

- **Assistant inspectors no longer fight pending answers.** Pending assistant
  turns are treated as running work even when the backend has already assigned
  an execution id, so the debug inspector does not request unavailable LLM
  context before the answer is complete. The evidence inspector also uses a
  fixed flex layout with an internal scroll region for long source lists.

- **The installer restarts the frontend after backend updates.** Local and stage
  upgrades now restart the nginx frontend after recreating backend roles, so the
  API proxy cannot keep a stale Docker DNS target after an update.

## 0.5.7 — 2026-07-06

### Added

- **The admin ingest queue supports bulk recovery actions.** Operators can
  select visible queue rows and apply retry/requeue, pause, resume, or cancel
  from a contextual actions bar. The backend returns per-job results with
  stable reason codes, and the UI keeps skipped or failed rows selected for
  follow-up.

### Fixed

- **The admin ingest queue keeps its constrained table layout for large result sets.** The queue now preserves the shared table width, internal scrolling, and footer pagination as row counts grow.

- **Stale job-level queue leases are recoverable.** Ingest jobs now store
  queue lease metadata when claimed, workers must match that lease before
  creating an attempt, and recovery requeues stale leased jobs that no longer
  have an active attempt. Completed and canceled jobs remain terminal and are
  not requeued from the queue surface.

## 0.5.6 — 2026-07-03

### Changed

- **AI configuration is simplified to accounts and inline-parameter
  bindings.** Model presets are removed as a separate entity; their
  parameters (system prompt, temperature, top-p, max output tokens, and
  extra request parameters) now live inline on each per-purpose binding.
  Provider credentials are renamed to *accounts* and the binding record is
  renamed to match, with the REST surface moving to `/v1/ai/accounts` and
  `/v1/ai/bindings`. The admin AI-configuration screen collapses to two
  setup tiles — bindings and accounts — plus a read-only model and pricing
  reference, and the guided setup becomes a two-step wizard (account, then
  model and parameters). A single idempotent migration performs the renames
  and backfills each preset's parameters into its binding; library snapshot
  import still accepts pre-migration archives by splicing preset fields into
  the restored bindings.

- **The web UI is unified around one design language.** Tables, filters,
  forms, status indicators, and empty states share a single set of
  primitives across every page: one panel surface, one status-badge
  vocabulary, filter controls with a leading icon, a authoritative table
  contract that degrades to cards on narrow viewports, and a shared
  form-field family for dialogs. The admin navigation is grouped by area,
  and a repository gate rejects raw color literals so the palette stays
  token-driven.

- **The sidebar collapses to an icon rail.** Clicking the logo toggles the
  desktop sidebar between the full labeled rail and a compact icon-only rail
  (the choice persists locally); collapsed entries stay fully functional and
  reveal their labels on hover. The redundant top breadcrumb strip is
  removed, and long workspace or library names now show their full text on
  hover wherever they are truncated.

- **Ingest concurrency defaults are lowered for baseline memory safety.** The
  default queue caps drop from 16/8/4 to 4/2/1
  (`IRONRAG_INGESTION_MAX_PARALLEL_JOBS_GLOBAL`, `_PER_WORKSPACE`,
  `_PER_LIBRARY`) in `docker-compose.yml`, `.env.example`, and the README, and
  the capacity-planning guide now derives per-worker limits from available
  memory instead of a fixed deployment-wide ceiling. Deployments that upgrade
  keep their ingest queue memory-safe by default on small hosts; raise the caps
  together with worker memory, replica count, and the database connection
  budget.

- **Assistant starter prompts are domain-neutral.** The suggested opening
  questions move from system- and deployment-specific wording to generic
  corpus-exploration prompts (overview, key points, open questions, recent
  items), so the assistant makes no assumption about the library's subject
  area.

### Fixed

- **Out-of-memory extraction failures name the setting to change.** When the
  worker container lacks the headroom a document extractor process needs, the
  raw error and the document remediation hint now name
  `IRONRAG_WORKER_MEMORY_LIMIT` and the container-recreate step instead of
  pointing operators at the file.

- **First-run binding bootstrap no longer stalls on the retrieval binding.**
  Environment-driven bootstrap mirrors the query-retrieval binding onto the
  chunk-embedding binding, so a fresh deployment configures a complete,
  self-consistent set of bindings without manual intervention.

- **The frontend type-check gate is real.** The `frontend-typecheck` target
  previously resolved zero input files and always passed; it now builds the
  project references, and the strict-type debt that surfaced has been cleared
  without loosening the TypeScript configuration.

## 0.5.5 — 2026-06-27

### Changed

- **Release metadata is aligned for 0.5.5.** The Rust workspace package
  version, generated OpenAPI `info.version`, Helm chart, and README image-tag
  example now point at 0.5.5.

### Fixed

- **Startup tolerates operator-side provider credential validation failures.**
  Env-keyed provider bootstrap now skips only provider validation errors with a
  sanitized warning, while still failing fast on internal, storage, or catalog
  errors. A stale, expired, or policy-blocked external API key can no longer
  prevent the database, API, worker, and frontend from starting.

## 0.5.4 — 2026-06-27

### Added

- **MiniMax is now a first-class AI provider.** The provider catalog seeds
  MiniMax credentials, runtime metadata, bootstrap presets, model discovery,
  model rows for the currently published chat and multimodal models, and USD
  price rows including context-range pricing where the provider publishes it.
  The same `IRONRAG_MINIMAX_API_KEY` secret supports regular API keys and
  Token Plan subscription keys through the OpenAI-compatible MiniMax endpoint.

- **Prompt-only structured-output providers are supported generically.** The
  provider runtime can now omit unsupported `response_format` request fields
  while still injecting the required JSON schema into the system prompt. This is
  exposed through the public provider profile contract as
  `prompt_only_json_object` and is not tied to a single corpus or domain.

### Changed

- **Provider bootstrap preserves preset-specific extra parameters.** Catalog
  metadata can now carry per-purpose `extraParametersJson` into instance
  presets, allowing provider-required request options to survive first-run
  bootstrap and credential auto-registration.

- **Public configuration surfaces include all shipped provider keys.** Docker
  Compose, Helm values/templates, installer prompts, `.env.example`, API docs,
  generated OpenAPI contracts, and the README provider matrix now include the
  MiniMax key and the router-provider secrets consistently.

- **Release metadata is aligned for 0.5.4.** The Rust workspace package
  version, generated OpenAPI `info.version`, Helm chart, and README image-tag
  example now point at 0.5.4.

### Fixed

- **Provider E2E now fails when answer generation silently falls back.** The
  multi-provider smoke test records query provider calls and requires the
  selected provider to perform the answer-producing call, covering both
  `query_answer` and agent-tool answer lanes.

- **AI binding readiness now reports the number of missing bindings.** The UI
  no longer shows a vague missing-binding badge when exact missing counts are
  available.

## 0.5.3 — 2026-06-26

### Changed

- **The knowledge plane is PostgreSQL-only.** The backend no longer ships the
  removed ArangoDB client, bootstrap, document/search/graph stores, dependency
  mode, or contract tests. Configuration now accepts only the authoritative
  PostgreSQL knowledge-plane backend, and operational diagnostics report any
  unsupported backend value as unsupported.

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
  authoritative ingest job before claiming more leases. Docker Compose and
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
  auto-selected when stdin is not a TTY (the authoritative `curl | bash` path) and
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
  keyed by a 32-byte SHA-256 of the normalized identity tuple (trimmed question
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
  smaller of the remaining turn deadline and a authoritative per-tool-call max, so a
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
  inline image) now records its authoritative `parent_document_id` and a typed
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

- **OpenTelemetry remains operator opt-in.** The backend exports traces and
  metrics only when `IRONRAG_OTEL_ENABLED=true` and an explicit
  operator-controlled `OTEL_EXPORTER_OTLP_ENDPOINT` are configured. Distribution
  defaults do not embed a project-owner collector.
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
  entity, relation, and evidence foreign keys reference the authoritative runtime
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
  hedged instead of answering. The contextual floor is pinned to the authoritative
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
  Both code paths now call the authoritative snapshot-restore replace
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
  deletes the orphan rows through the same authoritative sweep snapshot
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
  configured collector and carry authoritative resource labels; disable with
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
  authoritative maintenance classes on a rolling cadence (default 30 s
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
  `ironrag-maintenance` subcommands. Authoritative surface is one CLI with
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
  the same authoritative code path is used by the operator CLI and by the
  scheduler tick. Docker image installs only `ironrag-backend`,
  `ironrag-cli`, `ironrag-emit-openapi`, and `ironrag-maintenance` —
  the previous bin clutter (10 binaries) is reduced to 4.

#### Maintenance pipeline foundation

- New authoritative operator binary `ironrag-maintenance` with grouped
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
- New module `services::maintenance::lease` exposes the authoritative
  lease primitives (`acquire_next_due`, `heartbeat`, `update_cursor`,
  `complete`, `fail`, `clear_dead_letter`, `reap_stale_leases`,
  `count_by_state`). The scheduler tick and the operator CLI both
  depend on the same surface — there is one source of truth for
  "what is currently running, what failed, what to retry next".
- New module `services::maintenance::gc` houses the authoritative
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
  to longer authoritative graph labels and technical chunks, keeping configuration
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
  answer evidence, route wildcard identifier inventories through the authoritative
  answer tool instead of document listings, and keep internal scope wrappers out
  of model-authored answers with a stricter no-continuation-offer boundary.
- Broad comparison answers now preserve module, package, configuration-file,
  and filesystem-path intent through QueryIR compilation and retrieved-document
  briefs, keeping exact technical identifiers near introductory prose in the
  final grounded context without corpus-specific routing or extra retrieval
  fan-out.

### Documents

- The document editor now loads a authoritative editor-markdown representation
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

## 0.3.3 — 2026-08-16

- **Fix:** the connector could not start against a workspace at all. The
  framework's catalog client demanded a bare JSON array from both catalog list
  endpoints, but the library list is a bounded `{items, totalCount, offset,
  limit}` page, so reading it failed before any routing ref could be resolved
  and the process exited on every attempt. Picks up `ironrag-connector` 0.2.2,
  which reads each endpoint with its own reader, walks the library list to
  completion instead of stopping at the first page, treats an incomplete scan
  as an error rather than a silent miss, and reports the scanned/expected
  counts when a ref really is not visible.
- Bumped the release workflow's framework pin to `v0.2.2` so the published
  image carries the fix.
- Refreshed `uv.lock`, which had not been regenerated for the 0.3.2 bump.

## 0.3.2 — 2026-07-29

- **Fix:** the release workflow pinned framework `v0.2.0`, so the 0.3.1 image
  shipped without the `library_id` multipart fix it claimed to carry. Bumped
  the pin to `v0.2.1`.

## 0.3.1 — 2026-07-29

- **Fix:** picks up `ironrag-connector` 0.2.1, which stops sending the
  redundant `library_id` multipart field on document creation. IronRAG rejects
  unknown multipart fields with `400`, so every file-backed upload failed
  before this.

## 0.3.0 — 2026-07-18

- Migrated to connector framework 0.2.0 (redesigned IronRAG REST v2 content
  API): typed document resources, `201`-create / `202`-revision semantics
  with operation polling, and the RFC 9457 problem-details error family.
- Replaced removed framework settings in `.env.example`
  (`IRONRAG_MUTATION_TIMEOUT_SECONDS`,
  `CURSOR_LIBRARY_LOOKUP_TIMEOUT_SECONDS`,
  `CURSOR_LIBRARY_LOOKUP_MAX_ROWS_PER_SWEEP`) with the new
  `OPERATION_POLL_INTERVAL_SECONDS`, `OPERATION_POLL_BUDGET_SECONDS`, and
  `REWALK_CONCURRENCY` knobs; rewrote the README timeout guidance for the
  operation-poll budget model.
- Pinned the release workflow to framework `v0.2.0`.
- Bumped the package version to 0.3.0.

## 0.2.3 — 2026-08-16

- **Fix:** picks up `ironrag-connector` 0.2.2, which repairs catalog reads. The
  library-list endpoint returns a bounded page (`{items, totalCount, offset,
  limit}`) while the workspace-list endpoint returns a bare array, and the
  framework demanded an array from both — so a connector configured with a
  `<workspace-slug>/<library-slug>` routing ref could not resolve it and failed
  during startup instead of ingesting anything.
- **Fix:** library ref resolution now walks every page of the library list
  instead of reading the first one, so a valid ref in a workspace with more
  libraries than fit on one page is no longer reported as not visible. An
  incomplete scan is now an error carrying the row counts rather than a silent
  "not visible".
- Bumped the release workflow pin to framework `v0.2.2`; without it the
  published image would still carry the broken catalog client.

## 0.2.2 — 2026-07-29

- **Fix:** the release workflow pinned framework `v0.2.0`, so the 0.2.1 image
  shipped without the `library_id` multipart fix it claimed to carry. Bumped
  the pin to `v0.2.1`.

## 0.2.1 — 2026-08-16

- Pinned the release workflow to connector framework `v0.2.2`, moving the
  published image two framework versions forward from `v0.2.1`.
- Picks up the framework catalog-client fix (`0.2.2`): the two catalog list
  endpoints have different response shapes — the workspace list returns a bare
  JSON array while the library list returns a bounded `{items, totalCount,
  offset, limit}` page — and a single reader demanded an array from both, so
  startup failed before any routing ref could be resolved and the connector
  never came up. Library ref resolution now also walks every page instead of
  reading one, narrowing server-side with `search` and comparing slugs exactly,
  so a library beyond the first page is no longer reported as not visible.
- Picks up the framework upload fix (`0.2.1`): the redundant `library_id`
  multipart field is no longer sent when creating a document. The library is
  already identified by the path segment, and IronRAG rejects any field outside
  its multipart allow-list, so every file-backed upload failed.
- Bumped the package version to 0.2.1.

## 0.2.0 — 2026-07-18

- Migrated to connector framework 0.2.0 (redesigned IronRAG REST v2 content
  API): typed document resources, `201`-create / `202`-revision semantics with
  operation polling, and the RFC 9457 problem-details error family.
- Rewrote the lifecycle test double to the redesigned client surface
  (`create_document` returning typed resources, revision/delete operations
  polled to a terminal state, typed not-found errors) and updated the e2e
  runtime preparation script and architecture notes to the create/revise
  terminology and the content + operations endpoint surface.
- Replaced removed framework settings in `.env.example`
  (`IRONRAG_MUTATION_TIMEOUT_SECONDS`,
  `CURSOR_LIBRARY_LOOKUP_TIMEOUT_SECONDS`,
  `CURSOR_LIBRARY_LOOKUP_MAX_ROWS_PER_SWEEP`) with the new
  `OPERATION_POLL_INTERVAL_SECONDS`, `OPERATION_POLL_BUDGET_SECONDS`, and
  `REWALK_CONCURRENCY` knobs.
- Pinned the release workflow to framework `v0.2.0`.
- Bumped the package version to 0.2.0.

## 0.1.9 — 2026-06-18

- Build against framework `v0.0.11`: `routing.yaml` and per-kind policy
  overrides are reloaded before each sweep and webhook-handled item, so routing
  rule changes take effect without a connector restart.
- Invalid or temporarily unreadable routing configuration keeps the last valid
  routing state active, avoiding accidental document movement or deletion during
  configuration edits.

## 0.1.8 — 2026-06-18

- Build against framework `v0.0.10`: IronRAG upload, replace, and delete
  admission now fail fast with configurable mutation timeouts and are recorded
  as deferred instead of holding a sweep indefinitely.
- Inherits deferred cursor handling for primary and dependent items, plus a
  bounded post-sweep reaper list call, so a slow backend can be retried by a
  later sweep without advancing cursors for work that was not accepted.

## 0.1.7 — 2026-06-18

- Build against framework `v0.0.9`: one source ref's fetch/push work,
  including dependents, is bounded by `SYNC_ITEM_TIMEOUT_SECONDS`. A stuck item
  is cancelled and counted as an item error so the sweep can continue to
  `sync.done` instead of holding the single-flight sync lock indefinitely.
- Inherits `sync.item.start` and `sync.item_timeout` logs for identifying the
  source ref that is currently in flight or timed out.

## 0.1.6 — 2026-06-18

- Build against framework `v0.0.8`: full sweeps are now single-flight inside
  the connector process, so manual, startup, and periodic triggers cannot run
  overlapping mutation passes against the same cursor database. Concurrent
  manual `/sync/run` requests return HTTP 409 while an existing sweep is active.
- Inherits framework cancellation cleanup for manual sync requests that are
  disconnected before the sweep finishes.

## 0.1.5 — 2026-06-18

- Build against framework `v0.0.7`: large legacy cursor databases now cap
  best-effort document-detail backfill before source enumeration, and the
  same-route path backfills cursor ownership via the target library's
  external-key lookup before trying slower document-detail calls.

## 0.1.4 — 2026-06-18

- Build against framework `v0.0.6`: legacy cursor library backfill is now
  bounded and non-fatal, so a slow document-detail lookup no longer makes
  manual `/sync/run` return 500 before Confluence enumeration starts.
  Unresolved legacy cursor ownership still blocks possible duplicate uploads;
  destructive cleanup for unknown historical libraries is deferred to a later
  sweep.

## 0.1.3 — 2026-06-18

- Build against framework `v0.0.5`: cursor rows now retain the target
  library, orphan scans follow cursor pagination, route moves avoid deleting
  newly-routed documents, legacy cursor rows are backfilled from IronRAG
  document details, and transient conflicting mutations are retried on a later
  sweep without advancing the cursor.
- Tightened connector type annotations so strict mypy passes against the typed
  framework package.

## 0.1.2 — 2026-06-10

- Page attachments and inline images now ingest as attached context of
  their source page instead of as peer documents. Each dependent is
  auto-linked to its page via the framework's `parent_external_key`; no
  config change is required. Inherited from the connector SDK — IronRAG
  derives the child's role from the declared parent and media class.

## 0.1.1 — 2026-05-29

- Pages whose own body is empty but which host attachments are no longer
  dropped. The page is emitted with a synthesized index body listing its
  attachments, and each attachment is ingested as its own document.
  Previously an empty page body caused the whole item — including its
  attachments — to be skipped.
- Build against framework `v0.0.3`: content-addressed idempotency keys
  (fixes `409 idempotency_conflict` on re-rendered page updates),
  single-request external-key lookup via the list `search` filter, and
  cursor document-id persistence on the unchanged path (steady-state
  sweeps no longer re-scan the IronRAG list endpoint).

## 0.1.0 — 2026-07-14

- Replaced workspace and library UUID routing with canonical
  `<workspace-slug>/<library-slug>` references resolved by ConnectorTemplate
  0.1.0.
- Expanded `routing.yaml.example` into a complete strict schema reference with
  every YouTrack fact, item kind, key, type, default, match rule, and policy
  enum.
- Extended the full-stack bootstrap contract with workspace and library slugs.
  Generated routing files now contain only the friendly catalog ref, while
  internal IDs remain isolated to IAM and lifecycle assertions.
- Added regression coverage for friendly runtime generation and migrated both
  real-YouTrack lifecycle fixtures to compiled routing targets.
- Documented all environment constraints and retained the successful isolated
  Docker lifecycle coverage for create, update, empty content, delete, restart,
  reaping, worker chunks, and search.
- Rejects webhook-only and combined run modes because YouTrack Knowledge Base
  has no complete lifecycle webhooks; polling is the only safe mode.
- Pinned release builds to ConnectorTemplate 0.1.0 and bumped the package
  version to 0.1.0.

## 0.0.13 — 2026-06-18

- Build against framework `v0.0.11`: `routing.yaml` and per-kind policy
  overrides are reloaded before each sweep and webhook-handled item, so routing
  rule changes take effect without a connector restart.
- Invalid or temporarily unreadable routing configuration keeps the last valid
  routing state active, avoiding accidental document movement or deletion during
  configuration edits.

## 0.0.12 — 2026-06-18

- Build against framework `v0.0.10`: IronRAG upload/replace/delete admission
  requests now have their own bounded timeout via
  `IRONRAG_MUTATION_TIMEOUT_SECONDS`. When unset, it is derived from the
  generic request timeout and `SYNC_ITEM_TIMEOUT_SECONDS`.
- Inherits framework mutation phase logs and timeout deferral: a slow IronRAG
  mutation admission no longer holds the whole item until the outer item
  timeout fires, and the connector retries the source version on a later sweep
  without advancing the cursor.
- Dependent documents now wait for the next sweep when their primary document
  write is deferred, avoiding extra child mutations inside an already deferred
  item window.
- If a dependent document write is deferred after its primary document was
  accepted, the framework restores the primary cursor to the previous source
  version so the next sweep refetches the parent and retries the dependent.
- Inherits `REAPER_LIST_TIMEOUT_SECONDS`, preventing a slow post-sweep IronRAG
  prefix-list request from holding the connector's single-flight sync lock.

## 0.0.11 — 2026-06-18

- Build against framework `v0.0.9`: one source ref's fetch/push work,
  including dependents, is bounded by `SYNC_ITEM_TIMEOUT_SECONDS`. A stuck item
  is cancelled and counted as an item error so the sweep can continue to
  `sync.done` instead of holding the single-flight sync lock indefinitely.
- Inherits `sync.item.start` and `sync.item_timeout` logs for identifying the
  source ref that is currently in flight or timed out.

## 0.0.10 — 2026-06-18

- Build against framework `v0.0.8`: full sweeps are now single-flight inside
  the connector process, so manual, startup, and periodic triggers cannot run
  overlapping mutation passes against the same cursor database. Concurrent
  manual `/sync/run` requests return HTTP 409 while an existing sweep is active.
- Inherits framework cancellation cleanup for manual sync requests that are
  disconnected before the sweep finishes.

## 0.0.9 — 2026-06-18

- Build against framework `v0.0.7`: large legacy cursor databases now cap
  best-effort document-detail backfill before source enumeration, and the
  same-route path backfills cursor ownership via the target library's
  external-key lookup before trying slower document-detail calls.
- Added the `CURSOR_LIBRARY_LOOKUP_MAX_ROWS_PER_SWEEP` operator knob inherited
  from the framework.

## 0.0.8 — 2026-06-18

- Build against framework `v0.0.6`: legacy cursor library backfill is now
  bounded and non-fatal, so a slow document-detail lookup no longer makes
  manual `/sync/run` return 500 before BookStack enumeration starts. Unresolved
  legacy cursor ownership still blocks possible duplicate uploads; destructive
  cleanup for unknown historical libraries is deferred to a later sweep.
- Optimized full sweeps by inspecting routing rule keys structurally. Default
  routing, and rules satisfied by `/api/pages` summary fields, now yield page
  refs without fetching full page context or refreshing the shelf index.
  Shelf/tag rules still request the full BookStack context.
- Added regression coverage for summary-first routing and shelf/tag fallback.

## 0.0.7 — 2026-06-18

- Build against framework `v0.0.5`: cursor rows now retain the target
  library, orphan scans follow cursor pagination, route moves avoid deleting
  newly-routed documents, legacy cursor rows are backfilled from IronRAG
  document details, and transient conflicting mutations are retried on a later
  sweep without advancing the cursor.
- Tightened connector type annotations so strict mypy passes against the typed
  framework package.

## 0.0.6 — 2026-06-10

- Page attachments and inline images now ingest as attached context of
  their source page instead of as peer documents. Each dependent is
  auto-linked to its page via the framework's `parent_external_key`; no
  config change is required. Inherited from the connector SDK — IronRAG
  derives the child's role from the declared parent and media class.

## 0.0.5 — 2026-06-18

- Reload `repos.yaml` automatically between sweeps when the file changes.
  Include/exclude, branch, repo, and fact-rule changes now affect the next
  clean enumeration without restarting the connector process.
- Build against framework `v0.0.11`, including full cursor-paginated reaper
  listing, route-target-aware cursor handling, transient busy-document replace
  deferral, bounded mutation admission, deferred cursor handling, and bounded
  post-sweep reaper listing. Routing and per-kind policy changes in
  `routing.yaml` are also reloaded before sweeps and webhooks.

## 0.0.4 — 2026-06-10

- The framework can now ingest a page's attachments and inline images as
  attached context of their source document, auto-linked via
  `parent_external_key`. This connector emits one document per text file
  and skips binary blobs, so it has no image/attachment dependents — the
  capability is inherited from the SDK but currently unused here.

## 0.0.3 — 2026-05-30

- Binary blobs are no longer ingested. Detection is content-based — a
  blob is binary iff it carries a NUL byte in its first sniff window
  (the same heuristic git uses), so every text extension is ingested
  (including unknown ones) while images, archives, and compiled
  artifacts are skipped. No extension blocklist is consulted.
- `.gitignore` is honoured implicitly: enumeration walks the committed
  tree via `git ls-tree`, which never lists ignored/untracked files.
- Unknown text files now default to `text/plain` instead of
  `application/octet-stream`.
- Build against framework `v0.0.3` (content-addressed idempotency keys,
  single-request external-key lookup, cursor document-id persistence).

## 0.0.2 — 2026-05-17

- Added per-kind `document_hint` URLs so pages, attachments, and inline images
  point IronRAG citations back to their original BookStack browser URLs.

## 0.0.1 — 2026-07-13

- Added a poll-only YouTrack Knowledge Base connector on ConnectorTemplate
  v0.0.11.
- Added permanent-token validation, context-path-safe REST URLs, explicit
  pagination, retry/backoff, and two-pass snapshot consistency checks.
- Added stable database-ID mapping, project filters and routing facts,
  composite change tokens, deterministic Markdown rendering, and safe handling
  of empty content and attachment metadata.
- Added unit, framework lifecycle, and opt-in Docker E2E coverage for create,
  no-op, update, clear, delete, restart, and partial-enumeration safety.
- Added a reproducible full-stack Docker E2E fixture with real YouTrack,
  IronRAG v0.5.9, worker ingestion, scoped credentials, and a deterministic
  local OpenAI-compatible provider. The lifecycle verifies source revisions,
  knowledge chunks, search, soft deletion, and persisted connector state.
- Added identity revalidation, duplicate-page rejection, fetch-time route
  revalidation, a loopback-safe automated YouTrack bootstrap, and guarded live
  fixture cleanup.
- Added release test gates, safe tag validation, a commit-pinned framework
  checkout, and lockfile-based container dependency installation.
